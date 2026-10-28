    #[test]
    fn redis_stream_validation_preserves_bounds_and_payloads() {
        use crate::{
            redis_stream_entry_field_pair, redis_stream_entry_field_pairs_from_snapshot,
            redis_stream_entry_id_validation_error, redis_stream_maxlen_from_input,
            redis_stream_time_from_input,
        };

        let _guard = locale_guard();
        let temp = tempfile::tempdir().expect("locale root");
        init_i18n(&app_assets_base_path().join("locales"), temp.path(), "en");
        for (input, expected) in [
            ("", None),
            ("  ", None),
            ("1", Some(1)),
            ("18446744073709551615", Some(u64::MAX)),
        ] {
            assert_eq!(redis_stream_maxlen_from_input(input), Ok(expected));
        }
        for input in ["0", "-1", "1.5", "18446744073709551616", "invalid"] {
            assert_eq!(
                redis_stream_maxlen_from_input(input).expect_err("reject unsafe trim input"),
                "Stream MAXLEN must be a positive integer; leave blank to skip trimming",
                "{input}",
            );
        }
        assert_eq!(redis_stream_time_from_input(""), Ok(None));
        assert_eq!(
            redis_stream_time_from_input("not-a-time").expect_err("invalid time"),
            "Use the time format YYYY-MM-DD HH:MM:SS",
        );
        for id in ["*", "0-0", "18446744073709551615-18446744073709551615"] {
            assert_eq!(redis_stream_entry_id_validation_error(id), None, "{id}");
        }
        for id in ["", "1", "1-", "-1", "18446744073709551616-0"] {
            assert!(redis_stream_entry_id_validation_error(id).is_some(), "{id}");
        }
        assert_eq!(
            redis_stream_entry_field_pair(" field ".into(), " value ".into()),
            Ok(("field".into(), " value ".into())),
            "only field names are trimmed; payload whitespace is preserved",
        );
        assert_eq!(
            redis_stream_entry_field_pairs_from_snapshot(&[]).expect_err("at least one field"),
            "Add at least one field/value pair",
        );
    }

    #[gpui::test]
    fn redis_stream_localized_validation_prevents_writes(cx: &mut TestAppContext) {
        use crate::{AppMessageKind, RedisStreamEntryPage};
        use fluxdb_app::{AppCommand, AppEvent, TabKind};
        use fluxdb_core::{
            CellValue, Column, ConnectionConfig, ConnectionId, DataPage, DatabaseKind, Endpoint,
            ObjectKind, ObjectPath, Row,
        };

        let _guard = locale_guard();
        let temp = tempfile::tempdir().expect("temp profile");
        let storage = FileStorage::new(temp.path().join("profile"));
        write_user_pack(&storage.root_dir().join("locales"));
        fs::write(
            storage.root_dir().join("locales/ja/app.json"),
            r#"{"redis":{"stream":{"error":{"maxlen":"MAXLEN は正の整数で指定してください"}}}}"#,
        )
        .expect("external validation translation");
        let bootstrap = init_i18n(
            &app_assets_base_path().join("locales"),
            &storage.root_dir().join("locales"),
            "ja",
        );
        assert!(bootstrap.issues.is_empty(), "{:?}", bootstrap.issues);
        let key = "i18n-stream".to_string();
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
            id: ConnectionId(1),
            name: "i18n UI only".into(),
            kind: DatabaseKind::Redis,
            endpoint: Endpoint::Tcp {
                host: "i18n-e2e.invalid".into(),
                port: 1,
                database: None,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            sqlserver_profile: None,
            oracle_profile: None,
        }]));
        controller.dispatch(AppCommand::SaveSettings(Settings {
            locale: "ja".into(),
            show_sidebar: false,
            ..Settings::default()
        }));
        let AppEvent::TabOpened(tab_id) =
            controller.dispatch(AppCommand::OpenDataEditor(ObjectPath {
                connection_id: ConnectionId(1),
                database: Some("0".into()),
                schema: None,
                name: key.clone(),
                kind: ObjectKind::RedisKey,
            }))
        else {
            panic!("open data editor without connecting")
        };
        // 仅准备已加载快照；中文列名是现有跨层数据协议，不是待翻译的 UI 标签。
        let page = DataPage {
            columns: ["键", "类型", "值", "TTL", "大小"]
                .into_iter()
                .map(|name| Column {
                    name: name.into(),
                    type_name: None,
                    nullable: false,
                    primary_key: false,
                    comment: None,
                })
                .collect(),
            rows: vec![Row {
                values: [&key, "stream", "", "120", "0 B"]
                    .into_iter()
                    .map(|value| CellValue::Text(value.into()))
                    .collect(),
            }],
            offset: 0,
            limit: 100,
            has_more: false,
        incomplete: None,
        };
        assert!(matches!(
            controller.dispatch(AppCommand::FinishDataPageLoad {
                tab_id,
                generation: 0,
                result: Ok(page.clone()),
            }),
            AppEvent::DataLoaded(..)
        ));
        cx.update(|cx| {
            extend_component_i18n();
            gpui_component::init(cx);
        });
        let page_for_view = page.clone();
        let key_for_view = key.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = navicat_main_new(
                    window,
                    cx,
                    controller,
                    storage,
                    gpui_component::ThemeMode::Light,
                );
                // 页面选择及缓存属于前置；后续打开表单、输入、保存、取消和范围应用均由真实事件驱动。
                let table = view.data_table_state(tab_id, &page_for_view, window, cx);
                table.update(cx, |table, _| {
                    table.delegate_mut().selected_row = Some(0);
                });
                view.redis_stream_entry_active = Some((tab_id, key_for_view.clone()));
                view.redis_stream_entry_pages.insert(
                    (tab_id, key_for_view),
                    RedisStreamEntryPage {
                        entries: Vec::new(),
                        next_cursor: "0".into(),
                        total: 0,
                    },
                );
                view
            });
            Root::new(view, window, cx)
        });
        cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(1000.)));
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
        let bounds = cx
            .debug_bounds("redis-stream-add")
            .expect("production add control");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        for (selector, text) in [
            ("redis-stream-field:0", "payload"),
            ("redis-stream-input:MAXLEN", "-1"),
        ] {
            let bounds = cx.debug_bounds(selector).expect("production stream input");
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            cx.simulate_input(text);
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
        }
        cx.update(|_, cx| {
            let this = view.read(cx);
            assert_eq!(
                this.redis_stream_entry_field_rows[0]
                    .field_input
                    .read(cx)
                    .value()
                    .as_str(),
                "payload",
                "typed field value"
            );
            assert_eq!(
                this.redis_stream_maxlen_input.read(cx).value().as_str(),
                "-1",
                "typed MAXLEN value"
            );
        });
        let bounds = cx
            .debug_bounds("redis-stream-save")
            .expect("stream save control");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("app-message-overlay").is_some(),
            "visible warning feedback"
        );
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                let message = this.app_message.as_ref().expect("validation warning");
                assert_eq!(message.kind, AppMessageKind::Warning);
                assert_eq!(message.text, "MAXLEN は正の整数で指定してください");
                assert!(
                    this.pending_redis_stream_entry_add.is_some(),
                    "retain draft for correction"
                );
                assert!(
                    this._redis_key_value_apply_tasks.is_empty(),
                    "invalid trim must not start a write"
                );
                assert_eq!(
                    this.redis_stream_entry_id_input.read(cx).value().as_str(),
                    "*"
                );
                assert_eq!(
                    this.redis_stream_entry_field_rows[0]
                        .field_input
                        .read(cx)
                        .value()
                        .as_str(),
                    "payload"
                );
            });
        });
        let bounds = cx.debug_bounds("redis-stream-cancel").expect("cancel");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let bounds = cx
            .debug_bounds("redis-stream-since")
            .expect("range start input");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.simulate_input("not-a-time");
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let bounds = cx
            .debug_bounds("redis-stream-range-apply")
            .expect("range apply");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("app-message-overlay").is_some());
        cx.update(|_, cx| {
            view.update(cx, |this, _| {
                assert_eq!(
                    this.app_message.as_ref().expect("range warning").text,
                    "Start time: Use the time format YYYY-MM-DD HH:MM:SS"
                );
                assert!(
                    this.redis_stream_ranges.is_empty(),
                    "invalid range must not replace active range"
                );
                assert!(
                    this._data_load_tasks.is_empty(),
                    "invalid range must not query the server"
                );
                assert!(this._redis_key_value_apply_tasks.is_empty());
                assert!(
                    this.pending_redis_stream_entry_add.is_none(),
                    "cancel closes the draft"
                );
                let cached = this
                    .redis_stream_entry_pages
                    .get(&(tab_id, key.clone()))
                    .expect("cached entries");
                assert!(cached.entries.is_empty());
                assert_eq!(cached.total, 0);
                let tab = this
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .expect("tab");
                let TabKind::DataEditor(editor) = &tab.kind else {
                    panic!("data editor")
                };
                assert_eq!(
                    editor.page.as_ref(),
                    Some(&page),
                    "loaded data must remain unchanged"
                );
            });
        });
        init_i18n(&app_assets_base_path().join("locales"), temp.path(), "en");
    }
