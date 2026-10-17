//! 「生命周期诊断」读取面的回归验证（设计 §23.2）。
//!
//! 覆盖三条确定性链路（离线 Provider、隔离临时目录）：
//! 1. 模型请求失败时，Turn 错误码是**受控码**而不是中文文案，且失败原因落成
//!    会话里可重读的 notice 片段；
//! 2. 诊断读取投影能给出失败阶段、归属方、脱敏码与可重试性；
//! 3. 关闭遥测插件后，读取投影仍能解释终态与错误码（只是没有阶段遥测）。

use std::{path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiConversationJob, DiagnosticsQuery, KernelAuditHook, KernelPolicy, MapSqlResolver, TargetRef,
    ToolCatalog, TurnPermissionContext, TurnTargetPin, standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ErrorKind,
    ModelCapabilities,
};
use fluxdb_storage::FileStorage;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-diagnostics-read-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("创建隔离目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    /// 走生产命令配置一个离线模型并设为默认主模型；`prepare` 需要可解析的主模型。
    fn controller(&self) -> fluxdb_app::AppController {
        let mut controller = fluxdb_app::AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model_id = match controller.dispatch(fluxdb_app::AppCommand::SaveAiModel(
            fluxdb_app::AiModelSaveInput {
                display_name: "诊断读取 E2E 模型".into(),
                provider: "openai".into(),
                model: "offline".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                enabled: true,
                ..Default::default()
            },
        )) {
            fluxdb_app::AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        let revision = controller.state().ai_runtime_revision;
        assert!(
            matches!(
                controller.dispatch(fluxdb_app::AppCommand::SetAiDefaultModel {
                    model_id: Some(model_id),
                    expected_revision: revision,
                }),
                fluxdb_app::AppEvent::AiDefaultModelsSaved { .. }
            ),
            "设置默认主模型失败"
        );
        controller
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn demo_config() -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId(1),
        name: "diagnostics demo".into(),
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

fn job(controller: &mut fluxdb_app::AppController) -> AiConversationJob {
    controller
        .prepare_ai_conversation("查一下", &target())
        .expect("准备工具回合")
}

fn catalog() -> ToolCatalog {
    standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("装配只读目录")
}

/// 首次请求即返回鉴权失败：鉴权错误是确定性失败，不会被重试掩盖次数语义。
struct FailingProvider;

impl fluxdb_core::GenerationProvider for FailingProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        }
    }

    fn stream(
        &self,
        _request: fluxdb_core::ModelRequest,
        _cancel: CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            Err(fluxdb_core::Error::new(
                ErrorKind::Authentication,
                "上游拒绝：API Key 无效",
            ))
        })
    }
}

fn run_failing_turn(fixture: &Fixture, job: &AiConversationJob) {
    let cancel = CancelFlag::new();
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    fluxdb_app::run_ai_conversation_turn(
        &fixture.storage,
        job,
        &cancel,
        &tx,
        &FailingProvider,
        &catalog(),
        &KernelAuditHook,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );
}

fn report(
    fixture: &Fixture,
    job: &AiConversationJob,
    query: DiagnosticsQuery,
) -> serde_json::Value {
    fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_app::turn_diagnostics_report(conn, &job.session_id, &job.turn_id, query)
        })
        .expect("诊断投影")
}

#[test]
fn failed_turn_keeps_code_and_visible_reason() {
    let fixture = Fixture::new("failed-turn");
    let mut controller = fixture.controller();
    fixture.set_telemetry_enabled(true);
    let job = job(&mut controller);
    run_failing_turn(&fixture, &job);

    let (status, error_code, text) = fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)?;
            let text =
                fluxdb_storage::ai_platform::sessions::list_parts(conn, &job.assistant_message_id)?
                    .into_iter()
                    .find(|part| part.kind == fluxdb_storage::ai_platform::sessions::PartKind::Text)
                    .and_then(|part| {
                        serde_json::from_str::<serde_json::Value>(&part.payload_json).ok()
                    })
                    .and_then(|payload| payload["text"].as_str().map(str::to_owned))
                    .unwrap_or_default();
            Ok((turn.status.as_str().to_string(), turn.error_code, text))
        })
        .expect("读取 Turn 事实");

    assert_eq!(status, "failed");
    // 错误码列必须是受控码，而不是中文文案：否则诊断无法聚合。
    assert_eq!(error_code.as_deref(), Some("authentication"));
    assert!(
        text.contains("上游拒绝：API Key 无效"),
        "失败原因必须可见：{text}"
    );
}

#[test]
fn report_explains_failure_stage_owner_and_retryability() {
    let fixture = Fixture::new("report");
    let mut controller = fixture.controller();
    fixture.set_telemetry_enabled(true);
    let job = job(&mut controller);
    run_failing_turn(&fixture, &job);

    let value = report(
        &fixture,
        &job,
        DiagnosticsQuery {
            turns: 1,
            include_stages: true,
        },
    );
    let turn = &value["turns"][0];
    assert_eq!(turn["status"], "failed");
    assert_eq!(turn["error_code"], "authentication");
    assert_eq!(turn["current"], true, "当前轮必须可辨识");
    let diagnostics = &turn["diagnostics"];
    assert_eq!(diagnostics["failure"]["stage"], "model_request");
    assert_eq!(diagnostics["failure"]["owner"], "provider");
    assert_eq!(diagnostics["failure"]["code"], "authentication");
    assert_eq!(diagnostics["failure"]["retryable"], false);
    assert_eq!(diagnostics["model_failures"], 1);
    // 阶段明细里必须能看到这一次失败请求的定位与可重试判定。
    let stage = turn["stage_timings"]
        .as_array()
        .expect("阶段明细数组")
        .iter()
        .find(|stage| stage["stage"] == "model_request")
        .expect("模型请求阶段");
    assert_eq!(stage["iteration"], 1);
    assert_eq!(stage["ok"], false);
    assert_eq!(stage["code"], "authentication");
    assert_eq!(stage["retryable"], false);
    assert_eq!(value["telemetry_enabled"], true);
    // 只读投影不得携带任何凭据或正文材料。
    let encoded = value.to_string();
    for forbidden in ["API Key 无效", "runtime_snapshot", "arguments", "receipt"] {
        assert!(!encoded.contains(forbidden), "诊断输出不得包含 {forbidden}");
    }
}

#[test]
fn report_stays_explainable_with_telemetry_disabled() {
    let fixture = Fixture::new("read-no-telemetry");
    let mut controller = fixture.controller();
    fixture.set_telemetry_enabled(false);
    let job = job(&mut controller);
    run_failing_turn(&fixture, &job);

    let value = report(
        &fixture,
        &job,
        DiagnosticsQuery {
            turns: 1,
            include_stages: true,
        },
    );
    let turn = &value["turns"][0];
    assert_eq!(turn["status"], "failed");
    // 关掉插件只缺少阶段遥测，终态与错误码仍必须可解释。
    assert_eq!(turn["error_code"], "authentication");
    assert!(turn["diagnostics"].is_null(), "关闭时不应有轮级诊断汇总");
    assert_eq!(value["telemetry_enabled"], false);

    let events = fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::run_events::list_for_turn(conn, &job.turn_id)
        })
        .expect("查询运行事件");
    assert!(
        !events
            .iter()
            .any(|event| event.kind == fluxdb_app::KIND_STAGE_TIMING
                || event.kind == fluxdb_app::KIND_TURN_DIAGNOSTICS),
        "关闭时不得写诊断事件"
    );
}

#[test]
fn diagnostics_tool_is_available_and_reads_current_turn() {
    let fixture = Fixture::new("read-tool");
    let mut controller = fixture.controller();
    fixture.set_telemetry_enabled(true);
    let job = job(&mut controller);
    run_failing_turn(&fixture, &job);

    let mut catalog = catalog();
    fluxdb_app::add_ai_diagnostics_tool(&mut catalog, &fixture.storage, &job)
        .expect("装配诊断工具");
    assert!(
        catalog.prepare("diagnostics.read_turns", "{}").is_ok(),
        "无参数调用必须合法（默认最近 2 轮）"
    );
    for arguments in [
        r#"{"turns":0}"#,
        r#"{"turns":6}"#,
        r#"{"turns":"2"}"#,
        r#"{"detail":"all"}"#,
        r#"{"turn_id":"other-session-turn"}"#,
    ] {
        assert!(
            catalog
                .prepare("diagnostics.read_turns", arguments)
                .is_err(),
            "非法参数必须拒绝：{arguments}"
        );
    }

    let prepared = catalog
        .prepare("diagnostics.read_turns", r#"{"turns":1,"detail":"stages"}"#)
        .expect("合法参数");
    let tool = catalog
        .executable("diagnostics.read_turns")
        .expect("诊断工具可执行");
    let ctx = fluxdb_core::ToolExecutionContext {
        automatic_approval: false,
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        iteration: 1,
        call_id: "diag-call-1".into(),
        cancel: CancelFlag::new(),
        progress: None,
    };
    let receipt = fluxdb_app::ai_executor::block_on_cancellable(
        tool.execute(ctx, prepared.to_json_string()),
        &CancelFlag::new(),
    )
    .expect("执行器")
    .expect("执行诊断工具");
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    let value: serde_json::Value =
        serde_json::from_str(&receipt.result_json).expect("合法 JSON 回执");
    assert_eq!(value["ok"], true);
    assert_eq!(value["turns"][0]["error_code"], "authentication");
    assert_eq!(
        value["turns"][0]["diagnostics"]["failure"]["owner"],
        "provider"
    );
    // 会话级定位由宿主决定：工具无法读取别的会话。
    assert_eq!(value["turns"][0]["current"], true);
}
