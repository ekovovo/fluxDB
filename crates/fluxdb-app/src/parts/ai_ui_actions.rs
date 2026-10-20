// AI 发往宿主 UI 的结构化动作。
//
// 工具运行在后台 actor，不能直接持有 GPUI 窗口或 `AppState`。这里只传递
// 有界、可校验的动作；桌面端消费后仍通过 `AppCommand` / `AppState` 落地。

use std::sync::mpsc;

#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    /// 把非敏感设置字段写入当前设置页草稿；不保存，等待用户确认。
    PrefillSettings {
        request_id: String,
        page_ref: String,
        prefill: crate::SettingsPrefill,
    },
    /// 提交一个待确认的持久化设置变更；用户点击“保存并应用”后才落库。
    ProposeConfig(crate::ConfigProposal),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigProposalKind {
    SettingUpdate(crate::SettingsPrefill),
    ToolSetEnabled {
        tool_id: String,
        enabled: bool,
    },
    AgentCreate(crate::AgentCreateProposal),
    SessionArchive {
        id: String,
    },
    SessionPurge {
        id: String,
    },
    ModelCreate(crate::ModelCreateProposal),
    ModelUpdate { id: String, changes: serde_json::Map<String, serde_json::Value> },
    ModelDelete { id: String },
    AgentUpdate { id: String, changes: serde_json::Map<String, serde_json::Value> },
    AgentDelete { id: String },
    McpServerSetEnabled {
        id: String,
        enabled: bool,
    },
    McpSyncClient {
        client_key: String,
        registered: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigProposal {
    pub request_id: String,
    pub turn_id: String,
    pub expected_revision: u64,
    pub created_at_ms: i64,
    pub kind: ConfigProposalKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCreateProposal {
    pub display_name: String,
    pub description: String,
    pub system_prompt: String,
    pub model_profile_id: Option<String>,
    pub allowed_tools: Vec<String>,
    pub enabled: bool,
    pub compression_trigger_percent: u8,
    pub max_iterations: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCreateProposal {
    pub display_name: String,
    pub provider: String,
    pub endpoint: Option<String>,
    pub endpoint_is_full_url: bool,
    pub model: String,
    pub context_window_tokens: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub input_price_micros: Option<i64>,
    pub output_price_micros: Option<i64>,
    pub cached_input_price_micros: Option<i64>,
    pub enabled: bool,
    pub egress: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiActionError {
    Full,
    Disconnected,
}

impl UiActionError {
    pub fn retryable(self) -> bool {
        matches!(self, Self::Full)
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Full => "界面动作队列已满，请稍后重试",
            Self::Disconnected => "界面动作通道已关闭",
        }
    }
}

pub trait UiActionSink: Send + Sync {
    fn submit(&self, action: UiAction) -> Result<(), UiActionError>;
}

pub struct UiActionSender {
    sender: mpsc::SyncSender<UiAction>,
}

impl UiActionSink for UiActionSender {
    fn submit(&self, action: UiAction) -> Result<(), UiActionError> {
        match self.sender.try_send(action) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(UiActionError::Full),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(UiActionError::Disconnected),
        }
    }
}

pub struct UiActionReceiver {
    receiver: mpsc::Receiver<UiAction>,
}

impl UiActionReceiver {
    pub fn try_recv(&self) -> Result<UiAction, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

/// 每个 Turn 独占一个有界动作队列；UI 每帧最多消费固定条数，避免长任务挤占渲染。
pub fn ui_action_channel(capacity: usize) -> (UiActionSender, UiActionReceiver) {
    let (sender, receiver) = mpsc::sync_channel(capacity.max(1));
    (UiActionSender { sender }, UiActionReceiver { receiver })
}
