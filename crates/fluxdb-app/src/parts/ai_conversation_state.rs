// T028 公开会话状态。凭据不进入 DTO、事件或渲染快照。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiConversationView {
    pub session_id: Option<String>,
    pub title: String,
    pub target_json: Option<String>,
    pub sessions: Vec<AiConversationSummary>,
    pub messages: Vec<AiConversationMessage>,
    pub preferences: fluxdb_storage::ai_platform::settings::PanelPreferences,
    pub draft: String,
    pub expanded_parts: Vec<String>,
    pub active_turn_id: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AiConversationSummary {
    pub id: String,
    pub title: String,
    pub agent_id: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AiConversationMessage {
    pub id: String,
    pub role: String,
    pub parts: Vec<AiConversationPart>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AiConversationPart {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub payload_json: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AiConversationCommand {
    Load {
        session_id: Option<String>,
    },
    /// 宿主启动专用；不能在活跃任务期间或每次打开面板时扫描。
    RecoverOnStartup,
    New,
    SaveUi {
        draft: String,
        expanded_parts: Vec<String>,
        mode: fluxdb_storage::ai_platform::settings::PanelMode,
        drawer_width: f64,
        embedded_width: f64,
    },
    /// 审批决定（对象级 CAS：expected_revision 不符即失败，重复点击不再生效）。
    DecideApproval {
        approval_id: String,
        approved: bool,
        expected_revision: i64,
    },
    /// 后台已真实保存/加载的数据回桥；生产 UI 只接收宿主 service 返回值。
    Apply {
        view: AiConversationView,
    },
}

/// 后台任务身份（不是模型请求，不能携带凭据）。
#[derive(Clone, Debug)]
pub struct AiConversationJob {
    pub session_id: String,
    pub turn_id: String,
    pub assistant_message_id: String,
    pub model_profile_id: String,
}
