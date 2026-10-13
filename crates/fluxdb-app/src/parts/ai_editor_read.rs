// `editor.read_range`：按行区间读取本轮装配时捕获的编辑器全文（设计 §13.2）。
//
// 为什么全文只在工具实例里、而不进运行快照：§13.2 第 5 条要求"捕获存引用+区间，
// 不把整份内容永久复制进会话"。`runtime_snapshot_json` 是持久化的，把编辑器全文
// 写进去会让会话历史无限膨胀，也留下用户已经改掉的历史原文。
//
// 装配时机是发送那一刻（宿主持有编辑器文本），因此工具读到的是**发送时的快照**
// 而非实时内容——回执里用 `scope` 明确说明，避免模型当成当前屏幕上的真实文本。
//
// 与 Zed `read_file_tool` 的差别：Zed 的 agent 与编辑器同处一个事件循环，工具
// 签名带 `&mut App` 可直接开 buffer；fluxDB 的工具跑在后台 actor 线程，没有
// GPUI 上下文，所以改由宿主预取后随目录装配（对齐 `file.read`/`context.read`
// 拿 grant 的既有模式）。

use fluxdb_core::{ToolExecutionContext, ToolReceipt, ToolReceiptStatus};

/// `editor.read_range` 输入 schema：与 `context.read`/`file.read` 同样按字节有界。
pub const EDITOR_READ_RANGE_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "properties": {
    "tab_ref": {"type": "string", "description": "发送时的编辑器引用；来自界面摘录中的 editor_ref"},
    "offset": {"type": "integer", "minimum": 0, "description": "起始行号（0 起）"},
    "limit": {"type": "integer", "minimum": 1, "maximum": 400, "description": "本次最多读取的行数"},
    "byte_offset": {"type": "integer", "minimum": 0, "description": "仅续读被截断的超长行时使用：同一行的字节偏移，默认从行首开始"}
  },
  "required": ["offset"],
  "additionalProperties": false
}"##;

/// 单次读取的字节上限：与界面摘录的 16 KiB 同量级，避免一次续读顶满上下文。
pub const EDITOR_READ_MAX_BYTES: usize = 16 * 1024;

/// 装配时捕获的编辑器内容（仅本轮内存持有，不落盘）。
#[derive(Clone, Debug)]
pub struct EditorGrant {
    pub tab_ref: String,
    pub snapshot: fluxdb_editor_core::BufferSnapshot,
}

/// `editor.read_range` 工具实现。
struct AiEditorReadRangeTool {
    descriptor: fluxdb_core::ToolDescriptor,
    grant: EditorGrant,
}

/// `editor.search`：与 `editor.read_range` 共用发送时的快照，避免编辑后行号漂移。
struct AiEditorSearchTool {
    descriptor: fluxdb_core::ToolDescriptor,
    grant: EditorGrant,
}

impl fluxdb_core::AiTool for AiEditorReadRangeTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, ToolReceipt> {
        Box::pin(async move {
            let now = now_ms();
            // 受控失败也回执而不是 Err：Err 会让整个 Turn 失败，模型没有机会自我修正。
            let (status, value, error_code) = match read_window(&self.grant, &ctx, &arguments_json)
            {
                Ok(value) => (ToolReceiptStatus::Succeeded, value, None),
                Err(error) => {
                    let status = if ctx.cancel.is_cancelled() {
                        ToolReceiptStatus::Cancelled
                    } else {
                        ToolReceiptStatus::Failed
                    };
                    (
                        status,
                        serde_json::json!({
                            "error": error.message,
                            // 标签页已关闭/参数越界都不是重试能解决的，明确告知模型。
                            "retryable": false,
                        }),
                        Some("editor_unavailable".to_string()),
                    )
                }
            };
            Ok(ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: value.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: now_ms(),
            })
        })
    }
}

impl fluxdb_core::AiTool for AiEditorSearchTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, ToolReceipt> {
        Box::pin(async move {
            let now = now_ms();
            let result = search_editor_content(&self.grant, &ctx, &arguments_json);
            let (status, value, error_code) = match result {
                Ok(value) => (ToolReceiptStatus::Succeeded, value, None),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        ToolReceiptStatus::Cancelled
                    } else {
                        ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error": error.message, "retryable": false}),
                    Some("editor_unavailable".to_string()),
                ),
            };
            Ok(ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: value.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: now_ms(),
            })
        })
    }
}

fn editor_error(message: impl Into<String>) -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message)
}

fn read_window(
    grant: &EditorGrant,
    ctx: &ToolExecutionContext,
    arguments_json: &str,
) -> fluxdb_core::Result<serde_json::Value> {
    if ctx.cancel.is_cancelled() {
        return Err(editor_error("编辑器续读已取消"));
    }
    let args: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|_| editor_error("编辑器续读参数不是合法 JSON"))?;
    // 只允许读本轮装配的那一个标签页：模型不得凭猜测读别的编辑器内容。
    if let Some(tab_ref) = args["tab_ref"].as_str()
        && tab_ref != grant.tab_ref
    {
        return Err(editor_error(format!(
            "tab_ref 不属于本轮任务：仅可读取 {}",
            grant.tab_ref
        )));
    }
    let snapshot = &grant.snapshot;
    let total_lines = snapshot.line_count();
    let offset = args["offset"].as_u64().unwrap_or(0);
    let offset = usize::try_from(offset).map_err(|_| editor_error("offset 超出范围"))?;
    if offset >= total_lines {
        return Err(editor_error(format!(
            "offset 超出范围：起始行 {offset} 已超过全文行数 {total_lines}"
        )));
    }
    let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 400) as usize;
    let start_byte_offset = args["byte_offset"]
        .as_u64()
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize;
    let line_start = snapshot.line_start(offset);
    let mut line_end = snapshot.line_end_offset(offset);
    if snapshot.byte_at(line_end.saturating_sub(1)) == Some(b'\n') {
        line_end -= 1;
    }
    let start = line_start.saturating_add(start_byte_offset);
    if start > line_end || snapshot.clamp_to_char_boundary(start) != start {
        return Err(editor_error("byte_offset 不在起始行的 UTF-8 边界"));
    }

    // 按行取窗口，再按字节兜底。
    let end_line = total_lines.min(offset.saturating_add(limit));
    let mut selected: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    // 单行本身就超限时不能整行丢掉——否则 next_offset 不前进，模型无法续读。
    // 这里按字符边界切该行并标记 partial_line，用 next_byte_offset 续读余下部分
    // （对齐 Pi read.ts 对超长单行的处理：宁可给可续读的片段，也不给空结果）。
    let mut partial_line: Option<(usize, usize)> = None;
    for row in offset..end_line {
        let index = row - offset;
        let line_start = snapshot.line_start(row);
        let mut line_end = snapshot.line_end_offset(row);
        if snapshot.byte_at(line_end.saturating_sub(1)) == Some(b'\n') {
            line_end -= 1;
        }
        let line_byte_offset = if index == 0 { start_byte_offset } else { 0 };
        let start = line_start + line_byte_offset;
        let available = line_end.saturating_sub(start);
        let has_line_break = offset + index + 1 < total_lines;
        let size = available.saturating_add(usize::from(has_line_break));
        if bytes + size <= EDITOR_READ_MAX_BYTES {
            bytes += size;
            selected.push(snapshot.text_in_range(fluxdb_editor_core::Range::new(start, line_end)));
            continue;
        }
        // 字节预算只约束本次实际返回的剩余内容：按起始偏移裁剪，避免已经读过的
        // 前缀再次计入预算而让续读位置停在一半。
        let room = EDITOR_READ_MAX_BYTES.saturating_sub(bytes);
        let cut = snapshot.clamp_to_char_boundary(start.saturating_add(room).min(line_end));
        if cut > start {
            selected.push(snapshot.text_in_range(fluxdb_editor_core::Range::new(start, cut)));
            if cut < line_end {
                partial_line = Some((row, cut - line_start));
            }
        } else if available == 0 {
            // 空行本身没有正文可返回；消费该行使 next_offset 继续前进。
            selected.push(String::new());
        }
        truncated = true;
        break;
    }
    let consumed_lines = selected.len();
    let has_partial_line = partial_line.is_some();
    let next_offset = if let Some((row, _)) = partial_line {
        row
    } else {
        offset.saturating_add(consumed_lines)
    };
    let has_more = has_partial_line || next_offset < total_lines;

    Ok(serde_json::json!({
        "tab_ref": grant.tab_ref,
        "lines": selected,
        "line_numbers": {"from": offset, "to": offset + consumed_lines.saturating_sub(1)},
        "offset": offset,
        "byte_offset": start_byte_offset,
        "next_offset": next_offset,
        "total_lines": total_lines,
        "has_more": has_more,
        "truncated": truncated || has_more,
        // 超长单行被切时给出续读位置，否则模型只能看到不完整的行却无从继续。
        "partial_line": partial_line.map(|(line_index, next_byte_offset)| serde_json::json!({
            "line": line_index,
            "next_byte_offset": next_byte_offset,
            "note": "用 offset=partial_line.line 和 byte_offset=partial_line.next_byte_offset 续读",
        })),
        "untrusted": true,
        "scope": "发送本轮时捕获的编辑器快照，不是实时内容；用户可能已继续编辑",
    }))
}

fn search_editor_content(
    grant: &EditorGrant,
    ctx: &ToolExecutionContext,
    arguments_json: &str,
) -> fluxdb_core::Result<serde_json::Value> {
    if ctx.cancel.is_cancelled() {
        return Err(editor_error("编辑器搜索已取消"));
    }
    let args: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|_| editor_error("编辑器搜索参数不是合法 JSON"))?;
    if let Some(tab_ref) = args["tab_ref"].as_str()
        && tab_ref != grant.tab_ref
    {
        return Err(editor_error(format!(
            "tab_ref 不属于本轮任务：仅可搜索 {}",
            grant.tab_ref
        )));
    }
    let (query, options) = search_options_from_arguments(arguments_json)?;
    // 全文仅在后台搜索时物化；两个工具持有同一不可变 Rope 快照，不在 UI 线程复制。
    let result = search_text_content(&grant.snapshot.to_string(), &query, options, &ctx.cancel)?;
    let mut value = search_match_json(&result);
    value["tab_ref"] = serde_json::json!(grant.tab_ref);
    value["scope"] = serde_json::json!("发送本轮时捕获的编辑器快照，不是实时内容");
    Ok(with_editor_read_hints(value))
}

fn with_editor_read_hints(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(matches) = value["matches"].as_array_mut() {
        for found in matches {
            let line_number = found["line_number"].as_u64().unwrap_or(1);
            found["read_hint"] = serde_json::json!({
                "offset": line_number.saturating_sub(1),
                "byte_offset": 0,
                "note": "用 editor.read_range 的 offset 读取命中行；超长行按 partial_line.next_byte_offset 续读"
            });
        }
    }
    value
}

/// 装配 `editor.read_range`；没有编辑器内容时不注册（模型不会发现一个调不动的工具）。
///
/// 全文只留在工具实例里，随本轮目录释放，不写入 `ai_turns.runtime_snapshot_json`。
pub fn add_ai_editor_read_tools(
    catalog: &mut ToolCatalog,
    grant: Option<EditorGrant>,
) -> fluxdb_core::Result<()> {
    let Some(grant) = grant else {
        return Ok(());
    };
    if grant.snapshot.is_empty() {
        return Ok(());
    }
    catalog.add_tool_with_shape(
        Box::new(AiEditorReadRangeTool {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "editor.read_range".into(),
                version: 1,
                description: "按行区间读取发送本轮时捕获的编辑器全文，用于续读界面摘录中被省略的中段；不读取未授权的标签页".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: EDITOR_READ_RANGE_INPUT_SCHEMA.into(),
            },
            grant: grant.clone(),
        }),
        ToolOutputShape::Read,
    )?;
    catalog.add_tool_with_shape(
        Box::new(AiEditorSearchTool {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "editor.search".into(),
                version: 1,
                description: "在本轮捕获的编辑器快照中搜索正文，返回行号、上下文片段与续读位置；不读取未授权的标签页".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: ai_search_tool_schema(Some((
                    "tab_ref",
                    "发送时的编辑器引用；来自界面摘录中的 editor_ref",
                    false,
                )))
                .to_string(),
            },
            grant,
        }),
        ToolOutputShape::Read,
    )?;
    Ok(())
}

// Turn 作用域的编辑器全文登记处：桌面端在发送时登记，目录装配时取走。
//
// 为什么不放在 `AiConversationJob` 或运行快照里：
// - Job 是持久化身份的镜像，带上全文会让所有构造点都必须处理大字段；
// - 快照 `runtime_snapshot_json` 会落盘，违反设计 §13.2"不把整份内容永久复制进会话"。
//
// 登记项按 turn_id 隔离，取用即移除：装配成功或失败都不会留下可被别的 turn 读到的原文。

fn registry() -> &'static Mutex<HashMap<String, EditorGrant>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, EditorGrant>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记本轮的编辑器全文（由宿主持有 UI 文本时调用）。
///
/// 只持有共享不可变 Rope 快照；正文长度不决定能否发送，单次读取与搜索输出仍有界。
pub fn register_editor_grant(turn_id: &str, grant: EditorGrant) -> fluxdb_core::Result<()> {
    registry()
        .lock()
        .map_err(|_| editor_error("编辑器全文登记锁已损坏"))?
        .insert(turn_id.to_string(), grant);
    Ok(())
}

/// 取走本轮登记的全文（目录装配时调用，取走即不再保留）。
pub fn take_editor_grant(turn_id: &str) -> Option<EditorGrant> {
    registry().lock().ok()?.remove(turn_id)
}

/// 测试与崩溃恢复后清理残留登记项，避免同一 turn 复用时读到旧内容。
pub fn clear_editor_grant(turn_id: &str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(turn_id);
    }
}
