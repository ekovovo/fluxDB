#[gpui::test]
fn postgres_design_types_are_selected_instead_of_showing_placeholder(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent, CreateTableState};
    use fluxdb_core::{
        CompletionColumn, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ObjectKind,
        ObjectPath,
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
    let config = ConnectionConfig {
        id: ConnectionId(1),
        name: "PG designer UI only".into(),
        kind: DatabaseKind::Postgres,
        endpoint: Endpoint::Tcp {
            host: "pg-designer.invalid".into(),
            port: 1,
            database: None,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
    };
    controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
    let object = ObjectPath {
        connection_id: config.id,
        database: Some("app".into()),
        schema: Some("public".into()),
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    let column = |name: &str, type_name: &str| CompletionColumn {
        database: Some("app".into()),
        schema: Some("public".into()),
        table: "items".into(),
        name: name.into(),
        type_name: Some(type_name.into()),
        nullable: true,
        primary_key: false,
        comment: None,
        stable: None,
    };
    let create = CreateTableState::design(
        object,
        DatabaseKind::Postgres,
        vec![
            column("name", "character varying(64)"),
            column("state", "public.order_status"),
            column("created_at", "timestamp(6) without time zone"),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
    );
    let AppEvent::TabOpened(tab_id) = controller
        .dispatch(AppCommand::FinishOpenDesignTable { result: Ok(create) })
    else {
        panic!("design tab");
    };
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
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 1, cx)
                .as_deref(),
            Some("varchar"),
            "character varying must resolve to the editor type instead of the placeholder"
        );
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 2, cx)
                .as_deref(),
            Some("public.order_status"),
            "custom types must stay selected instead of showing the muted placeholder"
        );
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 3, cx)
                .as_deref(),
            Some("timestamp"),
            "timestamp precision must be separated from the type name"
        );
    });
    assert!(
        cx.debug_bounds("create-table-length-0-enabled:true").is_some(),
        "varchar must keep its length editable"
    );
    assert!(
        cx.debug_bounds("create-table-length-1-enabled:false").is_some(),
        "unknown custom types must not offer a length editor"
    );
    assert!(
        cx.debug_bounds("create-table-length-2-enabled:true").is_some(),
        "timestamp precision must be editable as length"
    );
}
