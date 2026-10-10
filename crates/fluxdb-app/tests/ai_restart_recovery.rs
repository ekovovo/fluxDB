//! T029：独立文件 SQLite 的崩溃窗口、启动恢复与防重放回归。
//! 经生产 prepare/checkpoint/tool/approval API 写入；只用故障 UPDATE / 临时触发器注入异常。
//! 不访问用户配置、凭据或网络；AppState 投影不等同于 GPUI / 真实窗口验收。

use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiConversationView,
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
    ai_platform::{audits, conversation, recovery, sessions},
};
use rusqlite::{Connection, types::Value};
use sessions::{PartKind, PartStatus, TurnStatus};

const NOW: i64 = 10_000;
const RECEIPT: &str = r#"{"status":"succeeded","rows_affected":1}"#;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-ai-restart-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("创建独立 fixture");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化真实 AI schema");
        Self { dir, storage }
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> fluxdb_core::Result<T>) -> T {
        self.storage
            .with_ai_connection(f)
            .expect("独立 SQLite 事实操作")
    }

    fn prepared(&self) -> (AppController, AiConversationJob) {
        let mut app = AppController::new();
        app.set_ai_storage(FileStorage::new(&self.dir));
        let model = match app.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T029 离线模型".into(),
            provider: "openai".into(),
            model: "restart-fixture-offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("保存隔离模型失败: {other:?}"),
        };
        assert!(matches!(
            app.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                display_name: "T029 fixture Agent".into(),
                description: "不连接模型或数据库".into(),
                system_prompt: "T029 独立恢复测试".into(),
                model_profile_id: Some(model),
                enabled: true,
                ..Default::default()
            })),
            AppEvent::AiAgentSaved { .. }
        ));
        let target = TargetRef {
            connection_id: Some(1),
            database: Some("main".into()),
            schema: None,
            object: Some("restart_fixture".into()),
            redis_db: None,
        };
        let pin = TurnTargetPin {
            target_json: target.to_json(),
            target,
            captured_tab_id: Some(1),
            captured_tab_revision: 1,
        };
        let job = app
            .prepare_ai_conversation("T029-restart-marker", &pin)
            .expect("生产发送入口");
        (app, job)
    }

    fn recover(&self, now: i64) -> recovery::RecoverySummary {
        let mut conn = self.storage.open_ai_connection().expect("重开独立 SQLite");
        recovery::recover_on_startup(&mut conn, now).expect("启动恢复扫描")
    }

    fn snapshot(&self) -> Facts {
        self.with(facts)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn sql_error(error: rusqlite::Error) -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string())
}

// 逐列比较真实事实行，包含 revision、时间戳、消息 seq、parts ordinal、UI 草稿/锚点。
// 不读取模型加密列；不复制生产投影逻辑。
type Facts = Vec<(String, Vec<Vec<Value>>)>;
fn facts(conn: &Connection) -> fluxdb_core::Result<Facts> {
    [
        ("ai_sessions", "id"),
        ("ai_turns", "id"),
        ("ai_messages", "id"),
        ("ai_message_parts", "id"),
        ("ai_tool_calls", "id"),
        ("ai_approvals", "id"),
        ("ai_results", "id"),
        ("ai_session_ui_state", "session_id"),
        ("ai_run_events", "turn_id,seq"),
        ("ai_panel_preferences", "id"),
    ]
    .into_iter()
    .map(|(table, order)| {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .map_err(sql_error)?;
        let columns = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..columns)
                    .map(|i| r.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        Ok((table.into(), rows))
    })
    .collect()
}

fn checkpoint(
    conn: &Connection,
    job: &AiConversationJob,
    text: &str,
    summary: &str,
    status: Option<TurnStatus>,
) -> fluxdb_core::Result<()> {
    conversation::checkpoint(
        conn,
        NOW - 100,
        &job.session_id,
        &job.turn_id,
        &job.assistant_message_id,
        text,
        summary,
        status,
        None,
    )
}

fn set_turn(
    conn: &Connection,
    job: &AiConversationJob,
    status: TurnStatus,
) -> fluxdb_core::Result<()> {
    let turn = sessions::load_turn(conn, &job.turn_id)?;
    sessions::update_turn_status(conn, NOW - 50, &job.turn_id, status, None, turn.revision)?;
    Ok(())
}

fn call(
    conn: &Connection,
    job: &AiConversationJob,
    id: &str,
    ordinal: i64,
    tool: &str,
    status: &str,
) -> fluxdb_core::Result<()> {
    let target = sessions::load_turn(conn, &job.turn_id)?.target_json;
    if status == "running" {
        sessions::start_tool_call(
            conn,
            NOW - 100,
            id,
            &job.session_id,
            &job.turn_id,
            1,
            ordinal,
            id,
            tool,
            1,
            None,
            r#"{"sql":"SELECT 1"}"#,
            &target,
            "restart-plan",
        )
    } else {
        sessions::create_tool_call(
            conn,
            NOW - 100,
            id,
            &job.session_id,
            &job.turn_id,
            1,
            ordinal,
            id,
            tool,
            1,
            None,
            r#"{"sql":"SELECT 1"}"#,
            &target,
            "restart-plan",
            status,
        )
    }
}

fn approval_card(
    conn: &Connection,
    job: &AiConversationJob,
    approved: bool,
    expires: Option<i64>,
) -> fluxdb_core::Result<audits::ApprovalView> {
    call(
        conn,
        job,
        "approval-call",
        0,
        "db.execute",
        "waiting_approval",
    )?;
    let approval = audits::create_approval(
        conn,
        NOW - 100,
        "approval",
        "approval-call",
        "restart-plan",
        "{}",
        expires,
    )?;
    let approval = if approved {
        audits::decide_approval(conn, NOW - 50, &approval.id, true, None, approval.revision)?
    } else {
        approval
    };
    let mut payload = serde_json::json!({"text":"T029 approval marker", "approval_id":approval.id,
        "approval_revision":approval.revision, "plan_hash":approval.plan_hash});
    if approved {
        payload["decision"] = serde_json::json!("approved");
    }
    conversation::upsert_event_part(
        conn,
        NOW - 25,
        &job.session_id,
        &job.turn_id,
        &job.assistant_message_id,
        "approval-card",
        PartKind::Notice,
        if approved {
            PartStatus::Complete
        } else {
            PartStatus::Pending
        },
        &payload.to_string(),
        Some("approval-call"),
        None,
    )?;
    Ok(approval)
}

fn saved_ui(
    conn: &Connection,
    job: &AiConversationJob,
) -> fluxdb_core::Result<sessions::SessionUiStateView> {
    let ui = sessions::load_ui_state(conn, &job.session_id)?;
    sessions::save_ui_state(
        conn,
        NOW - 20,
        &sessions::SessionUiStateEdit {
            session_id: job.session_id.clone(),
            selected_turn_id: Some(job.turn_id.clone()),
            expanded_part_ids_json: serde_json::json!([format!(
                "{}-summary",
                job.assistant_message_id
            )])
            .to_string(),
            scroll_anchor_message_id: Some(job.assistant_message_id.clone()),
            ui_options_json: r#"{"follow_tail":false}"#.into(),
            draft_json: Some(r#"{"text":"T029 preserved draft"}"#.into()),
        },
        ui.revision,
    )?;
    sessions::load_ui_state(conn, &job.session_id)
}

fn dispatch_view(app: &mut AppController, command: AiConversationCommand) -> AiConversationView {
    match app.dispatch(AppCommand::AiConversation(command)) {
        AppEvent::AiConversationLoaded(view) => {
            assert_eq!(
                &app.state().ai_conversation,
                &view,
                "AppEvent 必须同步真实 AppState"
            );
            view
        }
        other => panic!("生产会话命令失败: {other:?}"),
    }
}

fn startup_then_load(app: &mut AppController, job: &AiConversationJob) -> AiConversationView {
    let before = app.state().ai_conversation.clone();
    let scanned = dispatch_view(app, AiConversationCommand::RecoverOnStartup);
    assert_eq!(scanned, before, "启动只扫描事实，面板未打开不自动装载历史");
    dispatch_view(
        app,
        AiConversationCommand::Load {
            session_id: Some(job.session_id.clone()),
        },
    )
}

#[test]
fn crash_windows_prepared_running_and_receipted_calls() {
    for (tool, initial, receipt, expected) in [
        ("db.query", "prepared", None, "cancelled"),
        ("db.execute", "prepared", None, "cancelled"),
        ("plugin.unknown", "prepared", None, "cancelled"),
        ("db.query", "running", None, "cancelled"),
        ("db.explain", "running", None, "cancelled"),
        ("db.search_objects", "running", None, "cancelled"),
        ("db.execute", "running", None, "unknown"),
        ("plugin.unknown", "running", None, "unknown"),
        (
            "db.query",
            "running",
            Some(r#"{"partial":true}"#),
            "cancelled",
        ),
        (
            "db.execute",
            "running",
            Some(r#"{"partial":true}"#),
            "unknown",
        ),
        ("db.execute", "succeeded", Some(RECEIPT), "succeeded"),
        (
            "db.execute",
            "failed",
            Some(r#"{"status":"failed"}"#),
            "failed",
        ),
        ("db.execute", "cancelled", None, "cancelled"),
        ("db.execute", "denied", None, "denied"),
        ("plugin.unknown", "unknown", None, "unknown"),
        ("plugin.unknown", "succeeded", Some(RECEIPT), "succeeded"),
    ] {
        let f = Fixture::new();
        let (_, job) = f.prepared();
        let original = f.with(|conn| {
            checkpoint(
                conn,
                &job,
                "T029 retained body",
                "T029 retained summary",
                None,
            )?;
            set_turn(conn, &job, TurnStatus::ExecutingTools)?;
            call(conn, &job, "crash-call", 0, tool, initial)?;
            if let Some(receipt) = receipt {
                sessions::finish_tool_call(
                    conn,
                    "crash-call",
                    initial,
                    Some(receipt),
                    None,
                    Some(NOW - 10),
                )?;
            }
            sessions::load_turn(conn, &job.turn_id)
        });
        let summary = f.recover(NOW);
        f.with(|conn| {
            let actual = sessions::load_tool_call(conn, "crash-call")?;
            assert_eq!(actual.status, expected, "{tool}/{initial}/{receipt:?}");
            assert_eq!(actual.receipt_json.as_deref(), receipt, "保留现有回执");
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            assert_eq!(
                turn.status,
                if expected == "unknown" {
                    TurnStatus::OutcomeUnknown
                } else {
                    TurnStatus::Interrupted
                }
            );
            assert_eq!(turn.revision, original.revision + 1);
            assert_eq!(turn.target_json, original.target_json);
            assert_eq!(turn.runtime_snapshot_json, original.runtime_snapshot_json);
            for suffix in ["text", "summary"] {
                let part =
                    sessions::load_part(conn, &format!("{}-{suffix}", job.assistant_message_id))?;
                assert_eq!(part.status, PartStatus::Interrupted);
                assert!(part.payload_json.contains("T029 retained"));
            }
            assert!(sessions::list_tool_calls_for_turn(conn, &job.turn_id)?.len() == 1);
            Ok(())
        });
        assert_eq!(summary.unknown_turns, usize::from(expected == "unknown"));
        let before = f.snapshot();
        f.recover(NOW + 1);
        assert_eq!(f.snapshot(), before, "同一崩溃 fixture 重复扫描必须幂等");
    }
}

#[test]
fn every_scannable_turn_state_without_tools_recovers_without_replay() {
    for initial in [
        TurnStatus::Queued,
        TurnStatus::Preparing,
        TurnStatus::Generating,
        TurnStatus::ExecutingTools,
        TurnStatus::CancelRequested,
        TurnStatus::WaitingApproval,
        TurnStatus::WaitingUser,
        TurnStatus::Interrupted,
        TurnStatus::OutcomeUnknown,
    ] {
        let f = Fixture::new();
        let (_, job) = f.prepared();
        f.with(|conn| {
            checkpoint(conn, &job, "saved body", "saved summary", None)?;
            set_turn(conn, &job, initial)
        });
        f.recover(NOW);
        f.with(|conn| {
            let expected = match initial {
                TurnStatus::WaitingUser => TurnStatus::WaitingUser,
                TurnStatus::OutcomeUnknown => TurnStatus::OutcomeUnknown,
                _ => TurnStatus::Interrupted,
            };
            assert_eq!(
                sessions::load_turn(conn, &job.turn_id)?.status,
                expected,
                "{initial:?}"
            );
            assert!(sessions::list_tool_calls_for_turn(conn, &job.turn_id)?.is_empty());
            assert!(
                conversation::load(conn, Some(&job.session_id))?
                    .parts
                    .iter()
                    .filter(|p| matches!(p.kind, PartKind::Text | PartKind::ReasoningSummary))
                    .all(
                        |p| p.status == PartStatus::Complete || p.status == PartStatus::Interrupted
                    )
            );
            Ok(())
        });
        let before = f.snapshot();
        f.recover(NOW + 1);
        assert_eq!(f.snapshot(), before, "{initial:?} 二次扫描");
    }
}

#[test]
fn generating_with_empty_or_absent_assistant_gets_visible_restart_notice() {
    for empty_checkpoint in [false, true] {
        let f = Fixture::new();
        let (_, job) = f.prepared();
        f.with(|conn| {
            if empty_checkpoint {
                checkpoint(conn, &job, "", "", None)?;
            }
            set_turn(conn, &job, TurnStatus::Generating)
        });
        // 模拟全新宿主，仅绑定独立存储；不读取模型配置、不调用 Provider。
        let mut reopened = AppController::new();
        reopened.set_ai_storage(FileStorage::new(&f.dir));
        let view = startup_then_load(&mut reopened, &job);
        assert_eq!(view.session_id.as_deref(), Some(job.session_id.as_str()));
        assert!(view.active_turn_id.is_none());
        let assistant = view
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistant.len(), 1);
        assert_eq!(assistant[0].parts.len(), 1);
        assert_eq!(assistant[0].parts[0].kind, "notice");
        assert_eq!(assistant[0].parts[0].status, "complete");
        let payload: serde_json::Value =
            serde_json::from_str(&assistant[0].parts[0].payload_json).unwrap();
        assert_eq!(payload["recovery_status"], "interrupted");
        assert!(!payload["text"].as_str().unwrap().trim().is_empty());
        f.with(|conn| {
            let data = conversation::load(conn, Some(&job.session_id))?;
            assert_eq!(data.messages.len(), 2);
            assert_eq!(data.turns[0].status, TurnStatus::Interrupted);
            assert_eq!(
                data.parts
                    .iter()
                    .filter(|p| p.kind == PartKind::Notice)
                    .count(),
                1
            );
            assert!(sessions::list_tool_calls_for_turn(conn, &job.turn_id)?.is_empty());
            Ok(())
        });
        let before = f.snapshot();
        let again = dispatch_view(&mut reopened, AiConversationCommand::RecoverOnStartup);
        assert_eq!(again, view);
        assert_eq!(f.snapshot(), before);
    }
}

#[test]
fn pending_approved_and_expired_approvals_keep_fact_and_card_in_sync() {
    for (approved, expires) in [
        (false, None),
        (true, None),
        (false, Some(NOW)),
        (true, Some(NOW - 1)),
    ] {
        let f = Fixture::new();
        let (_, job) = f.prepared();
        let before = f.with(|conn| {
            checkpoint(
                conn,
                &job,
                "approval body",
                "",
                Some(TurnStatus::WaitingApproval),
            )?;
            approval_card(conn, &job, approved, expires)
        });
        let summary = f.recover(NOW);
        let expired = expires.is_some();
        f.with(|conn| {
            let actual = audits::load_approval(conn, "approval")?;
            assert_eq!(
                actual.status,
                if expired {
                    audits::ApprovalStatus::Expired
                } else {
                    audits::ApprovalStatus::Pending
                }
            );
            assert_eq!(
                actual.revision,
                before.revision + i64::from(approved || expired)
            );
            assert_eq!(actual.decided_at_ms, if expired { Some(NOW) } else { None });
            assert_eq!(
                actual.reason_code.as_deref(),
                if expired {
                    Some("restart_expired")
                } else if approved {
                    Some("restart_reapproval")
                } else {
                    None
                }
            );
            let part = sessions::load_part(conn, "approval-card")?;
            let payload: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
            assert_eq!(payload["approval_id"], actual.id);
            assert_eq!(payload["approval_revision"], actual.revision);
            assert_eq!(
                part.status,
                if expired {
                    PartStatus::Interrupted
                } else {
                    PartStatus::Pending
                }
            );
            if expired {
                assert_eq!(payload["decision"], "expired");
            } else {
                assert!(payload.get("decision").is_none());
            }
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            assert_eq!(
                turn.status,
                if expired {
                    TurnStatus::Interrupted
                } else {
                    TurnStatus::WaitingApproval
                }
            );
            let call = sessions::load_tool_call(conn, "approval-call")?;
            assert_eq!(call.status, "waiting_approval");
            assert!(call.receipt_json.is_none());
            Ok(())
        });
        assert_eq!(summary.waiting_approval_turns, usize::from(!expired));
        let recovered = f.snapshot();
        f.recover(NOW + 1);
        assert_eq!(
            f.snapshot(),
            recovered,
            "审批/卡片/消息 revision 不应重复增长"
        );
    }
}

#[test]
fn completed_failed_cancelled_turns_and_terminal_calls_remain_unchanged() {
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Cancelled,
    ] {
        let f = Fixture::new();
        let (_, job) = f.prepared();
        f.with(|conn| {
            checkpoint(conn, &job, "terminal body", "terminal summary", None)?;
            saved_ui(conn, &job)?;
            for (ordinal, call_status) in ["succeeded", "failed", "cancelled", "denied"]
                .into_iter()
                .enumerate()
            {
                let id = format!("terminal-{call_status}");
                call(conn, &job, &id, ordinal as i64, "db.execute", call_status)?;
                sessions::finish_tool_call(
                    conn,
                    &id,
                    call_status,
                    Some(RECEIPT),
                    None,
                    Some(NOW - 1),
                )?;
            }
            checkpoint(
                conn,
                &job,
                "terminal body",
                "terminal summary",
                Some(status),
            )
        });
        let before = f.snapshot();
        let summary = f.recover(NOW);
        assert_eq!(summary, recovery::RecoverySummary::default());
        assert_eq!(
            f.snapshot(),
            before,
            "终态 {status:?} 包括正文/草稿/展开/锚点完全不变"
        );
    }
}

#[test]
fn startup_app_command_preserves_checkpoint_draft_expansion_and_scroll_anchor() {
    let f = Fixture::new();
    let (_, job) = f.prepared();
    let (ui, text, summary) = f.with(|conn| {
        checkpoint(conn, &job, "T029 body marker", "T029 summary marker", None)?;
        set_turn(conn, &job, TurnStatus::Generating)?;
        let ui = saved_ui(conn, &job)?;
        Ok((
            ui,
            sessions::load_part(conn, &format!("{}-text", job.assistant_message_id))?,
            sessions::load_part(conn, &format!("{}-summary", job.assistant_message_id))?,
        ))
    });
    let mut reopened = AppController::new();
    reopened.set_ai_storage(FileStorage::new(&f.dir));
    let view = startup_then_load(&mut reopened, &job);
    assert_eq!(view.draft, "T029 preserved draft");
    assert_eq!(view.expanded_parts, vec![summary.id.clone()]);
    assert_eq!(view.messages.len(), 2);
    let assistant = view
        .messages
        .iter()
        .find(|m| m.id == job.assistant_message_id)
        .unwrap();
    for part in [&text, &summary] {
        let projected = assistant.parts.iter().find(|p| p.id == part.id).unwrap();
        assert_eq!(projected.payload_json, part.payload_json);
        assert_eq!(projected.status, "interrupted");
    }
    f.with(|conn| {
        assert_eq!(sessions::load_ui_state(conn, &job.session_id)?, ui);
        assert_eq!(
            sessions::load_turn(conn, &job.turn_id)?.status,
            TurnStatus::Interrupted
        );
        Ok(())
    });
    let before = f.snapshot();
    let reloaded = dispatch_view(
        &mut reopened,
        AiConversationCommand::Load {
            session_id: Some(job.session_id.clone()),
        },
    );
    assert_eq!(reloaded.messages, view.messages);
    assert_eq!(reloaded.draft, view.draft);
    // Load 更新面板偏好，故只对恢复扫描本身做全表幂等断言。
    let after_load = f.snapshot();
    dispatch_view(&mut reopened, AiConversationCommand::RecoverOnStartup);
    assert_eq!(f.snapshot(), after_load);
    assert_eq!(
        before.iter().find(|(t, _)| t == "ai_session_ui_state"),
        after_load.iter().find(|(t, _)| t == "ai_session_ui_state")
    );
}

#[test]
fn restart_expires_only_available_memory_results_and_keeps_retained_handles() {
    let f = Fixture::new();
    let (_, job) = f.prepared();
    f.with(|conn| {
        checkpoint(conn, &job, "result body", "", None)?;
        call(conn, &job, "result-call", 0, "db.query", "succeeded")?;
        sessions::finish_tool_call(
            conn,
            "result-call",
            "succeeded",
            Some(RECEIPT),
            None,
            Some(NOW - 1),
        )?;
        for (id, status) in [
            ("memory", "available"),
            ("expired", "expired"),
            ("deleted", "deleted"),
            ("failed", "failed"),
        ] {
            sessions::insert_result(
                conn,
                NOW - 50,
                id,
                &job.session_id,
                &job.turn_id,
                "result-call",
                r#"{"columns":["n"],"rows":1}"#,
                status,
                16,
                &sessions::ResultRetention::Memory,
            )?;
        }
        sessions::insert_result(
            conn,
            NOW - 50,
            "retained",
            &job.session_id,
            &job.turn_id,
            "result-call",
            r#"{"columns":["n"],"rows":1}"#,
            "available",
            16,
            &sessions::ResultRetention::Retained {
                relative_path: "results/t027-retained.json".into(),
                byte_count: 16,
                content_hash: "sha256:t027-retained".into(),
            },
        )?;
        conversation::upsert_event_part(
            conn,
            NOW - 25,
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            "result-card",
            PartKind::ToolResultRef,
            PartStatus::Complete,
            r#"{"result_id":"memory"}"#,
            Some("result-call"),
            Some("memory"),
        )?;
        checkpoint(conn, &job, "result body", "", Some(TurnStatus::Completed))
    });
    let before = f.snapshot();
    assert_eq!(f.recover(NOW).expired_results, 1);
    f.with(|conn| {
        for (id, expected) in [
            ("memory", "expired"),
            ("retained", "available"),
            ("expired", "expired"),
            ("deleted", "deleted"),
            ("failed", "failed"),
        ] {
            let status: String = conn
                .query_row("SELECT status FROM ai_results WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .map_err(sql_error)?;
            assert_eq!(status, expected, "结果 {id}");
        }
        assert_eq!(
            sessions::load_tool_call(conn, "result-call")?
                .receipt_json
                .as_deref(),
            Some(RECEIPT)
        );
        assert_eq!(
            sessions::load_part(conn, "result-card")?
                .result_id
                .as_deref(),
            Some("memory")
        );
        Ok(())
    });
    let after = f.snapshot();
    for ((table, old), (_, new)) in before.iter().zip(&after) {
        if table != "ai_results" {
            assert_eq!(old, new, "结果过期不改其他事实表 {table}");
        }
    }
    let old_results = &before
        .iter()
        .find(|(table, _)| table == "ai_results")
        .unwrap()
        .1;
    let new_results = &after
        .iter()
        .find(|(table, _)| table == "ai_results")
        .unwrap()
        .1;
    for row in old_results
        .iter()
        .filter(|row| row[0] != Value::Text("memory".into()))
    {
        assert!(
            new_results.contains(row),
            "retained/已过期/已删除/失败结果整行不变"
        );
    }
    assert_eq!(f.recover(NOW + 1).expired_results, 0);
    assert_eq!(f.snapshot(), after);
}

/// 保留结果的落盘事实可核对：相对路径/字节数/哈希入库，越界路径被拒绝；
/// 带 TTL 的保留结果到期才过期，未到期或无 TTL 的在重启后仍可用。
#[test]
fn retained_results_keep_verifiable_spill_facts_and_expire_only_by_ttl() {
    let f = Fixture::new();
    let (_, job) = f.prepared();
    f.with(|conn| {
        checkpoint(conn, &job, "retained body", "", None)?;
        call(conn, &job, "result-call", 0, "db.query", "succeeded")?;
        sessions::finish_tool_call(
            conn,
            "result-call",
            "succeeded",
            Some(RECEIPT),
            None,
            Some(NOW - 1),
        )?;
        sessions::insert_result(
            conn,
            NOW - 50,
            "kept",
            &job.session_id,
            &job.turn_id,
            "result-call",
            r#"{"columns":["n"],"rows":1}"#,
            "available",
            16,
            &sessions::ResultRetention::Retained {
                relative_path: "results/kept.json".into(),
                byte_count: 16,
                content_hash: "sha256:kept".into(),
            },
        )?;
        sessions::insert_result(
            conn,
            NOW - 50,
            "ttl",
            &job.session_id,
            &job.turn_id,
            "result-call",
            r#"{"columns":["n"],"rows":1}"#,
            "available",
            16,
            &sessions::ResultRetention::Retained {
                relative_path: "results/ttl.json".into(),
                byte_count: 16,
                content_hash: "sha256:ttl".into(),
            },
        )?;
        conn.execute(
            "UPDATE ai_results SET expires_at_ms=?1 WHERE id='ttl'",
            rusqlite::params![NOW + 1_000],
        )
        .map_err(sql_error)?;
        // 受控根之外的路径必须被拒绝：否则后续按 relative_path 回读会变成任意文件访问。
        for bad in ["/etc/passwd", "../escape.json", "results/../../escape.json"] {
            assert!(
                sessions::insert_result(
                    conn,
                    NOW - 50,
                    "bad",
                    &job.session_id,
                    &job.turn_id,
                    "result-call",
                    r#"{"columns":["n"],"rows":1}"#,
                    "available",
                    16,
                    &sessions::ResultRetention::Retained {
                        relative_path: bad.into(),
                        byte_count: 16,
                        content_hash: "sha256:bad".into(),
                    },
                )
                .is_err(),
                "越界路径必须拒绝: {bad}"
            );
        }
        checkpoint(conn, &job, "retained body", "", Some(TurnStatus::Completed))
    });
    assert_eq!(
        f.recover(NOW).expired_results,
        0,
        "未到期的保留结果不该过期"
    );
    f.with(|conn| {
        let rows: Vec<(String, String, String, i64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id,status,relative_path,byte_count,content_hash FROM ai_results ORDER BY id",
                )
                .map_err(sql_error)?;
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_error)?
        };
        assert_eq!(
            rows,
            vec![
                (
                    "kept".into(),
                    "available".into(),
                    "results/kept.json".into(),
                    16,
                    "sha256:kept".into()
                ),
                (
                    "ttl".into(),
                    "available".into(),
                    "results/ttl.json".into(),
                    16,
                    "sha256:ttl".into()
                ),
            ]
        );
        Ok(())
    });
    // 到期的保留结果：状态转 expired，且受控根内的 spill 文件要被删掉（不留孤儿文件）。
    let spill_dir = f.dir.join("ai-results").join("results");
    fs::create_dir_all(&spill_dir).unwrap();
    let kept_file = spill_dir.join("kept.json");
    let ttl_file = spill_dir.join("ttl.json");
    fs::write(&kept_file, b"[{}]").unwrap();
    fs::write(&ttl_file, b"[{}]").unwrap();
    // 走生产启动恢复命令：SQLite 事务改状态，文件删除在其后。
    let (mut app, _) = (f.prepared().0, ());
    dispatch_view(&mut app, AiConversationCommand::RecoverOnStartup);
    assert!(!ttl_file.exists(), "过期保留结果的文件必须被删除");
    assert!(kept_file.exists(), "未过期的保留结果文件不得删除");
    let summary = f.recover(NOW + 1_000);
    assert_eq!(
        summary.retained_paths_to_purge,
        Vec::<String>::new(),
        "过期已完成，不应再次回收同一落点"
    );
    assert_eq!(summary.expired_results, 0);
    assert_eq!(
        f.recover(NOW).expired_results,
        0,
        "上一轮已过期，不重复计数"
    );
    f.with(|conn| {
        let status: String = conn
            .query_row("SELECT status FROM ai_results WHERE id='ttl'", [], |r| {
                r.get(0)
            })
            .map_err(sql_error)?;
        assert_eq!(status, "expired");
        let kept: String = conn
            .query_row("SELECT status FROM ai_results WHERE id='kept'", [], |r| {
                r.get(0)
            })
            .map_err(sql_error)?;
        assert_eq!(kept, "available", "无 TTL 的保留结果不因重启失效");
        Ok(())
    });
}

#[test]
fn invalid_approval_card_reference_rolls_back_all_recovery_mutations() {
    let f = Fixture::new();
    let (mut app, job) = f.prepared();
    f.with(|conn| {
        checkpoint(
            conn,
            &job,
            "rollback body",
            "",
            Some(TurnStatus::WaitingApproval),
        )?;
        approval_card(conn, &job, true, None)?;
        call(
            conn,
            &job,
            "prepared-before-error",
            1,
            "db.execute",
            "prepared",
        )?;
        conn.execute(
            "UPDATE ai_message_parts SET payload_json=?1 WHERE id='approval-card'",
            [r#"{"approval_id":"missing-approval","approval_revision":1}"#],
        )
        .map_err(sql_error)?;
        Ok(())
    });
    let before = f.snapshot();
    let mut conn = f.storage.open_ai_connection().unwrap();
    let error = recovery::recover_on_startup(&mut conn, NOW).expect_err("卡片引用缺失必须报错");
    assert!(
        error.message.contains("审批记录不存在"),
        "不接受更早的 schema 错误: {}",
        error.message
    );
    assert_eq!(facts(&conn).unwrap(), before, "同连接事务应回滚");
    drop(conn);
    assert_eq!(f.snapshot(), before, "重开 SQLite 不能有部分写入");
    let app_before = app.state().ai_conversation.clone();
    match app.dispatch(AppCommand::AiConversation(
        AiConversationCommand::RecoverOnStartup,
    )) {
        AppEvent::AiSettingsError(message) => assert!(message.contains("审批记录不存在")),
        other => panic!("失败启动不能伪装成空会话: {other:?}"),
    }
    assert_eq!(app.state().ai_conversation, app_before);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn foreign_key_failure_during_restart_notice_rolls_back_transaction() {
    let f = Fixture::new();
    let (_, job) = f.prepared();
    f.with(|conn| {
        checkpoint(conn, &job, "FK rollback body", "", None)?;
        saved_ui(conn, &job)?;
        call(
            conn,
            &job,
            "prepared-before-fk",
            0,
            "db.execute",
            "prepared",
        )
    });
    let before = f.snapshot();
    let mut conn = f.storage.open_ai_connection().unwrap();
    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1, "fixture 保持真实外键保护");
    // 只在这个测试连接注入故障；生产 append_part 插入恢复 notice 时触发外键失败。
    conn.execute_batch(
        "CREATE TEMP TRIGGER fail_restart_notice BEFORE INSERT ON ai_message_parts
        WHEN NEW.kind='notice' AND json_extract(NEW.payload_json,'$.recovery_status') IS NOT NULL
        BEGIN UPDATE ai_session_ui_state SET scroll_anchor_message_id='missing-message'; END;",
    )
    .unwrap();
    let error = recovery::recover_on_startup(&mut conn, NOW).expect_err("外键失败必须中止启动恢复");
    assert!(
        error.message.contains("FOREIGN KEY constraint failed"),
        "实际错误: {}",
        error.message
    );
    assert_eq!(facts(&conn).unwrap(), before);
    drop(conn);
    assert_eq!(f.snapshot(), before);
    f.recover(NOW + 1);
    f.with(|conn| {
        assert_eq!(
            sessions::load_tool_call(conn, "prepared-before-fk")?.status,
            "cancelled"
        );
        Ok(())
    });
}

// 审批决定、执行占用与防重放；与上面的存储扫描测试分组。
#[path = "support/ai_restart_recovery_approval.rs"]
mod approval_replay;
