// 流式增量合并缓冲：网络事件按偏移去重，Markdown 在固定节拍追加整批新增内容。
// 借鉴 Goose 的 50ms / 长正文 250ms 渲染节流；不通过逐字动画伪造模型输出速度。
const STREAM_RENDER_INTERVAL_MS: u64 = 50;
const LARGE_STREAM_RENDER_INTERVAL_MS: u64 = 250;
const LARGE_STREAM_TEXT_BYTES: usize = 16_000;

#[derive(Clone, Copy)]
enum AiChatStreamFixture {
    Default,
    Fast,
    Burst,
    Error,
}

#[derive(Default)]
pub(crate) struct AiChatStream {
    /// 当前正在流式渲染的助手**消息** id（正文 part id 为 `{message_id}-text`）；
    /// `None` 表示没有活跃流。
    message_id: Option<String>,
    /// 已收到的完整正文（含已揭示与待揭示部分）。
    buffer: String,
    /// 已揭示到 `buffer` 的字节数，始终落在 UTF-8 char 边界且等于 entity 正文长度。
    revealed: usize,
    finished: bool,
}

impl AiChatStream {
    pub(crate) fn reset(&mut self) {
        self.message_id = None;
        self.buffer.clear();
        self.revealed = 0;
        self.finished = false;
    }

    pub(crate) fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    pub(crate) fn revealed_text(&self) -> &str {
        &self.buffer[..self.revealed]
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
        self.finished = false;
    }

    /// 追加一段增量到 buffer（不直接显示）；`begin` 必须已按消息对齐。
    pub(crate) fn push(&mut self, start_offset: usize, delta: &str) {
        if start_offset > self.buffer.len() {
            tracing::warn!(start_offset, buffered = self.buffer.len(), "AI 流式正文偏移不连续，等待快照校正");
            return;
        }
        let overlap = self.buffer.len() - start_offset;
        if overlap < delta.len() && delta.is_char_boundary(overlap) {
            self.buffer.push_str(&delta[overlap..]);
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

    pub(crate) fn render_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(if self.buffer.len() > LARGE_STREAM_TEXT_BYTES {
            LARGE_STREAM_RENDER_INTERVAL_MS
        } else { STREAM_RENDER_INTERVAL_MS })
    }

    pub(crate) fn finish(&mut self) {
        self.finished = true;
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.revealed < self.buffer.len()
    }

    /// 推进一步揭示，返回本次新增、应追加到 entity 的文本块。
    pub(crate) fn reveal_next(&mut self) -> Option<String> {
        let pending = self.buffer.len().saturating_sub(self.revealed);
        if pending == 0 {
            return None;
        }
        let next = self.buffer.len();
        let chunk = self.buffer[self.revealed..next].to_string();
        self.revealed = next;
        Some(chunk)
    }

}
