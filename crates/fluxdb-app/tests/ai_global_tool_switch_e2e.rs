//! 全局工具开关的端到端验证（设置页「工具与 MCP → 工具」）。
//!
//! 覆盖两条真实链路：
//! 1. 落库 + 级联收窄 Agent 白名单（走 `AppCommand` 与生产 dispatch）；
//! 2. 运行时装配按停用集合过滤工具目录，模型既发现不到、也无法执行。
//!
//! 不触网、不读用户真实配置，全部使用隔离临时目录与受控 Provider。

use std::{path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationJob, AiConversationUpdate, AppCommand, AppController, AppEvent,
    AuditInput, AuditOutcome, MapSqlResolver, ScriptedGenerationProvider, SecurityAuditHook,
    TargetRef, ToolCatalog, TurnPermissionContext, TurnSecurityContext, TurnTargetPin,
    ai_turn_security_context, standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{FileStorage, ai_platform::tool_settings};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-global-tool-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("创建隔离目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        // 走生产命令链配置一个离线模型档案：只读不触网，prepare 需要可解析的模型。
        let model_id =
            match controller.dispatch(AppCommand::SaveAiModel(fluxdb_app::AiModelSaveInput {
                display_name: "全局工具开关 E2E 模型".into(),
                provider: "openai".into(),
                model: "offline".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                enabled: true,
                context_window_tokens: Some(8_192),
                max_output_tokens: Some(1_024),
                ..Default::default()
            })) {
                AppEvent::AiModelSaved { id, .. } => id,
                other => panic!("配置模型失败: {other:?}"),
            };
        // prepare 需要一个可解析的默认主模型；走生产命令设置运行设置单例。
        let revision = controller.state().ai_runtime_revision;
        assert!(
            matches!(
                controller.dispatch(AppCommand::SaveAiDefaultModels {
                    default_model_id: Some(model_id),
                    default_decision_model_id: None,
                    expected_revision: revision,
                }),
                AppEvent::AiDefaultModelsSaved { .. }
            ),
            "设置默认主模型失败"
        );
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct AllowAudit;

impl SecurityAuditHook for AllowAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::Allow)
    }
}

fn demo_config() -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(1),
        name: "global-tool demo".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: true,
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        schema: None,
        object: Some("product".into()),
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: Some(1),
        captured_tab_revision: 1,
    }
}

fn permission(security: &TurnSecurityContext) -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: security.agent_allowed_tools.clone(),
        disabled_tools: security.disabled_tools.clone(),
        policy: fluxdb_app::kernel_policy_for_connection(Some(&demo_config())),
        plugin_digest: None,
    }
}

fn run_turn(
    fixture: &Fixture,
    job: &AiConversationJob,
    provider: &ScriptedGenerationProvider,
    catalog: &ToolCatalog,
    security: &TurnSecurityContext,
) -> AiConversationUpdate {
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(32);
    fluxdb_app::run_ai_conversation_turn(
        &fixture.storage,
        job,
        &cancel,
        &tx,
        provider,
        catalog,
        &AllowAudit,
        permission(security),
    );
    let mut terminal = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            terminal = Some(update);
        }
    }
    terminal.expect("任务必须发送终态回包")
}

fn query_call(call_id: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: "db.query".into(),
            arguments_json: r#"{"target":"conn:1/db:main","sql":"SELECT 1"}"#.into(),
        }),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]
}

fn text_call(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn tool_status(fixture: &Fixture, session_id: &str) -> Vec<(String, String)> {
    fixture
        .storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<Vec<(String, String)>> {
            let to_err = |e: rusqlite::Error| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
            };
            let mut stmt = conn
                .prepare("SELECT id,status FROM ai_tool_calls WHERE session_id=?1 ORDER BY id")
                .map_err(to_err)?;
            let rows = stmt
                .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(to_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(to_err)?;
            Ok(rows)
        })
        .expect("查询工具事实行")
}

#[test]
fn ai_global_tool_switch_disables_discovery_and_execution_end_to_end() {
    let fixture = Fixture::new("e2e");
    let mut controller = fixture.controller();

    // 会话绑定的是默认 Agent；把 db.query 显式写进它的白名单，
    // 全局关闭后必须被级联移除。
    let builtin = fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID.to_string();
    let builtin_revision = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::agents::load_agent(conn, &builtin)?.revision)
        })
        .expect("读取内置 Agent");
    let agent_id = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        max_iterations: 24,
        id: Some(builtin),
        display_name: "通用助手".into(),
        description: "默认智能助手，可修改配置，不可删除".into(),
        system_prompt: "只依据真实回执回答。".into(),
        model_profile_id: None,
        allowed_tools: vec!["db.query".into()],
        enabled: true,
        expected_revision: builtin_revision,
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("保存 Agent 失败: {other:?}"),
    };

    // 先在工具开启状态下跑一轮，证明这条链路本身能成功执行 db.query。
    let job = controller
        .prepare_ai_conversation("查询产品", &target())
        .expect("准备工具回合");
    let security =
        ai_turn_security_context(&fixture.storage, &job.turn_id).expect("读取安全上下文");
    assert!(security.disabled_tools.is_empty(), "初始没有全局停用工具");
    assert!(
        security
            .agent_allowed_tools
            .contains(&"db.query".to_string()),
        "Agent 白名单应含 db.query"
    );

    let mut catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("装配只读工具目录");
    catalog.without_tools(&security.disabled_tools);
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            query_call("e2e-call-1"),
            text_call("已依据真实回执完成查询。"),
        ],
    );
    let update = run_turn(&fixture, &job, &provider, &catalog, &security);
    assert!(update.terminal, "必须返回终态");
    assert!(
        tool_status(&fixture, &job.session_id)
            .iter()
            .any(|(_, status)| status == "succeeded"),
        "工具开启时 db.query 应真实执行成功"
    );

    // 全局关闭 db.query。
    let event = controller.dispatch(AppCommand::SetAiToolEnabled {
        tool_id: "db.query".into(),
        enabled: false,
        expected_revision: 0,
    });
    assert!(
        matches!(event, AppEvent::AiToolEnabled { .. }),
        "关闭工具应返回 AiToolEnabled，实际 {event:?}"
    );
    let stored = fixture
        .storage
        .with_ai_connection(|conn| tool_settings::disabled_tool_ids(conn))
        .expect("读取停用集合");
    assert_eq!(stored, ["db.query".to_string()].into());

    // 级联：Agent 白名单里的 db.query 已被移除。
    let agent = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::agents::load_agent(conn, &agent_id)?.allowed_tools)
        })
        .expect("读取 Agent 白名单");
    assert!(
        !agent.iter().any(|id| id == "db.query"),
        "全局关闭后 Agent 白名单必须同步收窄，实际 {agent:?}"
    );

    // 新一轮：工具目录不再含 db.query，模型无法发现也无法执行。
    let job2 = controller
        .prepare_ai_conversation("再查一次", &target())
        .expect("准备第二轮");
    let security2 =
        ai_turn_security_context(&fixture.storage, &job2.turn_id).expect("读取安全上下文");
    assert!(
        security2.disabled_tools.contains("db.query"),
        "第二轮必须看到全局停用集合"
    );

    let mut catalog2 = standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("装配只读工具目录");
    catalog2.without_tools(&security2.disabled_tools);
    assert!(
        catalog2.describe("db.query").is_none(),
        "全局关闭后工具目录不应再下发 db.query"
    );
    assert!(
        catalog2.discover("db.query", 10).is_empty(),
        "全局关闭的工具不应被工具发现返回"
    );

    // 模型仍按旧记忆发起 db.query：必须是受控拒绝，不得执行。
    let provider2 = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![query_call("e2e-call-2"), text_call("当前工具不可用。")],
    );
    let update2 = run_turn(&fixture, &job2, &provider2, &catalog2, &security2);
    assert!(update2.terminal, "必须返回终态");
    // 未知工具在 describe 阶段即被拒：受控拒绝走模型消息，不写 ai_tool_calls 事实行。
    // 断言第二轮没有新增"成功"事实行——这正是"不可发现且不可执行"的可见结果。
    let calls = tool_status(&fixture, &job2.session_id);
    assert!(
        !calls
            .iter()
            .any(|(id, status)| id.ends_with("e2e-call-2") && status == "succeeded"),
        "全局关闭的工具不得真实执行，实际 {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(_, status)| status == "succeeded")
            .count(),
        1,
        "只有工具开启那一轮产生过成功执行，实际 {calls:?}"
    );
}
