//! 模型路由 precedence（设计 §17，T033 剩余部分）。
//!
//! 用真实 SQLite + 生产 AppController 验证：会话显式选择 → Agent 角色绑定 →
//! Agent 默认 → 全局默认；绑定启停影响候选；system reconcile 不覆盖 user 绑定。
//! 全程离线：只准备模型/绑定元数据，不发模型请求、不接触真实凭据。

use std::{fs, path::PathBuf};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiModelSaveInput, AppCommand,
    AppController, AppEvent, TargetRef, TurnTargetPin,
};
use fluxdb_storage::FileStorage;
use fluxdb_storage::ai_platform::{
    model_bindings,
    model_bindings::{BindingSource, ModelBindingEdit, ModelRole},
    models, settings,
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-routing-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let event = controller.dispatch(AppCommand::AiConversation(
            fluxdb_app::AiConversationCommand::Load { session_id: None },
        ));
        assert!(
            matches!(event, AppEvent::AiConversationLoaded(_)),
            "{event:?}"
        );
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn create_model(controller: &mut AppController, name: &str, provider: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: provider.into(),
        model: format!("offline-{name}"),
        endpoint: Some("http://127.0.0.1:9/v1".into()),
        enabled: true,
        context_window_tokens: Some(32_768),
        max_output_tokens: Some(1_024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save model: {other:?}"),
    }
}

fn create_agent(controller: &mut AppController, name: &str, model: Option<String>) -> String {
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: name.into(),
        system_prompt: "routing-fixture".into(),
        model_profile_id: model,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("save agent: {other:?}"),
    }
}

fn set_default_agent(fixture: &Fixture, agent_id: &str) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let mut runtime = settings::load_runtime_settings(conn)?;
            let revision = runtime.revision;
            runtime.default_agent_id = Some(agent_id.to_string());
            settings::save_runtime_settings(conn, 1, &runtime, revision)?;
            Ok(())
        })
        .unwrap();
}

fn bind(
    fixture: &Fixture,
    id: &str,
    agent_id: &str,
    model_id: &str,
    role: ModelRole,
    source: BindingSource,
    enabled: bool,
) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let created = model_bindings::create_binding(
                conn,
                1_000,
                &ModelBindingEdit {
                    id: id.into(),
                    agent_id: agent_id.into(),
                    model_profile_id: model_id.into(),
                    role,
                    binding_source: source,
                    enabled,
                    priority: 100,
                },
            )?;
            assert_eq!(created.revision, 1);
            Ok(())
        })
        .unwrap();
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("fixture".into()),
        schema: None,
        object: None,
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    }
}

fn send_turn(controller: &mut AppController, prompt: &str) -> AiConversationJob {
    controller
        .prepare_ai_conversation(prompt, &target())
        .expect("routing should resolve a usable model")
}

fn send_model(controller: &mut AppController, prompt: &str) -> String {
    send_turn(controller, prompt).model_profile_id
}

/// 读取本轮 `model_route` 事件的摘要（脱敏公开字段）。
fn model_route_summary(fixture: &Fixture, turn_id: &str) -> serde_json::Value {
    let event = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(
                fluxdb_storage::ai_platform::run_events::list_for_turn(conn, turn_id)?
                    .into_iter()
                    .find(|event| {
                        event.kind == fluxdb_storage::ai_platform::run_events::KIND_MODEL_ROUTE
                    }),
            )
        })
        .unwrap()
        .expect("本轮应有模型路由事件");
    serde_json::from_str(&event.summary_json).expect("路由事件摘要为 JSON")
}

#[test]
fn configured_agent_binding_wins_over_agent_default_and_global_default() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let bound = create_model(&mut controller, "bound", "openai");
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let global = create_model(&mut controller, "global", "openai");
    let agent = create_agent(
        &mut controller,
        "routing-agent",
        Some(agent_default.clone()),
    );
    set_default_agent(&fixture, &agent);
    // 全局默认指向另一个模型，但 Agent 角色绑定优先级更高。
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(global.clone()),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    bind(
        &fixture,
        "b1",
        &agent,
        &bound,
        ModelRole::MainCandidate,
        BindingSource::System,
        true,
    );
    assert_eq!(send_model(&mut controller, "binding wins"), bound);
}

#[test]
fn route_event_records_binding_source_and_the_model_the_provider_will_use() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let bound = create_model(&mut controller, "bound", "openai");
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let agent = create_agent(
        &mut controller,
        "routing-event-agent",
        Some(agent_default.clone()),
    );
    set_default_agent(&fixture, &agent);
    bind(
        &fixture,
        "b1",
        &agent,
        &bound,
        ModelRole::MainCandidate,
        BindingSource::System,
        true,
    );

    let job = send_turn(&mut controller, "事件应记录真实路由");
    assert_eq!(job.model_profile_id, bound, "Provider 实际模型 = 绑定模型");

    let summary = model_route_summary(&fixture, &job.turn_id);
    assert_eq!(summary["role"], "main");
    assert_eq!(summary["source"], "agent_binding");
    assert_eq!(summary["model_profile_id"], serde_json::json!(bound));
    assert_eq!(summary["provider"], "openai");
    assert_eq!(summary["model"], "offline-bound");
    assert_eq!(summary["agent_id"], serde_json::json!(agent));
    // 事件是公开摘要：不得带凭据材料或 endpoint。
    assert!(summary.get("api_key").is_none(), "{summary}");
    assert!(summary.get("endpoint").is_none(), "{summary}");
}

#[test]
fn route_event_records_session_selection_over_agent_default() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let selected = create_model(&mut controller, "picked", "openai");
    let agent = create_agent(
        &mut controller,
        "routing-event-selection",
        Some(agent_default),
    );
    set_default_agent(&fixture, &agent);

    let revision = controller.state().ai_conversation.model_selection_revision;
    let event = controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::SelectModel {
            model_id: Some(selected.clone()),
            expected_revision: revision,
        },
    ));
    assert!(!matches!(event, AppEvent::AiSettingsError(_)), "{event:?}");

    let job = send_turn(&mut controller, "显式选择优先");
    assert_eq!(
        job.model_profile_id, selected,
        "页面选择 = Provider 实际模型"
    );
    let summary = model_route_summary(&fixture, &job.turn_id);
    assert_eq!(summary["source"], "session_selection");
    assert_eq!(summary["model_profile_id"], serde_json::json!(selected));
}

#[test]
fn denied_egress_model_fails_visibly_without_silent_fallback() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let allowed = create_model(&mut controller, "allowed", "openai");
    let denied = create_model(&mut controller, "denied", "openai");
    let agent = create_agent(&mut controller, "egress-agent", Some(denied.clone()));
    set_default_agent(&fixture, &agent);
    // 默认模型全局指向 allowed，但 Agent 默认绑定的是被禁止出站的 denied：
    // 固定模型不可换，必须因 egress 拒绝，而不是悄悄回退到 allowed。
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(allowed.clone()),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    let id = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == denied)
        .unwrap()
        .id
        .clone();
    // 用户显式关闭该模型的出站许可。
    let revision = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == denied)
        .unwrap()
        .revision;
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            id: Some(id),
            display_name: "disabled-egress".into(),
            provider: "openai".into(),
            model: "offline-denied".into(),
            enabled: true,
            egress: false,
            expected_revision: Some(revision),
            ..Default::default()
        })),
        AppEvent::AiModelSaved { .. }
    ));
    // 发送必须失败且带"禁止数据出站"的可见原因，而不是回退到 allowed。
    let err = controller
        .prepare_ai_conversation("不应静默回退", &target())
        .expect_err("禁止出站的模型必须可见失败");
    assert!(
        err.message.contains("禁止数据出站"),
        "实际: {}",
        err.message
    );
}

#[test]
fn global_egress_gate_blocks_even_allowed_models() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let global = create_model(&mut controller, "global", "openai");
    let bare = create_agent(&mut controller, "bare-gate", None);
    set_default_agent(&fixture, &bare);
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(global.clone()),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    // 关闭全局出站闸门：即使模型本身允许发送，也必须全部拒绝。
    let gate_revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    let gate_event = controller.dispatch(AppCommand::SetAiModelEgressEnabled {
        enabled: false,
        expected_revision: gate_revision,
    });
    assert!(
        matches!(gate_event, AppEvent::AiModelEgressEnabledChanged { .. }),
        "关闭全局闸门失败: {gate_event:?}"
    );
    let err = controller
        .prepare_ai_conversation("全局禁止出站", &target())
        .expect_err("全局闸门关闭时必须可见失败");
    assert!(
        err.message.contains("全局禁止模型数据出站"),
        "实际: {}",
        err.message
    );
    // 重新打开后恢复可用。
    assert!(matches!(
        controller.dispatch(AppCommand::SetAiModelEgressEnabled {
            enabled: true,
            expected_revision: gate_revision + 1,
        }),
        AppEvent::AiModelEgressEnabledChanged { .. }
    ));
    assert_eq!(send_model(&mut controller, "恢复后可用"), global);
}

#[test]
fn without_binding_falls_back_to_agent_default() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let agent = create_agent(&mut controller, "no-binding", Some(agent_default.clone()));
    set_default_agent(&fixture, &agent);
    assert_eq!(send_model(&mut controller, "agent default"), agent_default);
}

#[test]
fn agent_without_model_falls_back_to_global_default() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let global = create_model(&mut controller, "global", "openai");
    let bare = create_agent(&mut controller, "bare", None);
    set_default_agent(&fixture, &bare);
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(global.clone()),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    let job = send_turn(&mut controller, "global default");
    assert_eq!(job.model_profile_id, global);
    let summary = model_route_summary(&fixture, &job.turn_id);
    assert_eq!(summary["source"], "global_default");
    assert_eq!(summary["model_profile_id"], serde_json::json!(global));
}

#[test]
fn disabled_binding_is_skipped_and_system_reconcile_preserves_user_disable() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let bound = create_model(&mut controller, "bound", "openai");
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let agent = create_agent(
        &mut controller,
        "skip-disabled",
        Some(agent_default.clone()),
    );
    set_default_agent(&fixture, &agent);
    // 用户手动建绑定后停用；system reconcile 不得把它改回启用。
    bind(
        &fixture,
        "u1",
        &agent,
        &bound,
        ModelRole::MainCandidate,
        BindingSource::User,
        true,
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            model_bindings::set_binding_enabled(conn, 2_000, "u1", false, 1)?;
            let created = model_bindings::ensure_system_binding(
                conn,
                3_000,
                "sys1",
                &agent,
                &bound,
                ModelRole::MainCandidate,
                50,
            )?;
            assert!(!created, "已存在用户绑定时 reconcile 不应新建");
            Ok(())
        })
        .unwrap();
    // 停用的绑定不参与候选，回落到 Agent 默认模型。
    assert_eq!(
        send_model(&mut controller, "skip disabled binding"),
        agent_default
    );
}

#[test]
fn agent_default_that_is_not_usable_fails_visibly_without_silent_fallback() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let agent_default = create_model(&mut controller, "agent-default", "openai");
    let global = create_model(&mut controller, "global", "openai");
    let agent = create_agent(
        &mut controller,
        "broken-default",
        Some(agent_default.clone()),
    );
    set_default_agent(&fixture, &agent);
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(global),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    // 把 Agent 默认模型改成决策用途：主模型路由必须报错，不能静默换到全局默认。
    let old = fixture
        .storage
        .with_ai_connection(|conn| models::load_model(conn, &agent_default))
        .unwrap();
    let event = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        id: Some(agent_default.clone()),
        display_name: old.display_name.clone(),
        provider: "decisions".into(),
        model: old.model.clone(),
        endpoint: old.endpoint.clone(),
        enabled: true,
        context_window_tokens: old.context_window_tokens,
        max_output_tokens: old.max_output_tokens,
        expected_revision: Some(old.revision),
        ..Default::default()
    }));
    assert!(matches!(event, AppEvent::AiModelSaved { .. }), "{event:?}");
    let error = controller
        .prepare_ai_conversation("must not fallback", &target())
        .expect_err("decision-kind agent default must fail main routing");
    assert!(
        error.message.contains("用途不匹配"),
        "actual: {}",
        error.message
    );
    // 未产生任何会话。
    let sessions = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::sessions::list_sessions(conn, 10)?.len())
        })
        .unwrap();
    assert_eq!(sessions, 0);
}

#[test]
fn conversation_level_selection_overrides_agent_binding() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let bound = create_model(&mut controller, "bound", "openai");
    let explicit = create_model(&mut controller, "explicit", "openai");
    let agent = create_agent(&mut controller, "select-agent", Some(bound.clone()));
    set_default_agent(&fixture, &agent);
    bind(
        &fixture,
        "b1",
        &agent,
        &bound,
        ModelRole::MainCandidate,
        BindingSource::System,
        true,
    );
    let revision = controller.state().ai_conversation.model_selection_revision;
    assert!(matches!(
        controller.dispatch(AppCommand::AiConversation(
            fluxdb_app::AiConversationCommand::SelectModel {
                model_id: Some(explicit.clone()),
                expected_revision: revision,
            }
        )),
        AppEvent::AiConversationLoaded(_)
    ));
    assert_eq!(send_model(&mut controller, "explicit wins"), explicit);
}

#[test]
fn capability_labels_reach_the_settings_list_so_the_ui_shows_economic_and_premium_tiers() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    // 低成本模型：输出价 $1/1M → “经济”；小窗口不给“长上下文”。
    let cheap = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "经济档".into(),
        provider: "openai".into(),
        model: "offline-cheap".into(),
        endpoint: Some("https://model.example.invalid/v1".into()),
        enabled: true,
        egress: true,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(1_024),
        input_price_micros: Some(300_000),
        output_price_micros: Some(1_000),
        ..Default::default()
    }));
    let cheap = match cheap {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save cheap: {other:?}"),
    };
    // 高价 + 大窗口：应同时得到“高能力”和“长上下文”。
    let premium = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "高价档".into(),
        provider: "openai".into(),
        model: "offline-premium".into(),
        endpoint: Some("https://model.example.invalid/v1".into()),
        enabled: true,
        egress: true,
        context_window_tokens: Some(400_000),
        max_output_tokens: Some(32_000),
        input_price_micros: Some(15_000_000),
        output_price_micros: Some(80_000),
        ..Default::default()
    }));
    let premium = match premium {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save premium: {other:?}"),
    };

    // 列表项必须带标签，UI 才能渲染“经济 / 高能力”前缀；标签由价格与窗口推导，不看模型名。
    fn labels_of(controller: &AppController, id: &str) -> Vec<String> {
        controller
            .state()
            .ai_models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.capability_labels.clone())
            .unwrap_or_default()
    }
    assert_eq!(labels_of(&controller, &cheap), vec!["经济".to_string()]);
    let premium_labels = labels_of(&controller, &premium);
    assert!(
        premium_labels.contains(&"高能力".to_string()),
        "{premium_labels:?}"
    );
    assert!(
        premium_labels.contains(&"长上下文".to_string()),
        "{premium_labels:?}"
    );

    // 改价后标签必须跟着更新，而不是停留在旧值。
    let revision = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == cheap)
        .map(|m| m.revision)
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            id: Some(cheap.clone()),
            display_name: "经济档".into(),
            provider: "openai".into(),
            model: "offline-cheap".into(),
            endpoint: Some("https://model.example.invalid/v1".into()),
            enabled: true,
            egress: true,
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            input_price_micros: Some(15_000_000),
            output_price_micros: Some(80_000),
            expected_revision: Some(revision),
            ..Default::default()
        })),
        AppEvent::AiModelSaved { .. }
    ));
    assert_eq!(labels_of(&controller, &cheap), vec!["高能力".to_string()]);
}
