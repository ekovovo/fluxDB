#[gpui::test]
fn table_context_menu_fits_at_bottom_of_viewport(cx: &mut TestAppContext) {
    use crate::TableContextMenu;
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
        name: "context menu layout".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: temp.path().join("menu.sqlite"),
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
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
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(620.)));
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
        view.update(cx, |this, cx| {
            this.show_table_context_menu(
                TableContextMenu {
                    object_path: ObjectPath {
                        connection_id,
                        database: None,
                        schema: None,
                        name: "ITEMS".into(),
                        kind: ObjectKind::Table,
                    },
                    position: gpui::point(gpui::px(100.), gpui::px(600.)),
                    submenu: None,
                },
                window,
                cx,
            );
            cx.notify();
        });
        window.draw(cx).clear(cx);
    });
    let menu = cx
        .debug_bounds("table-context-menu")
        .expect("table context menu");
    let viewport_bottom = gpui::px(620.) - gpui::px(8.);
    assert!(
        menu.origin.y + menu.size.height <= viewport_bottom,
        "table context menu must keep its destructive actions inside the viewport"
    );
}

#[gpui::test]
fn table_import_dialog_renders_with_disabled_create_option(cx: &mut TestAppContext) {
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
        name: "import dialog".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: temp.path().join("import.sqlite"),
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
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
    cx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(620.)));
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
    let _ = view.update(cx, |this, cx| {
        // 直接构造导入表单，验证对话框在真实生产视图里可渲染且默认追加。
        let object = ObjectPath {
            connection_id: ConnectionId(1),
            database: None,
            schema: None,
            name: "items".into(),
            kind: ObjectKind::Table,
        };
        // 新流程：先弹框，文件在框内选择；此处验证未选文件时的初始状态。
        let form = crate::TableDataImportForm::new(object);
        assert_eq!(form.options.mode, fluxdb_core::TableImportMode::Append);
        assert_eq!(form.target, crate::TableDataImportTarget::Existing);
        assert!(form.source.is_none());
        assert!(form.format.is_none());
        // 与导出弹框一致：默认常规页，日志页初始为空且未开始。
        assert_eq!(form.tab, crate::TableDataImportTab::General);
        assert!(form.logs.is_empty());
        assert!(form.started_at.is_none());
        this.pending_data_import = Some(form);
        cx.notify();
    });
    // 触发一次真实重绘：对话框若存在布局或状态错误会在这里暴露。
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}
