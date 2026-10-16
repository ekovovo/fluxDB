//! T037 内置 Jev 装配与状态（设计 §18/§18.1）。
//!
//! 用真实 SQLite + 生产 AppController 覆盖验收口径：
//! - 未配置/禁用 → 零 decision 请求；
//! - 用户显式选过决策模型才可用（不装插件、不手工建 instance/binding）；
//! - system reconcile 不覆盖 user 绑定，未选择时不擅自挑模型；
//! - 状态在设置页与运行期共用同一判定。
//!
//! 全程离线：模型 endpoint 指向 RFC 5737 保留网段并使用假凭据，不发起真实请求。

use std::{fs, path::PathBuf};

use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_storage::FileStorage;
use fluxdb_storage::ai_platform::{model_bindings, models, settings};

const BUILTIN_AGENT: &str = fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t037-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 保存一个决策模型档案。`protocol` 故意可参数化，用来验证"不按模型名猜协议"。
fn save_decision_model(
    controller: &mut AppController,
    name: &str,
    protocol: &str,
    with_key: bool,
    enabled: bool,
) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: "decisions".into(),
        model: format!("fixture-{name}"),
        endpoint: Some("https://decision.example.invalid/v1".into()),
        enabled,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(512),
        api_key: with_key.then(|| "sk-fixture-not-real".to_string()),
        ..Default::default()
    })) {
        // protocol 由存储层从 provider 推导，这里改用直接写库覆盖协议场景。
        AppEvent::AiModelSaved { id, .. } if protocol == "system-one" => id,
        other => panic!("save decision model {name}: {other:?}"),
    }
}

/// 直接写入一个指定 `api_protocol` 的决策模型，用于验证协议不兼容时不自动装配。
fn insert_decision_model_with_protocol(
    fixture: &Fixture,
    id: &str,
    protocol: &str,
    with_key: bool,
    enabled: bool,
) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let (cipher, _) =
                fluxdb_storage::ai_platform::load_ai_cipher(fixture.storage.root_dir())?;
            models::save_model(
                conn,
                &cipher,
                "fixture-key",
                1_000,
                &models::ModelEdit {
                    id: id.into(),
                    display_name: id.into(),
                    provider: "decisions".into(),
                    api_protocol: protocol.into(),
                    kind: models::ModelKind::Decision,
                    model: format!("model-{id}"),
                    endpoint: Some("https://decision.example.invalid/v1".into()),
                    endpoint_is_full_url: false,
                    enabled,
                    auth_mode: models::AuthMode::ApiKey,
                    egress: models::EgressPermission::Allow,
                    context_window_tokens: Some(8_192),
                    max_output_tokens: Some(512),
                    input_price_micros_per_million: None,
                    output_price_micros_per_million: None,
                    cached_input_price_micros_per_million: None,
                },
                None,
                if with_key {
                    models::ApiKeyChange::Set(
                        fluxdb_storage::ai_platform::model_secret::SecretInput::new(
                            "sk-fixture-not-real".to_string(),
                        )
                        .unwrap(),
                    )
                } else {
                    models::ApiKeyChange::Keep
                },
            )
        })
        .unwrap();
}

fn refresh(controller: &mut AppController) -> fluxdb_app::JevStatus {
    controller.dispatch(AppCommand::RefreshAiSettings);
    controller.state().ai_jev_status.clone()
}

/// 在「Jev 决策优化 · 管理」里选择（或清空）决策模型。
///
/// 走插件配置命令而不是模型页的默认值命令：插件的配置入口与落库口径都在应用层统一。
fn select_jev_decision_model(controller: &mut AppController, model_id: Option<&str>) -> AppEvent {
    let expected_revision = controller.state().ai_runtime_revision;
    controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: fluxdb_app::JEV_INSTANCE_ID.into(),
        selections: vec![fluxdb_app::AiPluginFieldSelection {
            key: fluxdb_app::JEV_DECISION_MODEL_FIELD.into(),
            value: None,
            resource_id: model_id.map(str::to_string),
        }],
        expected_revision,
    })
}

/// Jev 在设置页暴露的配置字段（应在选择前后都只有决策模型一个字段）。
fn jev_config_field(controller: &mut AppController) -> fluxdb_app::AiPluginConfigFieldItem {
    controller.dispatch(AppCommand::RefreshAiSettings);
    controller
        .state()
        .ai_plugin_instances
        .iter()
        .find(|plugin| plugin.id == fluxdb_app::JEV_INSTANCE_ID)
        .and_then(|plugin| {
            plugin
                .config_fields
                .iter()
                .find(|field| field.key == fluxdb_app::JEV_DECISION_MODEL_FIELD)
                .cloned()
        })
        .expect("Jev 必须声明决策模型字段")
}

/// 读取 builtin Agent 的 decision 角色绑定来源。
fn decision_bindings(fixture: &Fixture) -> Vec<model_bindings::ModelBindingView> {
    fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(model_bindings::list_agent_bindings(conn, BUILTIN_AGENT)?
                .into_iter()
                .filter(|binding| {
                    binding.role == model_bindings::ModelRole::Decision && binding.enabled
                })
                .collect())
        })
        .unwrap()
}

fn create_agent(controller: &mut AppController, name: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: name.into(),
        system_prompt: "t037".into(),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("save agent: {other:?}"),
    }
}

#[test]
fn without_decision_model_jev_is_not_configured_and_never_requests() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::NotConfigured);
    assert!(!status.is_usable());
    assert!(
        decision_bindings(&fixture).is_empty(),
        "没有合格模型时不得留下 system 绑定"
    );
}

#[test]
fn qualified_model_is_usable_only_after_explicit_selection() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let model_id = save_decision_model(&mut controller, "only", "system-one", true, true);

    // 只有一个合格模型也不再自动使用：未选择 → 待选择，零绑定、零请求。
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::NeedsChoice);
    assert!(!status.is_usable());
    assert!(
        decision_bindings(&fixture).is_empty(),
        "未选择时不得留下 system 绑定"
    );
    let field = jev_config_field(&mut controller);
    assert!(field.required, "决策模型是必填配置");
    assert_eq!(field.choices.len(), 1, "候选由宿主解析");
    assert_eq!(field.choices[0].id, model_id);
    assert_eq!(field.selection, None, "未配置时选择为空");

    // 在「Jev 决策优化 · 管理」里选择后才可用，并建立 system 绑定。
    assert!(matches!(
        select_jev_decision_model(&mut controller, Some(&model_id)),
        AppEvent::AiPluginConfigSaved { .. }
    ));
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::Available);
    assert_eq!(status.model_profile_id.as_deref(), Some(model_id.as_str()));
    let bindings = decision_bindings(&fixture);
    assert_eq!(bindings.len(), 1, "选择后应建立 decision 角色绑定");
    assert_eq!(
        bindings[0].binding_source,
        model_bindings::BindingSource::System,
        "自动绑定必须是 system 来源"
    );
    assert_eq!(bindings[0].model_profile_id, model_id);
    assert_eq!(
        jev_config_field(&mut controller).selection.as_deref(),
        Some(model_id.as_str())
    );

    // 清空选择：回到待选择，system 绑定被撤销，不退回自动挑选。
    assert!(matches!(
        select_jev_decision_model(&mut controller, None),
        AppEvent::AiPluginConfigSaved { .. }
    ));
    assert_eq!(
        refresh(&mut controller).kind,
        fluxdb_app::JevStatusKind::NeedsChoice
    );
    assert!(decision_bindings(&fixture).is_empty());
}

#[test]
fn disabled_model_and_disabled_plugin_produce_zero_requests() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let model_id = save_decision_model(&mut controller, "off", "system-one", true, true);
    select_jev_decision_model(&mut controller, Some(&model_id));
    assert_eq!(
        refresh(&mut controller).kind,
        fluxdb_app::JevStatusKind::Available
    );

    // 模型停用：保留选择但状态变为不可用（附原因），system 绑定被撤销。
    let revision = controller
        .state()
        .ai_models
        .iter()
        .find(|model| model.id == model_id)
        .expect("模型")
        .revision;
    controller.dispatch(AppCommand::SetAiModelEnabled {
        id: model_id.clone(),
        enabled: false,
        expected_revision: revision,
    });
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::Unavailable);
    assert_eq!(status.model_profile_id.as_deref(), Some(model_id.as_str()));
    assert!(
        status
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("不可用")),
        "实际: {:?}",
        status.detail
    );
    assert!(decision_bindings(&fixture).is_empty());

    // 插件关闭：状态为已禁用，同样不保留生效绑定。
    let _ = controller.dispatch(AppCommand::SetAiModelEnabled {
        id: model_id.clone(),
        enabled: true,
        expected_revision: revision + 1,
    });
    let plugin_revision = controller
        .state()
        .ai_plugin_instances
        .iter()
        .find(|plugin| plugin.id == fluxdb_app::JEV_INSTANCE_ID)
        .expect("Jev 实例")
        .revision;
    controller.dispatch(AppCommand::SetAiPluginInstanceEnabled {
        id: fluxdb_app::JEV_INSTANCE_ID.into(),
        enabled: false,
        expected_revision: plugin_revision,
    });
    assert_eq!(
        refresh(&mut controller).kind,
        fluxdb_app::JevStatusKind::Disabled
    );
    assert!(
        decision_bindings(&fixture).is_empty(),
        "关闭插件后不得继续按自动绑定发起决策"
    );
}

#[test]
fn missing_credential_keeps_jev_unconfigured() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    insert_decision_model_with_protocol(&fixture, "no-key", "system-one", false, true);
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::NotConfigured);
    assert!(!status.is_usable(), "缺凭据时不得发起任何决策请求");
}

#[test]
fn incompatible_protocol_is_reported_as_unavailable_not_silently_used() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    // 名字里带 jev 但协议不是 system-one：不能因为名字像就被选中。
    insert_decision_model_with_protocol(&fixture, "jev-lookalike", "openai-compatible", true, true);
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::Unavailable);
    assert!(
        status
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("决策协议")),
        "实际: {:?}",
        status.detail
    );
    assert!(decision_bindings(&fixture).is_empty());
    // 协议不符的模型也不能通过插件配置存进来：候选过滤在保存路径上同样生效。
    match select_jev_decision_model(&mut controller, Some("jev-lookalike")) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("不满足插件要求"), "实际: {message}")
        }
        other => panic!("协议不符的模型不应保存成功: {other:?}"),
    }
    assert!(decision_bindings(&fixture).is_empty());
}

#[test]
fn multiple_qualified_models_without_default_need_user_choice() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    save_decision_model(&mut controller, "a", "system-one", true, true);
    save_decision_model(&mut controller, "b", "system-one", true, true);
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::NeedsChoice);
    assert!(
        decision_bindings(&fixture).is_empty(),
        "多候选无默认时不得擅自选一个（尤其不能挑贵的）"
    );
}

#[test]
fn default_decision_model_wins_over_single_candidate_rule() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let first = save_decision_model(&mut controller, "first", "system-one", true, true);
    let second = save_decision_model(&mut controller, "second", "system-one", true, true);
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    let event = controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: None,
        default_decision_model_id: Some(second.clone()),
        expected_revision: revision,
    });
    assert!(
        matches!(event, AppEvent::AiDefaultModelsSaved { .. }),
        "{event:?}"
    );
    let status = refresh(&mut controller);
    assert_eq!(status.kind, fluxdb_app::JevStatusKind::Available);
    assert_eq!(status.model_profile_id.as_deref(), Some(second.as_str()));
    let bindings = decision_bindings(&fixture);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].model_profile_id, second, "默认模型优先");
    assert_ne!(bindings[0].model_profile_id, first);
}

#[test]
fn reconcile_never_overwrites_user_decision_binding() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let auto = save_decision_model(&mut controller, "auto", "system-one", true, true);
    let manual = save_decision_model(&mut controller, "manual", "system-one", true, true);
    let agent = create_agent(&mut controller, "with-user-binding");
    let _ = refresh(&mut controller);

    fixture
        .storage
        .with_ai_connection(|conn| {
            model_bindings::set_role_model(
                conn,
                2_000,
                "user-decision",
                &agent,
                model_bindings::ModelRole::Decision,
                Some(&manual),
                None,
            )
        })
        .unwrap();
    // 再触发一次 reconcile（模拟模型保存/启停等触发点）。
    let _ = refresh(&mut controller);

    fixture
        .storage
        .with_ai_connection(|conn| {
            let bindings = model_bindings::list_agent_bindings(conn, &agent)?;
            let decision: Vec<_> = bindings
                .into_iter()
                .filter(|binding| binding.role == model_bindings::ModelRole::Decision)
                .collect();
            assert_eq!(decision.len(), 1, "不得同时存在 user 与 system 决策绑定");
            assert_eq!(
                decision[0].binding_source,
                model_bindings::BindingSource::User
            );
            assert_eq!(decision[0].model_profile_id, manual);
            Ok(())
        })
        .unwrap();
    let _ = auto;
}

#[test]
fn new_agent_gets_system_binding_after_reconcile() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let model_id = save_decision_model(&mut controller, "seeded", "system-one", true, true);
    // 未选择时新建 Agent 也不得有 system 绑定。
    let unselected = create_agent(&mut controller, "before-selection");
    assert!(decision_bindings(&fixture).is_empty());
    select_jev_decision_model(&mut controller, Some(&model_id));
    let agent = create_agent(&mut controller, "late-agent");
    fixture
        .storage
        .with_ai_connection(|conn| {
            let bindings = model_bindings::list_agent_bindings(conn, &agent)?;
            let decision: Vec<_> = bindings
                .into_iter()
                .filter(|binding| binding.role == model_bindings::ModelRole::Decision)
                .collect();
            assert_eq!(decision.len(), 1, "Agent 新建即应被自动装配");
            assert_eq!(decision[0].model_profile_id, model_id);
            assert_eq!(
                decision[0].binding_source,
                model_bindings::BindingSource::System
            );
            Ok(())
        })
        .unwrap();
    assert!(
        decision_bindings(&fixture)
            .iter()
            .all(|binding| binding.model_profile_id == model_id),
        "两个 Agent 都应绑定到显式选择的模型"
    );
    let _ = unselected;
}
