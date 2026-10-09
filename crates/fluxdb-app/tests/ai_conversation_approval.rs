//! T020/T029 审批闭环验收：暂停不执行 → 批准（CAS）→ 按 plan_hash 复核后执行一次 → 回执交给模型继续。
//! 只替换模型事件来源与审计结论，不替换保存/授权/执行链路；不发网络请求。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiModelSaveInput, AppCommand, AppController, AppEvent,
    AuditInput, AuditOutcome, KernelPolicy, MapSqlResolver, ScriptedGenerationProvider,
    SecurityAuditHook, TargetRef, TurnPermissionContext, TurnTargetPin,
    run_ai_conversation_resume_approval, run_ai_conversation_turn, standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{audits, sessions},
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t020-app-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn configured(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T020 离线模型".into(),
            provider: "openai".into(),
            model: "t020-offline".into(),
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
            display_name: "T020 审批 Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "T020 审批测试".into(),
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

/// 审计结论 = 需要审批（读工具也走审批，用来构造暂停态）。
struct RequireApprovalAudit;
impl SecurityAuditHook for RequireApprovalAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::RequireApproval)
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

fn demo_config() -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(1),
        name: "T020 demo".into(),
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

fn catalog() -> fluxdb_app::ToolCatalog {
    standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false).expect("只读目录")
}

fn read_step() -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: "call-approval".into(),
            tool_id: "db.query".into(),
            arguments_json:
                r#"{"target":"conn:1/db:main","sql":"SELECT * FROM product","max_rows":5}"#.into(),
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn text_step(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tool_calling: true,
        reasoning_summary: false,
    }
}

fn last_terminal(
    rx: &std::sync::mpsc::Receiver<fluxdb_app::AiConversationUpdate>,
) -> fluxdb_app::AiConversationView {
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = update.view;
        }
    }
    last.expect("终态回包必须携带视图")
}

#[test]
fn approval_pauses_without_executing_then_approve_executes_once_and_continues() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("看看产品表", &target())
        .expect("准备任务");

    // 1) 第一轮：审计要求审批 → 暂停，工具**不执行**。
    let prefix = "先读取产品，再总结。\n";
    let mut step = read_step();
    step.insert(0, ModelEvent::TextDelta(prefix.into()));
    let provider =
        ScriptedGenerationProvider::new(capabilities(), vec![step, text_step("不应发生")]);
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog(),
        &RequireApprovalAudit,
        permission(),
    );
    let _view = last_terminal(&rx);

    let (turn_status, call_status, approval_id, plan_hash) = fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let call = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少工具调用")
                })?;
            let approvals = audits::list_approvals_for_call(conn, &call.id)?;
            let approval = approvals.first().cloned().expect("应有待审批项");
            Ok((
                turn.status,
                call.status.clone(),
                approval.id.clone(),
                approval.plan_hash.clone(),
            ))
        })
        .expect("读取事实行");

    assert_eq!(turn_status, sessions::TurnStatus::WaitingApproval);
    assert_eq!(
        call_status, "waiting_approval",
        "审批前工具调用必须停在 waiting_approval"
    );
    assert!(!plan_hash.is_empty(), "审批必须绑定 plan_hash");

    // 2) 批准（对象级 CAS）。
    match controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval_id.clone(),
            approved: true,
            expected_revision: 1,
        },
    )) {
        AppEvent::AiConversationLoaded(_) => {}
        other => panic!("批准失败: {other:?}"),
    }
    let status = fixture
        .storage
        .with_ai_connection(|conn| Ok(audits::load_approval(conn, &approval_id)?.status))
        .unwrap();
    assert_eq!(status, audits::ApprovalStatus::Approved);

    // 决定后原审批片段必须离开 pending 并写入 decision，否则面板会继续显示可点的批准/拒绝按钮。
    let decided = fixture
        .storage
        .with_ai_connection(|conn| {
            let message_id = sessions::find_assistant_message_for_turn(conn, &job.turn_id)?
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少助手消息")
                })?;
            Ok(sessions::list_parts(conn, &message_id)?
                .into_iter()
                .filter(|part| part.kind == sessions::PartKind::Notice)
                .find(|part| part.payload_json.contains("approval_id"))
                .map(|part| (part.status, part.payload_json)))
        })
        .unwrap()
        .expect("应有审批片段");
    assert_eq!(decided.0, sessions::PartStatus::Complete);
    let payload: serde_json::Value = serde_json::from_str(&decided.1).unwrap();
    assert_eq!(payload["decision"].as_str(), Some("approved"));
    assert_eq!(
        payload["text_offset"].as_u64(),
        Some(prefix.len() as u64),
        "批准后审批卡不能移动到回复结尾"
    );

    // 3) 恢复：按 plan_hash 复核后执行一次，并把回执交给模型继续。
    let provider = ScriptedGenerationProvider::new(capabilities(), vec![text_step("已按回执完成")]);
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog(),
        &RequireApprovalAudit,
        permission(),
        &approval_id,
    );
    let view = last_terminal(&rx);

    let snapshot = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok((
                sessions::load_turn(conn, &job.turn_id)?,
                sessions::list_tool_calls_for_turn(conn, &job.turn_id)?,
            ))
        })
        .expect("读取事实行");
    let (turn, calls) = snapshot;
    let call = calls.first().expect("工具调用应存在");
    assert_eq!(call.status, "succeeded", "批准后必须真实执行一次");
    assert!(call.receipt_json.is_some(), "必须写回真实回执");
    assert_eq!(turn.status, sessions::TurnStatus::Completed);
    // 只执行一次：工具调用事实行仍然只有一行。
    assert_eq!(calls.len(), 1, "不得重复执行");
    let assistant = view
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("助手消息");
    assert!(
        assistant
            .parts
            .iter()
            .any(|part| part.kind == "tool_result_ref"),
        "恢复后应有结果卡"
    );
    let text = assistant
        .parts
        .iter()
        .find(|part| part.kind == "text")
        .and_then(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
        .and_then(|value| {
            value
                .get("text")
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    assert!(
        text.contains("已按回执完成"),
        "恢复后正文要续写而不是清空: {text}"
    );
    assert!(text.starts_with(prefix), "审批前正文必须保留");
}

#[test]
fn denied_approval_never_executes_the_tool() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("别执行", &target())
        .expect("准备任务");

    let provider =
        ScriptedGenerationProvider::new(capabilities(), vec![read_step(), text_step("不应发生")]);
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog(),
        &RequireApprovalAudit,
        permission(),
    );
    let _ = last_terminal(&rx);

    let approval_id = fixture
        .storage
        .with_ai_connection(|conn| {
            let call = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少工具调用")
                })?;
            Ok(audits::list_approvals_for_call(conn, &call.id)?
                .first()
                .map(|a| a.id.clone()))
        })
        .unwrap()
        .expect("应当有待审批项");

    match controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval_id.clone(),
            approved: false,
            expected_revision: 1,
        },
    )) {
        AppEvent::AiConversationLoaded(_) => {}
        other => panic!("拒绝失败: {other:?}"),
    }

    let call = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_tool_calls_for_turn(conn, &job.turn_id))
        .unwrap()
        .into_iter()
        .next()
        .expect("工具调用");
    assert_eq!(call.status, "denied");
    assert!(call.receipt_json.is_none(), "拒绝不能产生任何回执");
    assert_eq!(call.status, "denied");
}
