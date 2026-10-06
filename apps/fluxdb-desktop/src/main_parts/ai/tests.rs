#[cfg(test)]
mod ai_panel_tests {
    use super::*;
    use fluxdb_app::{AiAgentSaveInput, AiConversationCommand, AiModelSaveInput};
    use fluxdb_storage::ai_platform::{conversation, sessions, settings};
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point, size};
    use std::path::PathBuf;

    // 每个测试拥有独立 SQLite；只预置持久化数据，面板状态必须由生产 Load 回桥生成。
    struct Fixture {
        root: PathBuf,
        storage: FileStorage,
    }

    impl Fixture {
        fn new() -> Self {
            // 同一微秒内并发创建 fixture 时 nanos 可能相同；原子计数保证路径绝对唯一，
            // 否则两个并行用例会共用一个 SQLite 并互相触发唯一约束冲突。
            static FIXTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let root = std::env::temp_dir().join(format!(
                "fluxdb-ai-panel-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                seq
            ));
            let storage = FileStorage::new(root.clone());
            storage.with_ai_connection(|_| Ok(())).unwrap();
            Self { root, storage }
        }

        fn controller(&self) -> AppController {
            let mut controller = AppController::new();
            controller.set_ai_storage(self.storage.clone());
            assert!(matches!(
                controller.dispatch(AppCommand::RefreshAiSettings),
                AppEvent::AiSettingsLoaded
            ));
            controller
        }

        fn agent(&self, model_profile_id: Option<String>) -> String {
            match self
                .controller()
                .dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                    id: None,
                    display_name: "Panel fixture Agent".into(),
                    description: "只用于交互回归".into(),
                    system_prompt: "解释当前页面，不执行操作".into(),
                    model_profile_id,
                    allowed_tools: Vec::new(),
                    enabled: true,
                    expected_revision: 0,
                })) {
                AppEvent::AiAgentSaved { id, .. } => id,
                event => panic!("fixture Agent 保存失败: {event:?}"),
            }
        }

        fn seeded() -> Self {
            let fixture = Self::new();
            let model =
                match fixture
                    .controller()
                    .dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
                        id: None,
                        display_name: "Panel fixture model".into(),
                        provider: "openai".into(),
                        model: "panel-test-not-a-real-model".into(),
                        endpoint: Some("https://panel.example.invalid/v1".into()),
                        endpoint_is_full_url: false,
                        enabled: true,
                        context_window_tokens: Some(8192),
                        max_output_tokens: Some(1024),
                        input_price_micros: None,
                        output_price_micros: None,
                        cached_input_price_micros: None,
                        api_key: None,
                        clear_key: false,
                        expected_revision: None,
                    })) {
                    AppEvent::AiModelSaved { id, .. } => id,
                    event => panic!("fixture Model 保存失败: {event:?}"),
                };
            let agent = fixture.agent(Some(model));
            fixture
                .storage
                .with_ai_connection(|conn| {
                    for (index, session_id, user_id, assistant_id, body) in [
                        (
                            1,
                            "one",
                            "one-user",
                            "one-assistant",
                            "first persisted message",
                        ),
                        (
                            2,
                            "two",
                            "two-user",
                            "two-assistant",
                            "second persisted message",
                        ),
                    ] {
                        let turn_id = format!("{session_id}-turn");
                        conversation::send(
                            conn, index, true, session_id, &agent, &turn_id, user_id, body, "{}",
                            "{}",
                        )?;
                        conversation::checkpoint(
                            conn,
                            index + 10,
                            session_id,
                            &turn_id,
                            assistant_id,
                            &format!("saved reply for {session_id}"),
                            "Public reasoning summary for this saved conversation.",
                            Some(sessions::TurnStatus::Completed),
                            None,
                        )?;
                    }
                    Ok(())
                })
                .unwrap();
            // 用生产命令指定启动会话及初始草稿；不把 DTO 直接写入 UI/controller.state。
            let mut controller = fixture.controller();
            assert!(matches!(
                controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
                    session_id: Some("one".into())
                })),
                AppEvent::AiConversationLoaded(_)
            ));
            assert!(matches!(
                controller.dispatch(AppCommand::AiConversation(AiConversationCommand::SaveUi {
                    draft: "stored draft".into(),
                    expanded_parts: Vec::new(),
                    mode: settings::PanelMode::Drawer,
                    drawer_width: 460.,
                    embedded_width: 420.,
                })),
                AppEvent::AiConversationLoaded(_)
            ));
            fixture
        }

        fn counts(&self) -> (i64, i64, i64, i64) {
            self.storage
                .with_ai_connection(|conn| {
                    let count = |table: &str| {
                        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .unwrap()
                    };
                    Ok((
                        count("ai_sessions"),
                        count("ai_turns"),
                        count("ai_messages"),
                        count("ai_message_parts"),
                    ))
                })
                .unwrap()
        }

        fn preferences(&self) -> settings::PanelPreferences {
            self.storage
                .with_ai_connection(|conn| Ok(settings::load_panel_preferences(conn)?))
                .unwrap()
        }

        fn ui_state(&self, session: &str) -> sessions::SessionUiStateView {
            self.storage
                .with_ai_connection(|conn| Ok(sessions::load_ui_state(conn, session)?))
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // 仅清理由当前 fixture 创建的唯一临时目录，不接触用户的配置库。
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn open_main<'a>(
        cx: &'a mut TestAppContext,
        fixture: &Fixture,
        controller: AppController,
        theme: ThemeMode,
    ) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let storage = fixture.storage.clone();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            navicat_main_new(window, cx, controller, storage, theme)
        });
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        draw(cx);
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    fn click_selector(cx: &mut VisualTestContext, selector: &'static str) {
        draw(cx);
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("控件未渲染: {selector}"));
        assert!(
            bounds.size.width > px(0.) && bounds.size.height > px(0.),
            "控件必须可命中: {selector}"
        );
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    fn wait_idle(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        // TestDispatcher 驱动实际 spawn_in/background_spawn，不能用 Apply 替代异步 Load。
        cx.run_until_parked();
        assert!(
            view.read_with(cx, |main, _| {
                main.ai_chat._io.is_none()
                    && main.ai_chat.pending.is_empty()
                    && !main.ai_chat.loading
            }),
            "真实 AI 会话 IO 尚未完成"
        );
        draw(cx);
    }

    fn open_panel(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        click_selector(cx, "ask-ai");
        wait_idle(view, cx);
        assert!(view.read_with(cx, |main, _| main.ai_chat.open));
        assert!(cx.debug_bounds("ai-chat-panel").is_some());
    }

    fn input_value(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |main, cx| {
            main.ai_chat.input.read(cx).value().to_string()
        })
    }

    fn replace_input(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, text: &str) {
        draw(cx);
        let bounds = cx.debug_bounds("ai-chat-input").expect("输入容器必须渲染");
        // selector 包含底部发送行；点击容器上部的实际编辑区，不能点击整个容器中心。
        cx.simulate_click(
            point(bounds.left() + px(20.), bounds.top() + px(20.)),
            Modifiers::default(),
        );
        assert!(
            cx.update(|window, cx| view
                .read(cx)
                .ai_chat
                .input
                .focus_handle(cx)
                .is_focused(window)),
            "真实点击编辑区必须获得 Input 焦点，bounds={bounds:?}"
        );
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a backspace"
        } else {
            "ctrl-a backspace"
        });
        cx.simulate_input(text);
        assert_eq!(
            input_value(view, cx),
            text,
            "必须由 Input 的实际键盘输入写入草稿"
        );
    }

    fn assert_session(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, session: &str) {
        let (state, snapshot) = view.read_with(cx, |main, _| {
            (
                main.controller.state().ai_conversation.clone(),
                render_state_snapshot(main.controller.state()).ai_conversation,
            )
        });
        assert_eq!(
            state, snapshot,
            "AppState → render_state_snapshot 不得丢失会话数据"
        );
        assert_eq!(state.session_id.as_deref(), Some(session));
        assert_eq!(state.messages.len(), 2);
        let (user_selector, assistant_selector, other_selector) = if session == "one" {
            (
                "ai-message:one-user",
                "ai-message:one-assistant",
                "ai-message:two-user",
            )
        } else {
            (
                "ai-message:two-user",
                "ai-message:two-assistant",
                "ai-message:one-user",
            )
        };
        for selector in [user_selector, assistant_selector] {
            let bounds = cx
                .debug_bounds(selector)
                .expect("持久化消息必须成为真实控件");
            assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        }
        assert!(
            cx.debug_bounds(other_selector).is_none(),
            "旧会话消息不能串入新会话"
        );
        assert!(
            state
                .messages
                .iter()
                .all(|message| message.id.starts_with(session))
        );
        assert!(state.messages.iter().flat_map(|message| &message.parts).filter(|part| part.kind == "text")
            .all(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["text"].as_str().is_some()));
    }

    // GPUI timer 是虚拟时钟，scripted source 在真实线程上每 340ms 才 emit。
    // 允许 parking 并异步等待真实 Timer，逐次泵真实回桥；不 sleep、不跳过流式阶段。
    async fn wait_source(
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        phase: &str,
        ready: impl Fn(&NavicatMain) -> bool,
    ) -> fluxdb_app::AiConversationView {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            cx.run_until_parked();
            if view.read_with(cx, |main, _| ready(main)) {
                draw(cx);
                return view.read_with(cx, |main, _| {
                    main.controller.state().ai_conversation.clone()
                });
            }
            assert!(
                std::time::Instant::now() < deadline,
                "真实 source 超时: {phase}; state={:?}",
                view.read_with(cx, |main, _| (
                    main.ai_chat.error.clone(),
                    main.controller.state().ai_conversation.clone()
                ))
            );
            smol::Timer::after(std::time::Duration::from_millis(20)).await;
        }
    }

    fn start_scripted_send(
        fixture: &Fixture,
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        prompt: &str,
    ) -> fluxdb_app::AiConversationJob {
        assert!(
            cfg!(debug_assertions),
            "scripted source 仅支持 debug，禁止回退到真实模型"
        );
        fixture
            .storage
            .with_ai_connection(|conn| {
                let session = sessions::load_session(conn, "one")?;
                let agent =
                    fluxdb_storage::ai_platform::agents::load_agent(conn, &session.agent_id)?;
                let model = fluxdb_storage::ai_platform::models::load_model(
                    conn,
                    agent.default_model_profile_id.as_deref().unwrap(),
                )?;
                assert!(agent.enabled && agent.allowed_tools.is_empty() && model.enabled);
                assert_eq!(
                    model.auth_mode,
                    fluxdb_storage::ai_platform::models::AuthMode::None
                );
                assert_eq!(
                    model.endpoint.as_deref(),
                    Some("https://panel.example.invalid/v1")
                );
                Ok(())
            })
            .unwrap();
        // 唯一替换点是事件 source。Agent/Model/消息/Turn/回桥仍走生产装配与持久化。
        view.update(cx, |main, _| main.ai_chat.scripted_fixture = true);
        replace_input(view, cx, prompt);
        click_selector(cx, "ai-chat-send");
        wait_idle(view, cx);
        let job = view.read_with(cx, |main, cx| {
            assert!(
                main.ai_chat.error.is_none(),
                "发送错误: {:?}",
                main.ai_chat.error
            );
            assert!(main.ai_chat.input.read(cx).value().is_empty());
            assert!(main.ai_chat.follow_reply && !main.ai_chat.new_reply);
            main.ai_chat
                .running
                .clone()
                .expect("点击发送必须启动真实会话任务")
        });
        assert_eq!(job.session_id, "one");
        assert!(cx.debug_bounds("ai-chat-stop").is_some());
        assert!(cx.debug_bounds("ai-chat-send").is_none());
        job
    }

    fn assert_final_reply(
        fixture: &Fixture,
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        job: &fluxdb_app::AiConversationJob,
        status: sessions::TurnStatus,
        prompt: &str,
        text: &str,
    ) -> (
        sessions::TurnView,
        sessions::MessageView,
        Vec<sessions::MessagePartView>,
    ) {
        let state = view.read_with(cx, |main, _| {
            let state = main.controller.state().ai_conversation.clone();
            assert_eq!(
                render_state_snapshot(main.controller.state()).ai_conversation,
                state
            );
            assert!(main.ai_chat.running.is_none() && main.ai_chat.cancel.is_none());
            assert!(main.ai_chat.error.is_none() && state.error.is_none());
            state
        });
        assert_eq!(state.session_id.as_deref(), Some("one"));
        assert!(state.active_turn_id.is_none());
        assert_eq!(state.messages.len(), 4);
        assert_eq!(state.messages[2].role, "user");
        assert_eq!(state.messages[3].id, job.assistant_message_id);
        let saved =
            fixture
                .storage
                .with_ai_connection(|conn| {
                    let turn = sessions::load_turn(conn, &job.turn_id)?;
                    assert_eq!(turn.status, status);
                    assert!(turn.finished_at_ms.is_some() && turn.error_code.is_none());
                    let snapshot: serde_json::Value =
                        serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
                    assert_eq!(snapshot["model_profile_id"], job.model_profile_id);
                    assert!(snapshot["ui_context"].is_string());
                    let user = sessions::load_message(conn, &state.messages[2].id)?;
                    assert_eq!(user.turn_id.as_deref(), Some(job.turn_id.as_str()));
                    assert_eq!(user.seq, 3);
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(
                            &sessions::list_parts(conn, &user.id)?[0].payload_json
                        )
                        .unwrap()["text"],
                        prompt
                    );
                    let message = sessions::load_message(conn, &job.assistant_message_id)?;
                    assert_eq!(message.seq, 4);
                    assert_eq!(message.turn_id.as_deref(), Some(job.turn_id.as_str()));
                    assert_eq!(message.persistence_kind, sessions::PersistenceKind::Final);
                    let parts = sessions::list_parts(conn, &message.id)?;
                    assert_eq!(parts.len(), 1);
                    assert_eq!(parts[0].kind, sessions::PartKind::Text);
                    assert_eq!(
                        parts[0].status,
                        if status == sessions::TurnStatus::Cancelled {
                            sessions::PartStatus::Interrupted
                        } else {
                            sessions::PartStatus::Complete
                        }
                    );
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(&parts[0].payload_json).unwrap()
                            ["text"],
                        text
                    );
                    assert_eq!(
                        state.messages[3].parts[0].payload_json,
                        parts[0].payload_json
                    );
                    assert_eq!(state.messages[3].parts[0].status, parts[0].status.as_str());
                    for table in ["ai_tool_calls", "ai_approvals", "ai_tool_audits"] {
                        let count: i64 = conn
                            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                                row.get(0)
                            })
                            .unwrap();
                        assert_eq!(count, 0, "对话/停止不能执行工具或生成审批: {table}");
                    }
                    Ok((turn, message, parts))
                })
                .unwrap();
        assert_eq!(
            fixture.counts(),
            (2, 3, 6, 8),
            "仅新增一个 Turn、一条 user、一条 assistant 及各自正文 part"
        );
        // debug_bounds API 需要静态 selector；生成的消息身份只保留在测试进程生命周期内。
        for message in &state.messages[2..] {
            let selector: &'static str =
                Box::leak(format!("ai-message:{}", message.id).into_boxed_str());
            let bounds = cx
                .debug_bounds(selector)
                .expect("发送后的消息必须由真实控件渲染");
            assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        }
        assert!(cx.debug_bounds("ai-chat-stop").is_none());
        assert!(cx.debug_bounds("ai-chat-send").is_some());
        saved
    }

    #[gpui::test]
    async fn ai_panel_scripted_send_streams_persists_final_and_reopens_in_fresh_main(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let fixture = Fixture::seeded();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        let prompt = "explain current SQL without executing tools";
        let job = start_scripted_send(&fixture, &view, cx, prompt);
        let streamed = wait_source(&view, cx, "normal streaming part", |main| {
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
        let part = &streamed
            .messages
            .iter()
            .find(|message| message.id == job.assistant_message_id)
            .unwrap()
            .parts[0];
        let body: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
        let partial = body["text"].as_str().unwrap().to_owned();
        let complete = "已读取当前目标。\n以下是 SQL 解释示例，不表示已执行查询。\nSELECT COUNT(*) FROM orders;";
        assert!(!partial.is_empty() && complete.starts_with(&partial) && partial != complete);
        assert_eq!(
            streamed.active_turn_id.as_deref(),
            Some(job.turn_id.as_str())
        );
        fixture
            .storage
            .with_ai_connection(|conn| {
                assert_eq!(
                    sessions::load_message(conn, &job.assistant_message_id)?.persistence_kind,
                    sessions::PersistenceKind::Checkpoint
                );
                assert!(
                    !sessions::load_turn(conn, &job.turn_id)?
                        .status
                        .is_terminal()
                );
                Ok(())
            })
            .unwrap();
        wait_source(&view, cx, "normal terminal bridge", |main| {
            main.ai_chat.running.is_none()
        })
        .await;
        let saved = assert_final_reply(
            &fixture,
            &view,
            cx,
            &job,
            sessions::TurnStatus::Completed,
            prompt,
            complete,
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        let (reopened, cx) = open_main(&mut cx.cx, &fixture, fixture.controller(), ThemeMode::Dark);
        open_panel(&reopened, cx);
        assert_eq!(
            assert_final_reply(
                &fixture,
                &reopened,
                cx,
                &job,
                sessions::TurnStatus::Completed,
                prompt,
                complete
            ),
            saved
        );
        cx.executor().forbid_parking();
    }

    #[gpui::test]
    async fn ai_panel_scripted_stop_sets_cancel_flag_persists_interrupted_and_does_not_restart(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let fixture = Fixture::seeded();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        let prompt = "stop this explanation without executing tools";
        let job = start_scripted_send(&fixture, &view, cx, prompt);
        let streamed = wait_source(&view, cx, "partial before stop", |main| {
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
        let part = &streamed
            .messages
            .iter()
            .find(|message| message.id == job.assistant_message_id)
            .unwrap()
            .parts[0];
        let body: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
        let partial = body["text"].as_str().unwrap().to_owned();
        assert!(!partial.is_empty());
        assert!(
            !partial.contains("SELECT COUNT(*)"),
            "必须在已有部分正文而不是完成后停止"
        );
        let cancel = view.read_with(cx, |main, _| main.ai_chat.cancel.clone().unwrap());
        assert!(!cancel.is_cancelled());
        click_selector(cx, "ai-chat-stop");
        assert!(cancel.is_cancelled(), "实际点击停止必须触发生产 CancelFlag");
        let terminal = wait_source(&view, cx, "cancel terminal bridge", |main| {
            main.ai_chat.running.is_none()
        })
        .await;
        let part = &terminal
            .messages
            .iter()
            .find(|message| message.id == job.assistant_message_id)
            .unwrap()
            .parts[0];
        let body: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
        let text = body["text"].as_str().unwrap().to_owned();
        assert!(
            text.starts_with(&partial),
            "停止不能抹去已保存的部分回复；允许在途 delta 完成"
        );
        assert_eq!(part.status, "interrupted");
        let saved = assert_final_reply(
            &fixture,
            &view,
            cx,
            &job,
            sessions::TurnStatus::Cancelled,
            prompt,
            &text,
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        open_panel(&view, cx);
        assert_eq!(
            assert_final_reply(
                &fixture,
                &view,
                cx,
                &job,
                sessions::TurnStatus::Cancelled,
                prompt,
                &text
            ),
            saved
        );
        // 再过一个真实 source 周期，确认没有重试/新 Turn，也没有迟到 checkpoint 改写终态。
        smol::Timer::after(std::time::Duration::from_millis(380)).await;
        cx.run_until_parked();
        draw(cx);
        assert_eq!(
            assert_final_reply(
                &fixture,
                &view,
                cx,
                &job,
                sessions::TurnStatus::Cancelled,
                prompt,
                &text
            ),
            saved
        );
        cx.executor().forbid_parking();
    }

    #[gpui::test]
    fn ai_panel_open_loads_persisted_messages_through_state_snapshot_and_controls(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        assert!(view.read_with(cx, |main, _| {
            main.controller.state().ai_conversation.messages.is_empty()
        }));
        assert!(cx.debug_bounds("ai-chat-panel").is_none());
        open_panel(&view, cx);
        assert_session(&view, cx, "one");
        assert_eq!(input_value(&view, cx), "stored draft");
        assert_eq!(fixture.counts(), before, "打开不创建会话、Turn 或消息");
        // 仅尝试一次截图；offscreen 没有 Renderer 时明确报告受阻，不伪造图片证据。
        match cx.update(|window, _| window.render_to_image()) {
            Ok(_) => {
                println!("ai_panel_ offscreen renderer available; no bulk screenshots requested")
            }
            Err(error) => println!("ai_panel_ screenshot blocked: {error}"),
        }
    }

    #[gpui::test]
    fn ai_panel_loading_disables_new_mode_history_and_send_until_real_io_finishes(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        // 在同一 App update 中派发真实鼠标事件，暂不泵后台任务，捕获实际 Load 的 loading 帧。
        let click_before_tasks = |cx: &mut VisualTestContext, selector: &'static str| {
            draw(cx);
            let position = cx.debug_bounds(selector).expect("控件必须可命中").center();
            cx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
            });
        };
        click_before_tasks(cx, "ask-ai");
        assert!(view.read_with(cx, |main, _| main.ai_chat.open
            && main.ai_chat.loading
            && main.ai_chat._io.is_some()));
        let epoch = view.read_with(cx, |main, _| main.ai_chat.epoch);
        draw(cx);
        assert!(cx.debug_bounds("ai-chat-panel").is_some());
        assert!(
            cx.debug_bounds("ai-message:one-user").is_none(),
            "加载帧不能提前渲染未加载消息"
        );
        for selector in [
            "ai-chat-new",
            "ai-chat-mode",
            "ai-chat-history",
            "ai-chat-send",
        ] {
            click_before_tasks(cx, selector);
            assert!(view.read_with(cx, |main, _| main.ai_chat.loading
                && main.ai_chat._io.is_some()));
            assert!(
                view.read_with(cx, |main, _| main.ai_chat.pending.is_empty()
                    && !main.ai_chat.history
                    && main.ai_chat.epoch == epoch
                    && main.ai_chat.mode == settings::PanelMode::Drawer
                    && main.ai_chat.running.is_none()),
                "loading 时按钮必须 disabled: {selector}"
            );
        }
        wait_idle(&view, cx);
        assert_session(&view, cx, "one");
        assert_eq!(fixture.counts(), before);
    }

    #[gpui::test]
    fn ai_panel_close_reopen_restores_session_draft_without_new_session(cx: &mut TestAppContext) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        replace_input(&view, cx, "typed unsent draft");
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        assert!(!view.read_with(cx, |main, _| main.ai_chat.open));
        assert!(cx.debug_bounds("ai-chat-panel").is_none());
        let draft = fixture.ui_state("one").draft_json.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&draft).unwrap()["text"],
            "typed unsent draft"
        );
        open_panel(&view, cx);
        assert_session(&view, cx, "one");
        assert_eq!(input_value(&view, cx), "typed unsent draft");
        assert_eq!(fixture.counts(), before);
    }

    #[gpui::test]
    fn ai_panel_new_is_only_a_draft_and_blocks_duplicate_unsent_new(cx: &mut TestAppContext) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        click_selector(cx, "ai-chat-new");
        wait_idle(&view, cx);
        assert!(view.read_with(cx, |main, cx| {
            let chat = &main.controller.state().ai_conversation;
            chat.session_id.is_none()
                && chat.messages.is_empty()
                && main.ai_chat.input.read(cx).value().is_empty()
        }));
        assert!(fixture.preferences().last_session_id.is_none());
        assert_eq!(fixture.counts(), before, "新建按钮不能创建空历史");
        replace_input(&view, cx, "new pending draft");
        click_selector(cx, "ai-chat-new");
        wait_idle(&view, cx);
        assert_eq!(input_value(&view, cx), "new pending draft");
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &fixture.preferences().pending_draft_json.unwrap()
            )
            .unwrap()["text"],
            "new pending draft"
        );
        open_panel(&view, cx);
        assert!(view.read_with(cx, |main, _| {
            main.controller.state().ai_conversation.session_id.is_none()
        }));
        assert_eq!(input_value(&view, cx), "new pending draft");
        assert_eq!(fixture.counts(), before);
        // 重复新建会启动 show_message 的真实 Timer；测试完成必须释放窗口/root，
        // 再在原线程析构任务，避免通知 waker 在下一个用例触发已结束的 GPUI scheduler。
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn ai_panel_mode_switch_persists_and_fresh_main_restores_preferences(cx: &mut TestAppContext) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        replace_input(&view, cx, "mode retained draft");
        let revision = fixture.preferences().revision;
        click_selector(cx, "ai-chat-mode");
        wait_idle(&view, cx);
        let preferences = fixture.preferences();
        assert_eq!(preferences.preferred_mode, settings::PanelMode::Embedded);
        assert!(preferences.revision > revision);
        assert_eq!(
            view.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Embedded
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        open_panel(&view, cx);
        assert_eq!(
            view.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Embedded
        );
        // 重建生产 NavicatMain + Controller，证明不是仅恢复内存里的 mode。
        let (reopened, cx) = open_main(&mut cx.cx, &fixture, fixture.controller(), ThemeMode::Dark);
        open_panel(&reopened, cx);
        assert_eq!(
            view.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Embedded
        );
        assert_eq!(
            reopened.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Embedded
        );
        assert_eq!(input_value(&reopened, cx), "mode retained draft");
        click_selector(cx, "ai-chat-mode");
        wait_idle(&reopened, cx);
        assert_eq!(
            fixture.preferences().preferred_mode,
            settings::PanelMode::Drawer
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&reopened, cx);
        open_panel(&reopened, cx);
        assert_eq!(
            reopened.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Drawer
        );
        assert_session(&reopened, cx, "one");
        assert_eq!(fixture.counts(), before);
    }

    #[gpui::test]
    fn ai_panel_history_loads_other_session_without_message_or_draft_leak(cx: &mut TestAppContext) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        replace_input(&view, cx, "draft for one");
        click_selector(cx, "ai-chat-history");
        draw(cx);
        assert!(view.read_with(cx, |main, _| main.ai_chat.history));
        click_selector(cx, "ai-session:two");
        wait_idle(&view, cx);
        assert_session(&view, cx, "two");
        assert_eq!(
            fixture.preferences().last_session_id.as_deref(),
            Some("two")
        );
        assert!(view.read_with(cx, |main, cx| {
            main.ai_chat.input.read(cx).value().is_empty()
        }));
        replace_input(&view, cx, "draft for two");
        click_selector(cx, "ai-chat-history");
        click_selector(cx, "ai-session:one");
        wait_idle(&view, cx);
        assert_session(&view, cx, "one");
        assert_eq!(input_value(&view, cx), "draft for one");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fixture.ui_state("two").draft_json.unwrap())
                .unwrap()["text"],
            "draft for two"
        );
        assert_eq!(fixture.counts(), before);
    }

    fn invalid_send(cx: &mut TestAppContext, with_agent: bool, expected_error: &str) {
        let fixture = Fixture::new();
        if with_agent {
            fixture.agent(None);
        }
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        replace_input(&view, cx, "keep invalid input");
        let before = fixture.counts();
        let preferences = fixture.preferences();
        click_selector(cx, "ai-chat-send");
        wait_idle(&view, cx);
        let (error, draft, running, snapshot) = view.read_with(cx, |main, cx| {
            (
                main.ai_chat.error.clone(),
                main.ai_chat.input.read(cx).value().to_string(),
                main.ai_chat.running.is_some(),
                render_state_snapshot(main.controller.state()).ai_conversation,
            )
        });
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains(expected_error)),
            "发送应明确失败: {error:?}"
        );
        assert_eq!(draft, "keep invalid input");
        assert!(!running, "配置校验失败不能启动任何真实模型任务");
        assert!(snapshot.session_id.is_none() && snapshot.messages.is_empty());
        assert!(
            cx.debug_bounds("ai-chat-retry").is_some(),
            "错误必须有真实可见的反馈/重试控件"
        );
        assert_eq!(
            fixture.counts(),
            before,
            "校验失败不能创建 Session/Turn/Message/Part"
        );
        assert_eq!(
            fixture.preferences(),
            preferences,
            "失败不能写入偏好或新草稿"
        );
        assert_eq!(
            view.read_with(cx, |main, _| main.controller.state().ai_agents.len()),
            usize::from(with_agent)
        );
    }

    #[gpui::test]
    fn ai_panel_invalid_send_missing_agent_preserves_input_and_database(cx: &mut TestAppContext) {
        invalid_send(cx, false, "Agent");
    }

    #[gpui::test]
    fn ai_panel_invalid_send_missing_model_preserves_input_and_database(cx: &mut TestAppContext) {
        invalid_send(cx, true, "绑定模型");
    }

    #[gpui::test]
    fn ai_panel_load_error_is_not_empty_state_and_retry_runs_real_load(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        fixture
            .storage
            .with_ai_connection(|conn| {
                let mut prefs = settings::load_panel_preferences(conn)?;
                prefs.last_session_id = Some("does-not-exist".into());
                // 只在 fixture 制造损坏/过期指针；正常写入会被 SQLite FK 正确阻止。
                conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
                settings::save_panel_preferences(conn, 1, &prefs, prefs.revision)?;
                conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
                Ok(())
            })
            .unwrap();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(view.read_with(cx, |main, _| main.ai_chat.error.is_some()));
        assert!(cx.debug_bounds("ai-chat-retry").is_some());
        assert!(view.read_with(cx, |main, _| {
            main.controller.state().ai_conversation.messages.is_empty()
        }));
        // 修复 fixture 中的坏指针，再实际点击重试；不向主视图注入成功 DTO。
        fixture
            .storage
            .with_ai_connection(|conn| {
                let mut prefs = settings::load_panel_preferences(conn)?;
                let revision = prefs.revision;
                prefs.last_session_id = None;
                settings::save_panel_preferences(conn, 2, &prefs, revision)?;
                Ok(())
            })
            .unwrap();
        click_selector(cx, "ai-chat-retry");
        wait_idle(&view, cx);
        assert!(view.read_with(cx, |main, _| main.ai_chat.error.is_none()));
        assert!(
            cx.debug_bounds("ai-chat-retry").is_none(),
            "正常空态不得显示加载失败的重试按钮"
        );
        assert!(cx.debug_bounds("ai-chat-input").is_some());
        assert_eq!(fixture.counts(), (0, 0, 0, 0));
    }

    #[gpui::test]
    fn ai_panel_fold_part_persists_and_restores_in_fresh_main(cx: &mut TestAppContext) {
        let fixture = Fixture::seeded();
        let before = fixture.counts();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        let collapsed_height = cx
            .debug_bounds("ai-message:one-assistant")
            .unwrap()
            .size
            .height;
        click_selector(cx, "ai-part:one-assistant-summary");
        wait_idle(&view, cx);
        assert_eq!(
            view.read_with(cx, |main, _| main
                .controller
                .state()
                .ai_conversation
                .expanded_parts
                .clone()),
            vec!["one-assistant-summary"]
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&fixture.ui_state("one").expanded_part_ids_json)
                .unwrap(),
            vec!["one-assistant-summary"]
        );
        assert!(
            cx.debug_bounds("ai-message:one-assistant")
                .unwrap()
                .size
                .height
                > collapsed_height,
            "展开必须实际显示摘要内容"
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        let (view, cx) = open_main(&mut cx.cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .expanded_parts
                .contains(&"one-assistant-summary".into())
        }));
        click_selector(cx, "ai-part:one-assistant-summary");
        wait_idle(&view, cx);
        assert!(view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .expanded_parts
                .is_empty()
        }));
        assert_eq!(fixture.ui_state("one").expanded_part_ids_json, "[]");
        assert_eq!(
            cx.debug_bounds("ai-message:one-assistant")
                .unwrap()
                .size
                .height,
            collapsed_height
        );
        click_selector(cx, "ai-chat-close");
        wait_idle(&view, cx);
        open_panel(&view, cx);
        assert!(view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .expanded_parts
                .is_empty()
        }));
        assert_eq!(fixture.counts(), before);
    }

    #[gpui::test]
    fn ai_panel_small_viewports_enforce_drawer_embedded_caps_without_layout_panic(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::seeded();
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
        open_panel(&view, cx);
        for width in [900., 760.] {
            cx.simulate_resize(size(px(width), px(760.)));
            wait_idle(&view, cx);
            for mode in [settings::PanelMode::Drawer, settings::PanelMode::Embedded] {
                if view.read_with(cx, |main, _| main.ai_chat.mode) != mode {
                    click_selector(cx, "ai-chat-mode");
                    wait_idle(&view, cx);
                }
                let bounds = cx.debug_bounds("ai-chat-panel").unwrap();
                let viewport = cx.update(|window, _| window.viewport_size());
                let cap = (f32::from(viewport.width) as f64 * mode.max_ratio()).floor() as f32;
                assert!(
                    f32::from(bounds.size.width) <= cap + 1.,
                    "{mode:?}: panel {:?} exceeds {cap} at {width}",
                    bounds.size
                );
                assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
                assert!(
                    bounds.right() <= viewport.width + px(1.),
                    "面板不能超出窗口右边界"
                );
                assert_eq!(fixture.preferences().preferred_mode, mode);
                assert_session(&view, cx, "one");
                // 与组件官方测试一致，handle 命中点由真实 panel 左边界推导，不硬编码窗口坐标。
                let boundary = bounds.left();
                let y = bounds.center().y;
                let revision = fixture.preferences().revision;
                cx.simulate_mouse_down(
                    point(boundary - px(2.), y),
                    MouseButton::Left,
                    Modifiers::default(),
                );
                cx.simulate_mouse_move(
                    point(boundary + px(10.), y),
                    Some(MouseButton::Left),
                    Modifiers::default(),
                );
                cx.simulate_mouse_move(
                    point(boundary + px(45.), y),
                    Some(MouseButton::Left),
                    Modifiers::default(),
                );
                cx.simulate_mouse_up(
                    point(boundary + px(45.), y),
                    MouseButton::Left,
                    Modifiers::default(),
                );
                wait_idle(&view, cx);
                assert!(
                    view.read_with(cx, |main, _| main.ai_chat.open),
                    "resize 不能被遮罩当作关闭点击"
                );
                let resized = cx.debug_bounds("ai-chat-panel").unwrap();
                assert!(f32::from(resized.size.width) <= cap + 1.);
                let persisted = fixture.preferences();
                assert!(persisted.revision > revision, "resize 回调必须真实保存偏好");
                let saved_width = if mode == settings::PanelMode::Drawer {
                    persisted.drawer_width_dp
                } else {
                    persisted.embedded_width_dp
                };
                assert!(
                    (saved_width - f32::from(resized.size.width) as f64).abs() <= 1.,
                    "拖拽宽度必须和落库值一致"
                );
            }
        }
    }

    // ---- 审批卡交互（GPUI test-support，子 Agent B）----
    //
    // 这里不再新建孤立组件：先用生产存储入口造出与 `ai_conversation_record_approval`
    // 等价的"等待审批"片段（conversation::send 建会话/Turn/用户消息 → create_tool_call
    // (waiting_approval) → create_approval(pending) → upsert_event_part(notice) → Turn 置
    // waiting_approval），再一律走生产 Load → AppState → 渲染快照 → 实际控件。

    /// 造一条 pending 审批片段。Agent 故意不绑定模型：批准后 `prepare_ai_resume_job`
    /// 会明确失败，从而只验证"决定已提交"，不伪造恢复执行成功。
    fn seed_approval(
        fixture: &Fixture,
        session: &str,
        approval_id: &str,
        part_status: sessions::PartStatus,
        decision: Option<&str>,
    ) {
        let agent = fixture.agent(None);
        let now = 1_700_000_000_000_i64;
        let turn_id = format!("{session}-turn");
        let user_id = format!("{session}-user");
        let assistant_id = format!("{session}-assistant");
        let tool_call_id = format!("{turn_id}-toolcall-1");
        let part_id = format!("{assistant_id}-approval-1");
        let plan_hash = "plan-hash-approval-seed";
        fixture
            .storage
            .with_ai_connection(|conn| {
                conversation::send(
                    conn,
                    now,
                    true,
                    session,
                    &agent,
                    &turn_id,
                    &user_id,
                    "请帮我删除一行数据",
                    "{}",
                    "{}",
                )?;
                sessions::create_tool_call(
                    conn,
                    now,
                    &tool_call_id,
                    session,
                    &turn_id,
                    1,
                    0,
                    "provider-call-1",
                    "db.execute",
                    1,
                    None,
                    r#"{"sql":"DELETE FROM orders WHERE id=1"}"#,
                    "{}",
                    plan_hash,
                    "waiting_approval",
                )?;
                let approval = fluxdb_storage::ai_platform::audits::create_approval(
                    conn,
                    now,
                    approval_id,
                    &tool_call_id,
                    plan_hash,
                    "{}",
                    None,
                )?;
                let mut payload = serde_json::json!({
                    "text": "工具 db.execute 需要你先批准后才会执行。",
                    "approval_id": approval_id,
                    "plan_hash": plan_hash,
                    "approval_revision": approval.revision,
                });
                if let Some(decision) = decision {
                    payload["decision"] = serde_json::Value::from(decision);
                }
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &part_id,
                    sessions::PartKind::Notice,
                    part_status,
                    &payload.to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                let turn = sessions::load_turn(conn, &turn_id)?;
                sessions::update_turn_status(
                    conn,
                    now,
                    &turn_id,
                    sessions::TurnStatus::WaitingApproval,
                    None,
                    turn.revision,
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// 造一条普通片段（文本 + 工具调用/结果引用），用于审批卡之外的回归。
    fn seed_regular_parts(fixture: &Fixture, session: &str) {
        let agent = fixture.agent(None);
        let now = 1_700_000_000_100_i64;
        let turn_id = format!("{session}-turn");
        let user_id = format!("{session}-user");
        let assistant_id = format!("{session}-assistant");
        let tool_call_id = format!("{turn_id}-toolcall-1");
        fixture
            .storage
            .with_ai_connection(|conn| {
                conversation::send(
                    conn, now, true, session, &agent, &turn_id, &user_id, "解释这段 SQL", "{}",
                    "{}",
                )?;
                // tool_call_ref/tool_result_ref 受 CHECK 约束，必须绑定一条真实工具调用事实行。
                sessions::create_tool_call(
                    conn,
                    now,
                    &tool_call_id,
                    session,
                    &turn_id,
                    1,
                    0,
                    "provider-call-1",
                    "db.query",
                    1,
                    None,
                    r#"{"sql":"SELECT 1"}"#,
                    "{}",
                    "plan-hash-regular-seed",
                    "succeeded",
                )?;
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &format!("{assistant_id}-tool-call"),
                    sessions::PartKind::ToolCallRef,
                    sessions::PartStatus::Complete,
                    &serde_json::json!({"text": "调用 db.query"}).to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                conversation::upsert_event_part(
                    conn,
                    now,
                    session,
                    &turn_id,
                    &assistant_id,
                    &format!("{assistant_id}-tool-result"),
                    sessions::PartKind::ToolResultRef,
                    sessions::PartStatus::Complete,
                    &serde_json::json!({"text": "返回 3 行"}).to_string(),
                    Some(&tool_call_id),
                    None,
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// 直接从真实 SQLite 读回审批状态与 revision（不与 UI 断言混在一起）。
    fn approval_state(
        fixture: &Fixture,
        approval_id: &str,
    ) -> (fluxdb_storage::ai_platform::audits::ApprovalStatus, i64) {
        fixture
            .storage
            .with_ai_connection(|conn| {
                let view =
                    fluxdb_storage::ai_platform::audits::load_approval(conn, approval_id)?;
                Ok((view.status, view.revision))
            })
            .unwrap()
    }

    #[gpui::test]
    fn ai_panel_pending_approval_renders_approve_and_deny_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-pending",
            sessions::PartStatus::Pending,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        // 片段确实来自生产投影且状态是 pending，而不是给孤立组件塞数据。
        let projected = view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .flat_map(|message| &message.parts)
                .find(|part| part.id == "approval-assistant-approval-1")
                .map(|part| (part.kind.clone(), part.status.clone()))
        });
        assert_eq!(projected, Some(("notice".into(), "pending".into())));
        assert!(
            cx.debug_bounds("ai-message:approval-assistant").is_some(),
            "审批片段必须挂在助手消息上渲染"
        );
        let approve = cx
            .debug_bounds("ai-approve:appr-pending")
            .expect("pending 审批必须渲染'批准执行'控件");
        let deny = cx
            .debug_bounds("ai-deny:appr-pending")
            .expect("pending 审批必须渲染'拒绝'控件");
        assert!(approve.size.width > px(0.) && approve.size.height > px(0.));
        assert!(deny.size.width > px(0.) && deny.size.height > px(0.));
    }

    #[gpui::test]
    fn ai_panel_approve_click_dispatches_real_decision(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-approve",
            sessions::PartStatus::Pending,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        let before = approval_state(&fixture, "appr-approve");
        assert_eq!(before.0.as_str(), "pending", "初始审批必须是待处理");
        click_selector(cx, "ai-approve:appr-approve");
        wait_idle(&view, cx);
        let after = approval_state(&fixture, "appr-approve");
        assert_eq!(
            after.0.as_str(),
            "approved",
            "点击'批准执行'必须把审批决定写进数据库"
        );
        assert!(
            after.1 > before.1,
            "决定提交后审批 revision 必须递增，证明是真实 CAS 决定而非 UI 假象"
        );
    }

    #[gpui::test]
    fn ai_panel_decided_approval_hides_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-decided",
            sessions::PartStatus::Pending,
            Some("denied"),
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(cx.debug_bounds("ai-message:approval-assistant").is_some());
        assert!(
            cx.debug_bounds("ai-approve:appr-decided").is_none(),
            "已决定的审批不能再出现批准按钮"
        );
        assert!(
            cx.debug_bounds("ai-deny:appr-decided").is_none(),
            "已决定的审批不能再出现拒绝按钮"
        );
    }

    #[gpui::test]
    fn ai_panel_non_pending_approval_hides_controls(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_approval(
            &fixture,
            "approval",
            "appr-complete",
            sessions::PartStatus::Complete,
            None,
        );
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(cx.debug_bounds("ai-message:approval-assistant").is_some());
        assert!(
            cx.debug_bounds("ai-approve:appr-complete").is_none(),
            "非 pending 片段不能出现批准按钮"
        );
        assert!(
            cx.debug_bounds("ai-deny:appr-complete").is_none(),
            "非 pending 片段不能出现拒绝按钮"
        );
    }

    #[gpui::test]
    fn ai_panel_regular_text_and_tool_refs_still_render(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        seed_regular_parts(&fixture, "regular");
        let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
        open_panel(&view, cx);
        assert!(
            cx.debug_bounds("ai-message:regular-user").is_some(),
            "普通用户文本消息必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-message:regular-assistant").is_some(),
            "普通助手消息必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-part:regular-assistant-tool-call").is_some(),
            "tool_call_ref 卡片必须仍渲染"
        );
        assert!(
            cx.debug_bounds("ai-part:regular-assistant-tool-result").is_some(),
            "tool_result_ref 卡片必须仍渲染"
        );
    }
}
