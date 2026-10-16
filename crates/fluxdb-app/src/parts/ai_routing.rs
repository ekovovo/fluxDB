// 模型路由：候选过滤 + 固定优先级（设计 §17，T033 剩余部分）。
//
// precedence：用户明确选择 → Agent 角色绑定 → 全局默认。任一环节候选不合格都
// **不静默回退**到其它模型，而是给出可见失败原因——固定模型不能被插件/默认规则
// 悄悄换掉。路由只决定"用哪个已有模型"，不创建新 endpoint/凭据。
//
// 能力标签只来自模型真实能力（上下文窗口），价格不参与判定或展示；无路由插件时
// 本模块仍提供基础基线（Agent 绑定 → 全局默认）。

use fluxdb_storage::ai_platform::model_secret::CredentialStatus;
use fluxdb_storage::ai_platform::models::{EgressPermission, ModelKind, ModelProfileView};

/// 路由拒绝原因（面向用户的可读原因，不含凭据材料）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteRejection {
    Disabled,
    WrongKind { expected: ModelKind },
    MissingCredential,
    /// 该模型被用户显式禁止接收会话数据（设计 §29/§31.1）。
    EgressDenied,
}

impl RouteRejection {
    pub fn message(&self, role: CandidateRole) -> String {
        match self {
            RouteRejection::Disabled => "所选模型已停用".to_string(),
            RouteRejection::WrongKind { expected } => format!(
                "所选模型用途不匹配（{}角色需要{}模型）",
                role.label(),
                match expected {
                    ModelKind::Generation => "生成",
                    ModelKind::Decision => "决策",
                }
            ),
            RouteRejection::MissingCredential => "所选模型缺少可用凭据".to_string(),
            RouteRejection::EgressDenied => {
                "所选模型已禁止数据出站（可在模型设置中重新允许）".to_string()
            }
        }
    }
}

/// 路由角色：决定候选必须是 generation 还是 decision。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateRole {
    Main,
    Compaction,
    Artifact,
    Decision,
    /// 压缩决策：在压缩切点候选之间做选择的决策模型（与主决策角色用途相同，单独命名只为拒绝原因可读）。
    CompactionDecision,
}

/// 本轮实际选中模型的来源（precedence 的哪一级生效）。
///
/// 只用于说明与运行事件，不参与授权判断：写进 `ai_run_events` 后，
/// UI 与验收都能看到"Provider 真正用的是哪个模型、为什么是它"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteSource {
    SessionSelection,
    AgentBinding,
    AgentDefault,
    GlobalDefault,
}

impl RouteSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteSource::SessionSelection => "session_selection",
            RouteSource::AgentBinding => "agent_binding",
            RouteSource::AgentDefault => "agent_default",
            RouteSource::GlobalDefault => "global_default",
        }
    }
}

impl CandidateRole {
    pub(crate) fn required_kind(self) -> ModelKind {
        match self {
            CandidateRole::Decision | CandidateRole::CompactionDecision => ModelKind::Decision,
            _ => ModelKind::Generation,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            CandidateRole::Main => "主模型",
            CandidateRole::Compaction => "压缩模型",
            CandidateRole::Artifact => "产物模型",
            CandidateRole::Decision => "决策模型",
            CandidateRole::CompactionDecision => "压缩决策模型",
        }
    }
}

/// 判定单个模型是否可作为该角色候选。
///
/// 顺序：停用 → 用途 → 单模型出站许可 → 凭据。出站类拒绝排在凭据前，
/// 便于用户先看到"是我禁止发数据"，而不是被凭据状态掩盖。
pub fn candidate_check(model: &ModelProfileView, role: CandidateRole) -> Result<(), RouteRejection> {
    if !model.enabled {
        return Err(RouteRejection::Disabled);
    }
    if model.kind != role.required_kind() {
        return Err(RouteRejection::WrongKind {
            expected: role.required_kind(),
        });
    }
    if model.egress == EgressPermission::Deny {
        return Err(RouteRejection::EgressDenied);
    }
    // 需要 Key 但缺密文时不可路由；已配置但密文不可解（Unavailable）也应拒绝出站。
    if matches!(
        model.credential_status,
        CredentialStatus::Missing | CredentialStatus::Unavailable
    ) {
        return Err(RouteRejection::MissingCredential);
    }
    Ok(())
}

/// 从一组候选模型里按稳定顺序挑出第一个合格者（顺序由调用方给定）。
///
/// 返回所有不合格原因，便于 UI 说明"有候选但都不可用"，而不是显示成"无模型"。
pub fn first_usable<'a>(
    models: impl IntoIterator<Item = &'a ModelProfileView>,
    role: CandidateRole,
) -> (Option<&'a ModelProfileView>, Vec<RouteRejection>) {
    let mut rejections = Vec::new();
    for model in models {
        match candidate_check(model, role) {
            Ok(()) => return (Some(model), rejections),
            Err(reason) => rejections.push(reason),
        }
    }
    (None, rejections)
}

/// 能力标签：只由模型真实上下文窗口推导，不按模型名或价格硬编码。
///
/// `labels` 只用于设置页展示与用户决策，不参与安全判定。
pub fn model_capability_labels(model: &ModelProfileView) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if model.context_window_tokens.is_some_and(|window| window >= 200_000) {
        labels.push("长上下文");
    }
    labels
}

/// 解析本轮摘要使用的模型（设计 §16「摘要模型可单独配置，无配置用主模型」）。
///
/// 顺序：Agent 的 `compaction` 角色绑定（按优先级取第一个合格者）→ 无绑定则用主模型。
/// 绑定了但全部不合格时**不静默改用主模型**：固定模型被换掉属可见失败，调用方需据此
/// 给出说明并按预算决定是否暂停。
pub fn resolve_compaction_model(
    conn: &rusqlite::Connection,
    agent_id: &str,
    main_model_id: &str,
) -> fluxdb_core::Result<crate::CompactionModelChoice> {
    let bindings = fluxdb_storage::ai_platform::model_bindings::list_role_bindings(
        conn,
        agent_id,
        fluxdb_storage::ai_platform::model_bindings::ModelRole::Compaction,
    )?;
    if bindings.is_empty() {
        return Ok(crate::CompactionModelChoice::Main);
    }
    let mut first_rejection: Option<(String, String)> = None;
    for binding in &bindings {
        let model = fluxdb_storage::ai_platform::models::load_model(conn, &binding.model_profile_id)?;
        match candidate_check(&model, CandidateRole::Compaction) {
            Ok(()) => {
                return Ok(if model.id == main_model_id {
                    // 显式绑定到主模型：仍按主模型走，不需要第二个 Provider。
                    crate::CompactionModelChoice::Main
                } else {
                    crate::CompactionModelChoice::Model(model.id)
                });
            }
            Err(rejection) => {
                if first_rejection.is_none() {
                    first_rejection = Some((
                        binding.model_profile_id.clone(),
                        rejection.message(CandidateRole::Compaction),
                    ));
                }
            }
        }
    }
    let (model_profile_id, reason) = first_rejection
        .unwrap_or_else(|| (main_model_id.to_string(), "摘要模型不可用".to_string()));
    Ok(crate::CompactionModelChoice::Unavailable {
        model_profile_id,
        reason,
    })
}
