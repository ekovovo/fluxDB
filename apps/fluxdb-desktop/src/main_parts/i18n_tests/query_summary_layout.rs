#[gpui::test]
fn query_summary_columns_align_and_stay_readable_when_narrow(cx: &mut TestAppContext) {
    use crate::QUERY_SUMMARY_TABLE_MIN_WIDTH;
    use fluxdb_app::{AppCommand, AppEvent};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, QueryExecutionResult,
        QueryExecutionSummary, QueryStatementKind,
    };

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    let settings = Settings {
        locale: "en".into(),
        show_sidebar: false,
        ..Default::default()
    };
    storage.save_settings(&settings).expect("settings");
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    let mut controller = AppController::new();
    controller.dispatch(AppCommand::SaveSettings(settings));
    controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
        id: ConnectionId(1),
        name: "Query summary layout".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: temp.path().join("summary.sqlite"),
            read_only: true,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
    }]));
    let AppEvent::TabOpened(tab_id) = controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(1),
        database: None,
        schema: None,
    }) else {
        panic!("query tab");
    };
    controller.dispatch(AppCommand::FinishQueryExecution {
        tab_id,
        result: Ok(QueryExecutionResult {
            summaries: vec![QueryExecutionSummary {
                sql: "db.orders.createIndex({ customer_id: 1, created_at: -1 })".into(),
                kind: QueryStatementKind::Command,
                success: true,
                message: "OK, 0 rows affected".into(),
                returned_rows: 0,
                affected_rows: 0,
                elapsed_ms: 12,
            }],
            results: Vec::new(),
            rollback_snapshots: Vec::new(),
        }),
    });
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut app = navicat_main_new(
                window,
                cx,
                controller,
                storage,
                gpui_component::ThemeMode::Light,
            );
            app.ai_chat.recovery_pending = false;
            app
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
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
    assert_query_summary_columns(cx);
    assert!(
        cx.debug_bounds("query-summary-table")
            .expect("summary table")
            .size
            .width
            >= gpui::px(QUERY_SUMMARY_TABLE_MIN_WIDTH)
    );

    cx.simulate_resize(gpui::size(gpui::px(600.), gpui::px(700.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
    assert_query_summary_columns(cx);
    let table = cx.debug_bounds("query-summary-table").expect("summary table");
    assert!(
        table.size.width >= gpui::px(QUERY_SUMMARY_TABLE_MIN_WIDTH),
        "narrow layouts must scroll instead of shrinking columns: {table:?}"
    );
    assert!(cx.update(|_, cx| {
        view.read(cx).controller.state().tabs.iter().any(
            |tab| tab.id == tab_id && matches!(&tab.kind, fluxdb_app::TabKind::QueryEditor(editor) if editor.summaries.len() == 1)
        )
    }));
}

fn assert_query_summary_columns(cx: &mut gpui::VisualTestContext) {
    use crate::{
        QUERY_SUMMARY_AFFECTED_WIDTH, QUERY_SUMMARY_ELAPSED_WIDTH, QUERY_SUMMARY_RETURNED_WIDTH,
        QUERY_SUMMARY_STATEMENT_MIN_WIDTH, QUERY_SUMMARY_TYPE_WIDTH,
    };

    for (header_selector, row_selector, minimum_width) in [
        (
            "query-summary-header:statement",
            "query-summary-row:0:statement",
            QUERY_SUMMARY_STATEMENT_MIN_WIDTH,
        ),
        (
            "query-summary-header:type",
            "query-summary-row:0:type",
            QUERY_SUMMARY_TYPE_WIDTH,
        ),
        (
            "query-summary-header:returned",
            "query-summary-row:0:returned",
            QUERY_SUMMARY_RETURNED_WIDTH,
        ),
        (
            "query-summary-header:affected",
            "query-summary-row:0:affected",
            QUERY_SUMMARY_AFFECTED_WIDTH,
        ),
        (
            "query-summary-header:elapsed",
            "query-summary-row:0:elapsed",
            QUERY_SUMMARY_ELAPSED_WIDTH,
        ),
    ] {
        let header = cx.debug_bounds(header_selector).expect(header_selector);
        let row = cx.debug_bounds(row_selector).expect(row_selector);
        assert!(
            (f32::from(header.origin.x) - f32::from(row.origin.x)).abs() < 0.5,
            "header and row must start at the same x: {header:?} vs {row:?}"
        );
        assert!(
            (f32::from(header.size.width) - f32::from(row.size.width)).abs() < 0.5,
            "header and row must have the same width: {header:?} vs {row:?}"
        );
        assert!(
            header.size.width >= gpui::px(minimum_width),
            "column must keep its readable minimum width: {header:?}"
        );
    }
}
