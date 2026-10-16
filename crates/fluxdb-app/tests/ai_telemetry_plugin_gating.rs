//! 「生命周期诊断」插件的门控与落盘验证（设计 §23.2，`fluxdb.lifecycle.telemetry`）。
//!
//! 只关注两条确定性链路（不触网、不读用户配置，隔离临时目录）：
//! 1. 插件实例开启时，一轮结束会写 `turn_diagnostics` / `stage_timing` 事件；
//! 2. 插件实例关闭后，同一轮不写诊断事件（执行事实仍照常落盘）。

use std::{path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiConversationJob, AiConversationUpdate, AiModelSaveInput, AppCommand, AppController, AppEvent,
    KIND_STAGE_TIMING, KIND_TURN_DIAGNOSTICS, KernelAuditHook, KernelPolicy, MapSqlResolver,
    ScriptedGenerationProvider, TargetRef, ToolCatalog, TurnPermissionContext, TurnTargetPin,
    standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason,
    ModelCapabilities, ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{FileStorage, ai_platform::run_events};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-telemetry-plugin-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("创建隔离目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    fn set_telemetry_enabled(&self, enabled: bool) {
        let instance = self
            .storage
            .with_ai_connection(|conn| {
                Ok(
                    fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
                        .into_iter()
                        .find(|instance| instance.id == "builtin.telemetry")
                        .expect("内置诊断实例必须存在"),
                )
            })
            .expect("读取诊断实例");
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
            .expect("切换诊断插件");
    }

    /// 走生产命令配置一个离线模型并设为默认主模型；`prepare` 需要可解析的主模型。
    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "诊断插件 E2E 模型".into(),
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

    fn run_event_kinds(&self, turn_id: &str) -> Vec<String> {
        self.storage
            .with_ai_connection(|conn| {
                Ok(run_events::list_for_turn(conn, turn_id)?
                    .into_iter()
                    .map(|event| event.kind)
                    .collect::<Vec<_>>())
            })
            .expect("查询运行事件")
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
        name: "telemetry demo".into(),
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
        .prepare_ai_conversation("查一下", &target())
        .expect("准备工具回合")
}

fn catalog() -> ToolCatalog {
    standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("装配只读目录")
}

fn provider() -> ScriptedGenerationProvider {
    ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            vec![
                ModelEvent::ToolCallRequested(ToolCallRequest {
                    provider_call_id: "tele-call-1".into(),
                    tool_id: "db.query".into(),
                    arguments_json: r#"{"target":"conn:1/db:main","sql":"SELECT 1"}"#.into(),
                }),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![
                ModelEvent::TextDelta("完成".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ],
    )
}

fn run_turn(fixture: &Fixture, job: &AiConversationJob) -> AiConversationUpdate {
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    fluxdb_app::run_ai_conversation_turn(
        &fixture.storage,
        job,
        &cancel,
        &tx,
        &provider(),
        &catalog(),
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

#[test]
fn telemetry_writes_diagnostics_only_when_enabled() {
    let fixture = Fixture::new("gating");
    let mut controller = fixture.controller();

    // 开启：诊断事件必须落盘。
    fixture.set_telemetry_enabled(true);
    let job_on = job(&mut controller);
    let update = run_turn(&fixture, &job_on);
    assert!(update.terminal);
    let kinds_on = fixture.run_event_kinds(&job_on.turn_id);
    assert!(
        kinds_on.iter().any(|kind| kind == KIND_TURN_DIAGNOSTICS),
        "开启时应有 turn_diagnostics 事件，实际 {kinds_on:?}"
    );
    assert!(
        kinds_on.iter().any(|kind| kind == KIND_STAGE_TIMING),
        "开启时应有 stage_timing 事件，实际 {kinds_on:?}"
    );

    // 关闭：同一轮不得写诊断事件；工具回执等执行事实仍照常落盘。
    fixture.set_telemetry_enabled(false);
    let job_off = job(&mut controller);
    let update = run_turn(&fixture, &job_off);
    assert!(update.terminal);
    let kinds_off = fixture.run_event_kinds(&job_off.turn_id);
    assert!(
        !kinds_off.iter().any(|kind| kind == KIND_TURN_DIAGNOSTICS),
        "关闭时不得写 turn_diagnostics，实际 {kinds_off:?}"
    );
    assert!(
        !kinds_off.iter().any(|kind| kind == KIND_STAGE_TIMING),
        "关闭时不得写 stage_timing，实际 {kinds_off:?}"
    );
}
