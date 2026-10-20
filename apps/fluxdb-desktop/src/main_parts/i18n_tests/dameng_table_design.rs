#[gpui::test]
fn dameng_designer_explains_read_only_trigger_and_partition_tabs(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent, TabKind};
    use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint};

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), &storage.root_dir().join("locales"), "en");
    let mut controller = AppController::new();
    controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
        id: ConnectionId(1),
        name: "DM8 designer UI only".into(),
        kind: DatabaseKind::Dameng,
        endpoint: Endpoint::Tcp {
            host: "dm8-designer.invalid".into(),
            port: 1,
            database: None,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
    }]));
    controller.dispatch(AppCommand::SaveSettings(Settings {
        locale: "en".into(),
        show_sidebar: false,
        ..Default::default()
    }));
    let AppEvent::TabOpened(tab_id) = controller.dispatch(AppCommand::OpenCreateTable {
        connection_id: ConnectionId(1),
        database: None,
        schema: None,
    }) else {
        panic!("prepare disconnected DM8 draft");
    };
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(window, cx, controller, storage, gpui_component::ThemeMode::Light)
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(1000.)));
    let view = cx.update(|window, cx| {
        window.root::<Root>().expect("root").expect("typed root").read(cx).view()
            .clone().downcast::<NavicatMain>().expect("production view")
    });
    cx.update(|window, cx| { window.draw(cx).clear(cx); });
    for (tab, notice, key) in [
        ("create-table-tab:Triggers", "create-table-dameng-triggers-read-only", "app.create_table.trigger.dameng_read_only"),
        ("create-table-tab:Partitions", "create-table-dameng-partitions-read-only", "app.create_table.partition.dameng_read_only"),
    ] {
        let bounds = cx.debug_bounds(tab).expect("tab");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| { window.draw(cx).clear(cx); });
        assert!(cx.debug_bounds(notice).is_some(), "read-only explanation is rendered");
        assert!(!crate::tr(key).contains("app.create_table"), "notice is localized");
        assert!(cx.debug_bounds("create-table-add:Add trigger").is_none());
        cx.update(|_, cx| {
            let this = view.read(cx);
            let tab = this.controller.state().tabs.iter().find(|tab| tab.id == tab_id).expect("draft");
            let TabKind::CreateTable(create) = &tab.kind else { panic!("table draft"); };
            assert!(create.triggers.is_empty());
            assert!(!create.partition_enabled);
            assert!(create.partition_sql.is_empty());
            assert!(!tab.dirty, "read-only navigation must not change the draft");
            assert!(crate::create_table_add_row_command(tab_id, create).is_none());
        });
    }
}
