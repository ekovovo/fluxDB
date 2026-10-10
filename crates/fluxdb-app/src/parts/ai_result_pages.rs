// 结果分页读取（设计 §20.1 `result.read_page`，T027）。
//
// 契约：
// - 只读已取得的快照切片，**不重新执行来源工具**（不连数据库、不重跑 SQL/Redis 命令）；
// - 句柄归属会话校验在 `ResultStore::read_page` 内完成，跨会话按未知处理；
// - 过期/已释放/未知/游标非法/spill 损坏都是受控错误，不静默返回空页；
// - 行数与字节双上限，超限标记 `truncated` 并给出 `next_cursor`，模型据此决定是否续读。

/// `result.read_page` 的输入 schema。
pub const RESULT_READ_PAGE_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["result_ref"],
  "properties": {
    "result_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id"],
      "properties": {
        "id": { "type": "string", "minLength": 1, "maxLength": 128 },
        "tool_id": { "type": "string", "maxLength": 128 },
        "created_at_ms": { "type": "integer", "minimum": 0 }
      }
    },
    "cursor": { "type": "string", "maxLength": 256 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 },
    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 262144, "default": 65536 }
  }
}"#;

/// `result.read_page` 工具定义（公共只读工具，与数据库类型无关）。
pub fn result_read_page_spec() -> fluxdb_core::Result<ToolSpec> {
    Ok(ToolSpec::from_descriptor(&fluxdb_core::ToolDescriptor {
        id: "result.read_page".into(),
        version: 1,
        description: "按 ResultRef 游标分页读取已取得的结果快照；不重新执行来源工具".into(),
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json: RESULT_READ_PAGE_INPUT_SCHEMA.into(),
    })?)
}

/// 解析 `result.read_page` 入参（已过 schema 校验，这里只做取值与默认）。
struct ResultReadPageArgs {
    id: String,
    cursor: Option<String>,
    limit: usize,
    max_bytes: usize,
}

fn parse_result_read_page_args(arguments_json: &str) -> Result<ResultReadPageArgs, String> {
    let value: serde_json::Value =
        serde_json::from_str(arguments_json).map_err(|_| "工具入参不是合法 JSON".to_string())?;
    let id = value
        .get("result_ref")
        .and_then(|value| value.get("id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "缺少 result_ref.id".to_string())?
        .to_string();
    let limit = value
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let max_bytes = value
        .get("max_bytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(65_536)
        .clamp(1, 262_144) as usize;
    Ok(ResultReadPageArgs {
        id,
        cursor: value
            .get("cursor")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        limit,
        max_bytes,
    })
}

/// 结果分页读取工具：读已登记的结果句柄，不触碰任何数据库连接。
pub struct AiResultReadPageTool {
    descriptor: fluxdb_core::ToolDescriptor,
    results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
}

impl AiResultReadPageTool {
    pub fn new(
        results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        let spec = result_read_page_spec()?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: spec.input_schema.to_string(),
            },
            results,
        })
    }
}

impl fluxdb_core::AiTool for AiResultReadPageTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = loop_now_ms();
        let results = std::sync::Arc::clone(&self.results);
        let descriptor = self.descriptor.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            let finished_at_ms = loop_now_ms();
            let receipt = |status: fluxdb_core::ToolReceiptStatus,
                          value: serde_json::Value,
                          error_code: Option<&str>| fluxdb_core::ToolReceipt {
                call_id: ctx.call_id.clone(),
                tool_id: descriptor.id.clone(),
                status,
                result_json: value.to_string(),
                error_code: error_code.map(str::to_string),
                affected_rows: None,
                started_at_ms,
                finished_at_ms,
            };
            if cancel.is_cancelled() {
                return Ok(receipt(
                    fluxdb_core::ToolReceiptStatus::Cancelled,
                    serde_json::json!({
                        "ok": false,
                        "code": "cancelled",
                        "message": "结果分页读取已取消",
                    }),
                    Some("cancelled"),
                ));
            }
            let args = match parse_result_read_page_args(&arguments_json) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(receipt(
                        fluxdb_core::ToolReceiptStatus::Failed,
                        serde_json::json!({
                            "ok": false,
                            "code": "invalid_arguments",
                            "message": message,
                        }),
                        Some("invalid_arguments"),
                    ));
                }
            };
            let store = match results.lock() {
                Ok(store) => store,
                Err(_) => {
                    return Ok(receipt(
                        fluxdb_core::ToolReceiptStatus::Failed,
                        serde_json::json!({
                            "ok": false,
                            "code": "result_store_unavailable",
                            "message": "结果仓库不可用",
                        }),
                        Some("result_store_unavailable"),
                    ));
                }
            };
            match store.read_page(&args.id, args.cursor.as_deref(), args.limit, args.max_bytes) {
                Ok(page) => Ok(receipt(
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    serde_json::json!({
                        "ok": true,
                        "result_ref": {
                            "id": page.result.id,
                            "tool_id": page.result.tool_id,
                            "created_at_ms": page.result.created_at_ms,
                        },
                        "items": page.items,
                        "returned_items": page.items.len(),
                        "total_items": page.total_items,
                        "truncated": page.truncated,
                        "next_cursor": page.next_cursor,
                    }),
                    None,
                )),
                Err(error) => Ok(receipt(
                    fluxdb_core::ToolReceiptStatus::Failed,
                    serde_json::json!({
                        "ok": false,
                        "code": error.code,
                        "message": error.message,
                        // 句柄不可读时不可重试：重复同一句柄不会自己变可读。
                        "retryable": false,
                    }),
                    Some(error.code),
                )),
            }
        })
    }
}
