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
    for mode in [settings::PanelMode::Drawer, settings::PanelMode::Embedded] {
        if view.read_with(cx, |main, _| main.ai_chat.mode) != mode {
            click_selector(cx, "ai-chat-mode");
            wait_idle(&view, cx);
        }
        for height in [700., 500.] {
            cx.simulate_resize(size(px(1000.), px(height)));
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
                    px(if before.y < px(0.) { 100. } else { -100. }),
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
}
