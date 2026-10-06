    // ---- 审批卡交互（GPUI test-support，子 Agent B）----
    //
    // 这里不再新建孤立组件：先用生产存储入口造出与 `ai_conversation_record_approval`
    // 等价的"等待审批"片段（conversation::send 建会话/Turn/用户消息 → create_tool_call
    // (waiting_approval) → create_approval(pending) → upsert_event_part(notice) → Turn 置
    // waiting_approval），再一律走生产 Load → AppState → 渲染快照 → 实际控件。

    /// 造一条 pending 审批片段。Agent 故意不绑定模型：批准后 `prepare_ai_resume_job`
    /// 会明确失败，从而只验证"决定已提交"，不伪造恢复执行成功。
    fn seed_approval(
        fixture: &Fixture,
        session: &str,
        approval_id: &str,
        part_status: sessions::PartStatus,
        decision: Option<&str>,
    ) {
        let agent = fixture.agent(None);
        let now = 1_700_000_000_000_i64;
        let turn_id = format!("{session}-turn");
        let user_id = format!("{session}-user");
        let assistant_id = format!("{session}-assistant");
        let tool_call_id = format!("{turn_id}-toolcall-1");
        let part_id = format!("{assistant_id}-approval-1");
        let plan_hash = "plan-hash-approval-seed";
        fixture
            .storage
            .with_ai_connection(|conn| {
                conversation::send(
                    conn,
                    now,
                    true,
                    session,
                    &agent,
                    &turn_id,
                    &user_id,
                    "请帮我删除一行数据",
                    "{}",
                    "{}",
                )?;
                sessions::create_tool_call(
                    conn,
                    now,
                    &tool_call_id,
                    session,
                    &turn_id,
                    1,
                    0,
                    "provider-call-1",
                    "db.execute",
                    1,
                    None,
                    r#"{"sql":"DELETE FROM orders WHERE id=1"}"#,
                    "{}",
                    plan_hash,
                    "waiting_approval",
                )?;
                let approval = fluxdb_storage::ai_platform::audits::create_approval(
                    conn,
                    now,
                    approval_id,
                    &tool_call_id,
                    plan_hash,
                    "{}",
                    None,
                )?;
                let mut payload = serde_json::json!({
                    "text": "工具 db.execute 需要你先批准后才会执行。",
                    "approval_id": approval_id,
                    "plan_hash": plan_hash,
                    "approval_revision": approval.revision,
                });
                if let Some(decision) = decision {
                    payload["decision"] = serde_json::Value::from(decision);
                }
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &part_id,
                    sessions::PartKind::Notice,
                    part_status,
                    &payload.to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                let turn = sessions::load_turn(conn, &turn_id)?;
                sessions::update_turn_status(
                    conn,
                    now,
                    &turn_id,
                    sessions::TurnStatus::WaitingApproval,
                    None,
                    turn.revision,
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// 造一条普通片段（文本 + 工具调用/结果引用），用于审批卡之外的回归。
    fn seed_regular_parts(fixture: &Fixture, session: &str) {
        let agent = fixture.agent(None);
        let now = 1_700_000_000_100_i64;
        let turn_id = format!("{session}-turn");
        let user_id = format!("{session}-user");
        let assistant_id = format!("{session}-assistant");
        let tool_call_id = format!("{turn_id}-toolcall-1");
        fixture
            .storage
            .with_ai_connection(|conn| {
                conversation::send(
                    conn, now, true, session, &agent, &turn_id, &user_id, "解释这段 SQL", "{}",
                    "{}",
                )?;
                // tool_call_ref/tool_result_ref 受 CHECK 约束，必须绑定一条真实工具调用事实行。
                sessions::create_tool_call(
                    conn,
                    now,
                    &tool_call_id,
                    session,
                    &turn_id,
                    1,
                    0,
                    "provider-call-1",
                    "db.query",
                    1,
                    None,
                    r#"{"sql":"SELECT 1"}"#,
                    "{}",
                    "plan-hash-regular-seed",
                    "succeeded",
                )?;
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &format!("{assistant_id}-tool-call"),
                    sessions::PartKind::ToolCallRef,
                    sessions::PartStatus::Complete,
                    &serde_json::json!({"text": "调用 db.query"}).to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &format!("{assistant_id}-tool-result"),
                    sessions::PartKind::ToolResultRef,
                    sessions::PartStatus::Complete,
                    &serde_json::json!({"text": "返回 3 行"}).to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// 直接从真实 SQLite 读回审批状态与 revision（不与 UI 断言混在一起）。
    fn approval_state(
        fixture: &Fixture,
        approval_id: &str,
    ) -> (fluxdb_storage::ai_platform::audits::ApprovalStatus, i64) {
        fixture
            .storage
            .with_ai_connection(|conn| {
                let view =
                    fluxdb_storage::ai_platform::audits::load_approval(conn, approval_id)?;
                Ok((view.status, view.revision))
            })
            .unwrap()
    }

    #[gpui::test]
    fn ai_panel_pending_approval_renders_approve_and_deny_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-pending",
            sessions::PartStatus::Pending,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        // 片段确实来自生产投影且状态是 pending，而不是给孤立组件塞数据。
        let projected = view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .flat_map(|message| &message.parts)
                .find(|part| part.id == "approval-assistant-approval-1")
                .map(|part| (part.kind.clone(), part.status.clone()))
        });
        assert_eq!(projected, Some(("notice".into(), "pending".into())));
        assert!(
            cx.debug_bounds("ai-message:approval-assistant").is_some(),
            "审批片段必须挂在助手消息上渲染"
        );
        let approve = cx
            .debug_bounds("ai-approve:appr-pending")
            .expect("pending 审批必须渲染'批准执行'控件");
        let deny = cx
            .debug_bounds("ai-deny:appr-pending")
            .expect("pending 审批必须渲染'拒绝'控件");
        assert!(approve.size.width > px(0.) && approve.size.height > px(0.));
        assert!(deny.size.width > px(0.) && deny.size.height > px(0.));
    }

    #[gpui::test]
    fn ai_panel_approve_click_dispatches_real_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-approve",
            sessions::PartStatus::Pending,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        let before = approval_state(&fixture, "appr-approve");
        assert_eq!(before.0.as_str(), "pending", "初始审批必须是待处理");
        click_selector(cx, "ai-approve:appr-approve");
        wait_idle(&view, cx);
        let after = approval_state(&fixture, "appr-approve");
        assert_eq!(
            after.0.as_str(),
            "approved",
            "点击'批准执行'必须把审批决定写进数据库"
        );
        assert!(
            after.1 > before.1,
            "决定提交后审批 revision 必须递增，证明是真实 CAS 决定而非 UI 假象"
        );
    }

    #[gpui::test]
    fn ai_panel_decided_approval_hides_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-decided",
            sessions::PartStatus::Pending,
            Some("denied"),
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(cx.debug_bounds("ai-message:approval-assistant").is_some());
        assert!(
            cx.debug_bounds("ai-approve:appr-decided").is_none(),
            "已决定的审批不能再出现批准按钮"
        );
        assert!(
            cx.debug_bounds("ai-deny:appr-decided").is_none(),
            "已决定的审批不能再出现拒绝按钮"
        );
    }

    #[gpui::test]
    fn ai_panel_non_pending_approval_hides_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-complete",
            sessions::PartStatus::Complete,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(cx.debug_bounds("ai-message:approval-assistant").is_some());
        assert!(
            cx.debug_bounds("ai-approve:appr-complete").is_none(),
            "非 pending 片段不能出现批准按钮"
        );
        assert!(
            cx.debug_bounds("ai-deny:appr-complete").is_none(),
            "非 pending 片段不能出现拒绝按钮"
        );
    }

    #[gpui::test]
    fn ai_panel_regular_text_and_tool_refs_still_render(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_regular_parts(&fixture, "regular");
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(
            cx.debug_bounds("ai-message:regular-user").is_some(),
            "普通用户文本消息必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-message:regular-assistant").is_some(),
            "普通助手消息必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-part:regular-assistant-tool-call").is_some(),
            "tool_call_ref 卡片必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-part:regular-assistant-tool-result").is_some(),
            "tool_result_ref 卡片必须仍渲染"
        );
    }
