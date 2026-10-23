#[gpui::test]
fn copy_table_failure_is_shown_once_by_alert(cx: &mut TestAppContext) {
    use crate::{AppMessageKind, PendingCopyTable};
    use fluxdb_app::AppCommand;
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ObjectKind, ObjectPath,
    };

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    let mut controller = AppController::new();
    let connection_id = ConnectionId(1);
    controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
        id: connection_id,
        name: "SQLite copy error".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: temp.path().join("copy.sqlite"),
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
    }]));
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
    let error = "复制表失败：测试错误";
    cx.update(|window, cx| {
        view.update(cx, |this, cx| {
            this.pending_copy_table = Some(PendingCopyTable {
                object_path: ObjectPath {
                    connection_id,
                    database: Some("main".into()),
                    schema: None,
                    name: "ITEMS".into(),
                    kind: ObjectKind::Table,
                },
                new_name: "ITEMS_copy".into(),
                copy_data: false,
                source_ddl: Some(Ok("CREATE TABLE ITEMS (ID INTEGER)".into())),
                error: Some(error.into()),
            });
            this.show_message(error, AppMessageKind::Error, cx);
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    assert!(cx.debug_bounds("app-message-overlay").is_some());
    assert!(cx.debug_bounds("CopyTableModal-confirm").is_some());
    // 复制错误只由主窗口 Alert 展示，弹窗内不再渲染重复文字。
    assert!(
        cx.debug_bounds("CopyTableModal-error").is_none(),
        "copy failure must not be duplicated inside the modal"
    );
}
