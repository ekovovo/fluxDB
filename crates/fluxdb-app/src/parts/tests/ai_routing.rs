// 模型路由候选过滤与标签（设计 §17，T033）。
//
// 只使用内存模型视图，不读取凭据、不访问网络。

use fluxdb_storage::ai_platform::model_secret::CredentialStatus;
use fluxdb_storage::ai_platform::models::{EgressPermission, ModelKind, ModelProfileView};

fn model(
    id: &str,
    kind: ModelKind,
    enabled: bool,
    credential: CredentialStatus,
) -> ModelProfileView {
    ModelProfileView {
        id: id.into(),
        display_name: id.into(),
        provider: "openai".into(),
        api_protocol: "openai-compatible".into(),
        kind,
        model: format!("model-{id}"),
        endpoint: Some("https://model.example.invalid/v1".into()),
        endpoint_is_full_url: false,
        enabled,
        egress: EgressPermission::Allow,
        auth_mode: fluxdb_storage::ai_platform::models::AuthMode::ApiKey,
        context_window_tokens: Some(32_768),
        max_output_tokens: Some(1_024),
        input_price_micros_per_million: Some(1_000),
        output_price_micros_per_million: Some(2_000),
        cached_input_price_micros_per_million: None,
        credential_status: credential,
        secret_revision: 1,
        connection_status: "unknown".into(),
        connection_checked_at_ms: None,
        revision: 1,
    }
}

#[test]
fn candidate_check_enforces_kind_enabled_and_credential() {
    let generation = model("g", ModelKind::Generation, true, CredentialStatus::Configured);
    assert!(candidate_check(&generation, CandidateRole::Main).is_ok());

    let disabled = model("g2", ModelKind::Generation, false, CredentialStatus::Configured);
    assert_eq!(
        candidate_check(&disabled, CandidateRole::Main),
        Err(RouteRejection::Disabled)
    );

    let decision = model("d", ModelKind::Decision, true, CredentialStatus::Configured);
    assert_eq!(
        candidate_check(&decision, CandidateRole::Main),
        Err(RouteRejection::WrongKind {
            expected: ModelKind::Generation
        })
    );
    assert!(candidate_check(&decision, CandidateRole::Decision).is_ok());

    let missing = model("m", ModelKind::Generation, true, CredentialStatus::Missing);
    assert_eq!(
        candidate_check(&missing, CandidateRole::Main),
        Err(RouteRejection::MissingCredential)
    );

    // 无需凭据的模型即使没有密文仍可路由。
    let mut no_key = model("n", ModelKind::Generation, true, CredentialStatus::NotRequired);
    no_key.auth_mode = fluxdb_storage::ai_platform::models::AuthMode::None;
    assert!(candidate_check(&no_key, CandidateRole::Main).is_ok());
}

#[test]
fn candidate_check_rejects_denied_egress() {
    // 单模型拒绝出站：即使其它合格，也因用户显式收紧而不能路由。
    let mut denied = model("d1", ModelKind::Generation, true, CredentialStatus::Configured);
    denied.egress = EgressPermission::Deny;
    assert_eq!(
        candidate_check(&denied, CandidateRole::Main),
        Err(RouteRejection::EgressDenied)
    );
    let allowed = model("a1", ModelKind::Generation, true, CredentialStatus::Configured);
    assert!(candidate_check(&allowed, CandidateRole::Main).is_ok());
}

#[test]
fn capability_labels_come_from_context_window_not_price_or_name() {
    let mut premium = model("premium", ModelKind::Generation, true, CredentialStatus::Configured);
    premium.display_name = "cheap".into();
    premium.context_window_tokens = Some(400_000);
    assert_eq!(model_capability_labels(&premium), vec!["长上下文"]);

    // 价格高低既不产生标签，也不改变窗口推导出的标签。
    let mut expensive_small = model("small", ModelKind::Generation, true, CredentialStatus::Configured);
    expensive_small.output_price_micros_per_million = Some(80_000);
    expensive_small.context_window_tokens = Some(8_192);
    assert!(model_capability_labels(&expensive_small).is_empty());

    let mut free_large = model("free", ModelKind::Generation, true, CredentialStatus::Configured);
    free_large.output_price_micros_per_million = Some(0);
    free_large.context_window_tokens = Some(400_000);
    assert_eq!(model_capability_labels(&free_large), vec!["长上下文"]);

    // 窗口未知时不给任何标签，也不猜。
    let mut unknown = model("u", ModelKind::Generation, true, CredentialStatus::Configured);
    unknown.context_window_tokens = None;
    assert!(model_capability_labels(&unknown).is_empty());
}
