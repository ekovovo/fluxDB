//! 审批生命周期与恢复语义保护测试（设计 §25 审批闭环、§29 审计、§42.11 审批表）。
//!
//! 只走生产链路：
//! `prepare_ai_conversation` → `run_ai_conversation_turn`（暂停不执行）→
//! `AiConversationCommand::DecideApproval`（对象级 CAS）→ `prepare_ai_resume_job` →
//! `run_ai_conversation_resume_approval`。
//!
//! 每条断言都核对真实 SQLite 事实行（`ai_approvals` / `ai_tool_calls` / `ai_results` /
//! `ai_message_parts`）与工具真实执行次数（`RecordingTool::call_count()`）。
//! 不发网络、不读用户真实配置；仅把模型事件来源替换为脚本、审计结论固定为“需要审批”。

use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiConversationUpdate,
    AiModelSaveInput, AppCommand, AppController, AppEvent, AuditInput, AuditOutcome, KernelPolicy,
    RecordingTool, ScriptedGenerationProvider, SecurityAuditHook, TargetRef, ToolCatalog, ToolSpec,
    TurnPermissionContext, TurnTargetPin, run_ai_conversation_resume_approval,
    run_ai_conversation_turn,
};
use fluxdb_core::{
    AiFuture, AiTool, CancelFlag, FinishReason, ModelCapabilities, ModelEvent, ToolCallRequest,
    ToolDescriptor, ToolExecutionContext, ToolReceipt, ToolRisk,
};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{audits, sessions},
};
use rusqlite::params;

// ---------------------------------------------------------------------------
// 隔离 fixture（独立临时目录 + Drop 清理；不碰用户真实配置/数据库）
// ---------------------------------------------------------------------------

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("fluxdb-ai-approval-guard-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("创建隔离 fixture 目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    fn configured(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "审批 guard 模型".into(),
            provider: "openai".into(),
            model: "approval-guard-offline".into(),
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
            display_name: "审批 guard Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "审批 guard 测试".into(),
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

// ---------------------------------------------------------------------------
// 固定审计结论：读工具也要求审批，用来构造稳定的暂停态
// ---------------------------------------------------------------------------

struct RequireApprovalAudit;
impl SecurityAuditHook for RequireApprovalAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::RequireApproval)
    }
}

// ---------------------------------------------------------------------------
// 真实副作用计数工具：把 `Arc<RecordingTool>` 适配成 `AiTool` 注册进目录
// ---------------------------------------------------------------------------

struct SharedRecordingTool(Arc<RecordingTool>);

impl AiTool for SharedRecordingTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.0.descriptor()
    }

    fn execute(
        &self,
        ctx: ToolExecutionContext,
        arguments_json: String,
    ) -> AiFuture<'_, ToolReceipt> {
        self.0.execute(ctx, arguments_json)
    }
}

/// 用共享 `Arc<RecordingTool>` 构造目录；每次重建目录都复用同一句柄，故计数跨调用累加。
fn catalog_for(tool: &Arc<RecordingTool>) -> ToolCatalog {
    let spec = ToolSpec::from_descriptor(tool.descriptor()).expect("工具 spec");
    let mut catalog = ToolCatalog::new(vec![spec]);
    catalog
        .register(Box::new(SharedRecordingTool(tool.clone())))
        .expect("注册工具");
    catalog
}

fn read_tool(tool_id: &str) -> Arc<RecordingTool> {
    Arc::new(RecordingTool::new(tool_id, ToolRisk::Read))
}

// ---------------------------------------------------------------------------
// 通用脚本 / 权限 / 目标
// ---------------------------------------------------------------------------

fn caps() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tool_calling: true,
        reasoning_summary: false,
    }
}

fn permission() -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        disabled_tools: Default::default(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
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

fn tool_step(tool_id: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: "call-1".into(),
            tool_id: tool_id.into(),
            arguments_json: r#"{"sql":"SELECT 1"}"#.into(),
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
    rx: &std::sync::mpsc::Receiver<AiConversationUpdate>,
) -> fluxdb_app::AiConversationView {
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = update.view;
        }
    }
    last.expect("终态回包必须携带视图")
}

// ---------------------------------------------------------------------------
// 事实读取 helper（真实 SQLite 查询）
// ---------------------------------------------------------------------------

fn sql_error(error: rusqlite::Error) -> fluxdb_core::Error {
    fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        format!("SQLite 查询失败: {error}"),
    )
}

#[derive(Debug, Clone)]
struct CallFact {
    status: String,
    receipt_json: Option<String>,
    plan_hash: String,
}

fn call_fact(storage: &FileStorage, call_id: &str) -> CallFact {
    storage
        .with_ai_connection(|conn| {
            let call = sessions::load_tool_call(conn, call_id)?;
            Ok(CallFact {
                status: call.status,
                receipt_json: call.receipt_json,
                plan_hash: call.plan_hash,
            })
        })
        .expect("读取工具调用事实行")
}

fn approval_view(storage: &FileStorage, approval_id: &str) -> audits::ApprovalView {
    storage
        .with_ai_connection(|conn| audits::load_approval(conn, approval_id))
        .expect("读取审批事实行")
}

fn turn_status(storage: &FileStorage, turn_id: &str) -> sessions::TurnStatus {
    storage
        .with_ai_connection(|conn| Ok(sessions::load_turn(conn, turn_id)?.status))
        .expect("读取 Turn 事实行")
}

/// `ai_results` 行数（一个成功工具执行恰好写一行）。
fn result_rows(storage: &FileStorage, turn_id: &str) -> i64 {
    storage
        .with_ai_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM ai_results WHERE turn_id=?1",
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)
        })
        .expect("统计 ai_results 行数")
}

/// `tool_result_ref` 片段数（结果卡数量）。
fn result_ref_parts(storage: &FileStorage, turn_id: &str) -> i64 {
    storage
        .with_ai_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM ai_message_parts p JOIN ai_messages m ON m.id=p.message_id
                  WHERE m.turn_id=?1 AND p.kind='tool_result_ref'",
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)
        })
        .expect("统计 tool_result_ref 片段数")
}

/// 工具调用事实行总数（同一轮只应有一行；重复执行不会新增行，靠 call_count 发现）。
fn call_rows(storage: &FileStorage, turn_id: &str) -> i64 {
    storage
        .with_ai_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM ai_tool_calls WHERE turn_id=?1",
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)
        })
        .expect("统计 ai_tool_calls 行数")
}

fn tamper_plan_hash(storage: &FileStorage, call_id: &str, new_hash: &str) {
    storage
        .with_ai_connection(|conn| {
            conn.execute(
                "UPDATE ai_tool_calls SET plan_hash=?2 WHERE id=?1",
                params![call_id, new_hash],
            )
            .map_err(sql_error)?;
            Ok(())
        })
        .expect("篡改 plan_hash（模拟参数/目标漂移）");
}

// ---------------------------------------------------------------------------
// 生产链路 helper
// ---------------------------------------------------------------------------

struct Paused {
    job: AiConversationJob,
    approval_id: String,
    call_id: String,
    plan_hash: String,
}

/// 第 1 轮：模型请求读工具 → 审计要求审批 → 暂停（工具不执行），返回暂停事实。
fn pause_round(
    fixture: &Fixture,
    controller: &mut AppController,
    tool: &Arc<RecordingTool>,
) -> Paused {
    let tool_id = tool.descriptor().id.clone();
    let job = controller
        .prepare_ai_conversation("看看产品表", &target())
        .expect("准备任务");
    let provider =
        ScriptedGenerationProvider::new(caps(), vec![tool_step(&tool_id), text_step("不应发生")]);
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &CancelFlag::new(),
        &tx,
        &provider,
        &catalog_for(tool),
        &RequireApprovalAudit,
        permission(),
    );
    let _ = last_terminal(&rx);

    let (approval_id, call_id, plan_hash) = fixture
        .storage
        .with_ai_connection(|conn| {
            let call = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少工具调用")
                })?;
            let approval = audits::list_approvals_for_call(conn, &call.id)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少审批行")
                })?;
            Ok((approval.id, call.id, call.plan_hash))
        })
        .expect("读取暂停事实行");

    assert_eq!(
        turn_status(&fixture.storage, &job.turn_id),
        sessions::TurnStatus::WaitingApproval
    );
    Paused {
        job,
        approval_id,
        call_id,
        plan_hash,
    }
}

/// 经 CAS 批准当前审批。
fn approve(controller: &mut AppController, approval_id: &str, expected_revision: i64) {
    match controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval_id.into(),
            approved: true,
            expected_revision,
        },
    )) {
        AppEvent::AiConversationLoaded(_) => {}
        other => panic!("批准失败: {other:?}"),
    }
}

/// 经 CAS 拒绝当前审批，返回事件（可能是错误）。
fn decide(
    controller: &mut AppController,
    approval_id: &str,
    approved: bool,
    expected_revision: i64,
) -> AppEvent {
    controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval_id.into(),
            approved,
            expected_revision,
        },
    ))
}

/// 恢复执行：按 plan_hash 复核批准后执行一次并把回执续写回模型。
fn resume(
    fixture: &Fixture,
    tool: &Arc<RecordingTool>,
    job: &AiConversationJob,
    approval_id: &str,
) {
    let provider = ScriptedGenerationProvider::new(caps(), vec![text_step("已按回执完成")]);
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        job,
        &CancelFlag::new(),
        &tx,
        &provider,
        &catalog_for(tool),
        &RequireApprovalAudit,
        permission(),
        approval_id,
    );
    let _ = last_terminal(&rx);
}

// ---------------------------------------------------------------------------
// 1) CAS 冲突：stale revision 必须失败，且状态/执行都不变
// ---------------------------------------------------------------------------

#[test]
fn approval_cas_stale_revision_fails_without_executing() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);

    assert_eq!(tool.call_count(), 0, "暂停阶段不得执行工具");
    let before = call_fact(&fixture.storage, &paused.call_id);
    assert_eq!(before.status, "waiting_approval");
    assert!(before.receipt_json.is_none());

    // 第一次以 revision=1 批准成功 → revision 变 2。
    approve(&mut controller, &paused.approval_id, 1);
    let approved = approval_view(&fixture.storage, &paused.approval_id);
    assert_eq!(approved.status, audits::ApprovalStatus::Approved);
    assert_eq!(approved.revision, 2);

    // stale revision=1 再提交一次：CAS 必须失败。
    let event = decide(&mut controller, &paused.approval_id, true, 1);
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "stale revision 必须被 CAS 拒绝: {event:?}"
    );

    // 审批状态、revision 不变；工具仍未执行。
    let after = approval_view(&fixture.storage, &paused.approval_id);
    assert_eq!(after.status, audits::ApprovalStatus::Approved);
    assert_eq!(after.revision, 2, "失败的 CAS 不得推进 revision");
    let call = call_fact(&fixture.storage, &paused.call_id);
    assert_eq!(
        call.status, "waiting_approval",
        "CAS 失败不得改变工具调用状态"
    );
    assert!(call.receipt_json.is_none());
    assert_eq!(tool.call_count(), 0, "CAS 冲突路径不得执行工具");
}

// ---------------------------------------------------------------------------
// 2) 未批准不得执行：pending 直接 resume 必须 fail closed
// ---------------------------------------------------------------------------

#[test]
fn pending_approval_resume_fails_closed_without_executing() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);

    assert_eq!(
        approval_view(&fixture.storage, &paused.approval_id).status,
        audits::ApprovalStatus::Pending
    );

    let job = controller
        .prepare_ai_resume_job(&paused.approval_id)
        .expect("重建恢复 job");
    resume(&fixture, &tool, &job, &paused.approval_id);

    let call = call_fact(&fixture.storage, &paused.call_id);
    assert!(call.receipt_json.is_none(), "未批准的审批不得产生回执");
    assert_eq!(tool.call_count(), 0, "pending 恢复路径不得执行工具");
    assert_ne!(
        turn_status(&fixture.storage, &paused.job.turn_id),
        sessions::TurnStatus::Completed,
        "未批准时 Turn 不得标记完成"
    );
    assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 0);
}

// ---------------------------------------------------------------------------
// 3) plan_hash 失效：批准与当前计划不匹配时必须拒绝执行
// ---------------------------------------------------------------------------

#[test]
fn approval_plan_hash_drift_blocks_resume() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);
    approve(&mut controller, &paused.approval_id, 1);

    // 模拟参数/目标漂移：当前工具调用的 plan_hash 与批准绑定的不再一致。
    let drifted = if paused.plan_hash == "deadbeefdeadbeef" {
        "feedfacefeedface"
    } else {
        "deadbeefdeadbeef"
    };
    tamper_plan_hash(&fixture.storage, &paused.call_id, drifted);
    let call = call_fact(&fixture.storage, &paused.call_id);
    assert_eq!(call.plan_hash, drifted);
    assert_ne!(
        approval_view(&fixture.storage, &paused.approval_id).plan_hash,
        call.plan_hash,
        "审批绑定与新计划必须不一致"
    );

    let job = controller
        .prepare_ai_resume_job(&paused.approval_id)
        .expect("重建恢复 job");
    resume(&fixture, &tool, &job, &paused.approval_id);

    let after = call_fact(&fixture.storage, &paused.call_id);
    assert!(after.receipt_json.is_none(), "plan_hash 漂移后不得产生回执");
    assert_eq!(tool.call_count(), 0, "plan_hash 漂移必须 fail closed");
    assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 0);
}

#[test]
fn approved_call_payload_changes_block_resume_without_hash_tampering() {
    for changed_field in ["sql", "parameters", "target"] {
        let fixture = Fixture::new();
        let mut controller = fixture.configured();
        let tool = read_tool("db.query");
        let paused = pause_round(&fixture, &mut controller, &tool);
        approve(&mut controller, &paused.approval_id, 1);

        // 保持已批准 hash 不变，只改执行载荷，验证执行前真正重算计划而非只比较旧 hash。
        fixture
            .storage
            .with_ai_connection(|conn| {
                let call = sessions::load_tool_call(conn, &paused.call_id)?;
                if changed_field == "target" {
                    let mut target: serde_json::Value =
                        serde_json::from_str(&call.target_json).unwrap();
                    target["schema"] = "another_schema".into();
                    conn.execute(
                        "UPDATE ai_tool_calls SET target_json=?2 WHERE id=?1",
                        params![call.id, target.to_string()],
                    )
                    .map_err(sql_error)?;
                } else {
                    let mut arguments: serde_json::Value =
                        serde_json::from_str(&call.arguments_redacted_json).unwrap();
                    arguments[changed_field] = if changed_field == "sql" {
                        "SELECT 2".into()
                    } else {
                        serde_json::json!([{"type": "i64", "value": "42"}])
                    };
                    conn.execute(
                        "UPDATE ai_tool_calls SET arguments_redacted_json=?2 WHERE id=?1",
                        params![call.id, arguments.to_string()],
                    )
                    .map_err(sql_error)?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            call_fact(&fixture.storage, &paused.call_id).plan_hash,
            paused.plan_hash
        );
        assert_eq!(
            approval_view(&fixture.storage, &paused.approval_id).plan_hash,
            paused.plan_hash
        );

        let job = controller
            .prepare_ai_resume_job(&paused.approval_id)
            .expect("重建恢复 job");
        resume(&fixture, &tool, &job, &paused.approval_id);

        assert_eq!(tool.call_count(), 0, "{changed_field} 改变后不能执行");
        assert!(
            call_fact(&fixture.storage, &paused.call_id)
                .receipt_json
                .is_none(),
            "{changed_field} 改变后不能产生回执"
        );
        assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 0);
        assert_eq!(result_ref_parts(&fixture.storage, &paused.job.turn_id), 0);
        assert_ne!(
            turn_status(&fixture.storage, &paused.job.turn_id),
            sessions::TurnStatus::Completed
        );
    }
}

// ---------------------------------------------------------------------------
// 4) 撤销后不得执行
// ---------------------------------------------------------------------------

#[test]
fn invalidated_approval_blocks_resume() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);
    approve(&mut controller, &paused.approval_id, 1);
    assert_eq!(
        approval_view(&fixture.storage, &paused.approval_id).status,
        audits::ApprovalStatus::Approved
    );

    fixture
        .storage
        .with_ai_connection(|conn| {
            audits::invalidate_approval(conn, 9_999_999, &paused.approval_id, "config_changed")?;
            Ok(())
        })
        .expect("撤销审批");
    assert_eq!(
        approval_view(&fixture.storage, &paused.approval_id).status,
        audits::ApprovalStatus::Invalidated
    );

    let job = controller
        .prepare_ai_resume_job(&paused.approval_id)
        .expect("重建恢复 job");
    resume(&fixture, &tool, &job, &paused.approval_id);

    let call = call_fact(&fixture.storage, &paused.call_id);
    assert!(call.receipt_json.is_none(), "撤销后不得产生回执");
    assert_eq!(
        tool.call_count(),
        0,
        "撤销后的批准必须不可用（fail closed）"
    );
}

// ---------------------------------------------------------------------------
// 5) 拒绝路径：denied + 无回执 + 无结果，之后也不能再批准执行
// ---------------------------------------------------------------------------

#[test]
fn denied_approval_never_executes_and_cannot_be_reopened() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);

    decide(&mut controller, &paused.approval_id, false, 1);
    assert_eq!(
        approval_view(&fixture.storage, &paused.approval_id).status,
        audits::ApprovalStatus::Denied
    );

    let call = call_fact(&fixture.storage, &paused.call_id);
    assert_eq!(call.status, "denied", "拒绝后工具调用必须标记 denied");
    assert!(call.receipt_json.is_none(), "拒绝不得产生任何回执");
    assert_eq!(tool.call_count(), 0, "拒绝路径不得执行工具");
    assert_eq!(
        result_rows(&fixture.storage, &paused.job.turn_id),
        0,
        "拒绝不得写结果行"
    );
    assert_eq!(
        result_ref_parts(&fixture.storage, &paused.job.turn_id),
        0,
        "拒绝不得写结果卡"
    );

    // 已拒绝的审批不能再批准（状态机阻断，revision 已推进到 2）。
    let reopen = decide(&mut controller, &paused.approval_id, true, 2);
    assert!(
        matches!(reopen, AppEvent::AiSettingsError(_)),
        "已拒绝的审批不得重新批准: {reopen:?}"
    );
    assert_eq!(
        approval_view(&fixture.storage, &paused.approval_id).status,
        audits::ApprovalStatus::Denied
    );

    // 即便强行 resume，也不得执行。
    let job = controller
        .prepare_ai_resume_job(&paused.approval_id)
        .expect("重建恢复 job");
    resume(&fixture, &tool, &job, &paused.approval_id);
    assert_eq!(tool.call_count(), 0, "拒绝后再 resume 仍不得执行");
    assert!(
        call_fact(&fixture.storage, &paused.call_id)
            .receipt_json
            .is_none()
    );
    assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 0);
}

// ---------------------------------------------------------------------------
// 6) 批准后只执行一次：重复 resume 不得再执行一次
// ---------------------------------------------------------------------------

#[test]
fn approved_resume_executes_exactly_once_even_when_resumed_again() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);
    approve(&mut controller, &paused.approval_id, 1);

    let job = controller
        .prepare_ai_resume_job(&paused.approval_id)
        .expect("重建恢复 job");

    resume(&fixture, &tool, &job, &paused.approval_id);

    let call = call_fact(&fixture.storage, &paused.call_id);
    assert_eq!(
        call.status, "succeeded",
        "批准后必须真实执行一次并写 succeeded"
    );
    assert!(call.receipt_json.is_some(), "必须写回真实回执");
    assert_eq!(tool.call_count(), 1, "批准恢复恰好执行一次");
    assert_eq!(
        call_rows(&fixture.storage, &paused.job.turn_id),
        1,
        "工具调用事实行保持 1 行"
    );
    assert_eq!(
        result_rows(&fixture.storage, &paused.job.turn_id),
        1,
        "结果行恰好 1 行"
    );
    assert_eq!(
        result_ref_parts(&fixture.storage, &paused.job.turn_id),
        1,
        "结果卡恰好 1 个"
    );
    assert_eq!(
        turn_status(&fixture.storage, &paused.job.turn_id),
        sessions::TurnStatus::Completed
    );

    // 第二次 resume：批准已消费/轮次已完结，绝不能再执行一次。
    let job2 = controller.prepare_ai_resume_job(&paused.approval_id);
    match job2 {
        Ok(job2) => resume(&fixture, &tool, &job2, &paused.approval_id),
        // 若恢复入口在重建阶段就拒绝（视为已消费），同样满足“不得再执行”。
        Err(_) => {}
    }
    // 重复 resume 的副作用证据：事实行数保持不变（insert_result 因主键冲突被吞），
    // 但工具真实执行计数会 +1 —— 即“重复执行被静默吞掉、账面上看不出来”。
    assert_eq!(
        call_rows(&fixture.storage, &paused.job.turn_id),
        1,
        "重复 resume 不新增工具调用行"
    );
    assert_eq!(
        result_rows(&fixture.storage, &paused.job.turn_id),
        1,
        "重复 resume 不新增结果行"
    );
    assert_eq!(tool.call_count(), 1, "重复 resume 不得再次执行工具");
}

struct AllowAudit;
impl SecurityAuditHook for AllowAudit {
    fn audit(&self, _: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::Allow)
    }
}

#[test]
fn approval_resume_records_follow_up_calls_without_ordinal_collisions() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);
    approve(&mut controller, &paused.approval_id, 1);
    let step = |id: &str| {
        vec![
            ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: id.into(),
                tool_id: "db.query".into(),
                arguments_json: r#"{"sql":"SELECT 1"}"#.into(),
            }),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    };
    let provider = ScriptedGenerationProvider::new(
        caps(),
        vec![
            step("follow-up-1"),
            step("follow-up-2"),
            text_step("verified"),
        ],
    );
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &paused.job,
        &CancelFlag::new(),
        &tx,
        &provider,
        &catalog_for(&tool),
        &AllowAudit,
        permission(),
        &paused.approval_id,
    );
    let view = last_terminal(&rx);
    assert!(
        view.error.is_none(),
        "resume persistence error: {:?}",
        view.error
    );
    assert_eq!(
        turn_status(&fixture.storage, &paused.job.turn_id),
        sessions::TurnStatus::Completed
    );
    assert_eq!(
        tool.call_count(),
        3,
        "approved call and two follow-up calls execute once each"
    );
    assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 3);
    let calls = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_tool_calls_for_turn(conn, &paused.job.turn_id))
        .unwrap();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|call| call.status == "succeeded" && call.receipt_json.is_some())
    );
}

#[test]
fn approval_resume_can_pause_for_a_second_approval_then_complete() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = read_tool("db.query");
    let paused = pause_round(&fixture, &mut controller, &tool);
    approve(&mut controller, &paused.approval_id, 1);
    let mut step = tool_step("db.query");
    if let ModelEvent::ToolCallRequested(call) = &mut step[0] {
        call.provider_call_id = "second-approval".into();
    }
    let provider = ScriptedGenerationProvider::new(caps(), vec![step]);
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &paused.job,
        &CancelFlag::new(),
        &tx,
        &provider,
        &catalog_for(&tool),
        &RequireApprovalAudit,
        permission(),
        &paused.approval_id,
    );
    let view = last_terminal(&rx);
    assert!(
        view.error.is_none(),
        "second approval persistence error: {:?}",
        view.error
    );
    assert_eq!(
        turn_status(&fixture.storage, &paused.job.turn_id),
        sessions::TurnStatus::WaitingApproval
    );
    assert_eq!(tool.call_count(), 1);
    let approval = fixture
        .storage
        .with_ai_connection(|conn| {
            let calls = sessions::list_tool_calls_for_turn(conn, &paused.job.turn_id)?;
            assert_eq!(calls.len(), 2);
            let call = calls
                .iter()
                .find(|call| call.provider_call_id == "second-approval")
                .unwrap();
            assert_eq!(call.status, "waiting_approval");
            Ok(audits::list_approvals_for_call(conn, &call.id)?.remove(0))
        })
        .unwrap();
    approve(&mut controller, &approval.id, approval.revision);
    resume(&fixture, &tool, &paused.job, &approval.id);
    assert_eq!(tool.call_count(), 2);
    assert_eq!(result_rows(&fixture.storage, &paused.job.turn_id), 2);
    assert_eq!(
        turn_status(&fixture.storage, &paused.job.turn_id),
        sessions::TurnStatus::Completed
    );
}
