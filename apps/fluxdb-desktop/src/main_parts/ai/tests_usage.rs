use fluxdb_storage::ai_platform::usage_ledger::{self, UsageRequest, UsageTotals};

fn seed_usage_history() -> Fixture {
    let fixture = Fixture::seeded();
    fixture
        .storage
        .with_ai_connection(|conn| {
            let session = sessions::load_session(conn, "one")?;
            let agent = fluxdb_storage::ai_platform::agents::load_agent(conn, &session.agent_id)?;
            let model = agent.default_model_profile_id.as_deref().unwrap();
            let actual = |input, output, cached| {
                Some(fluxdb_core::Usage {
                    input_tokens: input,
                    output_tokens: output,
                    cached_input_tokens: cached,
                })
            };
            // 预置多个真实请求和轮次；缓存是输入的子集，且缺少价格不能污染实际计量。
            for (index, id, turn, new_session, requests) in [
                (
                    0,
                    "one",
                    "usage-one-first",
                    false,
                    vec![
                        ("main", actual(100, 20, 40)),
                        ("decision", actual(30, 10, 10)),
                    ],
                ),
                (
                    1,
                    "one",
                    "usage-one-second",
                    false,
                    vec![
                        ("compaction", actual(20, 5, 0)),
                        ("artifact", actual(10, 5, 0)),
                        ("title", actual(5, 2, 0)),
                    ],
                ),
                (
                    2,
                    "two",
                    "usage-two",
                    false,
                    vec![("main", actual(80, 20, 30)), ("title", None)],
                ),
                (
                    3,
                    "usage-unknown",
                    "usage-unknown-turn",
                    true,
                    vec![("main", None)],
                ),
                (
                    4,
                    "usage-zero",
                    "usage-zero-turn",
                    true,
                    vec![("main", actual(0, 0, 0))],
                ),
            ] {
                let now = 100 + index * 10;
                conversation::send(
                    conn,
                    now,
                    new_session,
                    id,
                    &session.agent_id,
                    turn,
                    &format!("{turn}-user"),
                    &format!("Persisted usage for {id}"),
                    "{}",
                    "{}",
                )?;
                for (request_index, (purpose, usage)) in requests.into_iter().enumerate() {
                    let request = UsageRequest {
                        request_id: format!("{turn}-request-{request_index}"),
                        session_id: id.into(),
                        turn_id: turn.into(),
                        model_profile_id: model.into(),
                        purpose: purpose.into(),
                        title: (purpose == "title" || purpose == "artifact")
                            .then(|| "Usage fixture artifact".into()),
                    };
                    assert!(usage_ledger::start_request(conn, now, &request)?);
                    assert!(usage_ledger::finish_request(
                        conn,
                        now + 1,
                        &request.request_id,
                        usage,
                    )?);
                }
                conversation::checkpoint(
                    conn,
                    now + 2,
                    id,
                    turn,
                    &format!("{turn}-assistant"),
                    &format!("Saved usage reply for {id}"),
                    "",
                    Some(sessions::TurnStatus::Completed),
                    None,
                )?;
            }
            let measurements = conn
                .prepare("SELECT measurement, COUNT(*) FROM ai_usage_ledger GROUP BY measurement")
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                measurements,
                vec![("actual".into(), 7), ("unknown".into(), 2)],
                "每次请求都已完成，不能残留 pending 或按价格把 actual 改为 unknown"
            );
            Ok(())
        })
        .unwrap();
    // send 会更新最后会话；预置完成后通过生产 Load 恢复明确的初始入口。
    assert!(matches!(
        fixture
            .controller()
            .dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
                session_id: Some("one".into())
            })),
        AppEvent::AiConversationLoaded(_)
    ));
    fixture
}

async fn wait_usage_loaded(
    view: &Entity<NavicatMain>,
    cx: &mut VisualTestContext,
    session: Option<&str>,
) {
    let handle = cx.window_handle();
    cx.cx
        .wait_for(handle, std::time::Duration::from_secs(3), |window, app| {
            let main = view.read(app);
            main.ai_chat.open
                && !main.ai_chat.loading
                && main.ai_chat._io.is_none()
                && main.ai_chat.pending.is_empty()
                && main
                    .controller
                    .state()
                    .ai_conversation
                    .session_id
                    .as_deref()
                    == session
                && window.try_find("ai-chat-tokens").is_some()
        })
        .await;
    assert!(
        view.read_with(cx, |main, _| main.ai_chat.error.is_none()),
        "生产会话加载不能失败"
    );
}

fn assert_usage_header(
    view: &Entity<NavicatMain>,
    cx: &mut VisualTestContext,
    label: &str,
    expected: UsageTotals,
) {
    let bounds = cx.update(|window, app| {
        window.render_frame(app);
        let tokens = window.find("ai-chat-tokens");
        assert!(tokens.visible());
        assert!(tokens.bounds().size.width > px(0.));
        assert_eq!(tokens.label(), Some(label));
        assert_eq!(
            view.read(app).controller.state().ai_conversation.usage,
            expected,
            "显示必须来自生产 Load 的会话累计量"
        );
        tokens.bounds()
    });
    let header = cx.debug_bounds("ai-chat-header").unwrap();
    assert!(
        bounds.top() >= header.top() && bounds.bottom() <= header.bottom(),
        "token 用量必须留在顶部，而不是回到输入区"
    );
}

async fn select_usage_history(
    view: &Entity<NavicatMain>,
    cx: &mut VisualTestContext,
    session: &str,
) {
    cx.update(|window, app| {
        window.render_frame(app);
        window.click("ai-chat-history", app);
    });
    cx.update(|window, app| {
        window.render_frame(app);
        window.click(SharedString::from(format!("ai-session:{session}")), app);
    });
    wait_usage_loaded(view, cx, Some(session)).await;
    assert!(
        !view.read_with(cx, |main, _| main.ai_chat.history),
        "选择历史后必须返回会话"
    );
}

#[gpui_kit::test]
async fn t030_tokens_initial_empty_light(cx: &mut TestAppContext) {
    check_tokens_initial_empty(cx, ThemeMode::Light).await;
}

#[gpui_kit::test]
async fn t030_tokens_initial_empty_dark(cx: &mut TestAppContext) {
    check_tokens_initial_empty(cx, ThemeMode::Dark).await;
}

async fn check_tokens_initial_empty(cx: &mut TestAppContext, theme: ThemeMode) {
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), theme);
    cx.update(|window, app| {
        window.render_frame(app);
        window.click("ask-ai", app);
    });
    wait_usage_loaded(&view, cx, None).await;
    assert_usage_header(&view, cx, "0 tokens", UsageTotals::default());
    assert_eq!(fixture.counts(), (0, 0, 0, 0));
    cx.update(|window, _| window.remove_window());
}

#[gpui_kit::test]
async fn t030_tokens_history_actual_unknown_and_new_light(cx: &mut TestAppContext) {
    check_tokens_history(cx, ThemeMode::Light).await;
}

#[gpui_kit::test]
async fn t030_tokens_history_actual_unknown_and_new_dark(cx: &mut TestAppContext) {
    check_tokens_history(cx, ThemeMode::Dark).await;
}

async fn check_tokens_history(cx: &mut TestAppContext, theme: ThemeMode) {
    cx.executor().allow_parking();
    let fixture = seed_usage_history();
    let before = fixture.counts();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), theme);
    cx.update(|window, app| {
        window.render_frame(app);
        window.click("ask-ai", app);
    });
    wait_usage_loaded(&view, cx, Some("one")).await;
    let actual = UsageTotals {
        requests: 5,
        input_tokens: 165,
        output_tokens: 42,
        total_tokens: 207,
        cached_input_tokens: 50,
        unknown_requests: 0,
    };
    assert_usage_header(&view, cx, "207 tokens", actual.clone());
    select_usage_history(&view, cx, "two").await;
    assert_usage_header(
        &view,
        cx,
        "100 tokens（部分未知）",
        UsageTotals {
            requests: 2,
            input_tokens: 80,
            output_tokens: 20,
            total_tokens: 100,
            cached_input_tokens: 30,
            unknown_requests: 1,
        },
    );
    assert_eq!(
        fixture.preferences().last_session_id.as_deref(),
        Some("two")
    );
    select_usage_history(&view, cx, "usage-unknown").await;
    assert_usage_header(
        &view,
        cx,
        "Token 用量未知",
        UsageTotals {
            requests: 1,
            unknown_requests: 1,
            ..UsageTotals::default()
        },
    );
    select_usage_history(&view, cx, "usage-zero").await;
    assert_usage_header(
        &view,
        cx,
        "0 tokens",
        UsageTotals {
            requests: 1,
            ..UsageTotals::default()
        },
    );
    select_usage_history(&view, cx, "one").await;
    assert_usage_header(&view, cx, "207 tokens", actual.clone());
    cx.update(|window, app| {
        window.render_frame(app);
        window.click("ai-chat-new", app);
    });
    wait_usage_loaded(&view, cx, None).await;
    assert_usage_header(&view, cx, "0 tokens", UsageTotals::default());
    assert!(fixture.preferences().last_session_id.is_none());
    assert!(view.read_with(cx, |main, _| {
        main.controller.state().ai_conversation.messages.is_empty()
    }));
    assert_eq!(
        fixture.counts(),
        before,
        "新建只清空面板，不创建空历史或改写消息"
    );
    // 返回同一历史必须仍为 207，而不是随 reload 重复累加或随新会话清掉旧账本。
    select_usage_history(&view, cx, "one").await;
    assert_usage_header(&view, cx, "207 tokens", actual.clone());
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert_eq!(usage_ledger::totals(conn, "one")?, actual);
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.counts(), before);
    cx.update(|window, _| window.remove_window());
}
