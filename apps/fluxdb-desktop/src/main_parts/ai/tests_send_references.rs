fn seed_send_reference(fixture: &Fixture) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::knowledge::save(
                conn,
                10,
                "k-send",
                "发送引用测试",
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
}

#[gpui::test]
async fn send_consumes_draft_references_but_keeps_message_tags(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    seed_send_reference(&fixture);
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    replace_input(&view, cx, "@发送引用测试");
    cx.simulate_keystrokes("enter");
    wait_idle(&view, cx);
    assert!(cx.debug_bounds("ai-reference:knowledge:k-send").is_some());
    let job = start_scripted_send(&fixture, &view, cx, "解释该引用");
    assert!(cx.debug_bounds("ai-chat-references-scroll").is_none());
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    }));
    let part_id = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .last()
            .unwrap()
            .parts
            .iter()
            .find(|part| part.kind == "context_ref")
            .unwrap()
            .id
            .clone()
    });
    // 当前 GPUI 的测试查询只接受静态 selector；仅泄漏本测试的一个 fixture ID。
    let selector: &'static str =
        Box::leak(format!("ai-message-reference:{part_id}").into_boxed_str());
    assert!(cx.debug_bounds(selector).is_some());
    wait_source(&view, cx, "reference send completion", |main| {
        main.ai_chat.running.is_none()
    })
    .await;
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    open_panel(&view, cx);
    assert!(cx.debug_bounds("ai-chat-references-scroll").is_none());
    assert!(cx.debug_bounds(selector).is_some());
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            assert_eq!(snapshot["context_references"][0]["id"], "knowledge:k-send");
            assert!(
                fluxdb_storage::ai_platform::context_references::list(conn, Some("one"))?
                    .is_empty()
            );
            Ok(())
        })
        .unwrap();
}

#[gpui::test]
fn failed_send_preserves_prompt_and_reference_chip(cx: &mut TestAppContext) {
    let fixture = Fixture::seeded();
    seed_send_reference(&fixture);
    fixture.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER fail_send_ref BEFORE INSERT ON ai_turns BEGIN SELECT RAISE(ABORT,'send failure'); END;").unwrap();
        Ok(())
    }).unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    replace_input(&view, cx, "@发送引用测试");
    cx.simulate_keystrokes("enter");
    wait_idle(&view, cx);
    replace_input(&view, cx, "发送失败必须保留");
    click_selector(cx, "ai-chat-send");
    wait_idle(&view, cx);
    assert_eq!(input_value(&view, cx), "发送失败必须保留");
    assert!(cx.debug_bounds("ai-reference:knowledge:k-send").is_some());
    assert!(view.read_with(cx, |main, _| main.ai_chat.error.is_some()
        && main.ai_chat.running.is_none()));
    assert_eq!(
        view.read_with(cx, |main, _| main
            .controller
            .state()
            .ai_conversation
            .context_references
            .len()),
        1
    );
}
