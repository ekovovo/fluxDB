//! Agent 默认值/删除应用链路：AppCommand → 独立文件 SQLite → AppState → 新 controller。
//! prepare 只生成待运行任务；HTTP 测试仅访问自建 localhost，不执行模型/工具或读取用户配置。
use std::{fs, path::PathBuf};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationView, AiDiscoveredModel,
    AiModelSaveInput, AppCommand, AppController, AppEvent, TargetRef, TurnTargetPin,
    fetch_ai_model_capabilities, parse_ai_model_capabilities,
};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{agents, conversation, plugins, sessions, settings},
};
use sessions::TurnStatus;

const BUILTIN: &str = agents::BUILTIN_AGENT_ID;
const SESSION: &str = "defaults-app-history";
const TURN: &str = "defaults-app-turn";

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-agent-defaults-app-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).expect("独占临时目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("生产入口建库");
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        assert_eq!(
            controller.dispatch(AppCommand::RefreshAiSettings),
            AppEvent::AiSettingsLoaded
        );
        controller
    }

    fn agent(&self, id: &str) -> agents::AgentView {
        self.storage
            .with_ai_connection(|conn| agents::load_agent(conn, id))
            .unwrap()
    }

    fn history(&self, agent: &str, status: TurnStatus) {
        self.storage
            .with_ai_connection(|conn| {
                conversation::send(
                    conn,
                    2_000,
                    true,
                    SESSION,
                    agent,
                    TURN,
                    "defaults-app-message",
                    "DEFAULTS_APP 唯一历史正文",
                    r#"{"kind":"sql","marker":"DEFAULTS_APP"}"#,
                    r#"{"marker":"DEFAULTS_APP","model":"offline-fixture"}"#,
                )?;
                sessions::append_run_event(conn, 2_001, TURN, "fixture", r#"{"public":"保留"}"#)?;
                let turn = sessions::load_turn(conn, TURN)?;
                sessions::update_turn_status(conn, 2_002, TURN, status, None, turn.revision)?;
                Ok(())
            })
            .unwrap();
    }

    fn references(&self, agent: &str) {
        self.storage
            .with_ai_connection(|conn| {
                let mut runtime = settings::load_runtime_settings(conn)?;
                let revision = runtime.revision;
                runtime.default_agent_id = Some(agent.into());
                settings::save_runtime_settings(conn, 1_500, &runtime, revision)?;
                let instance = plugins::list_plugin_instances(conn)?.remove(0);
                plugins::bind_plugin_system(
                    conn,
                    1_501,
                    "defaults-app-binding",
                    agent,
                    &instance.id,
                )
            })
            .unwrap();
    }

    fn raw_agents(&self) -> Vec<String> {
        let conn = self.storage.open_ai_connection().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT json_object('id',id,'name',display_name,'description',description,
                'enabled',enabled,'model',default_model_profile_id,'prompt',system_prompt,
                'budget',budget_policy_id,'tools',allowed_tools_json,
                'permissions',permissions_json,'options',options_json,'revision',revision,
                'created',created_at_ms,'updated',updated_at_ms) FROM ai_agents ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn snapshot(&self) -> sessions::SessionSnapshot {
        self.storage
            .with_ai_connection(|conn| sessions::session_snapshot(conn, SESSION))
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 每次 FileStorage API 已释放 SQLite 连接；只清理 fixture 独占 root。
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn input() -> AiAgentSaveInput {
    AiAgentSaveInput {
        display_name: "DEFAULTS_APP 自定义 Agent".into(),
        description: "独立业务 fixture".into(),
        system_prompt: "DEFAULTS_APP 保存前提示词".into(),
        allowed_tools: vec!["db.query".into()],
        enabled: true,
        ..Default::default()
    }
}

fn save(controller: &mut AppController, input: AiAgentSaveInput) -> (String, i64) {
    match controller.dispatch(AppCommand::SaveAiAgent(input)) {
        AppEvent::AiAgentSaved { id, revision } => (id, revision),
        other => panic!("应成功保存 Agent，实际: {other:?}"),
    }
}

fn assert_error(event: AppEvent) {
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(!message.trim().is_empty(), "拒绝应提供可见错误")
        }
        other => panic!("应返回 AiSettingsError，实际: {other:?}"),
    }
}

fn default_model(f: &Fixture, controller: &mut AppController) -> String {
    let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "DEFAULTS_APP 离线模型配置".into(),
        provider: "openai".into(),
        model: "defaults-offline-fixture".into(),
        endpoint: Some("http://127.0.0.1:9/v1".into()),
        enabled: true,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(1_024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存离线配置失败: {other:?}"),
    };
    let revision = f
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(model.clone()),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    model
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: None,
        database: None,
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

fn load_history(controller: &mut AppController) -> AiConversationView {
    match controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
        session_id: Some(SESSION.into()),
    })) {
        AppEvent::AiConversationLoaded(view) => {
            assert_eq!(controller.state().ai_conversation, view);
            assert!(
                view.messages
                    .iter()
                    .any(|message| message.parts.iter().any(|part| {
                        serde_json::from_str::<serde_json::Value>(&part.payload_json)
                            .is_ok_and(|json| json["text"] == "DEFAULTS_APP 唯一历史正文")
                    })),
                "真实历史正文进入生产 AppState 投影"
            );
            view
        }
        other => panic!("应恢复历史，实际: {other:?}"),
    }
}

fn assert_reloaded_history_view(before: &AiConversationView, mut after: AiConversationView) {
    // 显式 Load 会 CAS 保存 last_session_id；只允许这次加载递增的偏好 revision。
    assert!(after.preferences.revision > before.preferences.revision);
    after.preferences.revision = before.preferences.revision;
    assert_eq!(&after, before, "历史内容和其他公开视图字段不得改变");
}

fn assert_history_eq(before: &sessions::SessionSnapshot, after: &sessions::SessionSnapshot) {
    assert_eq!(before.session, after.session);
    assert_eq!(before.turns, after.turns);
    assert_eq!(before.messages, after.messages);
    assert_eq!(before.parts, after.parts);
    assert_eq!(before.run_events, after.run_events);
    assert_eq!(before.ui_state, after.ui_state);
}

#[test]
fn builtin_and_default_percent_appear_on_first_load_and_custom_create() {
    let f = Fixture::new();
    let mut controller = f.controller();
    assert_eq!(AiAgentSaveInput::default().compression_trigger_percent, 80);
    assert_eq!(controller.state().ai_agents.len(), 1);
    let builtin = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == BUILTIN)
        .unwrap();
    assert!(builtin.enabled);
    assert_eq!(builtin.compression_trigger_percent, 80);
    assert_eq!(builtin.system_prompt, f.agent(BUILTIN).system_prompt);
    let (id, revision) = save(&mut controller, input());
    let current = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == id)
        .unwrap();
    assert_eq!(current.revision, revision);
    assert_eq!(current.compression_trigger_percent, 80);
    assert_eq!(f.agent(&id).compression_trigger_percent, 80);
    let reopened = f.controller();
    assert_eq!(reopened.state().ai_agents.len(), 2);
    assert_eq!(
        reopened.state().ai_agents.iter().find(|a| a.id == id),
        Some(current)
    );
}

#[test]
fn builtin_edit_prompt_model_and_percent_updates_state_and_survives_reopen_seed() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let model = default_model(&f, &mut controller);
    let original = f.agent(BUILTIN);
    let (id, revision) = save(
        &mut controller,
        AiAgentSaveInput {
            id: Some(BUILTIN.into()),
            system_prompt: "DEFAULTS_APP 用户编辑的内置提示词".into(),
            model_profile_id: Some(model.clone()),
            compression_trigger_percent: 64,
            expected_revision: original.revision,
            ..input()
        },
    );
    assert_eq!(id, BUILTIN);
    assert_eq!(revision, original.revision + 1);
    let item = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == BUILTIN)
        .unwrap()
        .clone();
    assert_eq!(item.system_prompt, "DEFAULTS_APP 用户编辑的内置提示词");
    assert_eq!(item.model_profile_id.as_deref(), Some(model.as_str()));
    assert_eq!(item.compression_trigger_percent, 64);
    let db = f.agent(BUILTIN);
    assert_eq!(db.system_prompt, item.system_prompt);
    assert_eq!(db.default_model_profile_id, item.model_profile_id);
    assert_eq!(
        db.compression_trigger_percent,
        item.compression_trigger_percent
    );
    let raw = f.raw_agents();
    for now in [3_000, 4_000] {
        f.storage
            .with_ai_connection(|conn| agents::seed_builtin_agent(conn, now))
            .unwrap();
    }
    drop(controller);
    let reopened = f.controller();
    assert_eq!(
        reopened
            .state()
            .ai_agents
            .iter()
            .filter(|a| a.id == BUILTIN)
            .count(),
        1
    );
    assert_eq!(
        reopened.state().ai_agents.iter().find(|a| a.id == BUILTIN),
        Some(&item)
    );
    assert_eq!(f.raw_agents(), raw);
}

#[test]
fn builtin_delete_and_disable_errors_preserve_app_state_and_sqlite() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let original = f.agent(BUILTIN);
    let before = controller.state().ai_agents.clone();
    let raw = f.raw_agents();
    assert_error(controller.dispatch(AppCommand::DeleteAiAgent {
        id: BUILTIN.into(),
        expected_revision: original.revision,
    }));
    assert_eq!(controller.state().ai_agents, before);
    assert_eq!(f.raw_agents(), raw);
    assert_error(
        controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            id: Some(BUILTIN.into()),
            enabled: false,
            expected_revision: original.revision,
            ..input()
        })),
    );
    assert_eq!(controller.state().ai_agents, before);
    assert_eq!(f.raw_agents(), raw);
}

#[test]
fn invalid_percent_rejects_create_and_edit_without_state_or_db_changes() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let (id, revision) = save(&mut controller, input());
    let before = controller.state().ai_agents.clone();
    let raw = f.raw_agents();
    for percent in [0, 9, 96, 100, u8::MAX] {
        assert_error(
            controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                compression_trigger_percent: percent,
                ..input()
            })),
        );
        assert_error(
            controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                id: Some(id.clone()),
                compression_trigger_percent: percent,
                expected_revision: revision,
                ..input()
            })),
        );
        assert_eq!(controller.state().ai_agents, before);
        assert_eq!(f.raw_agents(), raw);
    }
}

#[test]
fn stale_save_and_delete_are_errors_and_leave_winning_edit_and_references_intact() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let (id, old_revision) = save(&mut controller, input());
    f.references(&id);
    f.history(&id, TurnStatus::Completed);
    let (_, revision) = save(
        &mut controller,
        AiAgentSaveInput {
            id: Some(id.clone()),
            system_prompt: "DEFAULTS_APP 并发获胜编辑".into(),
            compression_trigger_percent: 55,
            expected_revision: old_revision,
            ..input()
        },
    );
    assert_eq!(revision, old_revision + 1);
    let before = controller.state().ai_agents.clone();
    let raw = f.raw_agents();
    let history = f.snapshot();
    let runtime = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    let bindings = f
        .storage
        .with_ai_connection(|conn| plugins::list_agent_plugin_bindings(conn, &id))
        .unwrap();
    assert_error(
        controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            id: Some(id.clone()),
            expected_revision: old_revision,
            ..input()
        })),
    );
    assert_error(controller.dispatch(AppCommand::DeleteAiAgent {
        id: id.clone(),
        expected_revision: old_revision,
    }));
    assert_eq!(controller.state().ai_agents, before);
    assert_eq!(f.raw_agents(), raw);
    assert_eq!(
        f.storage
            .with_ai_connection(settings::load_runtime_settings)
            .unwrap(),
        runtime
    );
    assert_eq!(
        f.storage
            .with_ai_connection(|conn| plugins::list_agent_plugin_bindings(conn, &id))
            .unwrap(),
        bindings
    );
    assert_history_eq(&history, &f.snapshot());
}

#[test]
fn active_and_waiting_approval_agents_cannot_be_deleted_via_command() {
    for status in [
        TurnStatus::Generating,
        TurnStatus::WaitingApproval,
        TurnStatus::OutcomeUnknown,
    ] {
        let f = Fixture::new();
        let mut controller = f.controller();
        let (id, revision) = save(&mut controller, input());
        f.references(&id);
        f.history(&id, status);
        let conversation = load_history(&mut controller);
        let before = controller.state().ai_agents.clone();
        let raw = f.raw_agents();
        let history = f.snapshot();
        assert_error(controller.dispatch(AppCommand::DeleteAiAgent {
            id,
            expected_revision: revision,
        }));
        assert_eq!(controller.state().ai_agents, before);
        assert_eq!(controller.state().ai_conversation, conversation);
        assert_eq!(f.raw_agents(), raw);
        assert_history_eq(&history, &f.snapshot());
    }
}

#[test]
fn custom_delete_emits_event_removes_state_and_keeps_history_visible_after_restart() {
    let f = Fixture::new();
    let mut controller = f.controller();
    default_model(&f, &mut controller);
    let (id, revision) = save(&mut controller, input());
    f.references(&id);
    f.history(&id, TurnStatus::Completed);
    let history_view = load_history(&mut controller);
    let history = f.snapshot();
    assert_eq!(
        controller.dispatch(AppCommand::DeleteAiAgent {
            id: id.clone(),
            expected_revision: revision,
        }),
        AppEvent::AiAgentDeleted { id: id.clone() }
    );
    assert!(controller.state().ai_agents.iter().all(|a| a.id != id));
    assert!(
        controller
            .state()
            .ai_agents
            .iter()
            .any(|a| a.id == BUILTIN && a.enabled)
    );
    assert_reloaded_history_view(&history_view, load_history(&mut controller));
    assert_history_eq(&history, &f.snapshot());
    f.storage
        .with_ai_connection(|conn| {
            assert_ne!(
                settings::load_runtime_settings(conn)?
                    .default_agent_id
                    .as_deref(),
                Some(id.as_str())
            );
            assert!(plugins::list_agent_plugin_bindings(conn, &id)?.is_empty());
            let options: String = conn
                .query_row(
                    "SELECT options_json FROM ai_agents WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            let options: serde_json::Value = serde_json::from_str(&options).unwrap();
            assert_eq!(options["deleted"], true);
            let fk_errors: i64 = conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(fk_errors, 0);
            Ok(())
        })
        .unwrap();
    drop(controller);
    let mut reopened = f.controller();
    assert!(reopened.state().ai_agents.iter().all(|a| a.id != id));
    assert_reloaded_history_view(&history_view, load_history(&mut reopened));
    assert_history_eq(&history, &f.snapshot());
    assert!(matches!(
        reopened.dispatch(AppCommand::AiConversation(AiConversationCommand::New)),
        AppEvent::AiConversationLoaded(_)
    ));
    let job = reopened
        .prepare_ai_conversation("DEFAULTS_APP 删除后内置 fallback", &target())
        .unwrap();
    let session = f
        .storage
        .with_ai_connection(|conn| sessions::load_session(conn, &job.session_id))
        .unwrap();
    assert_eq!(session.agent_id, BUILTIN);
    assert_history_eq(&history, &f.snapshot());
}

#[test]
fn new_conversation_without_custom_agent_uses_builtin_and_default_model_offline() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let model = default_model(&f, &mut controller);
    assert_eq!(controller.state().ai_agents.len(), 1, "不先手工新建 Agent");
    let job = controller
        .prepare_ai_conversation("DEFAULTS_APP 内置首次发送", &target())
        .expect("内置 fallback 应可准备发送");
    assert_eq!(job.model_profile_id, model);
    assert_eq!(
        controller.state().ai_conversation.session_id.as_deref(),
        Some(job.session_id.as_str())
    );
    f.storage
        .with_ai_connection(|conn| {
            let session = sessions::load_session(conn, &job.session_id)?;
            assert_eq!(session.agent_id, BUILTIN);
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            assert_eq!(snapshot["agent_id"], BUILTIN);
            assert_eq!(snapshot["model_profile_id"], model);
            let messages = sessions::list_messages(conn, &job.session_id)?;
            assert_eq!(messages.len(), 1, "仅准备用户消息，未运行真实模型");
            assert_eq!(messages[0].role, "user");
            Ok(())
        })
        .unwrap();
}

#[test]
fn no_default_agent_prefers_builtin_not_most_recent_custom_agent() {
    let f = Fixture::new();
    let mut controller = f.controller();
    default_model(&f, &mut controller);
    let (custom, _) = save(&mut controller, input());
    // 自定义行更新时间最新，仍不能仅按最近编辑来决定新会话 Agent。
    f.storage
        .with_ai_connection(|conn| {
            conn.execute(
                "UPDATE ai_agents SET updated_at_ms=9223372036854775807 WHERE id=?1",
                [&custom],
            )
            .unwrap();
            let mut runtime = settings::load_runtime_settings(conn)?;
            let revision = runtime.revision;
            runtime.default_agent_id = None;
            settings::save_runtime_settings(conn, 3_000, &runtime, revision)?;
            Ok(())
        })
        .unwrap();
    let job = controller
        .prepare_ai_conversation("DEFAULTS_APP 不应随机选 Agent", &target())
        .unwrap();
    let session = f
        .storage
        .with_ai_connection(|conn| sessions::load_session(conn, &job.session_id))
        .unwrap();
    assert_eq!(session.agent_id, BUILTIN);
}

#[test]
fn missing_model_still_reports_model_error_without_empty_history() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let error = controller
        .prepare_ai_conversation("不能生成空历史", &target())
        .expect_err("缺模型应拒绝");
    assert!(error.message.contains("模型"));
    assert!(!error.message.contains("新建并启用一个 Agent"));
    assert!(controller.state().ai_conversation.session_id.is_none());
    f.storage
        .with_ai_connection(|conn| {
            assert!(sessions::list_sessions(conn, 100)?.is_empty());
            assert_eq!(agents::list_agents(conn)?.len(), 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn capacity_metadata_normal_aliases_and_top_provider_values_are_preserved() {
    let actual = parse_ai_model_capabilities(&serde_json::json!({"data":[
        {"id":"z-direct","context_window_tokens":131072,"max_output_tokens":8192},
        {"id":"a-nested","top_provider":{"context_length":200000,"max_completion_tokens":16000}},
        {"slug":"m-alias","max_input_tokens":32768,"max_tokens":4096}
    ]}))
    .unwrap();
    assert_eq!(
        actual,
        vec![
            AiDiscoveredModel {
                id: "a-nested".into(),
                context_window_tokens: Some(200000),
                max_output_tokens: Some(16000)
            },
            AiDiscoveredModel {
                id: "m-alias".into(),
                context_window_tokens: Some(32768),
                max_output_tokens: Some(4096)
            },
            AiDiscoveredModel {
                id: "z-direct".into(),
                context_window_tokens: Some(131072),
                max_output_tokens: Some(8192)
            },
        ]
    );
}

#[test]
fn capacity_missing_or_null_stays_unknown_instead_of_inventing_defaults() {
    let actual = parse_ai_model_capabilities(&serde_json::json!({"models":[
        {"id":"a-missing"},
        {"id":"b-null","context_window_tokens":null,"max_output_tokens":null},
        {"id":"c-output-only","max_output_tokens":4096},
        {"id":"d-unknown","context_size":8192,"output_limit":4096}
    ]}))
    .unwrap();
    assert_eq!(actual[0].context_window_tokens, None);
    assert_eq!(actual[0].max_output_tokens, None);
    assert_eq!(actual[1].context_window_tokens, None);
    assert_eq!(actual[1].max_output_tokens, None);
    assert_eq!(actual[2].context_window_tokens, None);
    assert_eq!(actual[2].max_output_tokens, Some(4096));
    assert_eq!(actual[3].context_window_tokens, None);
    assert_eq!(actual[3].max_output_tokens, None);
}

#[test]
fn capacity_invalid_numbers_types_and_output_over_window_are_not_adopted() {
    for invalid in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("8192"),
        serde_json::json!(true),
        serde_json::json!({}),
        serde_json::json!([]),
        serde_json::json!(1000000001_i64),
        serde_json::json!(u64::MAX),
    ] {
        let actual = parse_ai_model_capabilities(&serde_json::json!({"data":[{
            "id":"invalid-capacity","context_window_tokens":invalid,"max_output_tokens":invalid
        }]}))
        .unwrap();
        assert_eq!(actual[0].context_window_tokens, None, "非法容量 {invalid}");
        assert_eq!(actual[0].max_output_tokens, None, "非法容量 {invalid}");
    }
    let actual = parse_ai_model_capabilities(&serde_json::json!({"data":[{
        "id":"over-window","context_window_tokens":8192,"max_output_tokens":16384
    }]}))
    .unwrap();
    assert_eq!(actual[0].context_window_tokens, Some(8192));
    assert_eq!(actual[0].max_output_tokens, None);
}

#[test]
fn capacity_duplicate_ids_deduplicate_and_conflicting_fields_stay_unknown() {
    let actual = parse_ai_model_capabilities(&serde_json::json!({"data":[
        {"id":"same","context_window_tokens":8192,"max_output_tokens":1024},
        {"id":"same","context_window_tokens":8192,"max_output_tokens":1024},
        {"id":"window-conflict","context_window_tokens":8192,"max_output_tokens":1024},
        {"id":"window-conflict","context_window_tokens":16384,"max_output_tokens":1024},
        {"id":"output-conflict","context_window_tokens":8192,"max_output_tokens":1024},
        {"id":"output-conflict","context_window_tokens":8192,"max_output_tokens":2048},
        {"id":"both-conflict","context_window_tokens":8192,"max_output_tokens":1024},
        {"id":"both-conflict","context_window_tokens":16384,"max_output_tokens":2048},
        {"id":"both-conflict","context_window_tokens":8192,"max_output_tokens":1024}
    ]}))
    .unwrap();
    assert_eq!(
        actual,
        vec![
            AiDiscoveredModel {
                id: "both-conflict".into(),
                context_window_tokens: None,
                max_output_tokens: None
            },
            AiDiscoveredModel {
                id: "output-conflict".into(),
                context_window_tokens: Some(8192),
                max_output_tokens: None
            },
            AiDiscoveredModel {
                id: "same".into(),
                context_window_tokens: Some(8192),
                max_output_tokens: Some(1024)
            },
            AiDiscoveredModel {
                id: "window-conflict".into(),
                context_window_tokens: None,
                max_output_tokens: Some(1024)
            },
        ]
    );
}

#[test]
fn capacity_invalid_empty_or_oversized_model_lists_return_errors() {
    for body in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"data":{}}),
        serde_json::json!({"data":[]}),
        serde_json::json!({"models":[{"id":""},{"id":"  "},{"id":17}]}),
        serde_json::json!({"data":[{"id":"bad\ncontrol"},{"id":"x".repeat(513)}]}),
        serde_json::json!({"data":vec![serde_json::json!({"id":"many"});4097]}),
    ] {
        assert!(
            parse_ai_model_capabilities(&body).is_err(),
            "非法列表应明确拒绝"
        );
    }
}

// 只绑定回环随机端口；网络用例运行必须获得 sandbox 提权，不改任何用户配置。
struct LocalHttpMock {
    endpoint: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl LocalHttpMock {
    fn new(status: u16, headers: &str, body: &str) -> Self {
        use std::{
            io::{Read, Write},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::{Duration, Instant},
        };
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("localhost mock：需提权运行");
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept 失败: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                // 并行全量测试下 CPU 竞争会让 2s 读超时偶发触发；超时按"还没有客户端
                // 发完请求"处理并继续等待，直到整体 deadline，避免把负载抖动当成用例失败。
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) => panic!("请求头完成前连接关闭"),
                        Ok(count) => {
                            bytes.extend_from_slice(&buffer[..count]);
                            assert!(bytes.len() <= 16384, "mock 请求头超限");
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            assert!(Instant::now() < deadline, "等待 mock 请求头超过 5s");
                            continue;
                        }
                        Err(error) => panic!("mock 读取有界请求头失败: {error}"),
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                stream
                    .write_all(response.as_bytes())
                    .expect("mock 返回响应");
            }
            requests
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.worker.take().unwrap().join().expect("mock 正常退出")
    }
}

impl Drop for LocalHttpMock {
    fn drop(&mut self) {
        // 断言失败也回收线程/端口，避免污染其他并行独立 fixture。
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

#[test]
fn network_openai_get_preserves_id_without_inventing_capacity() {
    let mock = LocalHttpMock::new(200, "", r#"{"data":[{"id":"plain-id"}]}"#);
    let result =
        fetch_ai_model_capabilities("openai", Some(&mock.endpoint), "  fixture-openai-key  ");
    let requests = mock.finish();
    assert_eq!(
        result.unwrap(),
        vec![AiDiscoveredModel {
            id: "plain-id".into(),
            context_window_tokens: None,
            max_output_tokens: None,
        }]
    );
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert_eq!(
        request_header(&requests[0], "authorization"),
        Some("Bearer fixture-openai-key")
    );
    assert_eq!(request_header(&requests[0], "x-api-key"), None);
    assert_eq!(request_header(&requests[0], "anthropic-version"), None);
    assert!(request_header(&requests[0], "content-length").is_none_or(|value| value == "0"));
}

#[test]
fn network_anthropic_get_uses_api_key_and_version_not_bearer() {
    let mock = LocalHttpMock::new(
        200,
        "",
        r#"{"data":[{"id":"claude-fixture","max_input_tokens":200000,"max_output_tokens":8192}]}"#,
    );
    let endpoint = format!("{}/v1/", mock.endpoint);
    let result = fetch_ai_model_capabilities("anthropic", Some(&endpoint), "fixture-anthropic-key");
    let requests = mock.finish();
    assert_eq!(
        result.unwrap(),
        vec![AiDiscoveredModel {
            id: "claude-fixture".into(),
            context_window_tokens: Some(200000),
            max_output_tokens: Some(8192),
        }]
    );
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert_eq!(
        request_header(&requests[0], "x-api-key"),
        Some("fixture-anthropic-key")
    );
    assert_eq!(
        request_header(&requests[0], "anthropic-version"),
        Some("2023-06-01")
    );
    assert_eq!(request_header(&requests[0], "authorization"), None);
}

#[test]
fn network_openrouter_compatible_endpoint_parses_top_provider_capacity() {
    let mock = LocalHttpMock::new(
        200,
        "",
        r#"{"data":[{"id":"provider/model-fixture","top_provider":{"context_length":131072,"max_completion_tokens":32768}}]}"#,
    );
    // OpenRouter 复用现有 OpenAI-compatible Provider，只换 endpoint，不访问远端。
    let endpoint = format!("{}/api/v1", mock.endpoint);
    let result = fetch_ai_model_capabilities("openai", Some(&endpoint), "fixture-router-key");
    let requests = mock.finish();
    assert_eq!(
        result.unwrap(),
        vec![AiDiscoveredModel {
            id: "provider/model-fixture".into(),
            context_window_tokens: Some(131072),
            max_output_tokens: Some(32768),
        }]
    );
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/v1/models HTTP/1.1\r\n"));
    assert_eq!(
        request_header(&requests[0], "authorization"),
        Some("Bearer fixture-router-key")
    );
}

#[test]
fn network_redirect_is_rejected_without_request_or_key_reaching_destination() {
    let destination = LocalHttpMock::new(200, "", r#"{"data":[{"id":"should-not-fetch"}]}"#);
    let headers = format!("Location: {}/capture-key\r\n", destination.endpoint);
    let source = LocalHttpMock::new(302, &headers, r#"{"error":"fixture-redirect-key"}"#);
    let result =
        fetch_ai_model_capabilities("openai", Some(&source.endpoint), "fixture-redirect-key");
    let source_requests = source.finish();
    let destination_requests = destination.finish();
    let error = result.expect_err("302 必须拒绝而不是跟随");
    assert!(error.message.contains("302"));
    assert!(!error.message.contains("fixture-redirect-key"));
    assert_eq!(source_requests.len(), 1);
    assert!(source_requests[0].starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert!(
        destination_requests.is_empty(),
        "重定向目标不得收到请求或认证"
    );
}

#[test]
fn network_error_and_malformed_response_never_echo_fake_key_or_body() {
    for (status, body) in [
        (401, r#"{"error":"RESPONSE_BODY_MARKER fixture-error-key"}"#),
        (403, r#"{"error":"RESPONSE_BODY_MARKER fixture-error-key"}"#),
        (500, r#"{"error":"RESPONSE_BODY_MARKER fixture-error-key"}"#),
        (200, "RESPONSE_BODY_MARKER fixture-error-key: not JSON"),
    ] {
        let mock = LocalHttpMock::new(status, "", body);
        let result =
            fetch_ai_model_capabilities("openai", Some(&mock.endpoint), "fixture-error-key");
        let requests = mock.finish();
        let error = result.expect_err("错误响应/非法 JSON 应失败");
        assert!(!error.message.contains("fixture-error-key"));
        assert!(!error.message.contains("RESPONSE_BODY_MARKER"));
        assert!(!format!("{error:?}").contains("fixture-error-key"));
        if status != 200 {
            assert!(error.message.contains(&status.to_string()));
        }
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v1/models HTTP/1.1\r\n"));
    }
}

#[test]
fn deleting_default_agent_updates_runtime_revision_before_saving_default_models() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let model = default_model(&f, &mut controller);
    let (id, agent_revision) = save(&mut controller, input());
    f.references(&id);
    // fixture 直接设默认引用后先经生产刷新同步，排除测试自身预先制造的 stale revision。
    assert_eq!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    );
    let before = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    assert_eq!(before.default_agent_id.as_deref(), Some(id.as_str()));
    assert_eq!(controller.state().ai_runtime_revision, before.revision);
    assert_eq!(
        controller.dispatch(AppCommand::DeleteAiAgent {
            id: id.clone(),
            expected_revision: agent_revision,
        }),
        AppEvent::AiAgentDeleted { id }
    );
    let deleted = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    assert!(deleted.default_agent_id.is_none());
    assert_eq!(deleted.revision, before.revision + 1);
    let state_revision_after_delete = controller.state().ai_runtime_revision;
    // 模拟 UI 紧接删除操作保存默认模型，必须使用当前 AppState 提供的 CAS token。
    let event = controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(model.clone()),
        default_decision_model_id: None,
        expected_revision: state_revision_after_delete,
    });
    eprintln!(
        "delete→save-default-models: before={}, db_after_delete={}, state_after_delete={}, event={event:?}",
        before.revision, deleted.revision, state_revision_after_delete
    );
    assert!(
        matches!(event, AppEvent::AiDefaultModelsSaved { .. }),
        "删除默认 Agent 后保存模型不应遭遇宿主自身 stale CAS：{event:?}"
    );
    assert_eq!(
        state_revision_after_delete, deleted.revision,
        "删除必须即时回填 runtime revision"
    );
    let saved = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    assert_eq!(
        saved.default_model_profile_id.as_deref(),
        Some(model.as_str())
    );
    assert!(saved.default_agent_id.is_none());
    assert_eq!(saved.revision, deleted.revision + 1);
    assert_eq!(controller.state().ai_runtime_revision, saved.revision);
}

#[test]
fn new_conversation_with_disabled_custom_default_falls_back_to_builtin() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let model = default_model(&f, &mut controller);
    let (custom, _) = save(
        &mut controller,
        AiAgentSaveInput {
            enabled: false,
            ..input()
        },
    );
    f.references(&custom);
    assert_eq!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    );
    assert!(!f.agent(&custom).enabled);
    let runtime = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    assert_eq!(runtime.default_agent_id.as_deref(), Some(custom.as_str()));
    let job = controller
        .prepare_ai_conversation("DEFAULTS_APP 停用默认 Agent 新会话", &target())
        .unwrap();
    assert_eq!(job.model_profile_id, model);
    let session = f
        .storage
        .with_ai_connection(|conn| sessions::load_session(conn, &job.session_id))
        .unwrap();
    assert_eq!(session.agent_id, BUILTIN);
    let turn = f
        .storage
        .with_ai_connection(|conn| sessions::load_turn(conn, &job.turn_id))
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
    assert_eq!(snapshot["agent_id"], BUILTIN);
    assert_eq!(
        f.storage
            .with_ai_connection(settings::load_runtime_settings)
            .unwrap(),
        runtime,
        "fallback 不能偷偷重写默认引用"
    );
}

#[test]
fn existing_conversation_with_disabled_agent_keeps_identity_and_rejects_send() {
    let f = Fixture::new();
    let mut controller = f.controller();
    default_model(&f, &mut controller);
    let (custom, revision) = save(&mut controller, input());
    f.references(&custom);
    f.history(&custom, TurnStatus::Completed);
    let view = load_history(&mut controller);
    save(
        &mut controller,
        AiAgentSaveInput {
            id: Some(custom.clone()),
            enabled: false,
            expected_revision: revision,
            ..input()
        },
    );
    let history = f.snapshot();
    let raw = f.raw_agents();
    let error = controller
        .prepare_ai_conversation("不应偷偷切换为内置 Agent", &target())
        .expect_err("旧会话的 Agent 停用应明确拒绝");
    assert!(error.message.contains("停用"));
    assert_eq!(controller.state().ai_conversation, view);
    assert_eq!(f.snapshot().session.agent_id, custom);
    assert_history_eq(&history, &f.snapshot());
    assert_eq!(f.raw_agents(), raw);
    let mut reopened = f.controller();
    assert_reloaded_history_view(&view, load_history(&mut reopened));
    assert!(
        reopened
            .prepare_ai_conversation("重开也不应更换身份", &target())
            .unwrap_err()
            .message
            .contains("停用")
    );
    assert_history_eq(&history, &f.snapshot());
}

#[test]
fn async_delete_completion_updates_runtime_revision_and_late_result_cannot_regress_it() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let model = default_model(&f, &mut controller);
    let (id, revision) = save(&mut controller, input());
    f.references(&id);
    assert_eq!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    );
    let before = controller.state().ai_runtime_revision;
    let deleted_revision = f
        .storage
        .with_ai_connection(|conn| agents::delete_agent_with_runtime_revision(conn, &id, revision))
        .unwrap();
    assert_eq!(deleted_revision, before + 1);
    let complete = || AppCommand::CompleteAiAgentDelete {
        id: id.clone(),
        runtime_revision: deleted_revision,
        result: Ok(()),
    };
    assert_eq!(
        controller.dispatch(complete()),
        AppEvent::AiAgentDeleted { id: id.clone() }
    );
    assert_eq!(controller.state().ai_runtime_revision, deleted_revision);
    assert!(controller.state().ai_agents.iter().all(|a| a.id != id));
    let save_defaults = |expected_revision| AppCommand::SaveAiDefaultModels {
        default_model_id: Some(model.clone()),
        default_decision_model_id: None,
        expected_revision,
    };
    assert!(matches!(
        controller.dispatch(save_defaults(deleted_revision)),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    let newer_revision = controller.state().ai_runtime_revision;
    assert_eq!(newer_revision, deleted_revision + 1);
    assert_eq!(
        controller.dispatch(complete()),
        AppEvent::AiAgentDeleted { id }
    );
    assert_eq!(
        controller.state().ai_runtime_revision,
        newer_revision,
        "迟到删除回包不可倒退 CAS token"
    );
    assert!(matches!(
        controller.dispatch(save_defaults(newer_revision)),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    let committed = f
        .storage
        .with_ai_connection(settings::load_runtime_settings)
        .unwrap();
    assert_eq!(controller.state().ai_runtime_revision, committed.revision);
    assert!(committed.default_agent_id.is_none());
}
