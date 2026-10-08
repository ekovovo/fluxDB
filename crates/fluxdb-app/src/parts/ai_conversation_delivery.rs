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
}

/// 每次提交独占一个有界队列和一个终态槽；先消费队列，再消费终态。
/// UI 暂停消费不会阻塞数据库收尾或 actor 退出。
pub struct ConversationEventReceiver {
    updates: std::sync::mpsc::Receiver<AiConversationUpdate>,
    terminal: Arc<Mutex<Option<AiConversationUpdate>>>,
}

impl ConversationEventReceiver {
    pub fn try_recv(&self) -> Result<AiConversationUpdate, std::sync::mpsc::TryRecvError> {
        match self.updates.try_recv() {
            Ok(update) => Ok(update),
            Err(error) => self
                .terminal
                .lock()
                .ok()
                .and_then(|mut terminal| terminal.take())
                .ok_or(error),
        }
    }
}

impl ConversationEventSink for ConversationEventSender {
    fn try_publish(
        &self,
        update: AiConversationUpdate,
    ) -> Result<(), std::sync::mpsc::TrySendError<AiConversationUpdate>> {
        self.updates.try_send(update)
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
}

fn conversation_event_channel() -> (ConversationEventSender, ConversationEventReceiver) {
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let terminal = Arc::new(Mutex::new(None));
    (
        ConversationEventSender {
            updates: tx,
            terminal: terminal.clone(),
        },
        ConversationEventReceiver {
            updates: rx,
            terminal,
        },
    )
}
