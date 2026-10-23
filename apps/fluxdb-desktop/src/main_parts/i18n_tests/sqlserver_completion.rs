fn sqlserver_e2e_config() -> fluxdb_core::ConnectionConfig {
    use std::collections::BTreeMap;

    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, SecretRef,
        SqlServerAdvancedOptions, SqlServerBasicOptions, SqlServerConnectionProfile,
        SqlServerEncryption,
    };

    let raw = std::env::var("FLUXDB_MSSQL_SMOKE")
        .expect("FLUXDB_MSSQL_SMOKE must be host:port:user:password");
    let parts = raw.splitn(4, ':').collect::<Vec<_>>();
    assert_eq!(
        parts.len(),
        4,
        "FLUXDB_MSSQL_SMOKE must be host:port:user:password"
    );
    let host = parts[0];
    let port = parts[1].parse::<u16>().expect("SQL Server port");
    let database =
        std::env::var("FLUXDB_MSSQL_DATABASE").unwrap_or_else(|_| "fluxdb_smoke".into());
    ConnectionConfig {
        id: ConnectionId(78),
        name: "SQL Server completion E2E".into(),
        kind: DatabaseKind::SqlServer,
        endpoint: Endpoint::Tcp {
            host: host.into(),
            port,
            database: Some(database.clone()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: Some(SqlServerConnectionProfile {
            basic: SqlServerBasicOptions {
                host: host.into(),
                port,
                database,
                schema: "dbo".into(),
                instance: String::new(),
                username: parts[2].into(),
                password: SecretRef::inline(parts[3]),
            },
            advanced: SqlServerAdvancedOptions {
                encryption: SqlServerEncryption::TrustServerCertificate,
                connect_timeout_secs: 10,
                query_timeout_secs: 30,
                application_name: "FluxDB SQL Server E2E".into(),
            },
        }),
    }
}

/// SQL Server 补全：真实生产 View 键入表名前缀后，候选带正确的限定名。
#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 与 fluxdb_smoke.dbo.FLUXDB_COMPLETION_E2E 表"]
fn sqlserver_completion_lists_qualified_table_and_columns(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use fluxdb_core::ConnectionId;

    let config = sqlserver_e2e_config();
    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("temp profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "zh-CN");
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });

    let mut controller = AppController::new();
    let _ = controller.dispatch(fluxdb_app::AppCommand::ReplaceConnections(vec![config]));
    let _ = controller.dispatch(fluxdb_app::AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(78),
        database: Some("fluxdb_smoke".into()),
        schema: Some("dbo".into()),
    });
    let storage_for_view = storage.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                controller,
                storage_for_view,
                gpui_component::ThemeMode::Light,
            )
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
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
    cx.run_until_parked();

    let tab_id = cx.update(|_, cx| {
        view.read(cx)
            .controller
            .state()
            .active_tab
            .expect("query tab")
    });
    let editor = cx
        .update(|_, cx| view.read(cx).query_editors.get(&tab_id).cloned())
        .expect("SQL editor entity");
    let bounds = cx.debug_bounds("query-editor").expect("SQL editor");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.simulate_input("SELECT * FROM FLUXDB_COMPLETION");
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let ready = cx.update(|_, cx| {
            let editor = editor.read(cx);
            editor.completion_visible() && !editor.completion_items.is_empty()
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "SQL Server completion did not return within 15s"
        );
        std::thread::sleep(Duration::from_millis(50));
        cx.background_executor
            .advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    cx.update(|_, cx| {
        let editor = editor.read(cx);
        let labels = editor
            .completion_items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label.contains("FLUXDB_COMPLETION_E2E")),
            "{labels:?}"
        );
        let candidate = editor
            .completion_items
            .iter()
            .find(|item| item.label.contains("FLUXDB_COMPLETION_E2E"))
            .expect("SQL Server table candidate");
        assert_eq!(candidate.insert_text, "[dbo].[FLUXDB_COMPLETION_E2E]");
    });
}

/// 图一场景：`SELECT * FROM [dbo].` 后只应补该 schema 的表，
/// 不应再把整库表或 schema 候选重复列出。
#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 与 fluxdb_smoke.dbo.FLUXDB_COMPLETION_E2E 表"]
fn sqlserver_bracket_qualifier_filters_to_schema_tables(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use fluxdb_core::ConnectionId;

    let config = sqlserver_e2e_config();
    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("temp profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "zh-CN");
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });

    let mut controller = AppController::new();
    let _ = controller.dispatch(fluxdb_app::AppCommand::ReplaceConnections(vec![config]));
    let _ = controller.dispatch(fluxdb_app::AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(78),
        database: Some("fluxdb_smoke".into()),
        schema: Some("dbo".into()),
    });
    let storage_for_view = storage.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                controller,
                storage_for_view,
                gpui_component::ThemeMode::Light,
            )
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
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
    cx.run_until_parked();

    let tab_id = cx.update(|_, cx| {
        view.read(cx)
            .controller
            .state()
            .active_tab
            .expect("query tab")
    });
    let editor = cx
        .update(|_, cx| view.read(cx).query_editors.get(&tab_id).cloned())
        .expect("SQL editor entity");
    let bounds = cx.debug_bounds("query-editor").expect("SQL editor");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.simulate_input("SELECT * FROM [dbo].");
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let ready = cx.update(|_, cx| {
            let editor = editor.read(cx);
            editor.completion_visible() && !editor.completion_items.is_empty()
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "schema-qualified completion did not return within 15s"
        );
        std::thread::sleep(Duration::from_millis(50));
        cx.background_executor
            .advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    cx.update(|_, cx| {
        let editor = editor.read(cx);
        let labels = editor
            .completion_items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label.contains("FLUXDB_COMPLETION_E2E")),
            "{labels:?}"
        );
        assert!(
            !labels.contains(&"dbo"),
            "schema must not be suggested again after it was typed: {labels:?}"
        );
    });
}

/// 图一的完整交互：接受 schema 候选 `[dbo].` 后，必须立即再次弹出该 schema 的表。
#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 与 fluxdb_smoke.dbo.FLUXDB_COMPLETION_E2E 表"]
fn sqlserver_accepting_schema_candidate_continues_completion(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use fluxdb_core::ConnectionId;

    let config = sqlserver_e2e_config();
    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("temp profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "zh-CN");
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });

    let mut controller = AppController::new();
    let _ = controller.dispatch(fluxdb_app::AppCommand::ReplaceConnections(vec![config]));
    let _ = controller.dispatch(fluxdb_app::AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(78),
        database: Some("fluxdb_smoke".into()),
        schema: None,
    });
    let storage_for_view = storage.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                controller,
                storage_for_view,
                gpui_component::ThemeMode::Light,
            )
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
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
    cx.run_until_parked();

    let tab_id = cx.update(|_, cx| {
        view.read(cx)
            .controller
            .state()
            .active_tab
            .expect("query tab")
    });
    let editor = cx
        .update(|_, cx| view.read(cx).query_editors.get(&tab_id).cloned())
        .expect("SQL editor entity");
    let bounds = cx.debug_bounds("query-editor").expect("SQL editor");
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    // 过滤到 schema 候选，再通过生产浮层的点击事件接受。
    cx.simulate_input("SELECT * FROM dbo");
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();

    let wait_for_completion = |cx: &mut TestAppContext| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ready = cx.update(|cx| {
                let editor = editor.read(cx);
                editor.completion_visible() && !editor.completion_items.is_empty()
            });
            if ready {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "completion did not return within 15s"
            );
            std::thread::sleep(Duration::from_millis(50));
            cx.background_executor
                .advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
    };
    wait_for_completion(cx);

    cx.update(|_, cx| {
        let labels = editor
            .read(cx)
            .completion_items
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label.eq_ignore_ascii_case("dbo")),
            "{labels:?}"
        );
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let row = cx
        .debug_bounds("completion-item:dbo")
        .expect("dbo completion row");
    cx.simulate_click(row.center(), Modifiers::default());
    let text = cx.update(|_, cx| editor.read(cx).text());
    assert_eq!(text, "SELECT * FROM [dbo].");

    // 接受 schema 后应继续补全，不能停住。
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    wait_for_completion(cx);
    cx.update(|_, cx| {
        let editor = editor.read(cx);
        let labels = editor
            .completion_items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label.contains("FLUXDB_COMPLETION_E2E")),
            "{labels:?}"
        );
    });
    // 缩小候选列表后点击表，检查替换范围不会重复写入 schema。
    cx.simulate_input("FLUXDB_COMPLETION");
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    wait_for_completion(cx);
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let row = cx
        .debug_bounds("completion-item:FLUXDB_COMPLETION_E2E")
        .expect("table completion row");
    cx.simulate_click(row.center(), Modifiers::default());
    cx.update(|_, cx| {
        assert_eq!(
            editor.read(cx).text(),
            "SELECT * FROM [dbo].[FLUXDB_COMPLETION_E2E]"
        );
        assert!(!editor.read(cx).completion_visible());
    });

}
