// 由 main.rs 同级 include；fixture 独立，交互走生产 Select、后台命令和 Load 回桥。
#[cfg(test)]
mod ai_chat_model_selection_tests {
    use super::*;
    use fluxdb_app::{AiAgentSaveInput, AiConversationCommand, AiModelSaveInput};
    use fluxdb_storage::ai_platform::{conversation, model_selection, models, sessions};
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    const SESSION: &str = "b-model-selection-session";
    const USER: &str = "b-model-selection-user";
    const ASSISTANT: &str = "b-model-selection-assistant";

    struct Fixture {
        root: PathBuf,
        storage: FileStorage,
        ai_storage: bool,
        first: String,
        second: String,
    }

    impl Fixture {
        fn new(seeded_session: bool) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "fluxdb-ai-chat-model-b-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("fixture 时钟")
                    .as_nanos(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            let mut fixture = Self {
                storage: FileStorage::new(root.clone()),
                root,
                ai_storage: true,
                first: String::new(),
                second: String::new(),
            };
            let mut controller = fixture.controller();
            for (slot, label) in [
                (&mut fixture.first, "B selection Alpha"),
                (&mut fixture.second, "B selection Beta"),
            ] {
                let event = controller.dispatch(AppCommand::SaveAiModel(model_input(
                    None, label, true, None,
                )));
                let AppEvent::AiModelSaved { id, .. } = event else {
                    panic!("隔离模型保存失败: {event:?}");
                };
                *slot = id;
            }
            let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                compression_trigger_percent: 80,
                id: None,
                display_name: "B selection Agent".into(),
                description: "isolated interaction fixture".into(),
                system_prompt: "Explain only; do not execute tools".into(),
                model_profile_id: Some(fixture.first.clone()),
                allowed_tools: Vec::new(),
                enabled: true,
                expected_revision: 0,
            }));
            let AppEvent::AiAgentSaved { id: agent, .. } = event else {
                panic!("隔离 Agent 保存失败: {event:?}");
            };
            if seeded_session {
                fixture
                    .storage
                    .with_ai_connection(|conn| {
                        conversation::send(
                            conn,
                            1,
                            true,
                            SESSION,
                            &agent,
                            "b-model-selection-turn",
                            USER,
                            "B persisted selection message",
                            "{}",
                            "{}",
                        )?;
                        conversation::checkpoint(
                            conn,
                            2,
                            SESSION,
                            "b-model-selection-turn",
                            ASSISTANT,
                            "B persisted reply",
                            "",
                            Some(sessions::TurnStatus::Completed),
                            None,
                        )?;
                        Ok(())
                    })
                    .expect("隔离历史种子");
                for command in [
                    AiConversationCommand::Load {
                        session_id: Some(SESSION.into()),
                    },
                    AiConversationCommand::SelectModel {
                        model_id: Some(fixture.first.clone()),
                        expected_revision: 0,
                    },
                    AiConversationCommand::SaveUi {
                        draft: "B saved unsent draft".into(),
                        expanded_parts: Vec::new(),
                        mode: fluxdb_storage::ai_platform::settings::PanelMode::Drawer,
                        drawer_width: 460.,
                        embedded_width: 420.,
                    },
                ] {
                    assert!(matches!(
                        controller.dispatch(AppCommand::AiConversation(command)),
                        AppEvent::AiConversationLoaded(_)
                    ));
                }
            }
            fixture
        }

        fn controller(&self) -> AppController {
            let mut controller = AppController::new();
            if self.ai_storage {
                controller.set_ai_storage(self.storage.clone());
            }
            if self.ai_storage {
                assert!(matches!(
                    controller.dispatch(AppCommand::RefreshAiSettings),
                    AppEvent::AiSettingsLoaded
                ));
            }
            controller
        }

        fn without_ai_storage(mut self) -> Self {
            self.ai_storage = false;
            self
        }

        fn selection(&self, session: Option<&str>) -> (Option<String>, i64) {
            self.storage
                .with_ai_connection(|conn| model_selection::load(conn, session))
                .expect("独立读取模型选择")
        }

        fn counts(&self) -> (i64, i64, i64, i64) {
            self.storage
                .with_ai_connection(|conn| {
                    let count = |table: &str| {
                        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .expect("fixture 计数")
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
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.root) {
                eprintln!(
                    "清理 B 模型选择 fixture 失败: {}: {error}",
                    self.root.display()
                );
            }
        }
    }

    fn type_input(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, text: &str) {
        draw(cx);
        let bounds = cx.debug_bounds("ai-chat-input").expect("实际输入区域");
        cx.simulate_click(
            point(bounds.left() + px(20.), bounds.top() + px(20.)),
            Modifiers::default(),
        );
        assert!(cx.update(|window, cx| {
            view.read(cx)
                .ai_chat
                .input
                .focus_handle(cx)
                .is_focused(window)
        }));
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a backspace"
        } else {
            "ctrl-a backspace"
        });
        cx.simulate_input(text);
        wait_idle(view, cx);
    }

    fn reference_kinds(view: &Entity<NavicatMain>, cx: &VisualTestContext) -> Vec<String> {
        view.read_with(cx, |main, _| {
            main.ai_chat
                .reference_options
                .iter()
                .map(|reference| reference.kind.clone())
                .collect()
        })
    }

    fn chips(
        view: &Entity<NavicatMain>,
        cx: &VisualTestContext,
    ) -> Vec<fluxdb_app::AiContextReference> {
        view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_conversation
                .context_references
                .clone()
        })
    }

    fn model_input(
        id: Option<String>,
        label: &str,
        enabled: bool,
        revision: Option<i64>,
    ) -> AiModelSaveInput {
        AiModelSaveInput {
            id,
            display_name: label.into(),
            provider: "openai".into(),
            model: "b-selection-not-a-real-model".into(),
            endpoint: Some("https://b-selection.example.invalid/v1".into()),
            endpoint_is_full_url: false,
            enabled,
            egress: true,
            context_window_tokens: Some(8192),
            max_output_tokens: Some(1024),
            input_price_micros: None,
            output_price_micros: None,
            cached_input_price_micros: None,
            api_key: None,
            clear_key: false,
            expected_revision: revision,
        }
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        fixture: &Fixture,
        theme: ThemeMode,
    ) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
        cx.update(gpui_component::init);
        let storage = fixture.storage.clone();
        let controller = fixture.controller();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            navicat_main_new(window, cx, controller, storage, theme)
        });
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        click(cx, "ask-ai");
        wait_idle(&view, cx);
        assert!(cx.debug_bounds("ai-chat-panel").is_some());
        view.read_with(cx, |main, _| {
            let state = main.controller.state();
            let snapshot = render_state_snapshot(state);
            assert_eq!(state.ai_models.len(), 2, "生产加载应包含两条 fixture 模型");
            assert_eq!(snapshot.ai_models, state.ai_models);
            for id in [&fixture.first, &fixture.second] {
                let model = state
                    .ai_models
                    .iter()
                    .find(|model| &model.id == id)
                    .expect("AppState 应保留已存模型 ID");
                if !model.enabled {
                    if state.ai_conversation.selected_model_id.as_ref() == Some(id) {
                        assert!(
                            main.ai_chat
                                .model_options
                                .iter()
                                .any(|option| { &option.id == id && option.unavailable }),
                            "重开必须保留停用当前模型的不可用占位"
                        );
                    } else {
                        assert!(
                            !main
                                .ai_chat
                                .model_options
                                .iter()
                                .any(|option| &option.id == id)
                        );
                    }
                    continue;
                }
                assert!(
                    main.ai_chat
                        .model_options
                        .iter()
                        .any(|option| { &option.id == id && !option.unavailable }),
                    "已加载的生成模型未绑定到生产 Select: id={id}, kind={:?}, options={:?}",
                    model.kind,
                    main.ai_chat
                        .model_options
                        .iter()
                        .map(|option| (&option.id, &option.label, option.unavailable))
                        .collect::<Vec<_>>()
                );
            }
        });
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    fn click(cx: &mut VisualTestContext, selector: &str) {
        draw(cx);
        let bounds = bounds(cx, selector).unwrap_or_else(|| panic!("生产控件未渲染: {selector}"));
        assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
        // 锁定 GPUI 的 debug_bounds 仅收 &'static str；动态 fixture ID 仅在测试内使用。
        cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
    }

    fn wait_idle(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            cx.run_until_parked();
            if view.read_with(cx, |main, _| {
                main.ai_chat._io.is_none()
                    && !main.ai_chat.reference_loading
                    && main.ai_chat.pending.is_empty()
                    && !main.ai_chat.loading
                    && !main.ai_chat.recovery_pending
            }) {
                draw(cx);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "生产模型选择后台 IO 超时: {:?}",
                view.read_with(cx, |main, _| main.ai_chat.error.clone())
            );
        }
    }

    fn assert_selection(
        fixture: &Fixture,
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        session: Option<&str>,
        selected: Option<&str>,
    ) -> i64 {
        draw(cx);
        assert!(cx.debug_bounds("ai-chat-model-select").is_some());
        let stored = fixture.selection(session);
        let (state, snapshot, value, index, options) = view.read_with(cx, |main, cx| {
            (
                main.controller.state().ai_conversation.clone(),
                render_state_snapshot(main.controller.state()).ai_conversation,
                main.ai_chat.model_select.read(cx).selected_value().cloned(),
                main.ai_chat.model_select.read(cx).selected_index(cx),
                main.ai_chat.model_options.clone(),
            )
        });
        assert_eq!(state, snapshot, "AppState 到生产快照不能丢选择字段");
        assert_eq!(state.session_id.as_deref(), session);
        assert_eq!(state.selected_model_id.as_deref(), selected);
        assert_eq!(stored.0.as_deref(), selected);
        assert_eq!(state.model_selection_revision, stored.1);
        assert_eq!(value.as_deref(), Some(selected.unwrap_or("")));
        assert!(
            options
                .iter()
                .any(|option| option.id == selected.unwrap_or(""))
        );
        // Select 的标题按游标读 delegate；只核对 selected_value 会漏掉候选重排后的错标。
        assert_eq!(
            options[index.expect("实际 Select 必须有标题游标").row].id,
            selected.unwrap_or(""),
            "真实 Select 标题游标必须指向当前选择，而非旧候选索引"
        );
        stored.1
    }

    fn choose_mouse(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, id: &str) {
        click(cx, "ai-chat-model-select");
        click(cx, &format!("ai-chat-model-option:{id}"));
        wait_idle(view, cx);
    }

    fn input(view: &Entity<NavicatMain>, cx: &VisualTestContext) -> String {
        view.read_with(cx, |main, cx| {
            main.ai_chat.input.read(cx).value().to_string()
        })
    }

    fn replace_input(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, text: &str) {
        draw(cx);
        let bounds = cx.debug_bounds("ai-chat-input").expect("实际输入区域");
        cx.simulate_click(
            point(bounds.left() + px(20.), bounds.top() + px(20.)),
            Modifiers::default(),
        );
        assert!(cx.update(|window, cx| {
            view.read(cx)
                .ai_chat
                .input
                .focus_handle(cx)
                .is_focused(window)
        }));
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a backspace"
        } else {
            "ctrl-a backspace"
        });
        cx.simulate_input(text);
        assert_eq!(input(view, cx), text);
    }

    fn finish(view: Entity<NavicatMain>, cx: &mut VisualTestContext) {
        // 释放生产通知 Timer 和回桥任务，避免 waker 泄漏到下一个 GPUI scheduler。
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn ai_chat_model_selection_existing_data_loads_state_snapshot_and_select(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new(true);
        let before = fixture.counts();
        for theme in [ThemeMode::Light, ThemeMode::Dark] {
            let (view, visual) = open(cx, &fixture, theme);
            assert_eq!(
                assert_selection(&fixture, &view, visual, Some(SESSION), Some(&fixture.first)),
                1
            );
            assert_eq!(input(&view, visual), "B saved unsent draft");
            assert!(bounds(visual, &format!("ai-message:{USER}")).is_some());
            assert!(bounds(visual, &format!("ai-message:{ASSISTANT}")).is_some());
            click(visual, "ai-chat-model-select");
            draw(visual);
            for id in ["", fixture.first.as_str(), fixture.second.as_str()] {
                assert!(bounds(visual, &format!("ai-chat-model-option:{id}")).is_some());
            }
            let options = view.read_with(visual, |main, _| main.ai_chat.model_options.clone());
            assert_eq!(options.len(), 3);
            assert!(options.iter().any(|option| {
                option.id == fixture.first
                    && option.label.contains("B selection Alpha")
                    && !option.unavailable
            }));
            visual.simulate_keystrokes("escape");
            wait_idle(&view, visual);
            assert!(view.read_with(visual, |main, _| main.ai_chat.open));
            assert_eq!(fixture.counts(), before);
            finish(view, visual);
        }
    }

    #[gpui::test]
    fn ai_chat_model_selection_mouse_saves_immediately_and_fresh_main_restores(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new(true);
        let before = fixture.counts();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        choose_mouse(&view, visual, &fixture.second);
        assert_eq!(
            assert_selection(
                &fixture,
                &view,
                visual,
                Some(SESSION),
                Some(&fixture.second)
            ),
            2
        );
        assert_eq!(input(&view, visual), "B saved unsent draft");
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        click(visual, "ask-ai");
        wait_idle(&view, visual);
        assert_selection(
            &fixture,
            &view,
            visual,
            Some(SESSION),
            Some(&fixture.second),
        );
        finish(view, visual);
        let (view, visual) = open(cx, &fixture, ThemeMode::Dark);
        assert_selection(
            &fixture,
            &view,
            visual,
            Some(SESSION),
            Some(&fixture.second),
        );
        assert_eq!(fixture.counts(), before, "切换不能新增消息或重放工具");
        finish(view, visual);
    }

    #[gpui::test]
    fn ai_chat_model_selection_keyboard_confirms_and_escape_does_not_commit(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new(true);
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        let target = view.read_with(visual, |main, _| {
            let options = &main.ai_chat.model_options;
            options
                .iter()
                .position(|option| option.id == fixture.second)
                .unwrap()
        });
        click(visual, "ai-chat-model-select");
        draw(visual);
        // PopupMenu 打开时没有键盘游标，第一次 Down 选中首项。
        for _ in 0..=target {
            visual.simulate_keystrokes("down");
        }
        visual.simulate_keystrokes("enter");
        wait_idle(&view, visual);
        let revision = assert_selection(
            &fixture,
            &view,
            visual,
            Some(SESSION),
            Some(&fixture.second),
        );
        click(visual, "ai-chat-model-select");
        draw(visual);
        visual.simulate_keystrokes("up");
        visual.simulate_keystrokes("escape");
        wait_idle(&view, visual);
        assert_eq!(
            assert_selection(
                &fixture,
                &view,
                visual,
                Some(SESSION),
                Some(&fixture.second)
            ),
            revision
        );
        assert!(view.read_with(visual, |main, _| main.ai_chat.open));
        assert_eq!(input(&view, visual), "B saved unsent draft");
        choose_mouse(&view, visual, "");
        assert_eq!(
            assert_selection(&fixture, &view, visual, Some(SESSION), None),
            revision + 1
        );
        finish(view, visual);
    }

    #[gpui::test]
    fn ai_chat_model_selection_new_resets_follow_without_changing_previous_session(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new(true);
        let before = fixture.counts();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        choose_mouse(&view, visual, &fixture.second);
        let old = fixture.selection(Some(SESSION));
        click(visual, "ai-chat-new");
        wait_idle(&view, visual);
        assert_eq!(assert_selection(&fixture, &view, visual, None, None), 0);
        assert!(input(&view, visual).is_empty());
        assert!(view.read_with(visual, |main, _| {
            main.controller.state().ai_conversation.messages.is_empty()
        }));
        assert_eq!(fixture.selection(Some(SESSION)), old);
        choose_mouse(&view, visual, &fixture.first);
        assert_selection(&fixture, &view, visual, None, Some(&fixture.first));
        click(visual, "ai-chat-new");
        wait_idle(&view, visual);
        assert_eq!(assert_selection(&fixture, &view, visual, None, None), 0);
        assert_eq!(fixture.selection(Some(SESSION)), old);
        assert_eq!(fixture.counts(), before, "新建和选模型不能制造空历史");
        finish(view, visual);
    }

    #[gpui::test]
    fn ai_chat_model_selection_unsent_draft_restores_in_fresh_main(cx: &mut TestAppContext) {
        let fixture = Fixture::new(false);
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        assert_selection(&fixture, &view, visual, None, None);
        choose_mouse(&view, visual, &fixture.second);
        replace_input(&view, visual, "B draft model remains unsent");
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
        let (view, visual) = open(cx, &fixture, ThemeMode::Dark);
        assert_eq!(
            assert_selection(&fixture, &view, visual, None, Some(&fixture.second)),
            1
        );
        assert_eq!(input(&view, visual), "B draft model remains unsent");
        assert_eq!(fixture.counts(), (0, 0, 0, 0));
        finish(view, visual);
    }

    #[gpui::test]
    fn ai_chat_model_selection_disabled_model_error_preserves_selection_and_input(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new(true);
        let before = fixture.counts();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        let selection = fixture.selection(Some(SESSION));
        let stored = fixture
            .storage
            .with_ai_connection(|conn| models::load_model(conn, &fixture.first))
            .unwrap();
        assert!(matches!(
            fixture
                .controller()
                .dispatch(AppCommand::SaveAiModel(model_input(
                    Some(fixture.first.clone()),
                    "B selection Alpha",
                    false,
                    Some(stored.revision),
                ))),
            AppEvent::AiModelSaved { .. }
        ));
        // 外部配置变化通过生产 Refresh 更新 AppState，不手工注入停用 DTO。
        view.update(visual, |main, cx| {
            main.dispatch(AppCommand::RefreshAiSettings, cx);
        });
        wait_idle(&view, visual);
        assert_selection(&fixture, &view, visual, Some(SESSION), Some(&fixture.first));
        assert!(view.read_with(visual, |main, _| {
            main.ai_chat.model_options.iter().any(|option| {
                option.id == fixture.first && option.unavailable && option.label.contains("不可用")
            })
        }));
        click(visual, "ai-chat-model-select");
        click(visual, &format!("ai-chat-model-option:{}", fixture.first));
        visual.simulate_keystrokes("escape");
        wait_idle(&view, visual);
        assert_eq!(fixture.selection(Some(SESSION)), selection);
        replace_input(&view, visual, "B disabled model must not fallback");
        click(visual, "ai-chat-send");
        wait_idle(&view, visual);
        assert!(view.read_with(visual, |main, _| {
            main.ai_chat
                .error
                .as_deref()
                .is_some_and(|error| error.contains("不可用"))
                && main.ai_chat.running.is_none()
        }));
        assert!(visual.debug_bounds("ai-chat-retry").is_some());
        assert_eq!(input(&view, visual), "B disabled model must not fallback");
        assert_selection(&fixture, &view, visual, Some(SESSION), Some(&fixture.first));
        assert_eq!(fixture.selection(Some(SESSION)), selection);
        assert_eq!(fixture.counts(), before, "失败不能启动其他模型或改变历史");
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
        let (view, visual) = open(cx, &fixture, ThemeMode::Dark);
        assert_selection(&fixture, &view, visual, Some(SESSION), Some(&fixture.first));
        assert_eq!(input(&view, visual), "B disabled model must not fallback");
        finish(view, visual);
    }

    #[gpui::test]
    async fn ai_chat_model_selection_first_send_transfers_draft_to_session(
        cx: &mut TestAppContext,
    ) {
        assert!(cfg!(debug_assertions), "禁止 release 测试回退到真实模型");
        cx.executor().allow_parking();
        let fixture = Fixture::new(false);
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        choose_mouse(&view, visual, &fixture.second);
        let draft = fixture.selection(None);
        let model = fixture
            .storage
            .with_ai_connection(|conn| models::load_model(conn, &fixture.second))
            .unwrap();
        assert_eq!(model.auth_mode, models::AuthMode::None);
        assert_eq!(
            model.endpoint.as_deref(),
            Some("https://b-selection.example.invalid/v1")
        );
        // 仅替换已有事件 source；点击发送、Turn 创建、选择转移及回桥仍是生产链路。
        view.update(visual, |main, _| main.ai_chat.scripted_fixture = true);
        replace_input(&view, visual, "B first send selection transfer");
        click(visual, "ai-chat-send");
        wait_idle(&view, visual);
        let chat = view.read_with(visual, |main, _| {
            assert!(
                main.ai_chat.error.is_none(),
                "发送失败: {:?}",
                main.ai_chat.error
            );
            main.controller.state().ai_conversation.clone()
        });
        let session = chat.session_id.expect("真实发送应创建 Session");
        assert_eq!(fixture.selection(Some(&session)), draft);
        assert_eq!(fixture.selection(None), (None, 0));
        assert_selection(
            &fixture,
            &view,
            visual,
            Some(&session),
            Some(&fixture.second),
        );
        let turns = fixture
            .storage
            .with_ai_connection(|conn| sessions::list_turns(conn, &session))
            .unwrap();
        assert_eq!(turns.len(), 1);
        let snapshot: serde_json::Value =
            serde_json::from_str(&turns[0].runtime_snapshot_json).unwrap();
        assert_eq!(snapshot["model_profile_id"], fixture.second);
        assert!(input(&view, visual).is_empty());
        assert!(view.read_with(visual, |main, _| main.ai_chat.running.is_some()));
        click(visual, "ai-chat-model-select");
        draw(visual);
        assert!(
            bounds(visual, &format!("ai-chat-model-option:{}", fixture.first)).is_none(),
            "运行中模型 Select 必须 disabled，不能打开候选"
        );
        assert_eq!(fixture.selection(Some(&session)), draft);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            visual.run_until_parked();
            if view.read_with(visual, |main, _| {
                main.ai_chat.running.is_none()
                    && main
                        .controller
                        .state()
                        .ai_conversation
                        .active_turn_id
                        .is_none()
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "隔离 scripted source 完成超时");
            smol::Timer::after(Duration::from_millis(20)).await;
        }
        wait_idle(&view, visual);
        assert!(view.read_with(visual, |main, _| main.ai_chat.error.is_none()));
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        let before = fixture.counts();
        assert_eq!((before.0, before.1, before.2), (1, 1, 2));
        finish(view, visual);
        let (view, visual) = open(cx, &fixture, ThemeMode::Dark);
        assert_selection(
            &fixture,
            &view,
            visual,
            Some(&session),
            Some(&fixture.second),
        );
        assert_eq!(fixture.counts(), before, "恢复不能重新发送");
        finish(view, visual);
    }

    #[gpui::test]
    fn at_reference_lists_kinds_filters_and_shows_no_match(cx: &mut TestAppContext) {
        let fixture = Fixture::new(false);
        // 通过生产命令种入一条 confirmed 知识条目，让 `@` 候选包含"知识"类型。
        fixture
            .storage
            .with_ai_connection(|conn| {
                fluxdb_storage::ai_platform::knowledge::save(
                    conn,
                    10,
                    &fluxdb_storage::ai_platform::knowledge::KnowledgeEntryEdit {
                        id: "k-amount".into(),
                        title: "订单金额口径".into(),
                        kind: "metric".into(),
                        agent_id: None,
                        connection_id: Some(17),
                        database_name: Some("shop".into()),
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
        fixture
            .storage
            .save_saved_queries(&[fluxdb_core::SavedQuery {
                id: 91,
                connection_id: fluxdb_core::ConnectionId(17),
                database: Some("shop".into()),
                schema: None,
                name: "订单金额口径".into(),
                text: "SELECT 1".into(),
            }])
            .unwrap();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);

        // 输入 `@订单` → 打开候选下拉，至少包含知识条目。
        type_input(&view, visual, "@订单");
        assert!(
            bounds(visual, "ai-chat-reference-select").is_some(),
            "输入 @ 后应显示候选"
        );
        let kinds = reference_kinds(&view, visual);
        assert!(
            kinds.iter().any(|kind| kind == "knowledge"),
            "知识条目应作为 @ 候选：{kinds:?}"
        );
        assert!(
            kinds.iter().any(|kind| kind == "saved_query"),
            "保存查询应作为 @ 候选：{kinds:?}"
        );

        // 不存在的检索词 → 明确"无匹配"，且与"检索失败"区分。
        type_input(&view, visual, "@完全不存在的引用xyz");
        assert!(
            view.read_with(visual, |main, _| main.ai_chat.reference_no_match),
            "无匹配要可见且与检索失败区分"
        );
        assert!(view.read_with(visual, |main, _| main.ai_chat.reference_error.is_none()));

        // 命中查询 → 选中知识候选 → chip 出现。
        type_input(&view, visual, "@订单金额");
        let id = view.read_with(visual, |main, _| {
            main.ai_chat
                .reference_options
                .iter()
                .find(|reference| reference.kind == "knowledge")
                .map(|reference| reference.id.clone())
                .expect("应有知识候选")
        });
        click(visual, &format!("ai-reference-candidate:{id}"));
        wait_idle(&view, visual);
        let selected = chips(&view, visual);
        assert_eq!(selected.len(), 1, "选择后应写入一个引用 chip");
        assert_eq!(selected[0].kind, "knowledge");

        // 移除 chip → 引用清空，下一次提交不再包含。
        let chip_id = selected[0].id.clone();
        click(visual, &format!("ai-reference-remove:{chip_id}"));
        wait_idle(&view, visual);
        assert!(chips(&view, visual).is_empty(), "移除后不应保留引用");

        // Esc 关闭候选下拉但不关面板。
        type_input(&view, visual, "@订单");
        assert!(bounds(visual, "ai-chat-reference-select").is_some());
        visual.simulate_keystrokes("escape");
        draw(visual);
        assert!(
            view.read_with(visual, |main, _| !main.ai_chat.reference_open),
            "Esc 应关闭候选下拉"
        );
        assert!(
            bounds(visual, "ai-chat-panel").is_some(),
            "关闭候选不应关闭整个面板"
        );

        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
    }

    #[gpui::test]
    fn at_reference_candidate_ids_are_unique_and_typed(cx: &mut TestAppContext) {
        // 候选 ID 必须唯一，同名对象不能靠裸名区分（按连接/库/schema 全路径生成）。
        let fixture = Fixture::new(false);
        fixture
            .storage
            .with_ai_connection(|conn| {
                for (id, title) in [("k1", "订单金额口径"), ("k2", "订单金额口径")] {
                    fluxdb_storage::ai_platform::knowledge::save(
                        conn,
                        10,
                        &fluxdb_storage::ai_platform::knowledge::KnowledgeEntryEdit {
                            id: id.into(),
                            title: title.into(),
                            kind: "metric".into(),
                            agent_id: None,
                            connection_id: Some(17),
                            database_name: Some("shop".into()),
                            namespace: None,
                            scope_json: "{}".into(),
                            content_json: "{}".into(),
                            source_json: "{}".into(),
                            status: "confirmed".into(),
                        },
                        None,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        type_input(&view, visual, "@订单金额口径");
        let candidates = view.read_with(visual, |main, _| main.ai_chat.reference_options.clone());
        assert!(
            candidates.len() >= 2,
            "两条同名知识都应成为候选：{candidates:?}"
        );
        let mut ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "候选 ID 必须唯一");
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
    }

    #[gpui_kit::test]
    fn reference_search_error_is_visible_without_no_match(cx: &mut TestAppContext) {
        let fixture = Fixture::new(false).without_ai_storage();
        cx.update(gpui_component::init);
        let storage = fixture.storage.clone();
        let controller = fixture.controller();
        let (view, visual) = cx.add_window_view(move |window, cx| {
            navicat_main_new(window, cx, controller, storage, ThemeMode::Light)
        });
        visual.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        click(visual, "ask-ai");
        wait_idle(&view, visual);
        assert!(bounds(visual, "ai-chat-panel").is_some());
        type_input(&view, visual, "@anything");
        assert!(bounds(visual, "ai-reference-search-error").is_some());
        assert!(
            view.read_with(visual, |main, _| main.ai_chat.reference_error.is_some()
                && !main.ai_chat.reference_no_match)
        );
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
    }

    #[gpui_kit::test]
    fn invalid_database_reference_chip_is_visible(cx: &mut TestAppContext) {
        let fixture = Fixture::new(false);
        fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::context_references::add(
                conn,
                None,
                "connection:missing",
                r#"{"id":"connection:missing","label":"removed connection","kind":"connection","source":"17","summary":"连接 · 只读上下文"}"#,
            )
        })
        .unwrap();
        let (view, visual) = open(cx, &fixture, ThemeMode::Light);
        assert!(chips(&view, visual).len() == 1);
        assert!(bounds(visual, "ai-reference-invalid:connection:missing").is_some());
        click(visual, "ai-chat-close");
        wait_idle(&view, visual);
        finish(view, visual);
    }
}
