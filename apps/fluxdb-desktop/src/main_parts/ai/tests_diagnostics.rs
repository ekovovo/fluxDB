// AI 面板「诊断」只读弹框：真实 SQLite 失败事实 → 生产加载链 → 弹框控件。
// 断言用户可见状态 + 业务事实（Turn 错误码为受控码、notice 可见），不直接调用处理函数。

/// 预置一个失败 Turn（含受控错误码与可见的原因 notice），走生产 Load 投影到面板。
fn seed_failed_session(fixture: &Fixture, session: &str) -> String {
    let agent = fixture.agent(None);
    let now = 1_700_000_000_900_i64;
    let turn_id = format!("{session}-turn");
    let user_id = format!("{session}-user");
    let assistant_id = format!("{session}-assistant");
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
                "跑一个会失败的任务",
                "{}",
                "{}",
            )?;
            // 有正文的失败轮：原因必须落成可重读的 notice，正文只承载半截回答。
            conversation::upsert_event_part(
                conn,
                now + 2,
                session,
                &turn_id,
                &assistant_id,
                &format!("{assistant_id}-notice-failure"),
                sessions::PartKind::Notice,
                sessions::PartStatus::Error,
                &serde_json::json!({"text": "上游拒绝：API Key 无效", "text_offset": 2}).to_string(),
                None,
                None,
            )?;
            conversation::checkpoint(
                conn,
                now + 3,
                session,
                &turn_id,
                &assistant_id,
                "前半段回答",
                "",
                Some(sessions::TurnStatus::Failed),
                Some("authentication"),
            )
        })
        .unwrap();
    turn_id
}

fn open_failed_session<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
    session: &str,
) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
    let mut controller = fixture.controller();
    controller.dispatch(fluxdb_app::AppCommand::AiConversation(
        fluxdb_app::AiConversationCommand::Load {
            session_id: Some(session.into()),
        },
    ));
    let (view, cx) = open_main(cx, fixture, controller, ThemeMode::Light);
    cx.simulate_resize(size(px(1200.), px(800.)));
    open_panel(&view, cx);
    wait_idle(&view, cx);
    (view, cx)
}

/// 诊断读取是后台任务：有界等待弹框真正打开并加载完成。
fn wait_diagnostics_loaded(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        cx.run_until_parked();
        let (open, loading) = view.read_with(cx, |main, _| {
            let data = main.ai_diagnostics_modal.borrow();
            (data.dialog_open, data.loading)
        });
        if open && !loading {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "诊断弹框未加载完成（open={open} loading={loading}）"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    draw(cx);
}

fn diagnostics_dialog_open(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) -> bool {
    view.read_with(cx, |main, _| main.ai_diagnostics_modal.borrow().dialog_open)
}

#[gpui::test]
fn ai_diagnostics_dialog_shows_real_failure_facts(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    let session = "diag-open";
    seed_failed_session(&fixture, session);
    let (view, cx) = open_failed_session(&fixture, cx, session);

    assert!(
        cx.debug_bounds("ai-chat-diagnostics").is_some(),
        "面板头部必须有诊断入口"
    );
    click_selector(cx, "ai-chat-diagnostics");
    wait_diagnostics_loaded(&view, cx);

    // 弹框结构齐全：详情开关与关闭按钮。
    assert!(cx.debug_bounds("ai-diagnostics-close").is_some(), "弹框应渲染关闭按钮");
    assert!(
        cx.debug_bounds("ai-diagnostics-toggle-1").is_some(),
        "第一轮应有详情开关"
    );
    // 业务侧：读的是真实落盘事实——受控错误码 + 可重读的失败说明。
    let (error_code, notices) = view.read_with(cx, |main, _| {
        let rows = main.ai_diagnostics_modal.borrow().rows.clone();
        let row = rows.first().cloned().expect("至少一行诊断");
        (row.error_code.clone(), row.notices.clone())
    });
    assert_eq!(error_code.as_deref(), Some("authentication"));
    assert!(
        notices.iter().any(|notice| notice.contains("API Key 无效")),
        "失败原因必须能从诊断视图读到：{notices:?}"
    );

    // 「详情」展开后出现阶段/工具明细；再次点击收起。
    click_selector(cx, "ai-diagnostics-toggle-1");
    draw(cx);
    assert!(
        cx.debug_bounds("ai-diagnostics-detail-1").is_some(),
        "展开后应渲染明细区"
    );
    click_selector(cx, "ai-diagnostics-toggle-1");
    draw(cx);
    assert!(cx.debug_bounds("ai-diagnostics-detail-1").is_none(), "再次点击应收起");

    // Esc 走 Dialog 内置关闭；关闭后可再次打开（dialog_open 不得残留）。
    cx.simulate_keystrokes("escape");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        cx.run_until_parked();
        if !diagnostics_dialog_open(&view, cx) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "诊断弹框未关闭");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    draw(cx);
    click_selector(cx, "ai-chat-diagnostics");
    wait_diagnostics_loaded(&view, cx);
    assert!(cx.debug_bounds("ai-diagnostics-close").is_some(), "应能重新打开诊断");
}
