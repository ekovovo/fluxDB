#[gpui::test]
fn postgres_design_types_are_selected_instead_of_showing_placeholder(cx: &mut TestAppContext) {
    use fluxdb_app::{
        AppCommand, AppEvent, CreateTableMode, CreateTableState, TabKind, create_table_provider,
    };
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
            column("state", r#"public."OrderStatus""#),
            column("created_at", "timestamp(6) without time zone"),
            column("amounts", "numeric(12,4)[]"),
            column("event_times", "timestamp(6) with time zone[]"),
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
            Some(r#"public."OrderStatus""#),
            "custom types must stay selected instead of showing the muted placeholder"
        );
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 3, cx)
                .as_deref(),
            Some("timestamp"),
            "timestamp precision must be separated from the type name"
        );
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 4, cx)
                .as_deref(),
            Some("numeric(12,4)[]"),
            "array type parameters must stay part of the complete definition"
        );
        assert_eq!(
            view.create_table_type_value_for_test(tab_id, 5, cx)
                .as_deref(),
            Some("timestamp(6) with time zone[]"),
            "time arrays must retain precision and time zone"
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
    let type_select = cx
        .debug_bounds("create-table-type-select-0")
        .expect("type select bounds");
    let length_input = cx
        .debug_bounds("create-table-length-0-enabled:true")
        .expect("length input bounds");
    assert!(
        (type_select.center().y - length_input.center().y).abs() <= gpui::px(0.5),
        "type select and length input must share the same row center: select={type_select:?}, length={length_input:?}"
    );
    let editor = cx
        .debug_bounds("create-table-editor")
        .expect("create table editor bounds");
    let options = cx
        .debug_bounds("create-table-column-options")
        .expect("column options bounds");
    assert!(
        options.size.height <= editor.size.height * 0.3 + gpui::px(0.5),
        "column options must stay within 30% of the editor height: editor={editor:?}, options={options:?}"
    );
    for selector in [
        "create-table-length-3-enabled:false",
        "create-table-length-4-enabled:false",
    ] {
        let bounds = cx
            .debug_bounds(selector)
            .expect("array length input must be disabled");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.simulate_input("8");
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.update(|_, cx| {
            let view = view.read(cx);
            let tab = view.controller.state().tabs.iter()
                .find(|tab| tab.id == tab_id).expect("design tab");
            let TabKind::CreateTable(create) = &tab.kind else {
                panic!("design tab kind");
            };
            let CreateTableMode::Design { original, .. } = &create.mode else {
                panic!("design snapshot");
            };
            assert_eq!(create.columns, original.columns, "disabled input must not edit any column");
            assert!(!tab.dirty, "disabled array parameters must not dirty the draft");
            assert_eq!(
                create_table_provider(create.database_kind).design_statements(create),
                Ok(Vec::new())
            );
        });
    }
}
