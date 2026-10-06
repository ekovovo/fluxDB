// T029：只预置隔离 SQLite；恢复一律由生产 NavicatMain 首帧触发，交互一律真实点击。
// 与 tests.rs / tests_approval.rs 同作用域，复用 Fixture、生产装配及鼠标/输入 helpers。

fn recovery_seed_stream(fixture: &Fixture, tool: Option<&str>) {
    let agent = fixture.agent(None); // 不绑定模型，禁止 fixture 发出恢复网络请求。
    fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::send(
                conn,
                100,
                true,
                "recovery",
                &agent,
                "recovery-turn",
                "recovery-user",
                "T029 persisted request",
                "{}",
                "{}",
            )?;
            let turn = sessions::load_turn(conn, "recovery-turn")?;
            sessions::update_turn_status(
                conn,
                101,
                &turn.id,
                sessions::TurnStatus::Generating,
                None,
                turn.revision,
            )?;
            conversation::checkpoint(
                conn,
                101,
                "recovery",
                "recovery-turn",
                "recovery-assistant",
                "T029 saved partial stream",
                "T029 saved public summary",
                None,
                None,
            )?;
            if let Some(tool) = tool {
                sessions::create_tool_call(
                    conn,
                    102,
                    "recovery-call",
                    "recovery",
                    "recovery-turn",
                    1,
                    0,
                    "provider-recovery-call",
                    tool,
                    1,
                    None,
                    r#"{"sql":"SELECT 1"}"#,
                    "{}",
                    "recovery-plan",
                    "running",
                )?;
                let turn = sessions::load_turn(conn, "recovery-turn")?;
                sessions::update_turn_status(
                    conn,
                    103,
                    &turn.id,
                    sessions::TurnStatus::ExecutingTools,
                    None,
                    turn.revision,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mut controller = fixture.controller();
    assert!(matches!(
        controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
            session_id: Some("recovery".into()),
        })),
        AppEvent::AiConversationLoaded(_)
    ));
    assert!(matches!(
        controller.dispatch(AppCommand::AiConversation(AiConversationCommand::SaveUi {
            draft: "T029 saved unsent draft".into(),
            expanded_parts: vec!["recovery-assistant-summary".into()],
            mode: settings::PanelMode::Drawer,
            drawer_width: 460.,
            embedded_width: 420.,
        })),
        AppEvent::AiConversationLoaded(_)
    ));
}

fn recovery_assert_notice(
    view: &Entity<NavicatMain>,
    cx: &mut VisualTestContext,
    status: &str,
    selector: &'static str,
) -> fluxdb_app::AiConversationView {
    let state = view.read_with(cx, |main, _| {
        let state = main.controller.state().ai_conversation.clone();
        assert_eq!(
            render_state_snapshot(main.controller.state()).ai_conversation,
            state
        );
        assert!(
            !main.ai_chat.recovery_pending && !main.ai_chat.recovery_failed,
            "startup state: pending={}, failed={}, error={:?}, loading={}, io={}, view={:?}",
            main.ai_chat.recovery_pending,
            main.ai_chat.recovery_failed,
            main.ai_chat.error,
            main.ai_chat.loading,
            main.ai_chat._io.is_some(),
            state
        );
        assert!(
            main.ai_chat.running.is_none()
                && main.ai_chat.cancel.is_none()
                && main.ai_chat._pump.is_none(),
            "恢复扫描/Load 不能偷偷启动执行器或事件 source"
        );
        assert!(main.controller.state().connections.is_empty());
        state
    });
    let notices = state
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| {
            part.kind == "notice"
                && serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()
                    ["recovery_status"]
                    == status
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "恢复公告不能丢失或重复");
    assert_eq!(notices[0].status, "complete");
    let payload: serde_json::Value = serde_json::from_str(&notices[0].payload_json).unwrap();
    assert!(payload["text"].as_str().unwrap().contains("应用已重启"));
    // selector 属于生产 Alert 的容器，不是孤立组件或手工 Apply 的假恢复视图。
    let bounds = cx
        .debug_bounds(selector)
        .expect("恢复 notice 必须渲染实际 Alert");
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    assert!(cx.debug_bounds("ai-chat-send").is_some());
    assert!(
        cx.debug_bounds("ai-chat-stop").is_none(),
        "不能把恢复任务显示为仍在生成"
    );
    state
}

fn recovery_assert_call(fixture: &Fixture, id: &str, expected: &str, expected_started: i64) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let (status, receipt, started): (String, Option<String>, Option<i64>) = conn
                .query_row(
                    "SELECT status,receipt_json,started_at_ms FROM ai_tool_calls WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(status, expected);
            assert!(receipt.is_none(), "扫描/打开面板不能伪造执行回执");
            assert_eq!(
                started,
                Some(expected_started),
                "恢复不能改写原调用启动时间"
            );
            let calls: i64 = conn
                .query_row("SELECT COUNT(*) FROM ai_tool_calls", [], |row| row.get(0))
                .unwrap();
            assert_eq!(calls, 1, "恢复/按钮点击不能新建第二次工具执行");
            for table in ["ai_tool_audits", "ai_results"] {
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "不能伪造执行审计或结果: {table}");
            }
            Ok(())
        })
        .unwrap();
}

fn recovery_stream_case(cx: &mut TestAppContext, theme: ThemeMode, tool: Option<&str>) {
    let fixture = Fixture::new();
    recovery_seed_stream(&fixture, tool);
    let before_ui = fixture.ui_state("recovery");
    let before = fixture.counts();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), theme);
    // 首帧恢复必须来自生产 render，而非测试直接调用命令。
    assert!(view.read_with(cx, |main, _| !main.ai_chat.open));
    open_panel(&view, cx); // 正常 ask-ai；Load 必须排在扫描之后。
    let state = recovery_assert_notice(
        &view,
        cx,
        "interrupted",
        "ai-recovery:recovery-turn-restart-notice",
    );
    assert_eq!(state.session_id.as_deref(), Some("recovery"));
    assert!(state.active_turn_id.is_none());
    assert_eq!(state.messages.len(), 2);
    assert_eq!(input_value(&view, cx), "T029 saved unsent draft");
    assert_eq!(state.expanded_parts, vec!["recovery-assistant-summary"]);
    for selector in [
        "ai-message:recovery-user",
        "ai-message:recovery-assistant",
        "ai-part:recovery-assistant-summary",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "已有记录必须成为实际控件: {selector}"
        );
    }
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, "recovery-turn")?;
            assert_eq!(turn.status, sessions::TurnStatus::Interrupted);
            let saved = sessions::list_parts(conn, "recovery-assistant")?;
            assert_eq!(saved.len(), 3);
            for (id, text) in [
                ("recovery-assistant-text", "T029 saved partial stream"),
                ("recovery-assistant-summary", "T029 saved public summary"),
            ] {
                let part = saved.iter().find(|part| part.id == id).unwrap();
                assert_eq!(part.status, sessions::PartStatus::Interrupted);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["text"],
                    text
                );
                let projected = state.messages[1]
                    .parts
                    .iter()
                    .find(|part| part.id == id)
                    .unwrap();
                assert_eq!(projected.payload_json, part.payload_json);
                assert_eq!(projected.status, part.status.as_str());
            }
            Ok(())
        })
        .unwrap();
    if tool.is_some() {
        recovery_assert_call(&fixture, "recovery-call", "cancelled", 102);
    }
    assert_eq!(
        fixture.counts(),
        (before.0, before.1, before.2, before.3 + 1)
    );
    assert_eq!(
        fixture.ui_state("recovery").draft_json,
        before_ui.draft_json
    );
    assert_eq!(
        fixture.ui_state("recovery").expanded_part_ids_json,
        before_ui.expanded_part_ids_json
    );
    click_selector(cx, "ai-part:recovery-assistant-summary");
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .expanded_parts
            .is_empty()
    }));
    assert_eq!(fixture.ui_state("recovery").expanded_part_ids_json, "[]");
}

#[gpui::test]
fn ai_recovery_generating_stream_draft_expanded(cx: &mut TestAppContext) {
    recovery_stream_case(cx, ThemeMode::Light, None);
}

#[gpui::test]
fn ai_recovery_stream_read_draft_expanded_light(cx: &mut TestAppContext) {
    recovery_stream_case(cx, ThemeMode::Light, Some("db.query"));
}

#[gpui::test]
fn ai_recovery_stream_read_draft_expanded_dark(cx: &mut TestAppContext) {
    recovery_stream_case(cx, ThemeMode::Dark, Some("db.query"));
}

fn recovery_unknown_case(cx: &mut TestAppContext, tool: &str) {
    let fixture = Fixture::new();
    recovery_seed_stream(&fixture, Some(tool));
    let before = fixture.counts();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    let state = recovery_assert_notice(
        &view,
        cx,
        "outcome_unknown",
        "ai-recovery:recovery-turn-restart-notice",
    );
    let payload: serde_json::Value = serde_json::from_str(
        &state.messages[1]
            .parts
            .iter()
            .find(|part| part.id == "recovery-turn-restart-notice")
            .unwrap()
            .payload_json,
    )
    .unwrap();
    assert!(payload["text"].as_str().unwrap().contains("不会自动重放"));
    assert!(
        state
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .all(|part| part.status != "streaming")
    );
    let saved = fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, "recovery-turn")?;
            assert_eq!(turn.status, sessions::TurnStatus::OutcomeUnknown);
            assert_eq!(turn.error_code.as_deref(), Some("restart_outcome_unknown"));
            Ok(turn)
        })
        .unwrap();
    recovery_assert_call(&fixture, "recovery-call", "unknown", 102);
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    open_panel(&view, cx);
    click_selector(cx, "ai-chat-history");
    draw(cx);
    click_selector(cx, "ai-session:recovery");
    wait_idle(&view, cx);
    recovery_assert_notice(
        &view,
        cx,
        "outcome_unknown",
        "ai-recovery:recovery-turn-restart-notice",
    );
    recovery_assert_call(&fixture, "recovery-call", "unknown", 102);
    assert_eq!(
        fixture
            .storage
            .with_ai_connection(|conn| sessions::load_turn(conn, "recovery-turn"))
            .unwrap(),
        saved
    );
    assert_eq!(
        fixture.counts(),
        (before.0, before.1, before.2, before.3 + 1),
        "重开/历史不能生成重放 Turn 或重复公告"
    );
}

#[gpui::test]
fn ai_recovery_running_write_is_unknown_and_never_replayed(cx: &mut TestAppContext) {
    // 即使 SQL 看起来是 SELECT，db.execute 也不能按 SQL 前缀误判只读。
    recovery_unknown_case(cx, "db.execute");
}

#[gpui::test]
fn ai_recovery_unrecognized_running_tool_is_unknown(cx: &mut TestAppContext) {
    recovery_unknown_case(cx, "plugin.unknown_effect");
}

fn recovery_approval_case(cx: &mut TestAppContext, prior_approved: bool, approve_click: bool) {
    let fixture = Fixture::new();
    seed_approval(
        &fixture,
        "approval",
        "recovery-approval",
        sessions::PartStatus::Pending,
        None,
    );
    // 共用旧 fixture 的 part ID 是 approval-1；生产回写按 provider_call_id 定位同一卡。
    // 对齐隔离事实行，不能靠另建一张决定卡掩盖旧按钮仍可点击。
    fixture
        .storage
        .with_ai_connection(|conn| {
            conn.execute(
                "UPDATE ai_tool_calls SET provider_call_id='1' WHERE id='approval-turn-toolcall-1'",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    if prior_approved {
        fixture
            .storage
            .with_ai_connection(|conn| {
                let approval =
                    fluxdb_storage::ai_platform::audits::load_approval(conn, "recovery-approval")?;
                let approved = fluxdb_storage::ai_platform::audits::decide_approval(
                    conn,
                    104,
                    &approval.id,
                    true,
                    None,
                    approval.revision,
                )?;
                let part = sessions::list_parts(conn, "approval-assistant")?.remove(0);
                let mut payload: serde_json::Value =
                    serde_json::from_str(&part.payload_json).unwrap();
                payload["decision"] = serde_json::json!("approved");
                payload["approval_revision"] = serde_json::json!(approved.revision);
                sessions::update_part(
                    conn,
                    104,
                    &part.id,
                    sessions::PartStatus::Complete,
                    &payload.to_string(),
                    part.revision,
                )?;
                Ok(())
            })
            .unwrap();
    }
    let mut controller = fixture.controller();
    assert!(matches!(
        controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
            session_id: Some("approval".into())
        })),
        AppEvent::AiConversationLoaded(_)
    ));
    // 明确锁定恢复后不会接入真实 Provider。
    assert!(controller.state().ai_models.is_empty());
    assert!(
        controller
            .state()
            .ai_agents
            .iter()
            .all(|agent| agent.model_profile_id.is_none())
    );
    let before = approval_state(&fixture, "recovery-approval");
    let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
    open_panel(&view, cx);
    let state = recovery_assert_notice(
        &view,
        cx,
        "waiting_approval",
        "ai-recovery:approval-turn-restart-notice",
    );
    let pending = approval_state(&fixture, "recovery-approval");
    assert_eq!(pending.0.as_str(), "pending");
    assert_eq!(pending.1, before.1 + i64::from(prior_approved));
    let part = state
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find(|part| part.id == "approval-assistant-approval-1")
        .unwrap();
    assert_eq!(part.status, "pending");
    let payload: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
    assert_eq!(payload["approval_revision"], pending.1);
    assert!(payload.get("decision").is_none());
    assert!(cx.debug_bounds("ai-approve:recovery-approval").is_some());
    assert!(cx.debug_bounds("ai-deny:recovery-approval").is_some());
    recovery_assert_call(
        &fixture,
        "approval-turn-toolcall-1",
        "waiting_approval",
        1_700_000_000_000,
    );
    if prior_approved {
        // CAS 是独立后端校验，不是用处理函数代替下面的真实决定点击。
        let stale = fixture.controller().dispatch(AppCommand::AiConversation(
            AiConversationCommand::DecideApproval {
                approval_id: "recovery-approval".into(),
                approved: true,
                expected_revision: before.1,
            },
        ));
        assert!(
            matches!(stale, AppEvent::AiSettingsError(_)),
            "旧批准 revision 必须失效"
        );
        assert_eq!(approval_state(&fixture, "recovery-approval"), pending);
    }
    click_selector(
        cx,
        if approve_click {
            "ai-approve:recovery-approval"
        } else {
            "ai-deny:recovery-approval"
        },
    );
    wait_idle(&view, cx);
    let after = approval_state(&fixture, "recovery-approval");
    assert_eq!(
        after.0.as_str(),
        if approve_click { "approved" } else { "denied" }
    );
    assert_eq!(
        after.1,
        pending.1 + 1,
        "按钮必须使用恢复后的新 revision 提交 CAS"
    );
    assert!(cx.debug_bounds("ai-approve:recovery-approval").is_none());
    assert!(cx.debug_bounds("ai-deny:recovery-approval").is_none());
    fixture.storage.with_ai_connection(|conn| {
        let cards = sessions::list_parts(conn, "approval-assistant")?.into_iter().filter(|part| {
            serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["approval_id"] == "recovery-approval"
        }).collect::<Vec<_>>();
        assert_eq!(cards.len(), 1, "真实决定必须就地更新，不能追加卡片留下旧批准按钮");
        assert_eq!(cards[0].status, sessions::PartStatus::Complete);
        let payload: serde_json::Value = serde_json::from_str(&cards[0].payload_json).unwrap();
        assert_eq!(payload["decision"], after.0.as_str());
        let state = view.read_with(cx, |main, _| main.controller.state().ai_conversation.clone());
        assert_eq!(state.messages[1].parts.iter().find(|part| part.id == cards[0].id).unwrap().payload_json, cards[0].payload_json);
        Ok(())
    }).unwrap();
    recovery_assert_call(
        &fixture,
        "approval-turn-toolcall-1",
        if approve_click {
            "waiting_approval"
        } else {
            "denied"
        },
        1_700_000_000_000,
    );
    assert!(view.read_with(cx, |main, _| main.ai_chat.running.is_none()));
    // 批准但未配置模型会显示生产 show_message；释放窗口取消真实通知 Timer。
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[gpui::test]
fn ai_recovery_historical_approved_gets_new_revision_and_deny_click(cx: &mut TestAppContext) {
    recovery_approval_case(cx, true, false);
}

#[gpui::test]
fn ai_recovery_historical_approved_gets_new_revision_and_approve_click(cx: &mut TestAppContext) {
    recovery_approval_case(cx, true, true);
}

#[gpui::test]
fn ai_recovery_pending_approval_keeps_pending_and_accepts_real_deny(cx: &mut TestAppContext) {
    recovery_approval_case(cx, false, false);
}

#[gpui::test]
async fn ai_recovery_close_reopen_history_does_not_rescan_live_task(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    let job = start_scripted_send(
        &fixture,
        &view,
        cx,
        "T029 live task must not be recovered twice",
    );
    wait_source(&view, cx, "live generating turn", |main| {
        main.ai_chat.running.is_some()
            && main
                .controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .any(|message| {
                    message.id == job.assistant_message_id
                        && message.parts.iter().any(|part| part.status == "streaming")
                })
    })
    .await;
    let cancel = view.read_with(cx, |main, _| main.ai_chat.cancel.clone().unwrap());
    // scripted source 产生真实流/checkpoint，但不会像 TurnRunner 一样推进 queued 状态。
    // 比较原事实和 revision，仍然能严格抓住误扫描把 live turn 标成 interrupted 的回归。
    let live_turn = fixture
        .storage
        .with_ai_connection(|conn| sessions::load_turn(conn, &job.turn_id))
        .unwrap();
    assert!(!live_turn.status.is_terminal());
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    assert!(!cancel.is_cancelled());
    open_panel(&view, cx);
    assert!(view.read_with(cx, |main, _| {
        main.ai_chat
            .running
            .as_ref()
            .is_some_and(|running| running.turn_id == job.turn_id)
    }));
    click_selector(cx, "ai-chat-history");
    draw(cx);
    click_selector(cx, "ai-session:one");
    wait_idle(&view, cx);
    assert!(!cancel.is_cancelled());
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            assert_eq!(
                turn, live_turn,
                "打开/历史只能 Load，不能再扫描或改写 live turn/revision"
            );
            assert!(
                sessions::list_parts(conn, &job.assistant_message_id)?
                    .iter()
                    .all(|part| !part.id.ends_with("-restart-notice")
                        && part.status == sessions::PartStatus::Streaming)
            );
            Ok(())
        })
        .unwrap();
    let state = wait_source(&view, cx, "live task completes after history", |main| {
        main.ai_chat.running.is_none()
    })
    .await;
    assert!(
        state
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .all(|part| !part.id.ends_with("-restart-notice"))
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert_eq!(
                sessions::load_turn(conn, &job.turn_id)?.status,
                sessions::TurnStatus::Completed
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.counts().1, 3, "不能重放/创建第二个 live turn");
    cx.executor().forbid_parking();
}

#[gpui::test]
fn ai_recovery_startup_failure_stays_visible_and_send_disabled(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    recovery_seed_stream(&fixture, None);
    let controller = fixture.controller();
    // 只使扫描事务失败，普通 Load 仍然成功；覆盖失败被后续 Load 吞掉的竞争分支。
    fixture.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER recovery_fixture_failure BEFORE UPDATE ON ai_turns BEGIN SELECT RAISE(ABORT, 'T029 fixture startup recovery blocked'); END;").unwrap();
        Ok(())
    }).unwrap();
    let (view, cx) = open_main(cx, &fixture, controller, ThemeMode::Light);
    wait_idle(&view, cx);
    assert!(view.read_with(cx, |main, _| main.ai_chat.recovery_failed
        && main.ai_chat.error.as_deref().is_some_and(|error| {
            error.contains("T029 fixture startup recovery blocked")
        })));
    let before = fixture.counts();
    open_panel(&view, cx);
    replace_input(&view, cx, "T029 must not send after failed recovery");
    for selector in [
        "ai-chat-send",
        "ai-chat-new",
        "ai-chat-mode",
        "ai-chat-history",
    ] {
        click_selector(cx, selector);
        wait_idle(&view, cx);
    }
    assert_eq!(
        fixture.counts(),
        before,
        "启动扫描失败时禁用控件不能发送/新建/重放"
    );
    assert!(view.read_with(cx, |main, _| main.ai_chat.recovery_failed
        && !main.ai_chat.history
        && main.ai_chat.mode == settings::PanelMode::Drawer
        && main.ai_chat.running.is_none()));
    assert_eq!(
        input_value(&view, cx),
        "T029 must not send after failed recovery"
    );
    assert!(
        view.read_with(cx, |main, _| main.ai_chat.error.as_deref().is_some_and(
            |error| error.contains("T029 fixture startup recovery blocked")
        )),
        "启动恢复失败必须持续反馈，不能被正常 Load 清空成成功界面"
    );
    assert!(view.read_with(cx, |main, _| main.ai_chat._pump.is_none()));
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert_eq!(
                sessions::load_turn(conn, "recovery-turn")?.status,
                sessions::TurnStatus::Generating,
                "扫描异常必须事务回滚，不能部分提交中断事实"
            );
            assert!(
                sessions::list_parts(conn, "recovery-assistant")?
                    .iter()
                    .all(|part| part.status == sessions::PartStatus::Streaming)
            );
            conn.execute_batch("DROP TRIGGER recovery_fixture_failure;")
                .unwrap();
            Ok(())
        })
        .unwrap();
    // 关闭仍是真实点击，并保存失败期间输入；解除故障后只能新宿主启动重试扫描。
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.run_until_parked();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    recovery_assert_notice(
        &view,
        cx,
        "interrupted",
        "ai-recovery:recovery-turn-restart-notice",
    );
    assert_eq!(
        input_value(&view, cx),
        "T029 must not send after failed recovery"
    );
    assert_eq!(
        fixture.counts(),
        (before.0, before.1, before.2, before.3 + 1)
    );
    assert!(view.read_with(cx, |main, _| main.ai_chat.error.is_none()));
}
