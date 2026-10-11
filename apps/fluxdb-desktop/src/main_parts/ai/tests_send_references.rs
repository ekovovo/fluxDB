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

#[gpui::test]
async fn editor_context_follows_current_tab_not_stale_panel_snapshot(cx: &mut TestAppContext) {
    // 缺陷复现：`context_tab` 只在打开 AI 面板那一刻记录一次。面板开着时切换到
    // 另一个查询页签再发送，界面上下文仍来自旧页签（或为空），模型拿到错误的上下文。
    // 设计 §13 要求捕获"当前"界面；这里断言发送时取的是当前活动页签。
    use fluxdb_app::AppCommand;
    use fluxdb_core::ConnectionId;

    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    // 两个查询页签，内容明显不同，便于区分。
    let mut tab_ids = Vec::new();
    for text in ["SELECT 'TAB-ONE';", "SELECT 'TAB-TWO';"] {
        controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
            connection_id: ConnectionId(1),
            database: Some("main".into()),
            schema: None,
        });
        let id = controller.state().active_tab.unwrap();
        controller.dispatch(AppCommand::UpdateQueryText {
            tab_id: id,
            text: text.into(),
        });
        tab_ids.push(id);
    }
    let (first_tab, second_tab) = (tab_ids[0], tab_ids[1]);
    // 打开面板前停留在第一个页签，让 context_tab 记下它。
    controller.dispatch(AppCommand::ActivateTab(first_tab));
    let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
    open_panel(&view, cx);

    // 面板已打开的情况下切到第二个页签——这正是缺陷场景。
    let event = view.update(cx, |main, cx| {
        main.controller.dispatch(AppCommand::ActivateTab(second_tab))
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    wait_idle(&view, cx);
    let _ = event;

    // 走生产捕获路径：让宿主把本轮上下文写进运行快照，再核对快照内容。
    let job = view.update(cx, |main, cx| {
        let context = main.capture_ai_active_context(cx);
        let pin = fluxdb_app::pin_target(&context);
        let text = context
            .selected_text
            .or_else(|| context.excerpt.map(|e| e.text))
            .unwrap_or_default();
        let job = main
            .controller
            .prepare_ai_conversation_context("capture follows tab", &pin, &text)
            .expect("准备发送");
        (job, text)
    });
    let (job, text) = job;
    assert!(
        text.contains("TAB-TWO"),
        "摘录必须来自当前活动页签内容，实际: {text}"
    );
    assert!(
        !text.contains("TAB-ONE"),
        "不得残留旧页签内容，实际: {text}"
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            let ui = snapshot["ui_context"].as_str().unwrap_or_default();
            assert!(
                ui.contains("TAB-TWO"),
                "快照里的界面摘录必须是当前页签，实际: {ui}"
            );
            assert!(
                !ui.contains("TAB-ONE"),
                "快照不得残留旧页签内容，实际: {ui}"
            );
            Ok(())
        })
        .unwrap();
}
