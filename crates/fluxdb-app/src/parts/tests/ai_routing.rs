// 模型路由候选过滤与标签（设计 §17，T033）。
//
// 只使用内存模型视图，不读取凭据、不访问网络。

use fluxdb_storage::ai_platform::model_secret::CredentialStatus;
use fluxdb_storage::ai_platform::models::{ModelKind, ModelProfileView};

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
fn first_usable_picks_first_valid_and_reports_rejections() {
    let disabled = model("a", ModelKind::Generation, false, CredentialStatus::Configured);
    let wrong = model("b", ModelKind::Decision, true, CredentialStatus::Configured);
    let ok = model("c", ModelKind::Generation, true, CredentialStatus::Configured);
    let candidates = vec![disabled, wrong, ok];
    let (chosen, rejections) = first_usable(candidates.iter(), CandidateRole::Main);
    assert_eq!(chosen.map(|m| m.id.as_str()), Some("c"));
    assert_eq!(
        rejections,
        vec![RouteRejection::Disabled, RouteRejection::WrongKind {
            expected: ModelKind::Generation
        }]
    );
}

#[test]
fn first_usable_with_no_valid_candidate_returns_all_reasons() {
    let disabled = model("a", ModelKind::Generation, false, CredentialStatus::Configured);
    let missing = model("b", ModelKind::Generation, true, CredentialStatus::Missing);
    let candidates = vec![disabled, missing];
    let (chosen, rejections) = first_usable(candidates.iter(), CandidateRole::Main);
    assert!(chosen.is_none());
    assert_eq!(rejections.len(), 2, "应保留每条不合格原因，不能显示成无模型");
}

#[test]
fn capability_labels_come_from_price_and_window_not_name() {
    let mut cheap = model("cheap", ModelKind::Generation, true, CredentialStatus::Configured);
    cheap.display_name = "expensive-flash-name".into();
    cheap.output_price_micros_per_million = Some(1_000);
    cheap.context_window_tokens = Some(8_192);
    assert_eq!(model_capability_labels(&cheap), vec!["经济"]);

    let mut premium = model("premium", ModelKind::Generation, true, CredentialStatus::Configured);
    premium.display_name = "cheap".into();
    premium.output_price_micros_per_million = Some(80_000);
    premium.context_window_tokens = Some(400_000);
    let labels = model_capability_labels(&premium);
    assert!(labels.contains(&"高能力"));
    assert!(labels.contains(&"长上下文"));

    // 价格未知时不给任何价格标签，也不猜。
    let mut unknown = model("u", ModelKind::Generation, true, CredentialStatus::Configured);
    unknown.output_price_micros_per_million = None;
    unknown.context_window_tokens = None;
    assert!(model_capability_labels(&unknown).is_empty());
}
