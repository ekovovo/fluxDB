//! 模型路由 precedence（设计 §17，T033 剩余部分）。
//!
//! 用真实 SQLite + 生产 AppController 验证：会话显式选择 → Agent 角色绑定 →
//! Agent 默认 → 全局默认；绑定启停影响候选；system reconcile 不覆盖 user 绑定。
//! 全程离线：只准备模型/绑定元数据，不发模型请求、不接触真实凭据。

use std::{fs, path::PathBuf};

use fluxdb_app::{
    AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent, TargetRef,
    TurnTargetPin,
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

fn send_model(controller: &mut AppController, prompt: &str) -> String {
    controller
        .prepare_ai_conversation(prompt, &target())
        .expect("routing should resolve a usable model")
        .model_profile_id
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
    assert_eq!(send_model(&mut controller, "global default"), global);
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
