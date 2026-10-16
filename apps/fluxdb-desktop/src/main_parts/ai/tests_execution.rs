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

#[test]
fn compact_approval_hides_only_linked_superseded_notice() {
    let approval = fluxdb_app::AiConversationPart {
        id: "approval".into(), kind: "notice".into(), status: "complete".into(),
        payload_json: r#"{"approval_id":"a","decision":"denied"}"#.into(),
    };
    let paused = fluxdb_app::AiConversationPart {
        id: "paused".into(), kind: "notice".into(), status: "interrupted".into(),
        payload_json: r#"{"approval_id":"a","superseded_by":"approval"}"#.into(),
    };
    assert_eq!(ai_compact_parts(&[approval.clone(), paused.clone()]), vec![approval.clone()]);
    assert_eq!(ai_compact_parts(&[paused.clone()]), vec![paused.clone()]);
    let mut unrelated = paused;
    unrelated.payload_json = r#"{"approval_id":"b","superseded_by":"approval"}"#.into();
    assert_eq!(ai_compact_parts(&[approval.clone(), unrelated.clone()]), vec![approval, unrelated]);
}

#[test]
fn execution_timeline_interleaves_text_and_keeps_call_start_position() {
    let mut first = execution_part("first", "tool_call_ref", "streaming", "a");
    first.payload_json = serde_json::json!({"tool_call_id":"a","text_offset":"先查询。\n".len()}).to_string();
    let mut second = execution_part("second", "tool_call_ref", "complete", "b");
    second.payload_json = serde_json::json!({"text_offset":"先查询。\n再检查。\n".len()}).to_string();
    let mut result = execution_part("result", "tool_result_ref", "complete", "a");
    result.payload_json = serde_json::json!({"tool_call_id":"a","text_offset":"先查询。\n再检查。\n".len()}).to_string();
    let body = fluxdb_app::AiConversationPart {
        id: "body".into(), kind: "text".into(), status: "complete".into(),
        payload_json: serde_json::json!({"text":"先查询。\n再检查。\n最终结论。"}).to_string(),
    };
    let compact = ai_compact_parts(&[first, result, second, body.clone()]);
    let ordered = ai_timeline_parts(&compact, None);
    assert_eq!(ordered.iter().map(|part| part.kind.as_str()).collect::<Vec<_>>(),
        ["text", "tool_call_ref", "text", "tool_call_ref", "text"]);
    assert_eq!(ai_chat_part_text(&ordered[0]), "先查询。\n");
    assert_eq!(ai_chat_part_text(&ordered[2]), "再检查。\n");
    assert_eq!(ai_chat_part_text(&ordered[4]), "最终结论。");
    assert_eq!(ai_part_text_offset(&compact[0]), Some("先查询。\n".len()));
    let partial = ai_timeline_parts(&compact, Some("先查"));
    assert_eq!(partial.iter().filter(|part| part.kind == "text").map(ai_chat_part_text).collect::<String>(), "先查");
    let revealed = ai_timeline_parts(&compact, Some("先查询。\n再检查。\n最"));
    assert_eq!(revealed[2].id, ordered[2].id, "流式拆段身份必须稳定");
    let legacy = vec![body, execution_part("legacy", "tool_call_ref", "complete", "old")];
    assert_eq!(ai_timeline_parts(&legacy, None), legacy, "旧记录不猜测事件时点");
    for offset in [1, usize::MAX] {
        let mut corrupt = legacy.clone();
        corrupt[1].payload_json = serde_json::json!({"text_offset":offset}).to_string();
        assert_eq!(ai_timeline_parts(&corrupt, None), corrupt, "越界/非 UTF-8 边界不应拆段或丢正文");
    }
}

#[gpui::test]
fn execution_timeline_groups_only_adjacent_operations_light(cx: &mut TestAppContext) {
    check_execution_timeline(cx, ThemeMode::Light);
}

#[gpui::test]
fn execution_timeline_groups_only_adjacent_operations_dark(cx: &mut TestAppContext) {
    check_execution_timeline(cx, ThemeMode::Dark);
}

fn check_execution_timeline(cx: &mut TestAppContext, theme: ThemeMode) {
    let fixture = Fixture::new();
    seed_regular_parts(&fixture, "regular");
    fixture.storage.with_ai_connection(|conn| {
        let part = sessions::load_part(conn, "regular-assistant-tool-call")?;
        sessions::update_part(conn, 110, &part.id, sessions::PartStatus::Complete,
            &serde_json::json!({"tool":"db.query","text_offset":"操作前的说明。\n".len()}).to_string(), part.revision)?;
        sessions::create_tool_call(conn, 120, "regular-second-call", "regular", "regular-turn",
            2, 1, "second", "db.query", 1, None, "{}", "{}", "second-plan", "succeeded")?;
        conversation::upsert_event_part(conn, 120, "regular", "regular-turn", "regular-assistant",
            "regular-assistant-second", sessions::PartKind::ToolCallRef, sessions::PartStatus::Complete,
            &serde_json::json!({"tool":"db.query","text_offset":"操作前的说明。\n操作后的说明。\n".len()}).to_string(), Some("regular-second-call"), None)?;
        conversation::upsert_event_part(conn, 120, "regular", "regular-turn", "regular-assistant",
            "regular-assistant-error", sessions::PartKind::Notice, sessions::PartStatus::Error,
            &serde_json::json!({"text":"检查失败","text_offset":"操作前的说明。\n操作后的说明。\n".len()}).to_string(), None, None)?;
        conversation::checkpoint(conn, 130, "regular", "regular-turn", "regular-assistant",
            "操作前的说明。\n操作后的说明。\n最终回复。", "", Some(sessions::TurnStatus::Completed), None)
    }).unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), theme);
    open_panel(&view, cx);
    let before = cx.debug_bounds("regular-assistant-text:markdown").unwrap();
    let operation = cx.debug_bounds("execution:regular-assistant").unwrap();
    let after = cx.debug_bounds("regular-assistant-text:segment:22:markdown").unwrap();
    let second = cx.debug_bounds("execution:regular-assistant:regular-assistant-second").unwrap();
    let error = cx.debug_bounds("ai-part:regular-assistant-error").unwrap();
    assert!(before.bottom() <= operation.top() && operation.bottom() <= after.top());
    assert!(after.bottom() <= second.top() && second.bottom() <= error.top(), "不能跨正文合并操作，错误卡也应保留在真实时点");
    click_selector(cx, "execution:regular-assistant");
    wait_idle(&view, cx);
    assert!(cx.debug_bounds("ai-part:regular-assistant-tool-call").is_some());
    assert!(cx.debug_bounds("ai-part:regular-assistant-second").is_none(), "展开第一组不能展开第二组");
    click_selector(cx, "execution:regular-assistant:regular-assistant-second");
    wait_idle(&view, cx);
    assert!(cx.debug_bounds("ai-part:regular-assistant-second").is_some());
    click_selector(cx, "ai-part:regular-assistant-error");
    wait_idle(&view, cx);
    assert!(cx.debug_bounds("ai-part-detail:regular-assistant-error").is_some());
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

#[gpui_kit::test]
fn composer_controls_are_compact_and_tokens_are_in_header(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    cx.update(|window, cx| window.render_frame(cx));
    let header = cx.debug_bounds("ai-chat-header").unwrap();
    let tokens = cx.debug_bounds("ai-chat-tokens").unwrap();
    assert!(tokens.top() >= header.top() && tokens.bottom() <= header.bottom());
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
                &fluxdb_storage::ai_platform::knowledge::KnowledgeEntryEdit {
                    id: "k-keyboard".into(),
                    title: "测试键盘引用".into(),
                    kind: "metric".into(),
                    agent_id: None,
                    connection_id: None,
                    database_name: None,
                    namespace: None,
                    scope_json: "{}".into(),
                    content_json: "{}".into(),
                    source_json: "{}".into(),
                    status: "confirmed".into(),
                },
                None,
            )?;
            fluxdb_storage::ai_platform::knowledge::save(
                conn,
                11,
                &fluxdb_storage::ai_platform::knowledge::KnowledgeEntryEdit {
                    id: "k-mouse".into(),
                    title: "测试键盘引用二".into(),
                    kind: "metric".into(),
                    agent_id: None,
                    connection_id: None,
                    database_name: None,
                    namespace: None,
                    scope_json: "{}".into(),
                    content_json: "{}".into(),
                    source_json: "{}".into(),
                    status: "confirmed".into(),
                },
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
                &fluxdb_storage::ai_platform::knowledge::KnowledgeEntryEdit {
                    id: "k-visible".into(),
                    title: "已选引用标签".into(),
                    kind: "metric".into(),
                    agent_id: None,
                    connection_id: None,
                    database_name: None,
                    namespace: None,
                    scope_json: "{}".into(),
                    content_json: "{}".into(),
                    source_json: "{}".into(),
                    status: "confirmed".into(),
                },
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


/// 时间线工具卡显示审计判定（设计 §31.2）：判定写进片段后卡上出现"安全审计：…"一行。
#[gpui::test]
fn execution_timeline_shows_audit_decision_on_tool_card(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    seed_regular_parts(&fixture, "auditline");
    // 工具卡渲染时会用结果片段合并覆盖调用片段，因此判定要像真实链路一样写在两端。
    fixture
        .storage
        .with_ai_connection(|conn| {
            for id in ["auditline-assistant-tool-call", "auditline-assistant-tool-result"] {
                let part = sessions::load_part(conn, id)?;
                sessions::update_part(
                    conn,
                    110,
                    &part.id,
                    sessions::PartStatus::Complete,
                    &serde_json::json!({
                        "tool": "db.execute",
                        "text_offset": "解释这段 SQL".len(),
                        "audit_decision": "deny",
                        "audit_rule_ids": ["denied_tables"],
                        "audit_reason_codes": ["denied_table:orders"],
                    })
                    .to_string(),
                    part.revision,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    // 工具卡要展开操作分组后才渲染，先点开分组。
    click_selector(cx, "execution:auditline-assistant");
    wait_idle(&view, cx);
    // 工具卡在展开后显示审计判定行（判定是最该一眼看到的信息）。
    assert!(
        cx.debug_bounds("ai-part-audit:auditline-assistant-tool-call").is_some(),
        "工具卡应显示审计判定行"
    );
}
