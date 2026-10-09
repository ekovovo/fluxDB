fn composer_send_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    }
}

#[gpui::test]
async fn ai_composer_shortcut_sends_without_executing_sql_light(cx: &mut TestAppContext) {
    check_composer_shortcut(cx, ThemeMode::Light).await;
}

#[gpui::test]
async fn ai_composer_shortcut_sends_without_executing_sql_dark(cx: &mut TestAppContext) {
    check_composer_shortcut(cx, ThemeMode::Dark).await;
}

async fn check_composer_shortcut(cx: &mut TestAppContext, theme: ThemeMode) {
    cx.executor().allow_parking();
    for mode in [
        settings::PanelMode::Drawer,
        settings::PanelMode::Embedded,
        settings::PanelMode::Tab,
    ] {
        let fixture = Fixture::seeded();
        let mut controller = fixture.controller();
        controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
            connection_id: ConnectionId(1),
            database: Some("main".into()),
            schema: None,
        });
        let query_id = controller.state().active_tab.unwrap();
        controller.dispatch(AppCommand::UpdateQueryText {
            tab_id: query_id,
            text: "SELECT 1;".into(),
        });
        let shortcuts = controller.state().settings.clone();
        let (view, cx) = open_main(cx, &fixture, controller, theme);
        cx.update(|_, cx| register_shortcuts(cx, &shortcuts));
        open_panel(&view, cx);
        if mode != settings::PanelMode::Drawer {
            choose_ai_mode(cx, mode);
            wait_idle(&view, cx);
        }
        replace_input(&view, cx, "first line");
        let before = fixture.counts();
        cx.simulate_keystrokes("enter");
        cx.simulate_input("second line");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("third line");
        let prompt = "first line\nsecond line\nthird line";
        assert_eq!(input_value(&view, cx), prompt);
        assert_eq!(fixture.counts(), before, "回车与 Shift+Enter 只换行");
        draw(cx);
        let send = cx.debug_bounds("ai-chat-send").unwrap();
        assert_eq!(send.size.width, px(22.));
        assert_eq!(send.size.height, px(22.));
        // 底部快捷键提示文案已移除；发送按钮仍留在输入区内。
        assert!(cx.debug_bounds("ai-chat-input-hint").is_none());
        assert!(send.bottom() <= cx.debug_bounds("ai-chat-input").unwrap().bottom());
        view.update(cx, |main, _| main.ai_chat.scripted_fixture = true);
        cx.simulate_keystrokes(composer_send_key());
        wait_idle(&view, cx);
        let job = view.read_with(cx, |main, _| {
            assert!(
                main.ai_chat.error.is_none(),
                "发送错误: {:?}",
                main.ai_chat.error
            );
            main.ai_chat.running.clone().expect("快捷键必须启动会话")
        });
        assert_eq!(input_value(&view, cx), "");
        replace_input(&view, cx, "next turn draft");
        cx.simulate_keystrokes(composer_send_key());
        wait_idle(&view, cx);
        assert_eq!(
            input_value(&view, cx),
            "next turn draft",
            "生成中不能再发一轮或插入换行"
        );
        assert_eq!(
            view.read_with(cx, |main, _| main
                .ai_chat
                .running
                .as_ref()
                .map(|job| job.turn_id.clone())),
            Some(job.turn_id.clone())
        );
        wait_source(&view, cx, "composer shortcut terminal", |main| {
            main.ai_chat.running.is_none()
        })
        .await;
        assert_final_reply(
            &fixture,
            &view,
            cx,
            &job,
            sessions::TurnStatus::Completed,
            prompt,
            "已读取当前目标。\n以下是 SQL 解释示例，不表示已执行查询。\nSELECT COUNT(*) FROM orders;",
        );
        view.read_with(cx, |main, _| {
            let query = main
                .controller
                .state()
                .tabs
                .iter()
                .find(|tab| tab.id == query_id)
                .unwrap();
            let TabKind::QueryEditor(query) = &query.kind else {
                panic!("业务页必须保留");
            };
            assert!(!query.running && query.results.is_empty() && query.summaries.is_empty());
            assert!(
                main.controller.state().last_error.is_none(),
                "快捷键不能穿透到 SQL 执行"
            );
        });
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }
}

#[gpui::test]
fn ai_composer_shortcut_preserves_empty_and_loading_drafts(cx: &mut TestAppContext) {
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    cx.update(|_, cx| register_shortcuts(cx, &Settings::default()));
    open_panel(&view, cx);
    let before = fixture.counts();
    replace_input(&view, cx, "   ");
    cx.simulate_keystrokes(composer_send_key());
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "   ");
    replace_input(&view, cx, "loading draft");
    view.update(cx, |main, _| main.ai_chat.loading = true);
    draw(cx);
    cx.simulate_keystrokes(composer_send_key());
    assert_eq!(input_value(&view, cx), "loading draft");
    assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
    assert_eq!(fixture.counts(), before);
    view.update(cx, |main, _| main.ai_chat.loading = false);
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[gpui::test]
fn ai_composer_shortcut_does_not_submit_ime_candidates(cx: &mut TestAppContext) {
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    cx.update(|_, cx| register_shortcuts(cx, &Settings::default()));
    open_panel(&view, cx);
    replace_input(&view, cx, "");
    let before = fixture.counts();
    // 用 GPUI 文本输入协议预置输入法候选，发送行为仍由真实快捷键触发。
    cx.update(|window, cx| {
        let input = view.read(cx).ai_chat.input.clone();
        input.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                input,
                None,
                "候选文字",
                None,
                window,
                cx,
            );
        });
    });
    draw(cx);
    cx.simulate_keystrokes(composer_send_key());
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "候选文字");
    assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
    assert_eq!(fixture.counts(), before);
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[gpui::test]
fn ai_composer_shortcut_reports_send_errors_without_losing_input(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    cx.update(|_, cx| register_shortcuts(cx, &Settings::default()));
    open_panel(&view, cx);
    replace_input(&view, cx, "model unavailable prompt");
    cx.simulate_keystrokes(composer_send_key());
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "model unavailable prompt");
    assert!(view.read_with(cx, |main, _| main.ai_chat.error.is_some()
        && main.ai_chat.running.is_none()));
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[gpui::test]
fn ai_composer_shortcut_requires_message_input_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::seeded();
    let shortcuts = fixture.controller().state().settings.clone();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    cx.update(|_, cx| register_shortcuts(cx, &shortcuts));
    open_panel(&view, cx);
    replace_input(&view, cx, "unfocused draft");
    let before = fixture.counts();
    click_selector(cx, "ai-chat-model-select");
    assert!(!cx.update(|window, cx| {
        view.read(cx)
            .ai_chat
            .input
            .focus_handle(cx)
            .is_focused(window)
    }));
    cx.simulate_keystrokes(composer_send_key());
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "unfocused draft");
    assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
    assert_eq!(fixture.counts(), before);
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}
