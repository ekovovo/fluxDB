// T035：压缩状态经真实事件链出现在当前会话，重进会话仍与持久化一致。
// 预置只写隔离 SQLite；面板状态一律由生产 Load 回桥生成，渲染走真实窗口。

const COMPACTION_NOTICE: &str = "已自动压缩上下文（达到上下文 35%）；原始历史与审计保持不变";

/// 在会话里预置一轮已完成任务 + 一条压缩状态 notice。
fn seed_compaction_session(fixture: &Fixture, session_id: &str, turn_id: &str, agent: &str) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::send(
                conn,
                100,
                true,
                session_id,
                agent,
                turn_id,
                &format!("{turn_id}-user"),
                "T035 persisted request",
                "{}",
                "{}",
            )?;
            sessions::update_turn_status(
                conn,
                101,
                turn_id,
                sessions::TurnStatus::Generating,
                None,
                sessions::load_turn(conn, turn_id)?.revision,
            )?;
            let assistant_id = format!("{turn_id}-assistant");
            conversation::checkpoint(
                conn,
                101,
                session_id,
                turn_id,
                &assistant_id,
                "T035 saved reply",
                "",
                None,
                None,
            )?;
            // 压缩状态是普通 notice 片段：kind/status/负载都与生产写入一致。
            conversation::upsert_event_part(
                conn,
                102,
                session_id,
                turn_id,
                &assistant_id,
                &format!("{assistant_id}-notice-compaction"),
                sessions::PartKind::Notice,
                sessions::PartStatus::Complete,
                &serde_json::json!({ "text": COMPACTION_NOTICE }).to_string(),
                None,
                None,
            )?;
            sessions::update_turn_status(
                conn,
                103,
                turn_id,
                sessions::TurnStatus::Completed,
                None,
                sessions::load_turn(conn, turn_id)?.revision,
            )?;
            Ok(())
        })
        .unwrap();
}

#[gpui::test]
fn ai_ui_compaction_status_is_visible_and_scoped_to_its_session(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let agent = fixture.agent(None);
    seed_compaction_session(&fixture, "compaction", "compaction-turn", &agent);
    seed_compaction_session(&fixture, "other", "other-turn", &agent);

    let mut controller = fixture.controller();
    assert!(matches!(
        controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
            session_id: Some("compaction".into())
        })),
        AppEvent::AiConversationLoaded(_)
    ));
    let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
    open_panel(&view, cx);

    // 压缩状态必须真的渲染成一个可命中的片段，而不是只存在数据库里。
    let id = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .find(|part| part.kind == "notice")
            .map(|part| part.id.clone())
            .expect("会话里应有压缩状态片段")
    });
    assert_eq!(id, "compaction-turn-assistant-notice-compaction");
    let selector: &'static str = Box::leak(format!("ai-part:{id}").into_boxed_str());
    draw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("压缩状态片段未渲染：{selector}"));
    assert!(
        bounds.size.width > px(0.) && bounds.size.height > px(0.),
        "压缩状态片段必须可见"
    );

    // 切到另一个会话：压缩状态不跟着串台。
    click_selector(cx, "ai-chat-history");
    click_selector(cx, "ai-session:other");
    wait_idle(&view, cx);
    let other_ids: Vec<String> = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter(|part| part.kind == "notice")
            .map(|part| part.id.clone())
            .collect()
    });
    assert_eq!(
        other_ids,
        vec!["other-turn-assistant-notice-compaction".to_string()],
        "另一个会话只能看到自己的压缩状态"
    );
}
