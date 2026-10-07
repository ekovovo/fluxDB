fn execution_part(
    id: &str,
    kind: &str,
    status: &str,
    call: &str,
) -> fluxdb_app::AiConversationPart {
    fluxdb_app::AiConversationPart {
        id: id.into(),
        kind: kind.into(),
        status: status.into(),
        payload_json: serde_json::json!({"tool":"db.query","tool_call_id":call,"duration_ms":2})
            .to_string(),
    }
}

#[test]
fn compact_execution_matches_call_identity_and_preserves_errors() {
    let parts = vec![
        execution_part("call-a", "tool_call_ref", "interrupted", "a"),
        execution_part("call-b", "tool_call_ref", "streaming", "b"),
        execution_part("result-b", "tool_result_ref", "error", "b"),
        execution_part("result-a", "tool_result_ref", "complete", "a"),
        execution_part("orphan", "tool_result_ref", "complete", "c"),
    ];
    let merged = ai_compact_parts(&parts);
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0].id, "call-a");
    assert_eq!(merged[0].status, "complete");
    assert!(!ai_execution_needs_attention(&merged[0]));
    assert_eq!(merged[1].status, "error");
    assert!(ai_execution_needs_attention(&merged[1]));
    assert_eq!(merged[2].id, "orphan");
    let mut unknown = execution_part("unknown", "tool_call_ref", "complete", "d");
    unknown.payload_json = r#"{"execution_status":"unknown"}"#.into();
    assert!(ai_execution_needs_attention(&unknown));
    let mut unlinked = execution_part("unlinked", "tool_call_ref", "streaming", "e");
    unlinked.payload_json = "{}".into();
    assert_eq!(ai_compact_parts(&[unlinked.clone()]), vec![unlinked]);
}

#[gpui::test]
fn execution_summary_click_expands_and_collapses(cx: &mut TestAppContext) {
    check_execution_summary_clicks(cx, ThemeMode::Light);
}

#[gpui::test]
fn execution_summary_click_expands_and_collapses_dark(cx: &mut TestAppContext) {
    check_execution_summary_clicks(cx, ThemeMode::Dark);
}

fn check_execution_summary_clicks(cx: &mut TestAppContext, mode: ThemeMode) {
    let fixture = Fixture::new();
    seed_regular_parts(&fixture, "regular");
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), mode);
    open_panel(&view, cx);
    assert!(
        cx.debug_bounds("ai-part:regular-assistant-tool-call")
            .is_none()
    );
    click_selector(cx, "execution:regular-assistant");
    wait_idle(&view, cx);
    assert!(
        cx.debug_bounds("ai-part:regular-assistant-tool-call")
            .is_some()
    );
    assert!(
        cx.debug_bounds("ai-part:regular-assistant-tool-result")
            .is_none()
    );
    click_selector(cx, "ai-part:regular-assistant-tool-call");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .expanded_parts
            .contains(&"regular-assistant-tool-call".into())
    }));
    click_selector(cx, "execution:regular-assistant");
    wait_idle(&view, cx);
    assert!(
        cx.debug_bounds("ai-part:regular-assistant-tool-call")
            .is_none()
    );
}

#[gpui::test]
fn recovery_notice_details_toggle_without_warning_card(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    recovery_seed_stream(&fixture, None);
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    assert!(
        cx.debug_bounds("ai-recovery:recovery-turn-restart-notice")
            .is_some()
    );
    assert!(
        cx.debug_bounds("ai-part-detail:recovery-turn-restart-notice")
            .is_none()
    );
    click_selector(cx, "ai-part:recovery-turn-restart-notice");
    wait_idle(&view, cx);
    assert!(
        cx.debug_bounds("ai-part-detail:recovery-turn-restart-notice")
            .is_some()
    );
    click_selector(cx, "ai-part:recovery-turn-restart-notice");
    wait_idle(&view, cx);
    assert!(
        cx.debug_bounds("ai-part-detail:recovery-turn-restart-notice")
            .is_none()
    );
}

#[gpui::test]
fn composer_controls_are_compact_and_cost_is_in_header(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    let header = cx.debug_bounds("ai-chat-header").unwrap();
    let cost = cx.debug_bounds("ai-chat-cost").unwrap();
    assert!(cost.top() >= header.top() && cost.bottom() <= header.bottom());
    let model = cx.debug_bounds("ai-chat-model-select").unwrap();
    assert!(
        model.size.width < px(180.),
        "默认模型按钮不应保留表单固定宽度"
    );
    click_selector(cx, "ai-chat-add");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    let reference = cx.debug_bounds("ai-chat-reference-select").unwrap();
    let composer = cx.debug_bounds("ai-chat-composer").unwrap();
    assert!(
        reference.bottom() <= composer.top(),
        "reference={reference:?} composer={composer:?}"
    );
    assert!(reference.left() >= composer.left() && reference.right() <= composer.right());
}

#[gpui::test]
fn chat_enter_adds_newline_grows_and_caps_at_half_panel(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    replace_input(&view, cx, "第一行");
    draw(cx);
    let initial = cx.debug_bounds("ai-chat-input").unwrap().size.height;
    cx.simulate_keystrokes("enter");
    cx.simulate_input("第二行");
    assert_eq!(input_value(&view, cx), "第一行\n第二行");
    for _ in 0..5 {
        cx.simulate_keystrokes("enter");
        cx.simulate_input("更多内容");
    }
    cx.run_until_parked();
    draw(cx);
    assert!(cx.debug_bounds("ai-chat-input").unwrap().size.height > initial);
    for _ in 0..60 {
        cx.simulate_keystrokes("enter");
        cx.simulate_input("继续输入内容");
    }
    cx.run_until_parked();
    draw(cx);
    let panel = cx.debug_bounds("ai-chat-panel").unwrap();
    let composer = cx.debug_bounds("ai-chat-composer").unwrap();
    assert!(
        composer.size.height <= panel.size.height * 0.5,
        "输入区不得超过面板的一半：{composer:?}, {panel:?}"
    );
    let send = cx.debug_bounds("ai-chat-send").unwrap();
    assert!(send.bottom() <= panel.bottom());
    assert!(view.read_with(cx, |main, _| {
        main.controller.state().ai_conversation.messages.is_empty()
    }));
    assert!(
        view.read_with(cx, |main, cx| main.ai_chat.input.read(cx).scroll_offset().y
            < px(0.)),
        "达到上限后编辑器应内部滚动，而不是裁掉内容"
    );
    cx.simulate_resize(size(px(900.), px(600.)));
    cx.run_until_parked();
    draw(cx);
    let panel = cx.debug_bounds("ai-chat-panel").unwrap();
    let composer = cx.debug_bounds("ai-chat-composer").unwrap();
    assert!(
        composer.size.height <= panel.size.height * 0.5,
        "缩小窗口后也应重新计算上限"
    );
}

#[gpui::test]
fn chat_reference_list_is_above_input_and_keyboard_accepts(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::knowledge::save(
                conn,
                10,
                "k-keyboard",
                "测试键盘引用",
                "metric",
                None,
                None,
                None,
                "{}",
                "confirmed",
                None,
            )?;
            fluxdb_storage::ai_platform::knowledge::save(
                conn,
                11,
                "k-mouse",
                "测试键盘引用二",
                "metric",
                None,
                None,
                None,
                "{}",
                "confirmed",
                None,
            )?;
            Ok(())
        })
        .unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    replace_input(&view, cx, "@测试键盘");
    cx.run_until_parked();
    draw(cx);
    let list = cx.debug_bounds("ai-chat-reference-select").unwrap();
    let composer = cx.debug_bounds("ai-chat-composer").unwrap();
    assert!(
        list.bottom() <= composer.top(),
        "list={list:?} composer={composer:?}"
    );
    let expected = view.read_with(cx, |main, _| main.ai_chat.reference_options[1].id.clone());
    cx.simulate_keystrokes("down enter");
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "");
    assert_eq!(
        view.read_with(cx, |main, _| main
            .controller
            .state()
            .ai_conversation
            .context_references
            .len()),
        1
    );
    assert_eq!(
        view.read_with(cx, |main, _| main
            .controller
            .state()
            .ai_conversation
            .context_references[0]
            .id
            .clone()),
        expected
    );
    assert!(cx.debug_bounds("ai-chat-reference-select").is_none());
    assert!(view.read_with(cx, |main, _| {
        main.controller.state().ai_conversation.messages.is_empty()
    }));
    replace_input(&view, cx, "@测试键盘");
    draw(cx);
    click_selector(cx, "ai-reference-candidate:knowledge:k-keyboard");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .context_references
            .iter()
            .any(|r| r.id == "knowledge:k-keyboard")
    }));
    replace_input(&view, cx, "@测试键盘");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("ai-chat-reference-select").is_none());
    assert!(view.read_with(cx, |main, _| main.ai_chat.open));
    replace_input(&view, cx, "@测试键盘");
    click_selector(cx, "ai-chat-timeline");
    draw(cx);
    assert!(cx.debug_bounds("ai-chat-reference-select").is_none());
}

#[gpui::test]
fn selected_reference_chip_stays_visible_above_textarea(cx: &mut TestAppContext) {
    check_selected_reference_visibility(cx, ThemeMode::Light);
}

#[gpui::test]
fn selected_reference_chip_stays_visible_above_textarea_dark(cx: &mut TestAppContext) {
    check_selected_reference_visibility(cx, ThemeMode::Dark);
}

fn check_selected_reference_visibility(cx: &mut TestAppContext, mode: ThemeMode) {
    let fixture = Fixture::new();
    fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::knowledge::save(
                conn,
                10,
                "k-visible",
                "已选引用标签",
                "metric",
                None,
                None,
                None,
                "{}",
                "confirmed",
                None,
            )?;
            Ok(())
        })
        .unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), mode);
    open_panel(&view, cx);
    cx.simulate_resize(size(px(900.), px(600.)));
    cx.run_until_parked();
    replace_input(&view, cx, "@已选引用标签");
    cx.simulate_keystrokes("enter");
    wait_idle(&view, cx);
    assert_eq!(
        view.read_with(cx, |main, _| main
            .controller
            .state()
            .ai_conversation
            .context_references
            .len()),
        1
    );
    let assert_visible = |cx: &mut VisualTestContext| {
        draw(cx);
        let area = cx.debug_bounds("ai-chat-references-scroll").unwrap();
        let chip = cx.debug_bounds("ai-reference:knowledge:k-visible").unwrap();
        let input = cx.debug_bounds("ai-chat-input").unwrap();
        assert!(area.size.height >= px(28.), "引用区不能塌缩: {area:?}");
        assert!(
            chip.size.height >= px(24.),
            "应断言完整标签，不是关闭按钮: {chip:?}"
        );
        assert!(
            chip.top() >= area.top() && chip.bottom() <= area.bottom(),
            "标签被裁剪: chip={chip:?} area={area:?}"
        );
        assert!(area.bottom() <= input.top(), "标签应位于输入框上方");
    };
    assert_visible(cx);
    replace_input(&view, cx, &"多行草稿\n".repeat(50));
    cx.run_until_parked();
    assert_visible(cx);
    click_selector(cx, "ai-reference-remove:knowledge:k-visible");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    }));
    assert!(cx.debug_bounds("ai-chat-references-scroll").is_none());
}
