/// 流式正文/公开摘要的内存增量；不落盘、不建行，只把未 checkpoint 的尾部即时送 UI。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiConversationDeltaKind {
    /// 助手正文，对应 checkpoint 的 `-text` 片段。
    Text,
    /// 公开思考摘要，对应 checkpoint 的 `-summary` 片段。
    Summary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiConversationStreamDelta {
    pub message_id: String,
    pub kind: AiConversationDeltaKind,
    /// 相对于该正文/摘要的 UTF-8 字节偏移，用于快照与增量去重。
    pub start_offset: usize,
    pub delta: String,
}

/// 工具执行中的实时进度（尚未落盘；终态仍以工具回执为准）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiConversationToolProgress {
    /// 对应 `ToolReceipt.call_id`；UI 用它把进度贴到正确的工具卡。
    pub call_id: String,
    pub returned_rows: u64,
    pub affected_rows: u64,
    pub elapsed_ms: u64,
    pub success: bool,
}

/// 仅主线程消费的回桥包；身份用于拒绝切会话后的迟到回调。
///
/// 一个回包要么携带**重投影视图**（checkpoint/终态，画面来源仍是数据库），
/// 要么携带**内存增量**（未落盘的流式尾部），不会两者同时改写同一条数据。
#[derive(Clone, Debug)]
pub struct AiConversationUpdate {
    pub job: AiConversationJob,
    /// 重投影/终态视图；纯增量回包为 `None`。
    pub view: Option<AiConversationView>,
    /// 未落盘的内存增量；视图回包为 `None`。
    pub delta: Option<AiConversationStreamDelta>,
    /// 工具执行中的实时进度；视图/增量回包为 `None`。
    pub tool_progress: Option<AiConversationToolProgress>,
    pub terminal: bool,
}

impl AiConversationUpdate {
    /// 重投影视图回包（checkpoint 非终态或终态）。
    pub fn view(job: AiConversationJob, view: AiConversationView, terminal: bool) -> Self {
        Self {
            job,
            view: Some(view),
            delta: None,
            tool_progress: None,
            terminal,
        }
    }

    /// 未落盘的内存增量回包。
    pub fn stream_delta(
        job: AiConversationJob,
        message_id: &str,
        kind: AiConversationDeltaKind,
        start_offset: usize,
        delta: &str,
    ) -> Self {
        Self {
            job,
            view: None,
            delta: Some(AiConversationStreamDelta {
                message_id: message_id.to_string(),
                kind,
                start_offset,
                delta: delta.to_string(),
            }),
            tool_progress: None,
            terminal: false,
        }
    }

    /// 工具执行中的实时进度回包；不落盘、不建行，终态由回执快照覆盖。
    pub fn tool_progress(job: AiConversationJob, progress: AiConversationToolProgress) -> Self {
        Self {
            job,
            view: None,
            delta: None,
            tool_progress: Some(progress),
            terminal: false,
        }
    }
}

struct AiConversationStreamPublisher {
    text_sent: usize,
    summary_sent: usize,
}

impl AiConversationStreamPublisher {
    fn new(text_sent: usize) -> Self {
        Self { text_sent, summary_sent: 0 }
    }

    fn publish(
        &mut self,
        sender: &dyn ConversationEventSink,
        job: &AiConversationJob,
        text: &str,
        summary: &str,
    ) {
        for (kind, content, sent) in [
            (AiConversationDeltaKind::Text, text, &mut self.text_sent),
            (AiConversationDeltaKind::Summary, summary, &mut self.summary_sent),
        ] {
            if *sent == content.len() {
                continue;
            }
            // 队列满时不推进游标；下次合并发送整个未发送尾部，不阻塞模型读取。
            match sender.try_publish(AiConversationUpdate::stream_delta(
                job.clone(), &job.assistant_message_id, kind, *sent, &content[*sent..],
            )) {
                Ok(()) => *sent = content.len(),
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

