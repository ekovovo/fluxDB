//! 「计划与执行进度」插件的门控与落盘验证（设计 §23.2，`fluxdb.progress.plan`）。
//!
//! 覆盖三条真实链路（不触网、不读用户配置，使用隔离临时目录）：
//! 1. 插件实例开启时，`plan.update` 进入工具目录并写出 `Plan` 片段；
//! 2. 插件实例关闭后，工具不进入目录（模型发现不到），直接执行也无可执行实现；
//! 3. 关闭不删除已发布的历史计划片段。

use std::{path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiConversationJob, AiConversationUpdate, AiModelSaveInput, AppCommand, AppController, AppEvent,
    KernelAuditHook, KernelPolicy, MapSqlResolver, ScriptedGenerationProvider, TargetRef,
    ToolCatalog, TurnPermissionContext, TurnTargetPin, standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason,
    ModelCapabilities, ModelEvent, ToolCallRequest,
};
use fluxdb_storage::FileStorage;
use serde_json::json;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-plan-plugin-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("创建隔离目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    fn set_progress_enabled(&self, enabled: bool) {
        let instance = self
            .storage
            .with_ai_connection(|conn| {
                Ok(
                    fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
                        .into_iter()
                        .find(|instance| instance.id == "builtin.progress")
                        .expect("内置进度实例必须存在"),
                )
            })
            .expect("读取进度实例");
        self.storage
            .with_ai_connection(|conn| {
                fluxdb_storage::ai_platform::plugins::set_plugin_instance_enabled(
                    conn,
                    1,
                    &instance.id,
                    enabled,
                    instance.revision,
                )
            })
            .expect("切换进度插件");
    }

    /// 走生产命令配置一个离线模型并设为默认主模型；`prepare` 需要可解析的主模型。
    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "计划插件 E2E 模型".into(),
            provider: "openai".into(),
            model: "offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        let revision = controller.state().ai_runtime_revision;
        assert!(
            matches!(
                controller.dispatch(AppCommand::SetAiDefaultModel {
                    model_id: Some(model_id),
                    expected_revision: revision,
                }),
                AppEvent::AiDefaultModelsSaved { .. }
            ),
            "设置默认主模型失败"
        );
        controller
    }

    fn plan_payloads(&self, session_id: &str) -> Vec<serde_json::Value> {
        self.storage
            .with_ai_connection(|conn| -> fluxdb_core::Result<Vec<serde_json::Value>> {
                let to_err = |e: rusqlite::Error| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
                };
                let mut stmt = conn
                    .prepare(
                        "SELECT p.payload_json FROM ai_message_parts p
                          JOIN ai_messages m ON m.id = p.message_id
                          WHERE m.session_id=?1 AND p.kind='plan' ORDER BY p.id",
                    )
                    .map_err(to_err)?;
                let rows = stmt
                    .query_map([session_id], |row| row.get::<_, String>(0))
                    .map_err(to_err)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(to_err)?;
                Ok(rows
                    .into_iter()
                    .filter_map(|raw| serde_json::from_str(&raw).ok())
                    .collect())
            })
            .expect("查询计划片段")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn demo_config() -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId(1),
        name: "plan demo".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: true,
        },
        options: std::collections::BTreeMap::new(),
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

fn job(controller: &mut AppController) -> AiConversationJob {
    controller
        .prepare_ai_conversation("做个报表", &target())
        .expect("准备工具回合")
}

fn catalog(fixture: &Fixture, job: &AiConversationJob) -> ToolCatalog {
    let security = fluxdb_app::ai_turn_security_context(&fixture.storage, &job.turn_id)
        .expect("读取安全上下文");
    let mut catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("装配只读目录");
    fluxdb_app::add_ai_plan_tool(&mut catalog, &fixture.storage, &job.session_id)
        .expect("按插件门控装配 plan.update");
    catalog.without_tools(&security.disabled_tools);
    catalog
}

fn run_turn(
    fixture: &Fixture,
    job: &AiConversationJob,
    provider: &ScriptedGenerationProvider,
    catalog: &ToolCatalog,
) -> AiConversationUpdate {
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    fluxdb_app::run_ai_conversation_turn(
        &fixture.storage,
        job,
        &cancel,
        &tx,
        provider,
        catalog,
        &KernelAuditHook,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );
    let mut terminal = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            terminal = Some(update);
        }
    }
    terminal.expect("任务必须发送终态回包")
}

fn plan_call(call_id: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: "plan.update".into(),
            arguments_json: json!({
                "title": "生成订单报表",
                "steps": [
                    {"title": "读取订单表", "status": "done"},
                    {"title": "汇总指标", "status": "in_progress"}
                ]
            })
            .to_string(),
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

fn provider() -> ScriptedGenerationProvider {
    ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![plan_call("plan-call-1"), text_call("计划已发布。")],
    )
}

#[test]
fn enabled_plugin_publishes_plan_part_and_disabled_hides_tool_but_keeps_history() {
    let fixture = Fixture::new("gating");
    let mut controller = fixture.controller();

    // 1) 默认开启：plan.update 进入目录，执行后落成 Plan 片段。
    fixture.set_progress_enabled(true);
    let job1 = job(&mut controller);
    let catalog1 = catalog(&fixture, &job1);
    assert!(
        catalog1.specs().iter().any(|spec| spec.id == "plan.update"),
        "开启时 plan.update 必须在目录中"
    );
    let update = run_turn(&fixture, &job1, &provider(), &catalog1);
    assert!(update.terminal);
    let plans = fixture.plan_payloads(&job1.session_id);
    assert_eq!(plans.len(), 1, "应落成一条计划片段，实际 {plans:?}");
    assert_eq!(plans[0]["title"], "生成订单报表");
    assert_eq!(plans[0]["steps"][1]["status"], "in_progress");

    // 2) 关闭后：工具不再进入目录（模型发现不到）。
    fixture.set_progress_enabled(false);
    let job2 = job(&mut controller);
    let catalog2 = catalog(&fixture, &job2);
    assert!(
        !catalog2.specs().iter().any(|spec| spec.id == "plan.update"),
        "关闭后 plan.update 不得出现在目录中"
    );
    // 即使模型仍硬发调用，也没有可执行实现，会被内核拒绝而不是执行。
    assert!(
        catalog2.executable("plan.update").is_none(),
        "关闭后 plan.update 无可执行实现"
    );

    // 3) 关闭不删除历史计划片段。
    assert_eq!(
        fixture.plan_payloads(&job1.session_id).len(),
        1,
        "关闭插件不得删除已发布的历史计划"
    );
}

#[test]
fn repeated_plan_updates_replace_one_card_and_keep_execution_history() {
    let fixture = Fixture::new("updates");
    let mut controller = fixture.controller();
    let job = job(&mut controller);
    let catalog = catalog(&fixture, &job);
    let mut final_call = plan_call("plan-final");
    if let ModelEvent::ToolCallRequested(call) = &mut final_call[0] {
        let mut args: serde_json::Value = serde_json::from_str(&call.arguments_json).unwrap();
        args["title"] = json!("订单分析完成");
        args["steps"][1]["status"] = json!("done");
        call.arguments_json = args.to_string();
    }
    let next_call = final_call.clone();
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            plan_call("plan-first"),
            plan_call("plan-second"),
            final_call,
            text_call("完成"),
        ],
    );
    run_turn(&fixture, &job, &provider, &catalog);
    let plans = fixture.plan_payloads(&job.session_id);
    assert_eq!(plans.len(), 1, "同一助手消息里的更新不能追加计划卡");
    assert_eq!(
        plans[0]["title"], "订单分析完成",
        "标题变化也不能新建另一张卡"
    );
    assert_eq!(plans[0]["steps"][1]["status"], "done");
    fixture
        .storage
        .with_ai_connection(|conn| {
            let revision: i64 = conn
                .query_row(
                    "SELECT p.revision FROM ai_message_parts p
             WHERE p.message_id=?1 AND p.kind='plan'",
                    [&job.assistant_message_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(revision, 3, "复用同一片段并递增版本");
            let calls: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM ai_tool_calls WHERE turn_id=?1 AND tool_id='plan.update'",
                    [&job.turn_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(calls, 3, "卡片更新不删除真实调用/回执");
            Ok(())
        })
        .unwrap();

    // 相同标题的新一轮任务有自己的计划，不覆盖上一轮已完成的计划。
    let next = controller
        .prepare_ai_conversation("继续分析", &target())
        .unwrap();
    let next_catalog = self::catalog(&fixture, &next);
    let next_provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![next_call, text_call("下一轮完成")],
    );
    run_turn(&fixture, &next, &next_provider, &next_catalog);
    let plans = fixture.plan_payloads(&job.session_id);
    assert_eq!(
        plans.len(),
        2,
        "计划身份按助手消息隔离，不按标题或整个会话合并"
    );
}
