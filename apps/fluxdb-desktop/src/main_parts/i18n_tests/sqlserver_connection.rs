#[gpui::test]
fn sqlserver_connection_form_uses_localized_fields_and_does_not_persist_failed_connection(
    cx: &mut TestAppContext,
) {
    use fluxdb_core::DatabaseKind;

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("temp profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    write_user_pack(&storage.root_dir().join("locales"));
    fs::write(
        storage.root_dir().join("locales/ja/app.json"),
        r#"{
                "mock": { "placeholder": "mock" },
                "connection": {
                    "sqlserver": {
                        "schema": "スキーマ",
                        "instance": "インスタンス"
                    }
                }
            }"#,
    )
    .expect("write SQL Server labels");
    let bootstrap = init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "ja",
    );
    assert!(bootstrap.issues.is_empty(), "{:?}", bootstrap.issues);

    let controller = AppController::new();
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                controller,
                storage,
                gpui_component::ThemeMode::Light,
            )
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
        .debug_bounds("home-new-connection")
        .expect("new connection action");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let bounds = cx
        .debug_bounds("new-connection-kind-sqlserver")
        .expect("SQL Server tile");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    for selector in [
        "connection-host",
        "connection-port",
        "connection-username",
        "connection-password",
        "connection-database",
        "connection-sqlserver-schema",
        "connection-sqlserver-instance",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "SQL Server field must be visible: {selector}"
        );
    }
    cx.update(|_, cx| {
        let this = view.read(cx);
        assert_eq!(this.new_connection_kind, Some(DatabaseKind::SqlServer));
        assert_eq!(this.new_connection_form.port, "1433");
        assert_eq!(
            rust_i18n::t!("app.connection.sqlserver.schema").as_ref(),
            "スキーマ"
        );
        assert_eq!(
            rust_i18n::t!("app.connection.sqlserver.instance").as_ref(),
            "インスタンス"
        );
    });

    for (selector, text) in [
        ("connection-name", "SQL Server E2E"),
        ("connection-host", "sqlserver-e2e.invalid"),
        ("connection-port", "1433"),
        ("connection-username", "sa"),
        ("connection-password", "FluxDB_Smoke_2026!Aa"),
        ("connection-database", "fluxdb_smoke"),
    ] {
        let bounds = cx.debug_bounds(selector).expect("connection input");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.simulate_input(text);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    let bounds = cx
        .debug_bounds("new-connection-save")
        .expect("save and connect");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let status = cx.update(|_, cx| {
            let this = view.read(cx);
            this.new_connection_form
                .test_status
                .as_ref()
                .map(|status| match status {
                    crate::ConnectionTestStatus::Error(text) => Some(text.clone()),
                    _ => None,
                })
        });
        if let Some(Some(text)) = status {
            assert!(
                text.contains("SQL Server"),
                "failure must come from the SQL Server connector: {text}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "SQL Server connection failure was not observable"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    cx.update(|_, cx| {
        let this = view.read(cx);
        assert_eq!(
            this.controller.state().connections.len(),
            0,
            "failed connection must not be persisted"
        );
        assert!(
            this.new_connection_kind.is_some(),
            "form remains open for correction"
        );
    });
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "en");
}
