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
