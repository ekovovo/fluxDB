//! Agent 默认值/删除契约：独立文件 SQLite、公开存储入口、关闭重开及真实 FK。
//! 不使用用户配置，不调用模型；原始 SQL 只用于遗留格式/尚无编辑 API 的绑定 fixture 与落库证据。
use std::{fs, path::PathBuf};

use fluxdb_storage::ai_platform::{
    agents::{self, AgentEdit, BUILTIN_AGENT_ID},
    conversation, ensure_schema, load_ai_cipher, mcp, models, plugins, sessions, settings,
};
use rusqlite::Connection;
use sessions::TurnStatus;

const CUSTOM: &str = "defaults-custom";
const SESSION: &str = "defaults-session";
const TURN: &str = "defaults-turn";

struct Fixture {
    dir: PathBuf,
    conn: Option<Connection>,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-agent-defaults-storage-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&dir).expect("独占临时目录");
        let mut fixture = Self { dir, conn: None };
        fixture.reopen();
        fixture
    }

    fn db(&self) -> &Connection {
        self.conn.as_ref().expect("已打开 SQLite")
    }

    fn reopen(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.close().expect("关闭真实连接");
        }
        let conn = Connection::open(self.dir.join("agents.sqlite")).expect("文件 SQLite");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let enabled: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(enabled, "每次重开都启用 FK");
        ensure_schema(&conn).expect("生产 schema 和种子");
        self.conn = Some(conn);
    }

    fn model(&self) -> String {
        let (cipher, _) = load_ai_cipher(&self.dir).expect("仅读取独占 fixture 配置");
        models::save_model(
            self.db(),
            &cipher,
            "fixture-key",
            1_000,
            &models::ModelEdit {
                id: "defaults-model".into(),
                display_name: "离线模型配置，不发请求".into(),
                provider: "openai-compatible".into(),
                api_protocol: "openai-compatible".into(),
                kind: models::ModelKind::Generation,
                model: "offline-fixture".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                endpoint_is_full_url: false,
                enabled: true,
                egress: models::EgressPermission::Allow,
                auth_mode: models::AuthMode::None,
                context_window_tokens: Some(8_192),
                max_output_tokens: Some(1_024),
                input_price_micros_per_million: None,
                output_price_micros_per_million: None,
                cached_input_price_micros_per_million: None,
            },
            None,
            models::ApiKeyChange::Keep,
        )
        .expect("保存模型配置")
        .id
    }

    fn history(&self, agent: &str, status: TurnStatus) {
        conversation::send(
            self.db(),
            2_000,
            true,
            SESSION,
            agent,
            TURN,
            "defaults-message",
            "DEFAULTS 唯一历史正文",
            r#"{"kind":"sql","marker":"DEFAULTS"}"#,
            r#"{"marker":"DEFAULTS","model":"offline-fixture"}"#,
        )
        .expect("生产发送事务，不执行模型");
        sessions::append_run_event(self.db(), 2_001, TURN, "fixture", r#"{"public":"保留"}"#)
            .unwrap();
        let turn = sessions::load_turn(self.db(), TURN).unwrap();
        sessions::update_turn_status(self.db(), 2_002, TURN, status, None, turn.revision).unwrap();
    }

    fn references(&self, agent: &str) {
        let mut runtime = settings::load_runtime_settings(self.db()).unwrap();
        let revision = runtime.revision;
        runtime.default_agent_id = Some(agent.into());
        let model = self.model();
        runtime.default_model_profile_id = Some(model.clone());
        runtime.max_active_turns = 3;
        settings::save_runtime_settings(self.db(), 1_500, &runtime, revision).unwrap();
        let instance = plugins::list_plugin_instances(self.db()).unwrap().remove(0);
        plugins::bind_plugin_system(self.db(), 1_501, "defaults-binding", agent, &instance.id)
            .unwrap();
        mcp::save_mcp_setting(
            self.db(),
            1_502,
            &mcp::McpSettingView {
                id: "defaults-mcp".into(),
                display_name: "离线 MCP 配置".into(),
                transport: "stdio".into(),
                enabled: false,
                config_json: "{}".into(),
                revision: 1,
            },
            None,
        )
        .unwrap();
        // 两类绑定尚无公开编辑入口，仅按生产 DDL 预置，不运行 MCP 或模型。
        self.db().execute(
            "INSERT INTO ai_agent_model_bindings(id,agent_id,model_profile_id,role,created_at_ms,updated_at_ms)
             VALUES ('defaults-model-binding',?1,?2,'main_candidate',1503,1503)",
            rusqlite::params![agent, model],
        ).unwrap();
        self.db().execute(
            "INSERT INTO ai_agent_mcp_bindings(id,agent_id,mcp_setting_id,created_at_ms,updated_at_ms)
             VALUES ('defaults-mcp-binding',?1,'defaults-mcp',1504,1504)", [agent],
        ).unwrap();
        assert_eq!(binding_counts(self.db(), agent), [1, 1, 1]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Windows 也必须先释放文件句柄；只清理本 fixture 独占创建的目录。
        drop(self.conn.take());
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn edit(id: &str) -> AgentEdit {
    AgentEdit {
        max_iterations: 24,
        id: id.into(),
        display_name: "DEFAULTS 自定义 Agent".into(),
        description: "独立持久化 fixture".into(),
        enabled: true,
        default_model_profile_id: None,
        system_prompt: "DEFAULTS 保存前提示词".into(),
        budget_policy_id: None,
        allowed_tools: vec!["db.query".into()],
        compression_trigger_percent: 80,
    }
}

fn raw_agent(conn: &Connection, id: &str) -> String {
    conn.query_row(
        "SELECT json_object('id',id,'name',display_name,'description',description,
            'enabled',enabled,'model',default_model_profile_id,'prompt',system_prompt,
            'budget',budget_policy_id,'tools',allowed_tools_json,
            'permissions',permissions_json,'options',options_json,'revision',revision,
            'created',created_at_ms,'updated',updated_at_ms) FROM ai_agents WHERE id=?1",
        [id],
        |row| row.get(0),
    )
    .expect("真实行证据（包括删除标记和时间戳）")
}

fn binding_counts(conn: &Connection, agent: &str) -> [i64; 3] {
    [
        "ai_agent_plugin_bindings",
        "ai_agent_model_bindings",
        "ai_agent_mcp_bindings",
    ]
    .map(|table| {
        conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE agent_id=?1"),
            [agent],
            |row| row.get(0),
        )
        .unwrap()
    })
}

fn assert_history_eq(before: &sessions::SessionSnapshot, after: &sessions::SessionSnapshot) {
    assert_eq!(before.session, after.session);
    assert_eq!(before.turns, after.turns);
    assert_eq!(before.messages, after.messages);
    assert_eq!(before.parts, after.parts);
    assert_eq!(before.run_events, after.run_events);
    assert_eq!(before.ui_state, after.ui_state);
    assert!(!after.messages.is_empty() && !after.parts.is_empty() && !after.run_events.is_empty());
}

#[test]
fn ensure_schema_seeds_one_enabled_builtin_with_default_percent() {
    let f = Fixture::new();
    let builtin = agents::load_agent(f.db(), BUILTIN_AGENT_ID).expect("自动注册内置 Agent");
    assert!(builtin.enabled);
    assert_eq!(builtin.compression_trigger_percent, 80);
    assert!(!builtin.system_prompt.trim().is_empty());
    assert_eq!(agents::list_agents(f.db()).unwrap(), vec![builtin]);
    let before = raw_agent(f.db(), BUILTIN_AGENT_ID);
    for now in [3_000, 4_000, 5_000] {
        agents::seed_builtin_agent(f.db(), now).expect("重复 seed");
        ensure_schema(f.db()).expect("重复 ensure_schema");
    }
    assert_eq!(
        raw_agent(f.db(), BUILTIN_AGENT_ID),
        before,
        "seed 不写 revision/时间戳"
    );
    assert_eq!(agents::list_agents(f.db()).unwrap().len(), 1);
}

#[test]
fn edited_builtin_prompt_model_and_threshold_survive_seed_and_file_reopen() {
    let mut f = Fixture::new();
    let builtin = agents::load_agent(f.db(), BUILTIN_AGENT_ID).unwrap();
    let mut input = edit(BUILTIN_AGENT_ID);
    input.default_model_profile_id = Some(f.model());
    input.system_prompt = "DEFAULTS 用户编辑的内置提示词".into();
    input.compression_trigger_percent = 63;
    let saved = agents::save_agent(f.db(), 2_000, &input, builtin.revision).unwrap();
    assert_eq!(saved.revision, builtin.revision + 1);
    let before = raw_agent(f.db(), BUILTIN_AGENT_ID);
    agents::seed_builtin_agent(f.db(), 3_000).unwrap();
    f.reopen();
    assert_eq!(agents::load_agent(f.db(), BUILTIN_AGENT_ID).unwrap(), saved);
    assert_eq!(raw_agent(f.db(), BUILTIN_AGENT_ID), before);
}

#[test]
fn builtin_cannot_be_deleted_or_disabled_and_rejections_do_not_write() {
    let f = Fixture::new();
    let builtin = agents::load_agent(f.db(), BUILTIN_AGENT_ID).unwrap();
    let before = raw_agent(f.db(), BUILTIN_AGENT_ID);
    assert!(agents::delete_agent(f.db(), BUILTIN_AGENT_ID, builtin.revision).is_err());
    assert_eq!(raw_agent(f.db(), BUILTIN_AGENT_ID), before);
    let mut disabled = edit(BUILTIN_AGENT_ID);
    disabled.enabled = false;
    assert!(agents::save_agent(f.db(), 3_000, &disabled, builtin.revision).is_err());
    assert_eq!(raw_agent(f.db(), BUILTIN_AGENT_ID), before);
    assert_eq!(
        agents::load_agent(f.db(), BUILTIN_AGENT_ID).unwrap(),
        builtin
    );
}

#[test]
fn legacy_options_without_threshold_load_as_eighty_and_roundtrip_boundaries() {
    let mut f = Fixture::new();
    agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
    // 模拟此前 schema 的 options_json，不复制生产读取/投影逻辑。
    f.db()
        .execute(
            "UPDATE ai_agents SET options_json='{}' WHERE id=?1",
            [CUSTOM],
        )
        .unwrap();
    f.reopen();
    let mut current = agents::load_agent(f.db(), CUSTOM).unwrap();
    assert_eq!(current.compression_trigger_percent, 80);
    for percent in [10, 95] {
        let mut input = edit(CUSTOM);
        input.compression_trigger_percent = percent;
        current = agents::save_agent(f.db(), 2_000, &input, current.revision).unwrap();
        f.reopen();
        assert_eq!(agents::load_agent(f.db(), CUSTOM).unwrap(), current);
        assert_eq!(current.compression_trigger_percent, percent);
    }
}

#[test]
fn invalid_percent_rejects_create_and_save_without_mutation() {
    let f = Fixture::new();
    let current = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
    let before = raw_agent(f.db(), CUSTOM);
    let initial = agents::list_agents(f.db()).unwrap();
    for percent in [0, 9, 96, 100, u8::MAX] {
        let mut input = edit(&format!("invalid-{percent}"));
        input.compression_trigger_percent = percent;
        assert!(
            agents::create_agent(f.db(), 2_000, &input).is_err(),
            "新建 {percent}% 应拒绝"
        );
        input.id = CUSTOM.into();
        assert!(
            agents::save_agent(f.db(), 2_000, &input, current.revision).is_err(),
            "保存 {percent}% 应拒绝"
        );
        assert_eq!(raw_agent(f.db(), CUSTOM), before);
        assert_eq!(agents::list_agents(f.db()).unwrap(), initial);
    }
}

#[test]
fn stale_save_and_delete_preserve_agent_default_bindings_and_history() {
    let f = Fixture::new();
    let original = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
    f.references(CUSTOM);
    f.history(CUSTOM, TurnStatus::Completed);
    let mut input = edit(CUSTOM);
    input.system_prompt = "DEFAULTS 已获胜的并发编辑".into();
    input.compression_trigger_percent = 70;
    let winner = agents::save_agent(f.db(), 3_000, &input, original.revision).unwrap();
    let before = raw_agent(f.db(), CUSTOM);
    let runtime = settings::load_runtime_settings(f.db()).unwrap();
    let bindings = plugins::list_agent_plugin_bindings(f.db(), CUSTOM).unwrap();
    let history = sessions::session_snapshot(f.db(), SESSION).unwrap();
    input.system_prompt = "不应覆盖已获胜的编辑".into();
    assert!(agents::save_agent(f.db(), 4_000, &input, original.revision).is_err());
    assert!(agents::delete_agent(f.db(), CUSTOM, original.revision).is_err());
    assert_eq!(agents::load_agent(f.db(), CUSTOM).unwrap(), winner);
    assert_eq!(raw_agent(f.db(), CUSTOM), before);
    assert_eq!(settings::load_runtime_settings(f.db()).unwrap(), runtime);
    assert_eq!(
        plugins::list_agent_plugin_bindings(f.db(), CUSTOM).unwrap(),
        bindings
    );
    assert_eq!(binding_counts(f.db(), CUSTOM), [1, 1, 1]);
    assert_history_eq(
        &history,
        &sessions::session_snapshot(f.db(), SESSION).unwrap(),
    );
}

#[test]
fn all_active_turn_states_including_waiting_approval_reject_delete_atomically() {
    for status in [
        TurnStatus::Queued,
        TurnStatus::Preparing,
        TurnStatus::Generating,
        TurnStatus::ExecutingTools,
        TurnStatus::WaitingApproval,
        TurnStatus::WaitingUser,
        TurnStatus::CancelRequested,
        TurnStatus::OutcomeUnknown,
    ] {
        let f = Fixture::new();
        let agent = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
        f.references(CUSTOM);
        f.history(CUSTOM, status);
        let before = raw_agent(f.db(), CUSTOM);
        let runtime = settings::load_runtime_settings(f.db()).unwrap();
        let bindings = plugins::list_agent_plugin_bindings(f.db(), CUSTOM).unwrap();
        let history = sessions::session_snapshot(f.db(), SESSION).unwrap();
        assert!(
            agents::delete_agent(f.db(), CUSTOM, agent.revision).is_err(),
            "{status:?} 的 Agent 不可删除"
        );
        assert_eq!(raw_agent(f.db(), CUSTOM), before);
        assert_eq!(settings::load_runtime_settings(f.db()).unwrap(), runtime);
        assert_eq!(
            plugins::list_agent_plugin_bindings(f.db(), CUSTOM).unwrap(),
            bindings
        );
        assert_eq!(binding_counts(f.db(), CUSTOM), [1, 1, 1]);
        assert_history_eq(
            &history,
            &sessions::session_snapshot(f.db(), SESSION).unwrap(),
        );
    }
}

#[test]
fn custom_soft_delete_cleans_references_but_preserves_history_fk_and_reopen() {
    let mut f = Fixture::new();
    let agent = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
    f.references(CUSTOM);
    f.history(CUSTOM, TurnStatus::Completed);
    let before = sessions::session_snapshot(f.db(), SESSION).unwrap();
    let runtime = settings::load_runtime_settings(f.db()).unwrap();
    agents::delete_agent(f.db(), CUSTOM, agent.revision).expect("完成会话不阻止逻辑删除");
    let raw: serde_json::Value = serde_json::from_str(&raw_agent(f.db(), CUSTOM)).unwrap();
    let options: serde_json::Value =
        serde_json::from_str(raw["options"].as_str().unwrap()).unwrap();
    assert_eq!(options["deleted"], true, "仍保留真实 Agent 行和历史 FK");
    assert_eq!(raw["revision"].as_i64(), Some(agent.revision + 1));
    assert!(
        agents::list_agents(f.db())
            .unwrap()
            .iter()
            .all(|a| a.id != CUSTOM)
    );
    // 逻辑删除保留历史身份，单行读取仍可用；配置列表必须隐藏且不能再次启用。
    let retained = agents::load_agent(f.db(), CUSTOM).expect("历史身份仍可读取");
    assert!(!retained.enabled, "已删除对象不可再用于运行");
    assert_eq!(retained.system_prompt, agent.system_prompt);
    assert!(
        plugins::list_agent_plugin_bindings(f.db(), CUSTOM)
            .unwrap()
            .is_empty()
    );
    assert_eq!(binding_counts(f.db(), CUSTOM), [0, 0, 0]);
    let cleared = settings::load_runtime_settings(f.db()).unwrap();
    assert_ne!(cleared.default_agent_id.as_deref(), Some(CUSTOM));
    assert_eq!(
        cleared.default_model_profile_id,
        runtime.default_model_profile_id
    );
    assert_eq!(
        cleared.default_decision_model_profile_id,
        runtime.default_decision_model_profile_id
    );
    assert_eq!(cleared.max_active_turns, runtime.max_active_turns);
    assert_history_eq(
        &before,
        &sessions::session_snapshot(f.db(), SESSION).unwrap(),
    );
    let deleted_row = raw_agent(f.db(), CUSTOM);
    // 删除后不能通过 stale save、同 ID create 或重复 delete 复活/改写。
    assert!(agents::save_agent(f.db(), 5_000, &edit(CUSTOM), agent.revision + 1).is_err());
    assert!(agents::create_agent(f.db(), 5_000, &edit(CUSTOM)).is_err());
    assert!(agents::delete_agent(f.db(), CUSTOM, agent.revision).is_err());
    f.reopen();
    assert_eq!(raw_agent(f.db(), CUSTOM), deleted_row);
    assert_history_eq(
        &before,
        &sessions::session_snapshot(f.db(), SESSION).unwrap(),
    );
    let fk_errors: i64 = f
        .db()
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fk_errors, 0, "逻辑删除后的真实 FK 完整");
}

#[test]
fn delete_with_current_revision_after_terminal_turn_is_allowed() {
    for status in [
        TurnStatus::Cancelled,
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Interrupted,
    ] {
        let f = Fixture::new();
        let agent = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
        f.history(CUSTOM, status);
        let before = sessions::session_snapshot(f.db(), SESSION).unwrap();
        agents::delete_agent(f.db(), CUSTOM, agent.revision).expect("历史终态允许删除");
        assert_history_eq(
            &before,
            &sessions::session_snapshot(f.db(), SESSION).unwrap(),
        );
    }
}

#[test]
fn deleting_missing_id_does_not_change_existing_agents() {
    let f = Fixture::new();
    let before = agents::list_agents(f.db()).unwrap();
    assert!(agents::delete_agent(f.db(), "missing-agent", 1).is_err());
    assert_eq!(agents::list_agents(f.db()).unwrap(), before);
}

#[test]
fn delete_returns_committed_runtime_revision_with_and_without_default_reference() {
    for default in [None, Some(BUILTIN_AGENT_ID), Some(CUSTOM)] {
        let f = Fixture::new();
        let agent = agents::create_agent(f.db(), 1_000, &edit(CUSTOM)).unwrap();
        let mut runtime = settings::load_runtime_settings(f.db()).unwrap();
        let expected = runtime.revision;
        runtime.default_agent_id = default.map(str::to_owned);
        let before = settings::save_runtime_settings(f.db(), 1_500, &runtime, expected).unwrap();
        let returned =
            agents::delete_agent_with_runtime_revision(f.db(), CUSTOM, agent.revision).unwrap();
        let committed = settings::load_runtime_settings(f.db()).unwrap();
        assert_eq!(
            returned, committed.revision,
            "返回值来自删除事务提交的 runtime 状态"
        );
        assert_eq!(
            returned,
            before.revision + i64::from(default == Some(CUSTOM))
        );
        assert_eq!(
            committed.default_agent_id.as_deref(),
            default.filter(|id| *id != CUSTOM)
        );
        assert!(
            agents::list_agents(f.db())
                .unwrap()
                .iter()
                .all(|a| a.id != CUSTOM)
        );
    }
}
