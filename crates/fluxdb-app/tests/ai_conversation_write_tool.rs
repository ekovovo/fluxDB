//! T020/T024 写工具闭环：注册 `db.execute` 后，写权限来自固定目标连接，
//! 且写操作必须逐次审批；拒绝/越权不会执行。只替换模型事件来源，不替换保存/授权/执行链路。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiModelSaveInput, AppCommand, AppController, AppEvent,
    KernelAuditHook, KernelPolicy, MapSqlResolver, ScriptedGenerationProvider, TargetRef,
    TurnPermissionContext, TurnTargetPin, connection_is_read_only, kernel_policy_for_connection,
    run_ai_conversation_resume_approval, run_ai_conversation_turn, standard_sql_catalog,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{audits, sessions},
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    /// 连接 1 可写、连接 2 只读；两者都是 demo（内存 MockConnector，不触网）。
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-write-tool-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        storage
            .save_connections(&[demo_connection(1, false), demo_connection(2, true)])
            .expect("保存 fixture 连接");
        Self { dir, storage }
    }

    fn configured(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "写工具离线模型".into(),
            provider: "openai".into(),
            model: "write-tool-offline".into(),
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
            display_name: "写工具 Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "写工具测试".into(),
            model_profile_id: Some(model),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { .. } => {}
            other => panic!("配置 Agent 失败: {other:?}"),
        }
        controller
    }

    fn config(&self, id: u64) -> ConnectionConfig {
        self.storage
            .load_connections()
            .expect("读取连接")
            .into_iter()
            .find(|config| config.id == ConnectionId(id))
            .expect("连接存在")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn demo_connection(id: u64, read_only: bool) -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(id),
        name: format!("demo-{id}"),
        kind: DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: format!("demo-{id}.db").into(),
            read_only,
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

fn target(connection_id: i64) -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(connection_id),
        database: Some("main".into()),
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

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tool_calling: true,
        reasoning_summary: false,
    }
}

fn write_step(handle: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: "call-write".into(),
            tool_id: "db.execute".into(),
            arguments_json: format!(
                r#"{{"target":"{handle}","sql":"UPDATE product SET name='x' WHERE id=1"}}"#
            ),
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

fn calls(storage: &FileStorage, turn_id: &str) -> Vec<sessions::ToolCallView> {
    storage
        .with_ai_connection(|conn| sessions::list_tool_calls_for_turn(conn, turn_id))
        .expect("读取工具调用")
}

#[test]
fn read_only_flag_survives_connection_round_trip_and_drives_policy() {
    let fixture = Fixture::new();
    let writable = fixture.config(1);
    let read_only = fixture.config(2);

    assert!(!connection_is_read_only(&writable));
    assert!(connection_is_read_only(&read_only));
    assert!(kernel_policy_for_connection(Some(&writable)).write_allowed);
    assert!(!kernel_policy_for_connection(Some(&read_only)).write_allowed);
    // 没有固定目标时不能写：不能因为"没指定目标"落到任意连接。
    assert!(!kernel_policy_for_connection(None).write_allowed);
    assert!(kernel_policy_for_connection(None).read_allowed);
    assert!(!kernel_policy_for_connection(Some(&writable)).admin_allowed);
}

#[test]
fn write_tool_requires_approval_then_executes_exactly_once() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let config = fixture.config(1);
    let job = controller
        .prepare_ai_conversation("把这一行改掉", &target(1))
        .expect("准备任务");

    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: kernel_policy_for_connection(Some(&config)),
        plugin_digest: None,
    };
    let catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([config.clone()])), true)
        .expect("含写工具目录");
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);

    // 1) 第一次运行：必须停在审批，不得执行。
    let provider = ScriptedGenerationProvider::new(
        capabilities(),
        vec![write_step("conn:1/db:main"), text_step("不应发生")],
    );
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
    );
    let _ = last_terminal(&rx);

    let pending = calls(&fixture.storage, &job.turn_id);
    assert_eq!(pending.len(), 1, "写工具应先落一条待审批调用");
    assert_eq!(pending[0].status, "waiting_approval");
    assert!(pending[0].receipt_json.is_none(), "审批前绝不能有回执");
    let approvals = fixture
        .storage
        .with_ai_connection(|conn| audits::list_approvals_for_call(conn, &pending[0].id))
        .expect("读取审批");
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].status, audits::ApprovalStatus::Pending);
    assert!(!approvals[0].plan_hash.is_empty());

    // 2) 批准（CAS）。
    match controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approvals[0].id.clone(),
            approved: true,
            expected_revision: approvals[0].revision,
        },
    )) {
        AppEvent::AiConversationLoaded(_) => {}
        other => panic!("批准失败: {other:?}"),
    }

    // 3) 恢复：真实执行一次，并把回执交给模型收尾。
    let provider = ScriptedGenerationProvider::new(capabilities(), vec![text_step("已按回执完成")]);
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
        &approvals[0].id,
    );
    let view = last_terminal(&rx);

    let executed = calls(&fixture.storage, &job.turn_id);
    assert_eq!(executed.len(), 1, "写工具不得重复执行");
    assert_eq!(executed[0].status, "succeeded");
    assert!(executed[0].receipt_json.is_some(), "必须写回真实回执");
    let results = fixture
        .storage
        .with_ai_connection(|conn| Ok(sessions::load_turn(conn, &job.turn_id)?.status))
        .unwrap();
    assert_eq!(results, sessions::TurnStatus::Completed);
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
        "写操作必须有结果卡"
    );

    // 4) 重复恢复：批准已消费，不得再执行。
    let provider = ScriptedGenerationProvider::new(capabilities(), vec![text_step("不应再次执行")]);
    let (tx, _rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission,
        &approvals[0].id,
    );
    assert_eq!(
        calls(&fixture.storage, &job.turn_id)[0].status,
        "succeeded",
        "重复恢复不能改变已完成的调用"
    );
}

#[test]
fn write_tool_is_denied_when_connection_policy_forbids_write() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let config = fixture.config(1);
    let job = controller
        .prepare_ai_conversation("越权写入", &target(1))
        .expect("准备任务");

    // 目录里有写工具，但内核权限不允许写 → 必须 Deny，而不是弹审批。
    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
    };
    assert!(!permission.policy.write_allowed);
    let catalog =
        standard_sql_catalog(Arc::new(MapSqlResolver::new([config])), true).expect("目录");
    let provider = ScriptedGenerationProvider::new(
        capabilities(),
        vec![write_step("conn:1/db:main"), text_step("写被拒绝")],
    );
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission,
    );
    let view = last_terminal(&rx);

    let recorded = calls(&fixture.storage, &job.turn_id);
    assert_eq!(recorded.len(), 1, "被拒绝也要留事实行");
    assert_eq!(recorded[0].status, "denied");
    let receipt = recorded[0].receipt_json.clone().expect("拒绝回执");
    assert!(receipt.contains("write_not_allowed"), "拒绝原因: {receipt}");
    let approvals = fixture
        .storage
        .with_ai_connection(|conn| audits::list_approvals_for_call(conn, &recorded[0].id))
        .expect("读取审批");
    assert!(approvals.is_empty(), "权限拒绝不应产生审批项");
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
        "被拒绝也必须在 UI 有可见结果卡"
    );
}

#[test]
fn tool_target_outside_pinned_connection_is_rejected() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let config = fixture.config(1);
    let job = controller
        .prepare_ai_conversation("读另一个连接", &target(1))
        .expect("准备任务");

    // resolver 只暴露固定目标连接：模型不能用 conn:2 访问别的连接。
    let catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([config.clone()])), false)
        .expect("只读目录");
    let provider = ScriptedGenerationProvider::new(
        capabilities(),
        vec![
            vec![
                ModelEvent::ToolCallRequested(ToolCallRequest {
                    provider_call_id: "call-other".into(),
                    tool_id: "db.query".into(),
                    arguments_json: r#"{"target":"conn:2/db:main","sql":"SELECT 1"}"#.into(),
                }),
                ModelEvent::Finished(FinishReason::Stop),
            ],
            text_step("无法访问其它连接"),
        ],
    );
    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: kernel_policy_for_connection(Some(&config)),
        plugin_digest: None,
    };
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission,
    );
    let _ = last_terminal(&rx);

    let recorded = calls(&fixture.storage, &job.turn_id);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].status, "failed", "越权目标必须失败而不是执行");
    let receipt = recorded[0].receipt_json.clone().expect("失败回执");
    assert!(
        receipt.contains("connection_not_found") || receipt.contains("target"),
        "失败原因应指向无效目标: {receipt}"
    );
}
