#[cfg(unix)]
#[gpui_kit::test]
async fn reference_attachment_picker_authorizes_and_rejects_files_through_real_clicks(
    cx: &mut TestAppContext,
) {
    use gpui_kit::test::TestWindowExt as _;
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    let attachment = fixture.root.with_extension("sql");
    let invalid = fixture.root.with_extension("bin");
    std::fs::write(&attachment, "SELECT 'attachment-fixture';").unwrap();
    std::fs::write(&invalid, [0, 255]).unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    for (path, succeeds) in [(&attachment, true), (&invalid, false)] {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-add", cx);
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-upload", cx);
        });
        assert!(cx.did_prompt_for_paths());
        assert!(view.read_with(cx, |main, _| main.ai_chat.attachment_loading));
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        wait_source(&view, cx, "attachment authorization", |main| {
            !main.ai_chat.attachment_loading
                && main.ai_chat._io.is_none()
                && main.ai_chat.pending.is_empty()
        })
        .await;
        assert_eq!(
            view.read_with(cx, |main, _| main
                .controller
                .state()
                .ai_conversation
                .context_references
                .len()),
            1
        );
        if succeeds {
            let id = view.read_with(cx, |main, _| {
                main.controller.state().ai_conversation.context_references[0]
                    .id
                    .clone()
            });
            cx.update(|window, cx| {
                window.render_frame(cx);
                assert!(
                    window
                        .try_find(SharedString::from(format!("ai-reference-chip:{id}")))
                        .is_some()
                );
            });
        } else {
            assert!(view.read_with(cx, |main, _| main.app_message.as_ref().is_some()));
        }
    }
    let reference = view.read_with(cx, |main, _| {
        main.controller.state().ai_conversation.context_references[0].clone()
    });
    replace_input(&view, cx, &format!("@{}", reference.label));
    assert!(view.read_with(cx, |main, _| {
        main.ai_chat
            .reference_options
            .iter()
            .any(|item| item.id == reference.id)
    }));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        window.click(
            SharedString::from(format!("ai-reference-remove:{}", reference.id)),
            cx,
        );
    });
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    }));
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
    std::fs::remove_file(attachment).unwrap();
    std::fs::remove_file(invalid).unwrap();
}

#[gpui_kit::test]
fn reference_popup_outside_click_dismisses_without_activating_underlying_button(
    cx: &mut TestAppContext,
) {
    use gpui_kit::test::TestWindowExt as _;
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    replace_input(&view, cx, "@not-a-reference");
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("ai-chat-add", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("ai-chat-upload").is_none(),
            "关闭浮层的点击不能穿透打开附件菜单"
        );
    });
    assert!(view.read_with(cx, |main, _| !main.ai_chat.reference_open));
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
}
