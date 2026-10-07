//! T020/T023 应用数据链路验收：工具回合经 TurnRunner（schema→审计→授权→执行→投影）后，
//! 真实回执写成会话片段并进入 AppState；不替换生产投影逻辑，不发网络请求。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent, AuditInput,
    AuditOutcome, KernelPolicy, MapSqlResolver, ScriptedGenerationProvider, SecurityAuditHook,
    TargetRef, TurnPermissionContext, TurnTargetPin, run_ai_conversation_turn,
    standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{FileStorage, ai_platform::sessions};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t023-app-{}", uuid::Uuid::new_v4()));
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

    fn configured(&self) -> AppController {
        let mut controller = self.controller();
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T023 离线模型".into(),
            provider: "openai".into(),
            model: "t023-offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "T023 工具 Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "T023 工具测试提示词".into(),
            model_profile_id: Some(model),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { .. } => {}
            other => panic!("配置 Agent 失败: {other:?}"),
        }
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 测试用审计 Hook：只记录"审计被调用过"，返回放行。
struct AllowAudit(std::sync::atomic::AtomicUsize);

impl SecurityAuditHook for AllowAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(AuditOutcome::Allow)
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

/// demo 连接（内存 MockConnector，不触网）。
fn demo_config() -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(1),
        name: "T023 demo".into(),
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

fn permission() -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
    }
}

fn tool_call_stop(tool: &str, call_id: &str, args: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: tool.into(),
            arguments_json: args.into(),
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn text_stop(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn part_kinds(view: &fluxdb_app::AiConversationView) -> Vec<(String, String, String)> {
    view.messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .map(|part| {
            (
                part.kind.clone(),
                part.status.clone(),
                part.payload_json.clone(),
            )
        })
        .collect()
}

#[test]
fn read_tool_turn_persists_real_receipt_and_survives_storage_round_trip() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("统计一下产品", &target())
        .expect("准备任务");

    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call_stop(
                "db.query",
                "call-read",
                r#"{"target":"conn:1/db:main","sql":"SELECT * FROM product","max_rows":5}"#,
            ),
            text_stop("已按真实回执总结"),
        ],
    );
    let catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("标准只读目录");
    let audit = AllowAudit(std::sync::atomic::AtomicUsize::new(0));
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);

    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &audit,
        permission(),
    );

    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = Some(update.view);
        }
    }
    let view = last.expect("必须有终态回包");

    let parts = part_kinds(&view);
    let tool_call = parts
        .iter()
        .find(|(kind, _, _)| kind == "tool_call_ref")
        .expect("工具调用卡必须入库");
    assert_ne!(tool_call.1, "pending", "工具卡不得停留在 pending");
    assert!(
        tool_call.2.contains("db.query"),
        "工具卡要写明调用了哪个工具: {}",
        tool_call.2
    );
    let tool_result = parts
        .iter()
        .find(|(kind, _, _)| kind == "tool_result_ref")
        .expect("工具结果卡必须入库");
    let payload: serde_json::Value = serde_json::from_str(&tool_result.2).expect("结果载荷");
    assert_eq!(payload["status"].as_str(), Some("succeeded"));
    assert_eq!(payload["tool"].as_str(), Some("db.query"));

    let assistant = view
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("助手消息必须存在");
    let text = assistant
        .parts
        .iter()
        .find(|part| part.kind == "text")
        .expect("助手正文必须入库");
    let payload: serde_json::Value = serde_json::from_str(&text.payload_json).expect("正文载荷");
    assert_eq!(payload["text"].as_str(), Some("已按真实回执总结"));

    // 真实存储round-trip：重开存储后片段仍然存在（不是只在内存 view 里）。
    let reopened = FileStorage::new(&fixture.dir);
    let snapshot = reopened
        .with_ai_connection(|conn| sessions::session_snapshot(conn, &job.session_id))
        .expect("重开存储");
    let kinds: Vec<String> = snapshot
        .parts
        .iter()
        .map(|part| part.kind.as_str().to_string())
        .collect();
    assert!(kinds.contains(&"tool_call_ref".to_string()));
    assert!(kinds.contains(&"tool_result_ref".to_string()));
    assert_eq!(
        snapshot
            .turns
            .iter()
            .find(|turn| turn.id == job.turn_id)
            .map(|turn| turn.status),
        Some(sessions::TurnStatus::Completed)
    );
    assert!(
        audit.0.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "工具调用必须真的经过强制审计"
    );
}

#[test]
fn write_tool_is_absent_from_read_only_catalog_and_never_executes() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("删掉一行", &target())
        .expect("准备任务");

    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call_stop(
                "db.execute",
                "call-write",
                r#"{"target":"conn:1/db:main","sql":"DELETE FROM product WHERE id=1"}"#,
            ),
            text_stop("写工具不可用"),
        ],
    );
    let catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("标准只读目录");
    let audit = AllowAudit(std::sync::atomic::AtomicUsize::new(0));
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);

    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &audit,
        permission(),
    );

    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = Some(update.view);
        }
    }
    let view = last.expect("必须有终态回包");

    let parts = part_kinds(&view);
    assert!(
        !parts
            .iter()
            .any(|(kind, status, payload)| kind == "tool_result_ref"
                && status == "complete"
                && payload.contains("succeeded")),
        "只读目录不得出现成功的写工具结果"
    );
    let notice = parts
        .iter()
        .find(|(kind, _, _)| kind == "notice")
        .expect("未注册工具必须有可见通知");
    assert!(
        notice.2.contains("未注册") || notice.2.contains("未执行"),
        "通知应说明工具不可用: {}",
        notice.2
    );
    assert_eq!(
        audit.0.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "未知工具不应进入审计/执行"
    );

    // 真实存储核对：没有任何写工具执行痕迹。
    let reopened = FileStorage::new(&fixture.dir);
    let snapshot = reopened
        .with_ai_connection(|conn| sessions::session_snapshot(conn, &job.session_id))
        .expect("重开存储");
    assert!(
        !snapshot
            .parts
            .iter()
            .any(|part| part.kind.as_str() == "tool_result_ref"),
        "写工具被拒绝时不应产生工具结果片段"
    );
}
