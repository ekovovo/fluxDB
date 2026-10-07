// 流式正文的平滑揭示缓冲。
//
// 应用的 checkpoint 每 300ms 才回投影一次；如果直接渲染 checkpoint，文字会成块跳变。
// 这里把 checkpoint 之间的增量 delta 累积成 buffer，再按 16ms 节拍匀速"揭示"，
// 得到连续、可读的输出（做法参考 Zed agent 的 StreamingTextBuffer）。
//
// 不变式：TextViewState 的正文 == `buffer[..revealed]`，即 entity 只持有已揭示文本。
// 自愈：若某段 delta 因通道拥塞丢失，下一次 checkpoint 的投影（更长）会把 buffer
// 校正回权威文本；终态正文永远由后端 checkpoint 决定，不依赖本缓冲。
const STREAM_REVEAL_TARGET_MS: f32 = 200.;
const STREAM_REVEAL_TICK_MS: f32 = 16.;

#[derive(Default)]
pub(crate) struct AiChatStream {
    /// 当前正在流式渲染的助手**消息** id（正文 part id 为 `{message_id}-text`）；
    /// `None` 表示没有活跃流。
    message_id: Option<String>,
    /// 已收到的完整正文（含已揭示与待揭示部分）。
    buffer: String,
    /// 已揭示到 `buffer` 的字节数，始终落在 UTF-8 char 边界且等于 entity 正文长度。
    revealed: usize,
}

impl AiChatStream {
    pub(crate) fn reset(&mut self) {
        self.message_id = None;
        self.buffer.clear();
        self.revealed = 0;
    }

    pub(crate) fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.message_id.is_some()
    }

    pub(crate) fn target_matches(&self, message_id: &str) -> bool {
        self.message_id.as_deref() == Some(message_id)
    }

    /// 绑定（或切换）到某个流式消息。切换时清空 buffer 与揭示进度；
    /// 首次绑定时用 checkpoint 已有正文做基线，避免把历史内容重新"打字"一遍。
    pub(crate) fn begin(&mut self, message_id: &str, baseline: &str) {
        if self.message_id.as_deref() == Some(message_id) {
            return;
        }
        self.message_id = Some(message_id.to_string());
        self.buffer.clear();
        self.buffer.push_str(baseline);
        self.revealed = self.buffer.len();
    }

    /// 追加一段增量到 buffer（不直接显示）；`begin` 必须已按消息对齐。
    pub(crate) fn push(&mut self, delta: &str) {
        if !delta.is_empty() {
            self.buffer.push_str(delta);
        }
    }

    /// 用 checkpoint 权威正文校正 buffer：仅在投影更长时覆盖，
    /// 避免把已揭示进度倒退回去造成闪烁。
    pub(crate) fn reconcile(&mut self, payload: &str) {
        if payload.len() > self.buffer.len() {
            self.buffer.clear();
            self.buffer.push_str(payload);
            self.revealed = self.revealed.min(self.buffer.len());
        }
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.revealed < self.buffer.len()
    }

    /// 已揭示的正文字节数（等于揭示实体正文长度）；UI 回归据此确认逐帧吐字。
    #[cfg(test)]
    pub(crate) fn revealed_len(&self) -> usize {
        self.revealed.min(self.buffer.len())
    }

    /// 推进一步揭示，返回本次新增、应追加到 entity 的文本块。
    pub(crate) fn reveal_next(&mut self) -> Option<String> {
        let pending = self.buffer.len().saturating_sub(self.revealed);
        if pending == 0 {
            return None;
        }
        let per_tick = ((pending as f32 / STREAM_REVEAL_TARGET_MS) * STREAM_REVEAL_TICK_MS)
            .ceil()
            .max(1.) as usize;
        let mut next = (self.revealed + per_tick).min(self.buffer.len());
        while next > self.revealed && !self.buffer.is_char_boundary(next) {
            next -= 1;
        }
        if next == self.revealed {
            // 单字符步长大于剩余空间（罕见）；推进到下一个边界，避免永久卡住。
            next = (self.revealed + per_tick).min(self.buffer.len());
            while next < self.buffer.len() && !self.buffer.is_char_boundary(next) {
                next += 1;
            }
            next = next.min(self.buffer.len());
        }
        if next == self.revealed {
            return None;
        }
        let chunk = self.buffer[self.revealed..next].to_string();
        self.revealed = next;
        Some(chunk)
    }

    /// 立即揭示剩余全部（终态兜底，防止尾部滞留）。
    pub(crate) fn reveal_all(&mut self) -> Option<String> {
        if self.revealed >= self.buffer.len() {
            return None;
        }
        let chunk = self.buffer[self.revealed..].to_string();
        self.revealed = self.buffer.len();
        Some(chunk)
    }
}
