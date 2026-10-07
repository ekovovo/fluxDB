// 上下文压缩决策与切点（设计 §16.1 / T069）。
//
// 纯函数优先：触发判定、合法切点、保护项校验、source hash 都在这里，
// 方便单测覆盖；真正的模型摘要调用与 checkpoint 落库在 ai_conversation.rs 编排。
//
// 明确边界：
// - 只压"最早的部分"，不拆未完成的工具请求/回执对；
// - 原始消息/审计永不删除，压缩只影响下一次请求的可见上下文；
// - 取消/失败/无进展/校验失败都不覆盖旧 checkpoint。

use fluxdb_core::ModelMessage;

/// token 估算：无 tokenizer 依赖，按字符近似（CJK 约 1 token/字符，ASCII 约 4 字符/token）。
///
/// 只用于"是否接近窗口上限"的保守估计，不用于计费（计费用 Provider 的真实 usage）。
pub fn estimate_tokens(text: &str) -> i64 {
    let mut ascii = 0i64;
    let mut wide = 0i64;
    for ch in text.chars() {
        if (ch as u32) < 0x80 {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    // 向上取整，宁可提前压缩，不要撞上下文上限。
    (ascii + 3) / 4 + wide
}

/// 消息列表的估算 token 总量（含角色/结构开销的粗略常量）。
pub fn estimate_messages_tokens(messages: &[ModelMessage]) -> i64 {
    messages
        .iter()
        .map(|m| estimate_tokens(&m.text) + 4)
        .sum()
}

/// 压缩触发判定输入。
#[derive(Debug, Clone, Copy)]
pub struct CompactionTrigger {
    /// 生成模型的上下文窗口（token）；未知时不触发。
    pub context_window_tokens: Option<i64>,
    /// 本轮实际最大输出（token），用于预留空间。
    pub max_output_tokens: Option<i64>,
    /// Agent 配置的触发百分比（10–95）。
    pub trigger_percent: u8,
}

/// 触发原因（用于 UI 提示与测试断言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    /// 达到百分比阈值。
    Percent,
    /// 输出预留空间不足（余量小于输出预算 + 安全余量）。
    OutputReserve,
}

/// 安全余量：除输出外额外保留 5% 窗口（最少 512 tokens），避免估算误差顶到上限。
fn safety_margin(window: i64) -> i64 {
    (window / 20).max(512)
}

/// 是否应触发压缩；返回原因而非 bool，便于 UI/审计区分。
pub fn should_compact(
    estimated_input_tokens: i64,
    trigger: CompactionTrigger,
) -> Option<CompactionReason> {
    let window = trigger.context_window_tokens.filter(|w| *w > 0)?;
    let output = trigger.max_output_tokens.unwrap_or(0).max(0);
    let usable = window - output - safety_margin(window);
    if usable <= 0 {
        // 窗口连输出都装不下：必然要压缩。
        return Some(CompactionReason::OutputReserve);
    }
    let percent_threshold =
        ((window as f64) * (f64::from(trigger.trigger_percent) / 100.0)).floor() as i64;
    if estimated_input_tokens >= percent_threshold {
        return Some(CompactionReason::Percent);
    }
    if estimated_input_tokens >= usable {
        return Some(CompactionReason::OutputReserve);
    }
    None
}

/// 压缩切点：`[0, cut)` 进入摘要，`[cut, len)` 保留原始消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionCut {
    /// 进入摘要的消息下标（不含）。
    pub cut: usize,
    /// 摘要覆盖的最后一条消息 seq（1 起）；无消息时为 0。
    pub last_message_seq: i64,
}

/// 计算合法切点并保留最近完整交互。
///
/// 规则（对齐 Pi 的合法切点）：
/// - 保底保留 `keep_recent_messages` 条（不足则整体保留，返回 None 表示无可压缩内容）；
/// - 切点不能落在"assistant 请求了工具但回执还未回齐"的位置：向前回退到成对边界；
/// - 第一条 System 永不进入摘要（它是运行约束，每轮都重新拼装）。
pub fn compute_cut(
    messages: &[ModelMessage],
    keep_recent_messages: usize,
    seq_of: impl Fn(usize) -> i64,
) -> Option<CompactionCut> {
    if messages.len() <= keep_recent_messages + 1 {
        return None;
    }
    // 永远保留第一条 System。
    let earliest = 1usize;
    let latest = messages.len().saturating_sub(keep_recent_messages);
    if latest <= earliest {
        return None;
    }
    let mut cut = latest;
    // 第一条**保留**的消息不能是孤立的 Tool 回执：那说明它的 assistant(tool_calls)
    // 被压进了摘要、回执却留在可见上下文里，会破坏工具请求/回执配对。
    // 出现这种情况就把切点向前回退，直到 assistant(tool_calls) 与回执一起进入保留段。
    while cut > earliest {
        if cut >= messages.len() {
            cut -= 1;
            continue;
        }
        let first_kept = &messages[cut];
        if first_kept.role == fluxdb_core::ModelRole::Tool {
            cut -= 1;
            continue;
        }
        break;
    }
    if cut <= earliest {
        return None;
    }
    Some(CompactionCut {
        cut,
        last_message_seq: seq_of(cut - 1),
    })
}

/// 保护项模板：这些条目缺失时摘要视为无效，不落盘。
pub const REQUIRED_PROTECTED_ITEMS: &[&str] = &[
    "goal",
    "constraints",
    "verified_facts",
    "decisions",
    "next_steps",
];

/// 摘要校验：必须非空、未被截断标记、且涵盖全部保护项。
pub fn validate_summary(summary: &str) -> Result<(), String> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err("摘要为空".into());
    }
    if trimmed.contains("[truncated]") || trimmed.ends_with('…') {
        return Err("摘要疑似被截断".into());
    }
    let value:serde_json::Value=serde_json::from_str(trimmed).map_err(|_|"摘要不是合法 JSON")?;
    let object=value.as_object().ok_or("摘要必须是 JSON 对象")?;
    for key in REQUIRED_PROTECTED_ITEMS {
        let item=object.get(*key).ok_or_else(||format!("摘要缺少保护项：{key}"))?;
        if item.is_null() || item.as_str().is_some_and(|s|s.trim().is_empty()) {
            return Err(format!("摘要保护项为空：{key}"));
        }
    }
    Ok(())
}

/// 构造摘要请求的系统提示与用户内容（增量：旧摘要 + 新增旧消息）。
pub fn build_summary_prompt(previous_summary: Option<&str>, digest: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "你在为数据库智能助手压缩历史上下文。请输出 JSON，且必须包含字段：\
         goal、constraints、verified_facts、decisions、next_steps。\
         不要臆造未验证的事实；无法确认的内容写入 next_steps 或省略。\n",
    );
    if let Some(previous) = previous_summary.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("\n已有摘要（增量合并，不要丢失其中仍有效的内容）：\n");
        prompt.push_str(previous);
        prompt.push('\n');
    }
    prompt.push_str("\n需要纳入摘要的新增历史：\n");
    prompt.push_str(digest);
    prompt
}

/// 稳定的来源 hash（不引入新依赖；用简单 FNV-1a 64 位）。
pub fn source_hash(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
