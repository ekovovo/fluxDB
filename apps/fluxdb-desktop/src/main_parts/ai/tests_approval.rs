    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    // ---- 审批卡交互（生产 View + 隔离存储）----
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
        seed_approval_with_details(
            fixture,
            session,
            approval_id,
            part_status,
            decision,
            r#"{"sql":"DELETE FROM orders WHERE id=1"}"#,
            "{}",
            approval_details_json(),
        );
    }

    fn approval_details_json() -> &'static str {
        r#"{"risk":"medium","statements":["DELETE FROM orders WHERE id=1"],"guardrails":["单语句，不含事务或会话控制","未通过只读分类，必须逐次批准"],"impact_basis":"未核实","estimated_rows":null}"#
    }

    /// 带真实目标/参数/details 的审批片段：用于断言卡片各区块真实渲染，
    /// 而不是只断言按钮存在。
    #[allow(clippy::too_many_arguments)]
    fn seed_approval_with_details(
        fixture: &Fixture,
        session: &str,
        approval_id: &str,
        part_status: sessions::PartStatus,
        decision: Option<&str>,
        arguments_json: &str,
        target_json: &str,
        details_json: &str,
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
                    target_json,
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
                    "1",
                    "db.execute",
                    1,
                    None,
                    arguments_json,
                    target_json,
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
                    "details": serde_json::from_str::<serde_json::Value>(details_json).unwrap(),
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

    #[gpui_kit::test]
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
            .expect("pending 审批必须渲染'批准一次'控件");
        let deny = cx
            .debug_bounds("ai-deny:appr-pending")
            .expect("pending 审批必须渲染'拒绝'控件");
        assert!(approve.size.width > px(0.) && approve.size.height > px(0.));
        assert!(deny.size.width > px(0.) && deny.size.height > px(0.));
        // 设计 §31.9：批准是这次决定里唯一的主动作，宽度明显大于"拒绝"，
        // 但仍是一枚紧凑按钮，不能喧宾夺主占满整行。
        assert!(
            approve.size.width > deny.size.width,
            "批准一次应比拒绝更宽，实际 {:?} vs {:?}",
            approve.size.width,
            deny.size.width
        );
        assert!(approve.size.height <= px(28.) && approve.size.width < px(100.),
            "批准按钮必须紧凑：{approve:?}");
    }

    /// 设计 §31.9 第 1–6 项：头部、上下文行、计划内容、影响预估、护栏清单、提示
    /// 都必须来自持久化事实真实渲染，不能只剩一句内联文字。
    #[gpui_kit::test]
    fn ai_panel_approval_card_renders_all_designed_sections(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval_with_details(
            &fixture,
            "approval-sections",
            "appr-sections",
            sessions::PartStatus::Pending,
            None,
            r#"{"target":"conn:1/db:shop","sql":"DELETE FROM orders WHERE id=1"}"#,
            r#"{"connection_id":1,"database":"shop","schema":"public","object":"orders"}"#,
            approval_details_json(),
        );
        let mut controller = fixture.controller();
        controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
            id: ConnectionId(1),
            name: "隔离验收库".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: fluxdb_core::Endpoint::SqliteFile { path: fixture.root.join("orders.sqlite"), read_only: false },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }]));
        let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
        open_panel(&view, cx);
        // 卡片外层：设计要求头部 + 内容 + 按钮的完整卡片，而不是一行红框文字。
        assert!(
            cx.debug_bounds("ai-approval:approval-sections-assistant-approval-1").is_some(),
            "审批卡必须以完整卡片渲染"
        );
        // 计划内容：可复制的等宽代码块 + 复制入口。
        assert!(
            cx.debug_bounds("ai-approval-copy:approval-sections-assistant-approval-1:0").is_some(),
            "SQL 原文必须渲染为可复制代码块"
        );
        cx.update(|window, cx| {
            window.render_frame(cx);
            let card = window.within("ai-approval:approval-sections-assistant-approval-1");
            assert_eq!(card.find("heading").label(), Some("需要你批准这次写入"));
            assert_eq!(card.find("risk").label(), Some("风险：中"));
            assert!(card.find("连接环境").label().unwrap().contains("隔离验收库"));
            assert!(card.find("目标连接配置").label().unwrap().contains("rev 0"));
            assert!(card.find("目标连接配置").label().unwrap().contains("配置变更后请重新核对"));
            assert_eq!(card.find("数据库").label(), Some("数据库：shop / public / orders"));
            assert_eq!(card.find(("statement", 0_usize)).label(), Some("DELETE FROM orders WHERE id=1"));
            assert!(card.find("guardrails").label().unwrap().contains("未通过只读分类"));
            assert!(card.find("hint").label().unwrap().contains("拒绝本次"));
            window.click("ai-approval-copy:approval-sections-assistant-approval-1:0", cx);
        });
        cx.update(|_, cx| {
            assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some("DELETE FROM orders WHERE id=1"));
        });
    }

    /// 影响预估未核实时必须显示"未知"，不能拿估算行数冒充已核实。
    #[gpui_kit::test]
    fn ai_panel_approval_card_does_not_fabricate_impact_rows(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval_with_details(
            &fixture,
            "approval-impact",
            "appr-impact",
            sessions::PartStatus::Pending,
            None,
            r#"{"target":"conn:1/db:shop","sql":"DELETE FROM orders WHERE id=1"}"#,
            r#"{"connection_id":1,"database":"shop"}"#,
            approval_details_json(),
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
        open_panel(&view, cx);
        cx.update(|window, cx| {
            window.render_frame(cx);
            let card = window.within("ai-approval:approval-impact-assistant-approval-1");
            assert!(card.find("预计影响行数").label().unwrap().starts_with("预计影响行数：未知"));
            assert!(card.find("依据").label().unwrap().starts_with("依据：未核实"));
        });
    }

    /// 已决定 / 非 pending 的审批片段不能出现卡片按钮，也不能重复执行。
    #[gpui_kit::test]
    fn ai_panel_approval_card_hidden_after_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval_with_details(
            &fixture,
            "approval-decided-card",
            "appr-decided-card",
            sessions::PartStatus::Pending,
            Some("approved"),
            r#"{"target":"conn:1/db:shop","sql":"DELETE FROM orders WHERE id=1"}"#,
            r#"{"connection_id":1,"database":"shop"}"#,
            approval_details_json(),
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(
            cx.debug_bounds("ai-approval-copy:approval-decided-card-assistant-approval-1:0").is_none(),
            "已决定的审批不应再渲染计划内容与批准控件"
        );
        assert!(cx.debug_bounds("ai-approve:appr-decided-card").is_none());
        assert!(cx.debug_bounds("ai-deny:appr-decided-card").is_none());
    }

    #[gpui_kit::test]
    async fn ai_panel_approve_click_dispatches_real_decision(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
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
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-approve:appr-approve", cx);
            window.render_frame(cx);
            window.click("ai-approve:appr-approve", cx);
            assert!(view.read(cx).ai_chat.deciding_approval);
        });
        let handle = cx.window_handle();
        cx.cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window.try_find("app-message-overlay").is_some()
                && window.try_find("ai-approve:appr-approve").is_none()
        }).await;
        let after = approval_state(&fixture, "appr-approve");
        assert_eq!(
            after.0.as_str(),
            "approved",
            "点击'批准一次'必须把审批决定写进数据库"
        );
        assert!(
            after.1 > before.1,
            "决定提交后审批 revision 必须递增，证明是真实 CAS 决定而非 UI 假象"
        );
        assert_eq!(after.1, before.1 + 1, "提交中重复点击不能重复决定");
        cx.update(|window, cx| {
            window.render_frame(cx);
            let message = window.find("app-message-overlay");
            assert_eq!(message.role(), Some(gpui::Role::Status));
            assert!(message.label().unwrap().contains("无法恢复任务"));
            assert!(message.label().unwrap().contains("缺少原模型"));
            assert!(window.try_find("ai-approve:appr-approve").is_none());
            assert!(window.find("ai-part:approval-assistant-approval-1").label().unwrap().contains("已批准一次"));
        });
        assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
        // 释放真实 show_message 计时任务，避免测试结束后回调失效窗口。
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    async fn ai_panel_stale_approval_decision_shows_message(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let fixture = Fixture::new();
        seed_approval(&fixture, "approval", "appr-stale", sessions::PartStatus::Pending, None);
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        // 模拟另一窗口先处理审批；保留当前窗口的旧 revision，验证真实 CAS 错误反馈。
        fixture.storage.with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::audits::decide_approval(conn, 1_700_000_000_001, "appr-stale", false, None, 1)?;
            Ok(())
        }).unwrap();
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-approve:appr-stale", cx);
        });
        let handle = cx.window_handle();
        cx.cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window.try_find("app-message-overlay").is_some()
        }).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.find("app-message-overlay").label().unwrap().contains("审批状态已变化"));
        });
        assert_eq!(approval_state(&fixture, "appr-stale").0.as_str(), "denied");
        assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    async fn ai_panel_escape_rejects_pending_approval(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let fixture = Fixture::new();
        seed_approval(&fixture, "approval", "appr-escape", sessions::PartStatus::Pending, None);
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-input", cx);
            window.press("escape", cx);
        });
        let handle = cx.window_handle();
        cx.cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window.try_find("ai-approve:appr-escape").is_none()
        }).await;
        assert_eq!(approval_state(&fixture, "appr-escape").0.as_str(), "denied");
        assert!(cx.debug_bounds("ai-approve:appr-escape").is_none());
        assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
    }

    #[gpui_kit::test]
    async fn ai_panel_close_rejects_pending_approval(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let fixture = Fixture::new();
        seed_approval(&fixture, "approval", "appr-close", sessions::PartStatus::Pending, None);
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-close", cx);
        });
        let handle = cx.window_handle();
        cx.cx.wait_for(handle, Duration::from_secs(3), |window, app| {
            let main = view.read(app);
            !main.ai_chat.open && !main.ai_chat.deciding_approval
                && window.try_find("ai-chat-close").is_none()
        }).await;
        assert_eq!(approval_state(&fixture, "appr-close").0.as_str(), "denied");
        assert!(!view.read_with(cx, |main, _| main.ai_chat.open));
        open_panel(&view, cx);
        assert!(cx.debug_bounds("ai-approve:appr-close").is_none());
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
            cx.debug_bounds("execution:regular-assistant").is_some(),
            "工具执行摘要必须渲染"
        );
        assert!(
            cx.debug_bounds("ai-part:regular-assistant-tool-result").is_none(),
            "结果记录应并入调用而非重复渲染"
        );
    }
