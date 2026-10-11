use super::*;

struct RequireApproval;
impl SecurityAuditHook for RequireApproval {
    fn audit(&self, _: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::RequireApproval)
    }
}
struct SharedTool(Arc<RecordingTool>);
impl AiTool for SharedTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.0.descriptor()
    }
    fn execute(&self, ctx: ToolExecutionContext, args: String) -> AiFuture<'_, ToolReceipt> {
        self.0.execute(ctx, args)
    }
}
fn catalog(tool: &Arc<RecordingTool>) -> ToolCatalog {
    let mut catalog = ToolCatalog::new(vec![ToolSpec::from_descriptor(tool.descriptor()).unwrap()]);
    catalog
        .register(Box::new(SharedTool(tool.clone())))
        .unwrap();
    catalog
}
fn permission() -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        disabled_tools: Default::default(),
        policy: KernelPolicy {
            write_allowed: true,
            ..KernelPolicy::default()
        },
        plugin_digest: None,
    }
}

// 真正运行到生产待审批入口；plan_hash / 规范化参数 / 卡片由运行时生成，不手工伪造。
fn pause_for_approval(
    f: &Fixture,
    job: &AiConversationJob,
    tools: &ToolCatalog,
) -> audits::ApprovalView {
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![vec![
            ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: "write-request".into(),
                tool_id: "db.execute".into(),
                arguments_json: r#"{"sql":"UPDATE restart_fixture SET n=n+1"}"#.into(),
            }),
            ModelEvent::Finished(FinishReason::Stop),
        ]],
    );
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_turn(
        &f.storage,
        job,
        &CancelFlag::new(),
        &tx,
        &provider,
        tools,
        &RequireApproval,
        permission(),
    );
    assert_eq!(provider.call_count(), 1);
    assert!(rx.try_iter().any(|update| update.terminal));
    f.with(|conn| {
        assert_eq!(
            sessions::load_turn(conn, &job.turn_id)?.status,
            TurnStatus::WaitingApproval
        );
        let calls = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status, "waiting_approval");
        let approvals = audits::list_approvals_for_call(conn, &calls[0].id)?;
        assert_eq!(approvals.len(), 1);
        Ok(approvals[0].clone())
    })
}

#[test]
fn approved_startup_requires_new_revision_and_explicit_reapproval_before_one_execution() {
    let f = Fixture::new();
    let (mut app, job) = f.prepared();
    let tool = Arc::new(RecordingTool::new("db.execute", ToolRisk::Write));
    let tools = catalog(&tool);
    let approval = pause_for_approval(&f, &job, &tools);
    // AppCommand 每次重开连接，故只在独立 fixture 中安装持久触发器；结束立即删除。
    // 触发在审批 CAS 之后、原卡更新时，证明不是前置校验失败而是事务真实回滚。
    f.with(|conn| {
            conn.execute_batch("CREATE TRIGGER fail_approval_card_write
                BEFORE UPDATE OF payload_json ON ai_message_parts
                WHEN OLD.kind='notice' AND json_extract(NEW.payload_json,'$.approval_id') IS NOT NULL
                  AND json_extract(NEW.payload_json,'$.decision') IN ('approved','denied')
                BEGIN SELECT RAISE(ABORT,'T029 approval card write fault'); END;").map_err(sql_error)?;
            Ok(())
        });
    let before_fault = f.snapshot();
    let before_view = app.state().ai_conversation.clone();
    for approved in [true, false] {
        match app.dispatch(AppCommand::AiConversation(
            AiConversationCommand::DecideApproval {
                approval_id: approval.id.clone(),
                approved,
                expected_revision: approval.revision,
            },
        )) {
            AppEvent::AiSettingsError(message) => assert!(
                message.contains("T029 approval card write fault"),
                "必须到达卡片写入故障: {message}"
            ),
            other => panic!("卡片写入故障不能提交审批: {other:?}"),
        }
        assert_eq!(
            f.snapshot(),
            before_fault,
            "批准/拒绝卡片失败时审批 revision、调用、Turn、parts 必须全部回滚"
        );
        assert_eq!(app.state().ai_conversation, before_view);
        assert_eq!(tool.call_count(), 0);
    }
    f.with(|conn| {
        conn.execute_batch("DROP TRIGGER fail_approval_card_write;")
            .map_err(sql_error)?;
        Ok(())
    });
    dispatch_view(
        &mut app,
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: approval.revision,
        },
    );
    let approved = f.with(|conn| audits::load_approval(conn, &approval.id));
    let mut reopened = AppController::new();
    reopened.set_ai_storage(FileStorage::new(&f.dir));
    let view = startup_then_load(&mut reopened, &job);
    let pending = f.with(|conn| audits::load_approval(conn, &approval.id));
    assert_eq!(pending.status, audits::ApprovalStatus::Pending);
    assert_eq!(pending.revision, approved.revision + 1);
    assert_eq!(tool.call_count(), 0, "重启不自动执行历史批准");
    let card = view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .find(|part| {
            serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["approval_id"]
                == approval.id
        })
        .unwrap();
    assert_eq!(card.status, "pending");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&card.payload_json).unwrap()["approval_revision"],
        pending.revision
    );
    let before = f.snapshot();
    assert!(matches!(
        reopened.dispatch(AppCommand::AiConversation(
            AiConversationCommand::DecideApproval {
                approval_id: approval.id.clone(),
                approved: true,
                expected_revision: approved.revision,
            }
        )),
        AppEvent::AiSettingsError(_)
    ));
    assert_eq!(f.snapshot(), before, "旧 UI revision 不能批准");
    dispatch_view(
        &mut reopened,
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: pending.revision,
        },
    );
    let resume = reopened
        .prepare_ai_resume_job(&approval.id)
        .expect("明确重新批准后恢复");
    let provider = ScriptedGenerationProvider::text_only("T029 resumed once");
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_resume_approval(
        &f.storage,
        &resume,
        &CancelFlag::new(),
        &tx,
        &provider,
        &tools,
        &RequireApproval,
        permission(),
        &approval.id,
    );
    assert_eq!(tool.call_count(), 1);
    // 恢复成功后首轮完成会再发一次独立的标题请求；主请求之外多出一次属预期。
    assert_eq!(provider.call_count(), 2);
    assert!(rx.try_iter().any(|update| update.terminal));
    f.with(|conn| {
        let call = sessions::load_tool_call(conn, approval.tool_call_id.as_deref().unwrap())?;
        assert_eq!(call.status, "succeeded");
        assert!(call.receipt_json.is_some());
        assert_eq!(
            sessions::load_turn(conn, &job.turn_id)?.status,
            TurnStatus::Completed
        );
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_results WHERE turn_id=?1",
                [&job.turn_id],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        assert_eq!(rows, 1);
        Ok(())
    });
    let completed = f.snapshot();
    run_ai_conversation_resume_approval(
        &f.storage,
        &resume,
        &CancelFlag::new(),
        &tx,
        &provider,
        &tools,
        &RequireApproval,
        permission(),
        &approval.id,
    );
    assert_eq!(tool.call_count(), 1, "第二次 resume 不重复副作用");
    // 已消费的批准使得第二次 resume 不再发模型请求，计数保持第一次恢复后的值。
    assert_eq!(provider.call_count(), 2);
    assert_eq!(f.snapshot(), completed);
}

#[test]
fn unknown_write_same_target_cannot_be_replayed_even_after_pending_is_reapproved() {
    let f = Fixture::new();
    let (mut app, job) = f.prepared();
    let tool = Arc::new(RecordingTool::new("db.execute", ToolRisk::Write));
    let tools = catalog(&tool);
    let approval = pause_for_approval(&f, &job, &tools);
    dispatch_view(
        &mut app,
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: approval.revision,
        },
    );
    // 同一 Turn / 固定目标另一次写入已开始却没有终态回执：旧批准也不能解除未知副作用。
    f.with(|conn| {
        call(
            conn,
            &job,
            "unknown-same-target",
            1,
            "db.execute",
            "running",
        )
    });
    let mut reopened = AppController::new();
    reopened.set_ai_storage(FileStorage::new(&f.dir));
    let view = startup_then_load(&mut reopened, &job);
    let pending = f.with(|conn| {
        let unknown = sessions::load_tool_call(conn, "unknown-same-target")?;
        let waiting = sessions::load_tool_call(conn, approval.tool_call_id.as_deref().unwrap())?;
        assert_eq!(unknown.status, "unknown");
        assert_eq!(unknown.target_json, waiting.target_json);
        assert_eq!(
            sessions::load_turn(conn, &job.turn_id)?.status,
            TurnStatus::OutcomeUnknown
        );
        audits::load_approval(conn, &approval.id)
    });
    assert_eq!(pending.status, audits::ApprovalStatus::Pending);
    assert_eq!(tool.call_count(), 0);
    assert!(view.messages.iter().flat_map(|m| &m.parts).any(|part| {
        serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["recovery_status"]
            == "outcome_unknown"
    }));
    let before_decision = f.snapshot();
    let before_view = reopened.state().ai_conversation.clone();
    let mut decision_was_atomic = true;
    match reopened.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: pending.revision,
        },
    )) {
        AppEvent::AiSettingsError(_) => {
            decision_was_atomic = f.snapshot() == before_decision;
            assert_eq!(reopened.state().ai_conversation, before_view);
        }
        AppEvent::AiConversationLoaded(view) => {
            assert_eq!(reopened.state().ai_conversation, view)
        }
        other => panic!("审批命令的未知结果: {other:?}"),
    }
    // 即便另一个入口留下了有效批准，执行占用守卫也必须独立拒绝 unknown Turn。
    f.with(|conn| {
        let current = audits::load_approval(conn, &approval.id)?;
        let approved = if current.status == audits::ApprovalStatus::Pending {
            audits::decide_approval(conn, NOW, &current.id, true, None, current.revision)?
        } else {
            current
        };
        assert_eq!(approved.status, audits::ApprovalStatus::Approved);
        assert_eq!(
            sessions::load_turn(conn, &job.turn_id)?.status,
            TurnStatus::OutcomeUnknown
        );
        Ok(())
    });
    let before = f.snapshot();
    let provider = ScriptedGenerationProvider::text_only("must not run");
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_resume_approval(
        &f.storage,
        &job,
        &CancelFlag::new(),
        &tx,
        &provider,
        &tools,
        &RequireApproval,
        permission(),
        &approval.id,
    );
    assert_eq!(tool.call_count(), 0, "再次批准也不能执行同目标未知写");
    assert_eq!(provider.call_count(), 0, "拒绝发生在模型/工具调用前");
    assert!(rx.try_iter().any(|update| {
        update.terminal
            && update
                .view
                .as_ref()
                .is_some_and(|view| view.error.is_some())
    }));
    assert_eq!(
        f.snapshot(),
        before,
        "拒绝 resume 不消耗批准、不抹掉 unknown 事实"
    );
    assert!(
        decision_was_atomic,
        "拒绝 unknown 审批必须原子回滚，不能先批准再返回卡片写入失败"
    );
}
