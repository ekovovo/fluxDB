#[gpui::test]
fn ai_page_query_opens_sidebar_table_fills_conditions_and_shows_rows(cx: &mut TestAppContext) {
    use fluxdb_app::{AiModelSaveInput, AppCommand, AppEvent, TabKind};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ObjectKind, ObjectPath,
    };
    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    let database = temp.path().join("page-query.sqlite");
    let connection = fluxdb_storage::AiConnection::open(&database).expect("SQLite database");
    connection
        .execute_batch(
            "CREATE TABLE records (id INTEGER PRIMARY KEY, name TEXT, status TEXT);
         INSERT INTO records VALUES (1, 'alpha', 'failed'), (2, 'beta', 'ok'),
                                    (3, 'gamma', 'failed');",
        )
        .expect("fixture rows");
    drop(connection);
    let settings = Settings {
        locale: "en".into(),
        show_sidebar: true,
        ..Default::default()
    };
    storage.save_settings(&settings).expect("settings");
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    controller.dispatch(AppCommand::SaveSettings(settings));
    controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
        id: ConnectionId(1),
        name: "Page query fixture".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: database.clone(),
            read_only: true,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
    }]));
    let event = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "offline page query fixture".into(),
        provider: "openai".into(),
        model: "fixture".into(),
        endpoint: Some("https://models.invalid/v1".into()),
        enabled: true,
        ..Default::default()
    }));
    let AppEvent::AiModelSaved { id, .. } = event else {
        panic!("{event:?}")
    };
    let revision = storage
        .with_ai_connection(fluxdb_storage::ai_platform::settings::load_runtime_settings)
        .expect("runtime")
        .revision;
    controller.dispatch(AppCommand::SetAiDefaultModel {
        model_id: Some(id),
        expected_revision: revision,
    });
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let profile = storage.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut app = navicat_main_new(
                window,
                cx,
                controller,
                profile,
                gpui_component::ThemeMode::Light,
            );
            app.ai_chat.recovery_pending = false;
            app.ai_chat.loaded = true;
            app.ai_chat.open = true;
            app.ai_chat.scripted_fixture = true;
            app.ai_chat.scripted_table_query = Some((
                ObjectPath {
                    connection_id: ConnectionId(1),
                    database: Some("main".into()),
                    schema: None,
                    name: "records".into(),
                    kind: ObjectKind::Table,
                },
                serde_json::from_value(serde_json::json!({
                    "page_ref":"fixture", "execute":true, "limit":100,
                    "filters":[{"field":"status","op":"eq","values":["failed"]}],
                    "sort":[{"field":"id","descending":true}]
                }))
                .expect("table query"),
            ));
            app
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1500.), gpui::px(950.)));
    let view = cx.update(|window, cx| {
        window
            .root::<Root>()
            .expect("root")
            .expect("typed root")
            .read(cx)
            .view()
            .clone()
            .downcast::<NavicatMain>()
            .expect("production view")
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("ai-table-query-mode").is_none(),
        "page query mode button must not be rendered"
    );
    let input_bounds = cx.debug_bounds("ai-chat-input").expect("chat input");
    cx.simulate_click(input_bounds.center(), Modifiers::default());
    cx.simulate_input("Find failed records, newest first.");
    cx.update(|window, cx| {
        assert_eq!(
            view.read(cx).ai_chat.input.read(cx).value(),
            "Find failed records, newest first."
        );
        window.draw(cx).clear(cx);
    });
    let send = cx.debug_bounds("ai-chat-send").expect("send button");
    cx.simulate_click(send.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|_, cx| {
        let app = view.read(cx);
        assert!(
            app.ai_chat.running.is_some() || app.ai_chat.loading || app.ai_chat._io.is_some(),
            "send did not start: mode={}, input={:?}, error={:?}",
            false,
            app.ai_chat.input.read(cx).value(),
            app.ai_chat.error,
        );
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    loop {
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
        let finished = cx.update(|_, cx| {
            let app = view.read(cx);
            app.ai_chat.running.is_none()
                && app.ai_chat._io.is_none()
                && app.ai_chat.table_page_task.is_none()
                && !app.ai_chat.loading
        });
        if finished {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "page query did not finish"
        );
        std::thread::yield_now();
    }
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let tab_id = cx.update(|_, cx| {
        let app = view.read(cx);
        let (tab_id, editor) = app
            .controller
            .state()
            .tabs
            .iter()
            .find_map(|tab| match &tab.kind {
                TabKind::DataEditor(editor) if editor.object.name == "records" => {
                    Some((tab.id, editor))
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "opened table page: chat_error={:?}, message={:?}, tabs={:?}, objects={:?}",
                    app.ai_chat.error,
                    app.app_message.as_ref().map(|m| m.text.clone()),
                    app.controller
                        .state()
                        .tabs
                        .iter()
                        .map(|t| t.title.clone())
                        .collect::<Vec<_>>(),
                    app.controller
                        .state()
                        .connections
                        .iter()
                        .flat_map(|c| c.objects.iter().map(|o| o.path.clone()))
                        .collect::<Vec<_>>(),
                )
            });
        assert!(app.data_filter_panels.contains(&tab_id));
        assert!(app.show_connection_browser);
        let bindings = &app
            .ai_chat
            .table_page_binding
            .as_ref()
            .expect("page binding");
        assert_eq!(bindings.filters[0].field, "status");
        assert_eq!(bindings.sort[0].field, "id");
        let page = editor.page.as_ref().expect("query result");
        assert_eq!(page.rows.len(), 2);
        assert_eq!(
            page.rows[0].values[0],
            fluxdb_core::CellValue::Text("3".into())
        );
        let session = app
            .controller
            .state()
            .ai_conversation
            .session_id
            .as_deref()
            .expect("session");
        storage
            .with_ai_connection(|conn| {
                let turns = fluxdb_storage::ai_platform::sessions::list_turns(conn, session)?;
                let snapshot: serde_json::Value =
                    serde_json::from_str(&turns.last().expect("turn").runtime_snapshot_json)
                        .expect("snapshot");
                assert!(
                    snapshot["ui_context"]
                        .as_str()
                        .expect("context")
                        .contains("current_page")
                );
                assert!(
                    !snapshot["ui_context"]
                        .as_str()
                        .expect("context")
                        .contains("alpha")
                );
                Ok(())
            })
            .expect("bounded persisted context");
        let message = app.app_message.as_ref().expect("visible completion");
        assert_eq!(
            message.text,
            crate::tr("app.ai.page_query.queried").to_string()
        );
        tab_id
    });

    assert!(cx.debug_bounds("data-filter-mode-builder").is_some());
    assert!(cx.debug_bounds("data-filter-mode-input").is_some());
    assert!(cx.debug_bounds("data-filter-condition-editor").is_none());
    cx.update(|_, cx| {
        view.update(cx, |this, cx| {
            this.ai_chat.open = false;
            cx.notify();
        });
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let input_mode = cx
        .debug_bounds("data-filter-mode-input")
        .expect("input mode button");
    cx.simulate_click(input_mode.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|_, cx| {
        let app = view.read(cx);
        assert_eq!(
            app.data_filter_modes.get(&tab_id),
            Some(&crate::DataFilterMode::Text)
        );
        assert_eq!(
            app.data_filter_text_input.read(cx).value().to_string(),
            "\"status\" = 'failed'"
        );
    });
    assert!(cx.debug_bounds("data-filter-condition-editor").is_some());
    cx.update(|_, cx| {
        let app = view.read(cx);
        assert_eq!(
            app.data_sort_draft_rules.get(&tab_id).map(Vec::len),
            Some(1),
            "sort form must remain independent from filter input mode"
        );
    });

    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.data_filter_texts
                .insert(tab_id, "\"status\" = 'ok'".to_string());
            this.data_filter_text_input.update(cx, |editor, cx| {
                editor.set_value("\"status\" = 'ok'", window, cx);
            });
        });
    });
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Builder,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });
    cx.update(|_, cx| {
        let app = view.read(cx);
        let rules = app
            .data_filter_draft_rules
            .get(&tab_id)
            .expect("input must flow back into the form");
        assert_eq!(
            rules.len(),
            1,
            "structured input must replace the form contents"
        );
        assert_eq!(rules[0].field.as_deref(), Some("status"));
        assert_eq!(rules[0].operator, crate::DataFilterOperator::Eq);
        assert_eq!(rules[0].values, vec!["ok".to_string()]);
        assert_eq!(
            app.data_filter_modes.get(&tab_id),
            Some(&crate::DataFilterMode::Builder)
        );
    });
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Text,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
    });
    assert_eq!(
        cx.update(|_, cx| view.read(cx).data_filter_text_input.read(cx).value().to_string()),
        "\"status\" = 'ok'"
    );

    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.data_filter_texts.insert(tab_id, "\"id\" > 1".to_string());
            this.data_filter_text_input
                .update(cx, |editor, cx| editor.set_value("\"id\" > 1", window, cx));
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Builder,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
    });
    cx.update(|_, cx| {
        let rules = view
            .read(cx)
            .data_filter_draft_rules
            .get(&tab_id)
            .expect("typed form");
        assert_eq!(
            rules[0].typed_values,
            Some(vec![fluxdb_core::FilterValue::Integer(1)])
        );
    });
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Text,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
    });
    assert_eq!(
        cx.update(|_, cx| view.read(cx).data_filter_text_input.read(cx).value().to_string()),
        "\"id\" > 1",
        "numeric input must keep its type when round-tripping through the form"
    );

    cx.update(|_, cx| {
        view.update(cx, |this, _| {
            this.data_filter_texts
                .insert(tab_id, "lower(\"status\") = 'ok'".to_string());
        });
    });
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Builder,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });
    cx.update(|_, cx| {
        let rules = view
            .read(cx)
            .data_filter_draft_rules
            .get(&tab_id)
            .expect("advanced rule");
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].advanced_expression.as_deref(),
            Some("lower(\"status\") = 'ok'")
        );
    });
    assert!(cx.debug_bounds("data-filter-advanced-rule-0").is_some());

    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.data_filter_texts.remove(&tab_id);
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Text,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
            this.data_filter_texts.remove(&tab_id);
            this.set_data_filter_mode(
                tab_id,
                crate::DataFilterMode::Builder,
                crate::sql_editor_adapter::SqlDialect::Sqlite,
                window,
                cx,
            );
        });
    });
    assert!(
        cx.update(|_, cx| !view.read(cx).data_filter_draft_rules.contains_key(&tab_id)),
        "empty input must clear the previous form"
    );

    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.data_filter_modes
                .insert(tab_id, crate::DataFilterMode::Builder);
            this.data_filter_draft_rules.insert(
                tab_id,
                vec![
                    crate::DataFilterRule {
                        enabled: true,
                        field: Some("status".into()),
                        operator: crate::DataFilterOperator::Eq,
                        values: vec!["failed".into()],
                        advanced_expression: None,
                        typed_values: None,
                        connector: fluxdb_core::FilterCombinator::And,
                        grouped: false,
                    },
                    crate::DataFilterRule {
                        enabled: true,
                        field: Some("status".into()),
                        operator: crate::DataFilterOperator::Eq,
                        values: vec!["ok".into()],
                        advanced_expression: None,
                        typed_values: None,
                        connector: fluxdb_core::FilterCombinator::And,
                        grouped: false,
                    },
                ],
            );
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    let connector = cx
        .debug_bounds("data-filter-connector-0")
        .expect("row connector");
    cx.simulate_click(connector.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|_, cx| {
        let app = view.read(cx);
        let rules = app
            .data_filter_draft_rules
            .get(&tab_id)
            .expect("connector rules");
        assert_eq!(
            rules[0].connector,
            fluxdb_core::FilterCombinator::Or
        );
        assert!(app
            .data_filter_texts
            .get(&tab_id)
            .is_some_and(|text| text.contains(" OR ")));
    });
    let connector = cx
        .debug_bounds("data-filter-connector-0")
        .expect("switched connector");
    cx.simulate_click(connector.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|_, cx| {
        let rules = view
            .read(cx)
            .data_filter_draft_rules
            .get(&tab_id)
            .expect("connector rules");
        assert_eq!(
            rules[0].connector,
            fluxdb_core::FilterCombinator::And
        );
    });
}
