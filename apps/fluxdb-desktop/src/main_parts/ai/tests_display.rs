fn choose_ai_mode(cx: &mut VisualTestContext, mode: settings::PanelMode) {
    click_selector(cx, "ai-chat-mode");
    let trigger = cx.debug_bounds("ai-chat-mode").unwrap();
    let first = cx.debug_bounds("ai-chat-mode-option:drawer").unwrap();
    assert!(
        first.top() >= trigger.bottom(),
        "显示方式菜单应在按钮下方展开"
    );
    click_selector(
        cx,
        match mode {
            settings::PanelMode::Drawer => "ai-chat-mode-option:drawer",
            settings::PanelMode::Embedded => "ai-chat-mode-option:embedded",
            settings::PanelMode::Tab => "ai-chat-mode-option:tab",
        },
    );
}

#[gpui::test]
fn ai_display_modes_keep_session_draft_and_single_tab_light(cx: &mut TestAppContext) {
    check_ai_display_modes(cx, ThemeMode::Light);
}

#[gpui::test]
fn ai_display_modes_keep_session_draft_and_single_tab_dark(cx: &mut TestAppContext) {
    check_ai_display_modes(cx, ThemeMode::Dark);
}

fn check_ai_display_modes(cx: &mut TestAppContext, theme: ThemeMode) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
    });
    let source_id = controller.state().active_tab.unwrap();
    let (view, cx) = open_main(cx, &fixture, controller, theme);
    open_panel(&view, cx);
    replace_input(&view, cx, "keep this draft");
    let input = view.read_with(cx, |main, _| main.ai_chat.input.clone());
    let before = fixture.counts();
    click_selector(cx, "ai-chat-mode");
    for selector in [
        "ai-chat-mode-option:drawer",
        "ai-chat-mode-option:embedded",
        "ai-chat-mode-option:tab",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "菜单必须展示全部三种形态"
        );
    }
    cx.simulate_keystrokes("escape");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| main.ai_chat.open));
    assert!(cx.debug_bounds("ai-chat-mode-option:tab").is_none());

    for mode in [settings::PanelMode::Embedded, settings::PanelMode::Tab] {
        choose_ai_mode(cx, mode);
        wait_idle(&view, cx);
        assert_eq!(input_value(&view, cx), "keep this draft");
        assert_session(&view, cx, "one");
        assert_eq!(
            input,
            view.read_with(cx, |main, _| main.ai_chat.input.clone())
        );
    }
    let ai_id = view.read_with(cx, |main, _| {
        assert_eq!(main.controller.state().tabs.len(), 2);
        assert_eq!(main.controller.state().active_tab, main.ai_chat.tab_id);
        main.ai_chat.tab_id.unwrap()
    });
    assert_eq!(
        view.read_with(cx, |main, cx| main.capture_ai_active_context(cx).tab_id),
        Some(source_id.0)
    );
    assert!(cx.debug_bounds("ai-chat-drawer").is_none());
    assert!(cx.debug_bounds("ai-chat-embedded").is_none());
    cx.simulate_keystrokes("escape");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| main.ai_chat.open));
    click_selector(cx, "ask-ai");
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main.ai_chat.tab_id),
        Some(ai_id)
    );
    // 普通页签的关闭快捷键同样只隐藏 AI，不丢会话或草稿。
    cx.update(|_, cx| {
        cx.bind_keys([KeyBinding::new("ctrl-w", CloseCurrentTab, None)]);
    });
    cx.simulate_keystrokes("ctrl-w");
    wait_idle(&view, cx);
    assert!(!view.read_with(cx, |main, _| main.ai_chat.open));
    assert!(view.read_with(cx, |main, _| main.ai_chat.tab_id.is_none()));
    open_panel(&view, cx);
    assert_eq!(input_value(&view, cx), "keep this draft");
    let ai_id = view.read_with(cx, |main, _| main.ai_chat.tab_id.unwrap());
    assert_eq!(
        view.read_with(cx, |main, _| main.controller.state().tabs.len()),
        2
    );
    // 通过标签实际点击切到业务页，再用统一入口唤起同一个 AI 页签。
    assert_eq!(source_id, TabId(1));
    click_selector(cx, "workspace-tab:1");
    wait_idle(&view, cx);
    assert!(cx.debug_bounds("ai-chat-panel").is_none());
    open_panel(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main.ai_chat.tab_id),
        Some(ai_id)
    );
    choose_ai_mode(cx, settings::PanelMode::Drawer);
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main.controller.state().active_tab),
        Some(source_id)
    );
    assert_eq!(
        view.read_with(cx, |main, _| main.controller.state().tabs.len()),
        1
    );
    assert_session(&view, cx, "one");
    assert_eq!(fixture.counts(), before);
    choose_ai_mode(cx, settings::PanelMode::Tab);
    wait_idle(&view, cx);
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    assert!(!view.read_with(cx, |main, _| main.ai_chat.open));
    assert_eq!(
        fixture.preferences().preferred_mode,
        settings::PanelMode::Tab
    );
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
    let (reopened, cx) = open_main(&mut cx.cx, &fixture, fixture.controller(), theme);
    open_panel(&reopened, cx);
    assert_eq!(
        reopened.read_with(cx, |main, _| main.ai_chat.mode),
        settings::PanelMode::Tab
    );
    assert!(reopened.read_with(cx, |main, _| matches!(
        main.controller.state().active_tab().unwrap().kind,
        TabKind::AiChat { .. }
    )));
    assert_eq!(input_value(&reopened, cx), "keep this draft");
    cx.update(|window, _| window.remove_window());
    drop(reopened);
    cx.run_until_parked();
}

#[gpui::test]
async fn ai_display_switch_does_not_restart_running_reply(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    let job = start_scripted_send(&fixture, &view, cx, "display switch while generating");
    wait_source(&view, cx, "display switch partial reply", |main| {
        main.ai_chat.running.is_some() && !main.ai_chat.stream.revealed_text().is_empty()
    })
    .await;
    let (partial, stream_view) = view.read_with(cx, |main, _| {
        (
            main.ai_chat.stream.revealed_text().to_owned(),
            main.ai_chat.stream_view.clone(),
        )
    });
    choose_ai_mode(cx, settings::PanelMode::Tab);
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main
            .ai_chat
            .running
            .as_ref()
            .map(|job| job.turn_id.clone())),
        Some(job.turn_id.clone())
    );
    assert!(
        view.read_with(cx, |main, _| {
            main.ai_chat.stream.revealed_text().starts_with(&partial)
                && main.ai_chat.stream_view == stream_view
        }),
        "切换显示方式不能重置流式正文或 Markdown 实体"
    );
    click_selector(cx, "ask-ai");
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main
            .ai_chat
            .running
            .as_ref()
            .map(|job| job.turn_id.clone())),
        Some(job.turn_id.clone())
    );
    choose_ai_mode(cx, settings::PanelMode::Embedded);
    wait_idle(&view, cx);
    wait_source(&view, cx, "display switch final reply", |main| {
        main.ai_chat.running.is_none()
    })
    .await;
    assert_final_reply(
        &fixture,
        &view,
        cx,
        &job,
        sessions::TurnStatus::Completed,
        "display switch while generating",
        "已读取当前目标。\n以下是 SQL 解释示例，不表示已执行查询。\nSELECT COUNT(*) FROM orders;",
    );
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[gpui::test]
fn ai_tab_entry_recaptures_current_business_page(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    for _ in 0..2 {
        controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
            connection_id: ConnectionId(1),
            database: Some("main".into()),
            schema: None,
        });
    }
    let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
    open_panel(&view, cx);
    choose_ai_mode(cx, settings::PanelMode::Tab);
    wait_idle(&view, cx);
    let ai_id = view.read_with(cx, |main, _| main.ai_chat.tab_id);
    assert_eq!(
        view.read_with(cx, |main, cx| main.capture_ai_active_context(cx).tab_id),
        Some(2)
    );
    click_selector(cx, "workspace-tab:1");
    wait_idle(&view, cx);
    open_panel(&view, cx);
    assert_eq!(view.read_with(cx, |main, _| main.ai_chat.tab_id), ai_id);
    assert_eq!(
        view.read_with(cx, |main, cx| main.capture_ai_active_context(cx).tab_id),
        Some(1)
    );
    click_selector(cx, "workspace-tab:2");
    wait_idle(&view, cx);
    click_selector(cx, "workspace-tab:3");
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, cx| main.capture_ai_active_context(cx).tab_id),
        Some(2)
    );
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}
