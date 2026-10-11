// 模型路由：候选过滤 + 固定优先级（设计 §17，T033 剩余部分）。
//
// precedence：用户明确选择 → Agent 角色绑定 → 全局默认。任一环节候选不合格都
// **不静默回退**到其它模型，而是给出可见失败原因——固定模型不能被插件/默认规则
// 悄悄换掉。路由只决定"用哪个已有模型"，不创建新 endpoint/凭据。
//
// 经济/高能力标签来自用户可编辑的价格与能力信息，不按模型名硬编码；无路由插件时
// 本模块仍提供基础基线（Agent 绑定 → 全局默认）。

use fluxdb_storage::ai_platform::model_secret::CredentialStatus;
use fluxdb_storage::ai_platform::models::{ModelKind, ModelProfileView};

/// 路由拒绝原因（面向用户的可读原因，不含凭据材料）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteRejection {
    Disabled,
    WrongKind { expected: ModelKind },
    MissingCredential,
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
}

impl CandidateRole {
    pub(crate) fn required_kind(self) -> ModelKind {
        match self {
            CandidateRole::Decision => ModelKind::Decision,
            _ => ModelKind::Generation,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            CandidateRole::Main => "主模型",
            CandidateRole::Compaction => "压缩模型",
            CandidateRole::Artifact => "产物模型",
            CandidateRole::Decision => "决策模型",
        }
    }
}

/// 判定单个模型是否可作为该角色候选。
pub fn candidate_check(model: &ModelProfileView, role: CandidateRole) -> Result<(), RouteRejection> {
    if !model.enabled {
        return Err(RouteRejection::Disabled);
    }
    if model.kind != role.required_kind() {
        return Err(RouteRejection::WrongKind {
            expected: role.required_kind(),
        });
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

/// 经济/高能力标签：由价格与上下文窗口推导，不按模型名硬编码。
///
/// `labels` 只用于设置页展示与用户决策，不参与安全判定。
pub fn model_capability_labels(model: &ModelProfileView) -> Vec<&'static str> {
    let mut labels = Vec::new();
    let output_price = model.output_price_micros_per_million;
    if let Some(price) = output_price {
        if price == 0 {
            labels.push("免费");
        } else if price <= 5_000 {
            labels.push("经济");
        } else if price >= 50_000 {
            labels.push("高能力");
        }
    }
    if model.context_window_tokens.is_some_and(|window| window >= 200_000) {
        labels.push("长上下文");
    }
    labels
}
