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
    "limit": {"type": "integer", "minimum": 1, "maximum": 400, "description": "本次最多读取的行数"}
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
    pub full_text: String,
}

/// `editor.read_range` 工具实现。
struct AiEditorReadRangeTool {
    descriptor: fluxdb_core::ToolDescriptor,
    grant: EditorGrant,
    session_id: String,
    turn_id: String,
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
            let (status, value, error_code) =
                match read_window(&self.grant, &ctx, &arguments_json) {
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

/// 把字节预算回退到最近的 UTF-8 字符边界。
fn char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
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
    let lines: Vec<&str> = grant.full_text.split('\n').collect();
    let total_lines = lines.len();
    let offset = args["offset"].as_u64().unwrap_or(0);
    let offset = usize::try_from(offset).map_err(|_| editor_error("offset 超出范围"))?;
    if offset >= total_lines {
        return Err(editor_error(format!(
            "offset 超出范围：起始行 {offset} 已超过全文行数 {total_lines}"
        )));
    }
    let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 400) as usize;

    // 按行取窗口，再按字节兜底。
    let end_line = total_lines.min(offset.saturating_add(limit));
    let mut selected: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    // 单行本身就超限时不能整行丢掉——否则 next_offset 不前进，模型无法续读。
    // 这里按字符边界切该行并标记 partial_line，用 next_byte_offset 续读余下部分
    // （对齐 Pi read.ts 对超长单行的处理：宁可给可续读的片段，也不给空结果）。
    let mut partial_line: Option<(usize, usize)> = None;
    for (index, line) in lines[offset..end_line].iter().enumerate() {
        let size = line.len() + 1;
        if bytes + size <= EDITOR_READ_MAX_BYTES {
            bytes += size;
            selected.push((*line).to_string());
            continue;
        }
        let room = EDITOR_READ_MAX_BYTES.saturating_sub(bytes).max(1);
        let cut = char_boundary(line, room);
        if cut > 0 {
            selected.push(line[..cut].to_string());
            partial_line = Some((offset + index, cut));
        }
        truncated = true;
        break;
    }
    let consumed_lines = selected.len();
    let next_offset = offset + consumed_lines;
    let has_more = next_offset < total_lines;

    Ok(serde_json::json!({
        "tab_ref": grant.tab_ref,
        "lines": selected,
        "line_numbers": {"from": offset, "to": offset + consumed_lines.saturating_sub(1)},
        "offset": offset,
        "next_offset": next_offset,
        "total_lines": total_lines,
        "has_more": has_more,
        "truncated": truncated || has_more,
        // 超长单行被切时给出续读位置，否则模型只能看到不完整的行却无从继续。
        "partial_line": partial_line.map(|(line_index, byte_offset)| serde_json::json!({
            "line": line_index,
            "next_byte_offset": byte_offset,
            "note": "该行超过单次字节上限；用同样的 offset 加上 next_byte_offset 续读该行剩余部分",
        })),
        "untrusted": true,
        "scope": "发送本轮时捕获的编辑器快照，不是实时内容；用户可能已继续编辑",
    }))
}

/// 装配 `editor.read_range`；没有编辑器内容时不注册（模型不会发现一个调不动的工具）。
///
/// 全文只留在工具实例里，随本轮目录释放，不写入 `ai_turns.runtime_snapshot_json`。
pub fn add_ai_editor_read_tools(
    catalog: &mut ToolCatalog,
    grant: Option<EditorGrant>,
    session_id: &str,
    turn_id: &str,
) -> fluxdb_core::Result<()> {
    let Some(grant) = grant else {
        return Ok(());
    };
    if grant.full_text.trim().is_empty() {
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
            grant,
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
        }),
        ToolOutputShape::Opaque,
    )
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
/// 超出上限的全文不登记：界面摘录已有 16 KiB 限额，全文再大时应提示用户缩小编辑范围，
/// 而不是让 turn 长期持有大字符串。
pub fn register_editor_grant(turn_id: &str, grant: EditorGrant) -> fluxdb_core::Result<()> {
    if grant.full_text.len() > EDITOR_FULL_TEXT_MAX_BYTES {
        return Err(editor_error(format!(
            "编辑器全文超过 {} KiB，请缩小范围或先保存为文件后再引用",
            EDITOR_FULL_TEXT_MAX_BYTES / 1024
        )));
    }
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

/// 单轮可持有的编辑器全文上限：与界面摘录同量级，避免长期占用大字符串。
pub const EDITOR_FULL_TEXT_MAX_BYTES: usize = 256 * 1024;

/// 测试与崩溃恢复后清理残留登记项，避免同一 turn 复用时读到旧内容。
pub fn clear_editor_grant(turn_id: &str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(turn_id);
    }
}
