// 工具输出预算与有界历史：只改变模型投影，原始执行事实保持不变。
// 普通回执默认预算：对齐同类产品的常规量级（Pi/OpenCode 默认 50 KiB），
// 取 32 KiB 在“一次给足”与上下文占用之间折中；仅远超时才落快照续读。
pub const TOOL_AUTO_OUTPUT_BYTES: usize = 32 * 1024;
pub const TOOL_AUTO_OUTPUT_TOKENS: usize = 8_192;
// 主动读取不应比自动下发更少：模型明确要的内容至少要能拿到普通回执额度。
pub const TOOL_READ_OUTPUT_BYTES: usize = TOOL_AUTO_OUTPUT_BYTES;
pub const TOOL_READ_OUTPUT_TOKENS: usize = TOOL_AUTO_OUTPUT_TOKENS;
// 跨轮历史事实只是定位/状态摘要，独立设限，不复用回执预算。
const TOOL_HISTORY_FACT_BYTES: usize = 4 * 1024;
const TOOL_HISTORY_FACT_TOKENS: usize = 1_000;
// 轮内历史预算必须 >= 单条回执上限：否则一条满额回执就放不下，循环会提前暂停。
const TOOL_HISTORY_BYTES: usize = 64 * 1024;
const TOOL_HISTORY_TOKENS: usize = 16_000;

#[derive(Clone, Copy, Debug)]
pub struct ToolOutputBudget {
    pub max_bytes: usize,
    pub max_tokens: usize,
}

impl ToolOutputBudget {
    fn for_spec(spec: &ToolSpec) -> Self {
        // 主动读取与普通回执共用常规量级；Opaque（元数据/契约类）也在此档，
        // 只有远超 32 KiB 才落快照续读。Sample 仍按条目裁完整前缀。
        let (bytes, tokens) = match spec.output_shape {
            ToolOutputShape::Read => (TOOL_READ_OUTPUT_BYTES, TOOL_READ_OUTPUT_TOKENS),
            _ => (TOOL_AUTO_OUTPUT_BYTES, TOOL_AUTO_OUTPUT_TOKENS),
        };
        Self {
            max_bytes: bytes.min(spec.output_limit_bytes as usize),
            max_tokens: tokens,
        }
    }

    fn fits(self, text: &str) -> bool {
        text.len() <= self.max_bytes && estimate_tokens(text) as usize <= self.max_tokens
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProjection {
    pub text: String,
    pub truncated: bool,
    pub result_ref: Option<ResultRef>,
}

pub fn project_receipt(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    max_bytes: usize,
) -> ModelProjection {
    project_receipt_with_shape(receipt, result_ref, max_bytes, &ToolOutputShape::Opaque)
}

/// 显式字节上限入口；生产发送使用同时约束 token 的 `ToolCatalog::project`。
pub fn project_receipt_with_shape(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    max_bytes: usize,
    shape: &ToolOutputShape,
) -> ModelProjection {
    project_receipt_with_budget(
        receipt,
        result_ref,
        ToolOutputBudget {
            max_bytes,
            max_tokens: usize::MAX,
        },
        shape,
    )
}

fn result_reference_json(reference: &ResultRef) -> serde_json::Value {
    serde_json::json!({
        "id": reference.id, "tool_id": reference.tool_id,
        "created_at_ms": reference.created_at_ms,
        "scope": "turn", "expires_on": "execution_end",
        "content_type": match reference.format {
            ResultFormat::Items => "items",
            ResultFormat::OutputFragments => "output_fragments",
        },
    })
}

/// 集合只裁完整前缀；未知结构和分段读取不随机删除业务字段或伪造续读游标。
pub fn project_receipt_with_budget(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    budget: ToolOutputBudget,
    shape: &ToolOutputShape,
) -> ModelProjection {
    let raw = &receipt.result_json;
    let value = serde_json::from_str::<serde_json::Value>(raw).ok();
    let encoded = if value.is_some() {
        raw.clone()
    } else {
        serde_json::Value::String(raw.clone()).to_string()
    };
    if budget.fits(&encoded) {
        return ModelProjection {
            text: encoded,
            truncated: false,
            result_ref,
        };
    }
    if let (
        Some(value),
        ToolOutputShape::Sample {
            omit_field,
            rows_omitted_field,
        },
    ) = (&value, shape)
        && let Some(items) = value.get(omit_field).and_then(serde_json::Value::as_array)
    {
        let prefix = |count: usize| {
            let mut candidate = value.clone();
            candidate[omit_field] = serde_json::Value::Array(items[..count].to_vec());
            let previous = rows_omitted_field
                .as_ref()
                .and_then(|field| value[field].as_u64())
                .unwrap_or(0);
            let omitted = items.len() - count;
            if let Some(field) = rows_omitted_field {
                candidate[field] = serde_json::json!(previous + omitted as u64);
            }
            candidate["delivery_truncated"] = serde_json::json!(true);
            candidate["returned_items"] = serde_json::json!(count);
            // 原工具的游标对应整页，不对应投影前缀；不可用它跳过未展示的条目。
            if candidate.get("next_cursor").is_some() {
                candidate["source_next_cursor"] = candidate["next_cursor"].clone();
                candidate.as_object_mut().unwrap().remove("next_cursor");
            }
            if let Some(reference) = &result_ref {
                candidate["result_ref"] = result_reference_json(reference);
                if omit_field == "rows" && reference.format == ResultFormat::Items {
                    candidate["next_cursor"] =
                        serde_json::json!(format!("{}:{count}", reference.id));
                }
            }
            candidate.to_string()
        };
        let empty = prefix(0);
        if budget.fits(&empty) {
            let (mut low, mut high) = (0, items.len());
            while low < high {
                let mid = low + (high - low).div_ceil(2);
                if budget.fits(&prefix(mid)) {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            return ModelProjection {
                text: prefix(low),
                truncated: true,
                result_ref,
            };
        }
    }
    let text = output_snapshot_marker(receipt, value.as_ref(), result_ref.as_ref(), budget);
    ModelProjection {
        text,
        truncated: true,
        result_ref,
    }
}

fn output_snapshot_marker(
    receipt: &fluxdb_core::ToolReceipt,
    value: Option<&serde_json::Value>,
    reference: Option<&ResultRef>,
    budget: ToolOutputBudget,
) -> String {
    let mut marker = serde_json::json!({
        "delivery_truncated": true,
        "status": loop_tool_status_label(receipt.status),
        "execution_status": loop_tool_status_label(receipt.status),
        "error_code": receipt.error_code,
        "affected_rows": receipt.affected_rows,
        "details_complete": false,
        "message": if reference.is_some_and(|reference| reference.format == ResultFormat::OutputFragments) {
            "详细输出已保存；用 result.read_page 从头读取并按 offset_bytes 重组本轮快照，不重新执行来源工具。"
        } else if reference.is_some() {
            "数据条目可用 result.read_page 读取；被省略的其他元数据不在条目快照中，请缩小来源请求。"
        } else {
            "输出超过预算；没有可用续读快照，请缩小读取范围，不可把省略当作空结果。"
        },
    });
    if let Some(reference) = reference {
        marker["result_ref"] = result_reference_json(reference);
    }
    // 执行结果的关键状态优先保留；其余内容只作为快照存在，不猜测字段用途。
    if let Some(object) = value.and_then(serde_json::Value::as_object) {
        let mut omitted = Vec::new();
        let fields = [
            "ok",
            "status",
            "code",
            "message",
            "error",
            "warnings",
            "partial",
            "unknown",
            "exit_code",
            "task_id",
            "artifact_id",
            "file_ref",
            "resource_ref",
            "content_type",
            "mime_type",
            "affected_rows",
        ];
        let reservation = serde_json::json!({"omitted_detail_fields":fields}).to_string();
        let detail_budget = ToolOutputBudget {
            max_bytes: budget.max_bytes.saturating_sub(reservation.len()),
            max_tokens: budget
                .max_tokens
                .saturating_sub(estimate_tokens(&reservation) as usize),
        };
        for key in fields {
            if let Some(field) = object.get(key) {
                let previous = marker.get(key).cloned();
                marker[key] = field.clone();
                if !detail_budget.fits(&marker.to_string()) {
                    if let Some(previous) = previous {
                        marker[key] = previous;
                    } else {
                        marker.as_object_mut().unwrap().remove(key);
                    }
                    omitted.push(key);
                }
            }
        }
        if !omitted.is_empty() {
            marker["omitted_detail_fields"] = serde_json::json!(omitted);
            marker["details_complete"] = serde_json::json!(false);
        }
    }
    let text = marker.to_string();
    if budget.fits(&text) {
        return text;
    }
    let compact =
        serde_json::json!({"error":"tool_output_budget_exceeded", "delivery_truncated":true})
            .to_string();
    if budget.fits(&compact) {
        return compact;
    }
    if budget.fits("null") {
        "null".into()
    } else if budget.fits("{}") {
        "{}".into()
    } else if budget.fits("0") {
        "0".into()
    } else {
        String::new()
    }
}

/// 收紧旧回执但保留调用配对和最新读取页；最小协议也放不下时停止发送。
fn bounded_tool_history(
    messages: &[fluxdb_core::ModelMessage],
) -> Result<Vec<fluxdb_core::ModelMessage>, String> {
    let mut result = messages.to_vec();
    let indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == fluxdb_core::ModelRole::Tool)
        .map(|(index, _)| index)
        .collect();
    let size = |text: &str| (text.len(), estimate_tokens(text) as usize);
    let (original_bytes, original_tokens) = indices
        .iter()
        .map(|index| size(&messages[*index].text))
        .fold((0usize, 0usize), |(bytes, tokens), (b, t)| {
            (bytes + b, tokens + t)
        });
    if original_bytes <= TOOL_HISTORY_BYTES && original_tokens <= TOOL_HISTORY_TOKENS {
        return Ok(result);
    }
    for index in &indices {
        let message = &mut result[*index];
        let value = serde_json::from_str::<serde_json::Value>(&message.text).unwrap_or_default();
        let detail = value.get("result").unwrap_or(&value);
        let mut summary = serde_json::json!({
            "tool": message.tool_name, "status": value["status"],
            "history_details_omitted": true, "result_ref": detail["result_ref"],
        });
        for key in [
            "error_code",
            "code",
            "affected_rows",
            "ok",
            "partial",
            "unknown",
        ] {
            if let Some(field) = detail.get(key) {
                summary[key] = field.clone();
            }
        }
        message.text = summary.to_string();
    }
    let (mut bytes, mut tokens) = indices
        .iter()
        .map(|index| size(&result[*index].text))
        .fold((0usize, 0usize), |(bytes, tokens), (b, t)| {
            (bytes + b, tokens + t)
        });
    if bytes > TOOL_HISTORY_BYTES || tokens > TOOL_HISTORY_TOKENS {
        return Err(
            "工具历史的最小状态与调用配对已超过预算，已暂停；请开始新轮或减少批量工具调用".into(),
        );
    }
    for index in indices.iter().rev() {
        let (old_bytes, old_tokens) = size(&result[*index].text);
        let (new_bytes, new_tokens) = size(&messages[*index].text);
        let candidate_bytes = bytes - old_bytes + new_bytes;
        let candidate_tokens = tokens - old_tokens + new_tokens;
        if candidate_bytes <= TOOL_HISTORY_BYTES && candidate_tokens <= TOOL_HISTORY_TOKENS {
            result[*index] = messages[*index].clone();
            bytes = candidate_bytes;
            tokens = candidate_tokens;
        } else if Some(index) == indices.last() {
            return Err(
                "最新工具输出与必要历史超过上下文预算，未丢弃读取内容；请缩小读取范围".into(),
            );
        }
    }
    Ok(result)
}
