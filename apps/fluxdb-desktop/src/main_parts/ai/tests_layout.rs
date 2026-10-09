#[gpui_kit::test]
fn chat_message_roles_are_distinct_light(cx: &mut TestAppContext) {
    check_chat_message_roles(cx, ThemeMode::Light);
}

#[gpui_kit::test]
fn chat_message_roles_are_distinct_dark(cx: &mut TestAppContext) {
    check_chat_message_roles(cx, ThemeMode::Dark);
}

fn check_chat_message_roles(cx: &mut TestAppContext, theme: ThemeMode) {
    let fixture = Fixture::new();
    seed_regular_parts(&fixture, "regular");
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), theme);
    open_panel(&view, cx);
    for width in [1000., 640.] {
        cx.simulate_resize(size(px(width), px(700.)));
        draw(cx);
        let user = cx.debug_bounds("ai-message:regular-user").unwrap();
        let bubble = cx.debug_bounds("ai-message-content:regular-user").unwrap();
        let assistant = cx.debug_bounds("ai-message:regular-assistant").unwrap();
        let reply = cx.debug_bounds("ai-message-content:regular-assistant").unwrap();
        let summary = cx.debug_bounds("execution:regular-assistant").unwrap();
        assert!(bubble.size.width > px(0.));
        assert!(bubble.size.width <= user.size.width * 0.82 + px(1.), "用户气泡应保留左侧留白");
        assert!((bubble.right() - user.right()).abs() <= px(1.), "用户气泡应靠右");
        assert!((reply.left() - assistant.left()).abs() <= px(1.), "AI 回复应靠左");
        assert!(assistant.top() - user.bottom() >= px(16.), "轮次间距应明显大于段落间距");
        assert!(summary.left() >= reply.left() + px(10.), "工具摘要应缩进");
        assert!(summary.center().x < reply.center().x, "工具摘要不应居中");
        assert!(summary.top() - reply.top() <= px(36.), "Agent 标题后不能有空布局撑高");
    }
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("execution:regular-assistant", cx);
    });
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| main.controller.state().ai_conversation.expanded_parts
        .contains(&"execution:regular-assistant".into())));
    assert!(cx.debug_bounds("ai-part:regular-assistant-tool-call").is_some());
}

#[gpui::test]
fn long_conversation_keeps_statusbar_fixed_light(cx: &mut TestAppContext) {
    check_long_conversation_statusbar(cx, ThemeMode::Light);
}

#[gpui::test]
fn long_conversation_keeps_statusbar_fixed_dark(cx: &mut TestAppContext) {
    check_long_conversation_statusbar(cx, ThemeMode::Dark);
}

fn check_long_conversation_statusbar(cx: &mut TestAppContext, theme: ThemeMode) {
    let fixture = Fixture::seeded();
    let agent = fixture.agent(None);
    fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::send(
                conn,
                90,
                true,
                "long",
                &agent,
                "long-turn",
                "long-user",
                "展示长回复",
                "{}",
                "{}",
            )?;
            conversation::checkpoint(
                conn,
                100,
                "long",
                "long-turn",
                "long-assistant",
                &"很长的回复，消息应在自己的区域滚动。\n".repeat(300),
                "公开思考摘要",
                Some(sessions::TurnStatus::Completed),
                None,
            )
        })
        .unwrap();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
        session_id: Some("one".into()),
    }));
    let (view, cx) = open_main(cx, &fixture, controller, theme);
    cx.simulate_resize(size(px(1000.), px(700.)));
    open_panel(&view, cx);
    let original_status = cx.debug_bounds("fluxdb-statusbar").unwrap();
    // 通过真实历史菜单重新加载长回复，不绕开生产渲染与异步加载链路。
    click_selector(cx, "ai-chat-history");
    click_selector(cx, "ai-session:long");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        let scroll = &main.ai_chat.scroll;
        scroll.max_offset().y > px(0.) && scroll.offset().y == -scroll.max_offset().y
    }), "历史会话加载后应直接定位底部");
    for mode in [settings::PanelMode::Drawer, settings::PanelMode::Embedded] {
        if view.read_with(cx, |main, _| main.ai_chat.mode) != mode {
            choose_ai_mode(cx, mode);
            wait_idle(&view, cx);
        }
        for height in [700., 500.] {
            cx.simulate_resize(size(px(1000.), px(height)));
            cx.run_until_parked();
            draw(cx);
            cx.run_until_parked();
            draw(cx);
            let root = cx.debug_bounds("fluxdb-main").unwrap();
            let status = cx.debug_bounds("fluxdb-statusbar").unwrap();
            let panel = cx.debug_bounds("ai-chat-panel").unwrap();
            let timeline = cx.debug_bounds("ai-chat-timeline").unwrap();
            let composer = cx.debug_bounds("ai-chat-composer").unwrap();
            let send = cx.debug_bounds("ai-chat-send").unwrap();
            assert_eq!(
                status.size.height,
                px(24.),
                "底部状态栏不能被压缩：{status:?}"
            );
            assert!(
                status.bottom() <= root.bottom(),
                "状态栏不能被长回复挤出窗口"
            );
            assert!(
                panel.bottom() <= status.top(),
                "AI 面板不能延伸到状态栏之下：{panel:?}, {status:?}"
            );
            assert!(
                timeline.bottom() <= composer.top(),
                "长回复只能在时间线内滚动"
            );
            assert!(composer.bottom() <= status.top() && send.bottom() <= status.top());
            if height == 700. {
                assert_eq!(status, original_status);
            }
            let before = view.read_with(cx, |main, _| main.ai_chat.scroll.offset());
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: timeline.center(),
                delta: gpui::ScrollDelta::Pixels(point(
                    px(0.),
                    px(if before.y < px(-1.) { 100. } else { -100. }),
                )),
                ..Default::default()
            });
            draw(cx);
            assert_ne!(
                view.read_with(cx, |main, _| main.ai_chat.scroll.offset()),
                before,
                "长对话仍应能滚动，不能只裁掉内容"
            );
            assert_eq!(cx.debug_bounds("fluxdb-statusbar").unwrap(), status);
        }
    }
    assert!(view.read_with(cx, |main, _| !main.ai_chat.follow_reply));
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    open_panel(&view, cx);
    // 重开会重建 Markdown 实体，驱动首次渲染启动的异步解析，再验证最终布局。
    cx.run_until_parked();
    draw(cx);
    assert!(view.read_with(cx, |main, _| {
        let scroll = &main.ai_chat.scroll;
        main.ai_chat.follow_reply && !main.ai_chat.new_reply
            && scroll.max_offset().y > px(0.)
            && scroll.offset().y == -scroll.max_offset().y
    }), "向上浏览后重新打开询问 AI 也应定位底部");
}
