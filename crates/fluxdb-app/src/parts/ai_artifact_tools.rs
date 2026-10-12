// ---- 工具 ----

/// 一个产物块的 schema：三种块形状互斥。
///
/// 校验器只实现受支持子集，`oneOf` 由 Rust 侧严格解析兜底（serde 拒绝未知字段与
/// 缺少必填字段），因此这里的枚举仍是给模型看的**准确契约**而不是唯一校验点。
fn artifact_block_schema() -> serde_json::Value {
    serde_json::json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "result_ref", "chart", "x", "y"],
                "properties": {
                    "type": {"type": "string", "enum": ["chart"]},
                    "result_ref": {"type": "string", "minLength": 1, "maxLength": 128},
                    "chart": {"type": "string", "enum": ["line", "bar"]},
                    "x": {"type": "string", "minLength": 1, "maxLength": 200},
                    "y": {"type": "string", "minLength": 1, "maxLength": 200}
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "result_ref", "columns"],
                "properties": {
                    "type": {"type": "string", "enum": ["table"]},
                    "result_ref": {"type": "string", "minLength": 1, "maxLength": 128},
                    "columns": {
                        "type": "array", "minItems": 1, "maxItems": 64,
                        "items": {"type": "string", "minLength": 1, "maxLength": 200}
                    }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "text"],
                "properties": {
                    "type": {"type": "string", "enum": ["note"]},
                    "text": {"type": "string", "minLength": 1, "maxLength": 2000}
                }
            }
        ]
    })
}

fn artifact_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "title", "renderer", "sources"],
        "properties": {
            "schema_version": {"type":"integer","enum":[1]},
            "kind": {"type": "string", "enum": ["report", "chart", "table"]},
            "title": {"type": "string", "minLength": 1, "maxLength": 200},
            "renderer": {"type": "string", "minLength": 1, "maxLength": 128},
            "metric": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "timezone"],
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": 200},
                    "definition_ref": {"type": "string", "minLength": 1, "maxLength": 128},
                    "timezone": {"type": "string", "minLength": 1, "maxLength": 64,
                        "description":"UTC 或 +/-HH:MM 固定偏移；IANA 时区名本期不支持"},
                    "assumption": {"type":"string","minLength":1,"maxLength":2000}
                }
            },
            "sources": {
                "type": "array", "minItems": 1, "maxItems": 32,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["result_ref"],
                    "properties": {
                        "result_ref": {"type": "string", "minLength": 1, "maxLength": 128},
                        "tool_call_ref": {"type": "string", "minLength": 1, "maxLength": 128}
                    }
                }
            },
            "blocks": {"type": "array", "maxItems": 64, "items": artifact_block_schema()},
            "time_range": {
                "type":"object","additionalProperties":false,"required":["start","end"],
                "properties": {
                    "start":{"type":"string","minLength":1,"maxLength":64},
                    "end":{"type":"string","minLength":1,"maxLength":64}
                }
            },
            "filters":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":2000}},
            "refresh":{"type":"string","enum":["snapshot"]},
            "template_version":{"type":"string","minLength":1,"maxLength":64},
            "accessibility_description":{"type":"string","maxLength":2000}
        }
    })
}

fn artifact_add_block_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["artifact_id", "block"],
        "properties": {
            "artifact_id": {"type": "string", "minLength": 1, "maxLength": 128},
            "block": artifact_block_schema()
        }
    })
}

fn artifact_finalize_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["artifact_id"],
        "properties": {
            "artifact_id": {"type": "string", "minLength": 1, "maxLength": 128}
        }
    })
}

fn artifact_tool_descriptor(id: &str, description: &str, schema: String) -> fluxdb_core::ToolDescriptor {
    fluxdb_core::ToolDescriptor {
        id: id.into(),
        version: 1,
        description: description.into(),
        // 产物操作只写会话内的声明与受控快照，不碰用户数据库，因此是只读风险。
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json: schema,
    }
}

/// 回执公共部分：产物事实 + 真实数据点；错误用受控 code，不伪造成功。
fn artifact_receipt(
    descriptor: &fluxdb_core::ToolDescriptor,
    call_id: &str,
    started_at_ms: i64,
    result: Result<ArtifactOutcome, ArtifactError>,
    max_points: usize,
) -> fluxdb_core::ToolReceipt {
    let finished_at_ms = ai_chat_now();
    match result {
        Ok(outcome) => {
            let mut points = outcome.points.clone();
            let points_truncated = points.len() > max_points;
            points.truncate(max_points);
            fluxdb_core::ToolReceipt {
                call_id: call_id.to_string(),
                tool_id: descriptor.id.clone(),
                status: fluxdb_core::ToolReceiptStatus::Succeeded,
                result_json: serde_json::json!({
                    "ok": true,
                    "artifact_id": outcome.artifact_id,
                    "status": outcome.status,
                    "revision": outcome.revision,
                    "kind": outcome.kind,
                    "title": outcome.title,
                    "renderer": outcome.renderer,
                    "metric": outcome.metric,
                    "truncated": outcome.truncated || points_truncated,
                    "points_truncated": points_truncated,
                    "sources": outcome.sources.iter().map(|source| serde_json::json!({
                        "result_ref": source.declared_ref,
                        "result_id": source.result_id,
                        "tool_call_id": source.tool_call_id,
                        "snapshot_at_ms": source.snapshot_at_ms,
                        "rows": source.rows,
                        "truncated": source.truncated,
                    })).collect::<Vec<_>>(),
                    "points": points,
                    "file": outcome.file.map(|file| serde_json::json!({
                        "relative_path": file.relative_path,
                        "byte_count": file.byte_count,
                        "content_hash": file.content_hash,
                    })),
                })
                .to_string(),
                error_code: None,
                affected_rows: None,
                started_at_ms,
                finished_at_ms,
            }
        }
        Err(error) => fluxdb_core::ToolReceipt {
            call_id: call_id.to_string(),
            tool_id: descriptor.id.clone(),
            status: if error.code == "cancelled" { fluxdb_core::ToolReceiptStatus::Cancelled } else { fluxdb_core::ToolReceiptStatus::Failed },
            result_json: serde_json::json!({
                "ok": false,
                "code": error.code,
                "message": error.message,
                "retryable": matches!(error.code, "invalid_arguments" | "invalid_spec"
                    | "unknown_field" | "non_numeric_field" | "empty_artifact"),
            })
            .to_string(),
            error_code: Some(error.code.to_string()),
            affected_rows: None,
            started_at_ms,
            finished_at_ms,
        },
    }
}

struct ArtifactCreateTool {
    descriptor: fluxdb_core::ToolDescriptor,
    producer: std::sync::Arc<dyn ArtifactProducer>,
}

impl ArtifactCreateTool {
    fn new(producer: std::sync::Arc<dyn ArtifactProducer>) -> fluxdb_core::Result<Self> {
        let description = format!(
            "创建报告/图表/表格产物草稿：只声明来源结果句柄、字段与口径，数字从真实结果取；不执行 SQL。可用 renderer：{}。来源必须来自同一任务。",
            producer.descriptor().renderers.join(", ")
        );
        Ok(Self {
            descriptor: artifact_tool_descriptor(
                "artifact.create",
                &description,
                artifact_create_schema().to_string(),
            ),
            producer,
        })
    }
}

impl fluxdb_core::AiTool for ArtifactCreateTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = ai_chat_now();
        let producer = std::sync::Arc::clone(&self.producer);
        let descriptor = self.descriptor.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Ok(artifact_cancelled(&descriptor, &ctx.call_id, started_at_ms));
            }
            let result = parse_artifact_spec(&arguments_json)
                .map_err(|message| ArtifactError::new("invalid_arguments", message))
                .and_then(|spec| producer.create(spec, &cancel));
            Ok(artifact_receipt(
                &descriptor,
                &ctx.call_id,
                started_at_ms,
                result,
                BI_RECEIPT_MAX_POINTS,
            ))
        })
    }
}

struct ArtifactAddBlockTool {
    descriptor: fluxdb_core::ToolDescriptor,
    producer: std::sync::Arc<dyn ArtifactProducer>,
}

impl ArtifactAddBlockTool {
    fn new(producer: std::sync::Arc<dyn ArtifactProducer>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: artifact_tool_descriptor(
                "artifact.add_block",
                "给产物草稿增加图表/表格/说明块；块只引用已声明来源的字段，字段必须真实存在",
                artifact_add_block_schema().to_string(),
            ),
            producer,
        })
    }
}

impl fluxdb_core::AiTool for ArtifactAddBlockTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = ai_chat_now();
        let producer = std::sync::Arc::clone(&self.producer);
        let descriptor = self.descriptor.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Ok(artifact_cancelled(&descriptor, &ctx.call_id, started_at_ms));
            }
            let result = parse_add_block(&arguments_json)
                .map_err(|message| ArtifactError::new("invalid_arguments", message))
                .and_then(|(artifact_id, block)| producer.add_block(&artifact_id, block, &cancel));
            Ok(artifact_receipt(
                &descriptor,
                &ctx.call_id,
                started_at_ms,
                result,
                BI_RECEIPT_MAX_POINTS,
            ))
        })
    }
}

struct ArtifactFinalizeTool {
    descriptor: fluxdb_core::ToolDescriptor,
    producer: std::sync::Arc<dyn ArtifactProducer>,
}

impl ArtifactFinalizeTool {
    fn new(producer: std::sync::Arc<dyn ArtifactProducer>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: artifact_tool_descriptor(
                "artifact.finalize",
                "完成产物：复核来源仍可解析、字段与数值一致后渲染快照并提交；renderer 失败时产物标记失败且不改写来源结果",
                artifact_finalize_schema().to_string(),
            ),
            producer,
        })
    }
}

impl fluxdb_core::AiTool for ArtifactFinalizeTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = ai_chat_now();
        let producer = std::sync::Arc::clone(&self.producer);
        let descriptor = self.descriptor.clone();
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Ok(artifact_cancelled(&descriptor, &ctx.call_id, started_at_ms));
            }
            let result = parse_finalize(&arguments_json)
                .map_err(|message| ArtifactError::new("invalid_arguments", message))
                .and_then(|artifact_id| producer.finalize(&artifact_id, &cancel));
            Ok(artifact_receipt(
                &descriptor,
                &ctx.call_id,
                started_at_ms,
                result,
                BI_RECEIPT_MAX_POINTS,
            ))
        })
    }
}

fn artifact_cancelled(
    descriptor: &fluxdb_core::ToolDescriptor,
    call_id: &str,
    started_at_ms: i64,
) -> fluxdb_core::ToolReceipt {
    fluxdb_core::ToolReceipt {
        call_id: call_id.to_string(),
        tool_id: descriptor.id.clone(),
        status: fluxdb_core::ToolReceiptStatus::Cancelled,
        result_json: serde_json::json!({
            "ok": false,
            "code": "cancelled",
            "message": "产物操作已取消",
            "retryable": false,
        })
        .to_string(),
        error_code: Some("cancelled".into()),
        affected_rows: None,
        started_at_ms,
        finished_at_ms: ai_chat_now(),
    }
}

/// 解析 `artifact.create` 入参：schema 只做粗筛，这里做严格构造并补 schema 版本。
fn parse_artifact_spec(arguments_json: &str) -> Result<fluxdb_core::ArtifactSpec, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(arguments_json).map_err(|_| "工具入参不是合法 JSON".to_string())?;
    let object = value.as_object_mut().ok_or_else(|| "产物声明必须是对象".to_string())?;
    object.entry("schema_version").or_insert(serde_json::json!(fluxdb_core::ARTIFACT_SCHEMA_VERSION));
    let mut spec: fluxdb_core::ArtifactSpec = serde_json::from_value(value)
        .map_err(|error| format!("产物声明不合法: {error}"))?;
    if spec.accessibility_description.trim().is_empty() {
        spec.accessibility_description = spec.title.clone();
    }
    Ok(spec)
}

fn parse_add_block(
    arguments_json: &str,
) -> Result<(String, fluxdb_core::ArtifactBlock), String> {
    let value: serde_json::Value =
        serde_json::from_str(arguments_json).map_err(|_| "工具入参不是合法 JSON".to_string())?;
    let artifact_id = string_field(&value, "artifact_id")?;
    let block = value
        .get("block")
        .ok_or_else(|| "缺少 block".to_string())
        .and_then(|raw| {
            serde_json::from_value::<fluxdb_core::ArtifactBlock>(raw.clone())
                .map_err(|error| format!("产物块不合法: {error}"))
        })?;
    Ok((artifact_id, block))
}

fn parse_finalize(arguments_json: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(arguments_json).map_err(|_| "工具入参不是合法 JSON".to_string())?;
    string_field(&value, "artifact_id")
}

fn string_field(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("缺少字段 {key}"))
}

/// `builtin.bi` 启用时才装配产物工具；关闭时不会给模型任何产物入口。
fn bi_enabled(conn: &rusqlite::Connection, session_id: &str) -> fluxdb_core::Result<bool> {
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .iter().any(|instance| instance.id == BI_INSTANCE_ID && instance.enabled
            && instance.runtime_kind == "builtin" && instance.package_key == "fluxdb.output.bi");
    if !enabled {
        return Ok(false);
    }
    let session = fluxdb_storage::ai_platform::sessions::load_session(conn, session_id)?;
    let bindings = fluxdb_storage::ai_platform::plugins::list_agent_plugin_bindings(conn, &session.agent_id)?;
    Ok(!bindings.iter().any(|binding| binding.plugin_instance_id == BI_INSTANCE_ID && !binding.enabled))
}

/// 装配产物工具（内置 BI producer）：仅在 `builtin.bi` 启用时注册。
pub fn add_ai_artifact_tools(
    catalog: &mut ToolCatalog,
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
) -> fluxdb_core::Result<()> {
    if !with_ai_storage(storage, |conn| bi_enabled(conn, &job.session_id))? {
        return Ok(());
    }
    let producer = BuiltinBiProducer::new(
        storage.clone(),
        job.session_id.clone(),
        job.turn_id.clone(),
        job.assistant_message_id.clone(),
        results,
    );
    add_ai_artifact_tools_with_producer(catalog, std::sync::Arc::new(producer))
}

/// 用指定 producer 装配产物工具（能力可替换：换 producer 即换掉整套产物规则）。
pub fn add_ai_artifact_tools_with_producer(
    catalog: &mut ToolCatalog,
    producer: std::sync::Arc<dyn ArtifactProducer>,
) -> fluxdb_core::Result<()> {
    let shape = ToolOutputShape::Sample {
        keep_rows: 0,
        omit_field: "points".into(),
        rows_omitted_field: None,
    };
    let tools: [Box<dyn fluxdb_core::AiTool>; 3] = [
        Box::new(ArtifactCreateTool::new(producer.clone())?),
        Box::new(ArtifactAddBlockTool::new(producer.clone())?),
        Box::new(ArtifactFinalizeTool::new(producer)?),
    ];
    for tool in tools {
        let mut spec = ToolSpec::from_descriptor(tool.descriptor())?;
        // 不修改目标数据库，但会修改本会话产物，不能声明成 PureRead。
        spec.effect = EffectClass::Mutating;
        spec.output_shape = shape.clone();
        if catalog.describe(&spec.id).is_some() {
            return Err(Error::new(ErrorKind::Internal, "产物工具 ID 重复"));
        }
        catalog.specs.push(spec);
        catalog.register(tool)?;
    }
    Ok(())
}
