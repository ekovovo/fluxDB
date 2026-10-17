/// 为单个工具调用创建实时进度接收端。
///
/// 每次调用都必须绑定**自己的** `call_id`，UI 才能把进度贴到正确的工具卡；
/// 因此这里按调用做工厂，而不是给整个 Turn 复用同一个接收端。
pub trait ToolProgressSource: Send + Sync + std::fmt::Debug {
    fn sink_for(&self, call_id: &str) -> std::sync::Arc<dyn fluxdb_core::ToolProgressSink>;
}

/// 会话投影的发布边界：可合并增量与不可丢弃终态分别投递。
pub trait ConversationEventSink: Send + Sync {
    fn try_publish(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::TrySendError<AiConversationUpdate>>;

    fn publish_terminal(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::SendError<AiConversationUpdate>>;

    /// 本轮工具执行期的实时进度出口；返回 `None` 表示该回桥不订阅进度。
    ///
    /// 默认不订阅，避免测试用的极简回桥被迫实现进度通道。
    fn tool_progress_source(
        &self,
        _job: &AiConversationJob,
    ) -> Option<std::sync::Arc<dyn ToolProgressSource>> {
        None
    }
}

impl ConversationEventSink for std::sync::mpsc::SyncSender<AiConversationUpdate> {
    fn try_publish(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::TrySendError<AiConversationUpdate>> {
        self.try_send(update)
    }

    fn publish_terminal(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::SendError<AiConversationUpdate>> {
        self.send(update)
    }
}

struct ConversationEventSender {
    updates: std::sync::mpsc::SyncSender<AiConversationUpdate>,
    terminal: Arc<Mutex<Option<AiConversationUpdate>>>,
    pending_deltas: Arc<Mutex<Vec<AiConversationUpdate>>>,
}

/// 每次提交独占一个有界队列和一个终态槽；先消费队列，再消费终态。
/// UI 暂停消费不会阻塞数据库收尾或 actor 退出。
pub struct ConversationEventReceiver {
    updates: std::sync::mpsc::Receiver<AiConversationUpdate>,
    terminal: Arc<Mutex<Option<AiConversationUpdate>>>,
    pending_deltas: Arc<Mutex<Vec<AiConversationUpdate>>>,
}

impl ConversationEventReceiver {
    pub fn try_recv(&self) -> Result<AiConversationUpdate, std::sync::mpsc::TryRecvError> {
        match self.updates.try_recv() {
            Ok(update) => Ok(update),
            Err(error) => self.pending_deltas.lock().ok().and_then(|mut deltas| {
                if deltas.is_empty() { None } else { Some(deltas.remove(0)) }
            }).or_else(|| self
                .terminal
                .lock()
                .ok()
                .and_then(|mut terminal| terminal.take()))
                .ok_or(error),
        }
    }
}

impl ConversationEventSink for ConversationEventSender {
    fn try_publish(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::TrySendError<AiConversationUpdate>> {
        if update.delta.is_none() { return self.updates.try_send(update); }
        // 正文/摘要各一个合并槽，UI 停顿不阻塞网络，也不依赖下一 token 才能补发尾片。
        // 槽总字节数与检查点同限（4 MiB），终态独立保存且最后交付。
        let Ok(mut pending) = self.pending_deltas.lock() else {
            return Err(std::sync::mpsc::TrySendError::Disconnected(update));
        };
        let delta = update.delta.as_ref().expect("增量回包");
        let buffered: usize = pending.iter().filter_map(|item| item.delta.as_ref())
            .map(|item| item.delta.len()).sum();
        if buffered.saturating_add(delta.delta.len()) > 4 * 1024 * 1024 {
            return Err(std::sync::mpsc::TrySendError::Full(update));
        }
        if let Some(previous) = pending.iter_mut().find(|item|
            item.delta.as_ref().is_some_and(|old| old.kind == delta.kind)) {
            let old = previous.delta.as_mut().expect("增量槽");
            let end = old.start_offset + old.delta.len();
            if previous.job.turn_id != update.job.turn_id || old.message_id != delta.message_id
                || delta.start_offset != end {
                return Err(std::sync::mpsc::TrySendError::Full(update));
            }
            old.delta.push_str(&delta.delta);
        } else {
            pending.push(update);
        }
        Ok(())
    }

    fn publish_terminal(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::SendError<AiConversationUpdate>> {
        match self.terminal.lock() {
            Ok(mut terminal) if terminal.is_none() => {
                *terminal = Some(update);
                Ok(())
            }
            _ => Err(std::sync::mpsc::SendError(update)),
        }
    }

    fn tool_progress_source(
        &self,
        job: &AiConversationJob,
    ) -> Option<std::sync::Arc<dyn ToolProgressSource>> {
        Some(std::sync::Arc::new(ConversationToolProgressSource {
            updates: self.updates.clone(),
            job: job.clone(),
            last: Arc::new(Mutex::new(None)),
        }))
    }
}

/// 把工具执行期的进度即时投递到会话回桥通道。
///
/// 必须非阻塞：通道满时**丢弃这一条进度**（终态仍由工具回执快照覆盖），
/// 不能反过来阻塞工具执行线程。进度只是过程快照，允许合并/丢弃。
struct ConversationToolProgressSource {
    updates: std::sync::mpsc::SyncSender<AiConversationUpdate>,
    job: AiConversationJob,
    /// 上一次成功投递进度的时刻；用于节流，避免高频进度挤占有界回桥通道。
    last: Arc<Mutex<Option<std::time::Instant>>>,
}

impl std::fmt::Debug for ConversationToolProgressSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationToolProgressSource")
            .field("turn_id", &self.job.turn_id)
            .finish_non_exhaustive()
    }
}

impl ToolProgressSource for ConversationToolProgressSource {
    fn sink_for(&self, call_id: &str) -> std::sync::Arc<dyn fluxdb_core::ToolProgressSink> {
        std::sync::Arc::new(ConversationToolProgressSink {
            updates: self.updates.clone(),
            job: self.job.clone(),
            call_id: call_id.to_string(),
            last: self.last.clone(),
        })
    }
}

/// 进度节流间隔：过程快照可由终态回执覆盖，不必逐条送到 UI。
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::from_millis(150);

struct ConversationToolProgressSink {
    updates: std::sync::mpsc::SyncSender<AiConversationUpdate>,
    job: AiConversationJob,
    call_id: String,
    last: Arc<Mutex<Option<std::time::Instant>>>,
}

impl std::fmt::Debug for ConversationToolProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationToolProgressSink")
            .field("turn_id", &self.job.turn_id)
            .field("call_id", &self.call_id)
            .finish_non_exhaustive()
    }
}

impl fluxdb_core::ToolProgressSink for ConversationToolProgressSink {
    fn report(&self, event: fluxdb_core::ToolProgressEvent) {
        // 节流：同一 Turn 内进度共享一个节流窗口，避免长查询高频进度把有界通道打满，
        // 挤掉正文/工具卡等更要紧的回包。
        if let Ok(mut last) = self.last.lock() {
            let now = std::time::Instant::now();
            if last.is_some_and(|at| now.duration_since(at) < PROGRESS_THROTTLE) {
                return;
            }
            *last = Some(now);
        }
        let update = AiConversationUpdate::tool_progress(
            self.job.clone(),
            AiConversationToolProgress {
                call_id: self.call_id.clone(),
                returned_rows: event.returned_rows,
                affected_rows: event.affected_rows,
                elapsed_ms: event.elapsed_ms,
                success: event.success,
            },
        );
        // 通道满/已断开都是"来不及送达"，不是错误：丢弃这一条，不影响执行。
        let _ = self.updates.try_send(update);
    }
}

fn conversation_event_channel() -> (ConversationEventSender, ConversationEventReceiver) {
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let terminal = Arc::new(Mutex::new(None));
    let pending_deltas = Arc::new(Mutex::new(Vec::with_capacity(2)));
    (
        ConversationEventSender {
            updates: tx,
            terminal: terminal.clone(),
            pending_deltas: pending_deltas.clone(),
        },
        ConversationEventReceiver {
            updates: rx,
            terminal,
            pending_deltas,
        },
    )
}

