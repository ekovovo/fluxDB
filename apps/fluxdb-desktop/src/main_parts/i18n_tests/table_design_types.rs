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
        mongo_profile: None,
        sqlserver_profile: None,
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
    assert!(
        cx.debug_bounds("create-table-custom-type-summary-1").is_none(),
        "standard types must not show the custom type summary"
    );
    let state_row = cx
        .debug_bounds("create-table-row-1")
        .expect("custom type row");
    cx.simulate_click(state_row.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("create-table-custom-type-summary-2").is_some(),
        "opaque custom types must expose a summary"
    );
    assert!(
        cx.debug_bounds("create-table-custom-type-input-1").is_none(),
        "opening a custom type row must not start inline editing automatically"
    );
    let type_select = cx
        .debug_bounds("create-table-type-select-1")
        .expect("custom type select");
    cx.simulate_click(type_select.center(), Modifiers::default());
    cx.simulate_input("custom type");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let custom_entry = cx
        .debug_bounds("create-table-type-option:custom")
        .expect("custom type dropdown action");
    cx.simulate_click(custom_entry.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("create-table-custom-type-input-1").is_some(),
        "the custom type action must reveal an inline input in the type cell"
    );
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
        assert_eq!(create.columns, original.columns, "opening the editor must not mutate the draft");
    });
    cx.simulate_input("order_state_v2");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(|_, cx| {
        let view = view.read(cx);
        let tab = view
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .expect("design tab");
        let TabKind::CreateTable(create) = &tab.kind else {
            panic!("design tab kind");
        };
        assert_eq!(create.columns[1].data_type, "order_state_v2");
        assert!(tab.dirty, "committing the custom type must dirty the draft");
    });
    assert!(
        cx.debug_bounds("create-table-custom-type-input-1").is_none(),
        "Enter must close inline custom type editing"
    );

    let custom_type_edit = cx
        .debug_bounds("create-table-custom-type-edit-2")
        .expect("custom type summary edit action");
    cx.simulate_click(custom_type_edit.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.simulate_input("discarded_type");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(|_, cx| {
        let view = view.read(cx);
        let tab = view
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .expect("design tab");
        let TabKind::CreateTable(create) = &tab.kind else {
            panic!("design tab kind");
        };
        assert_eq!(
            create.columns[1].data_type, "order_state_v2",
            "Escape must discard the inline custom type draft"
        );
    });
    assert!(
        cx.debug_bounds("create-table-custom-type-input-1").is_none(),
        "Escape must close inline custom type editing"
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
        options.size.height >= gpui::px(74.),
        "column options must keep a visible minimum height: options={options:?}"
    );
    assert!(
        options.size.height <= editor.size.height * 0.3 + gpui::px(0.5),
        "column options must stay within 30% of the editor height: editor={editor:?}, options={options:?}"
    );
    let (columns_before_disabled_input, dirty_before_disabled_input, statements_before_disabled_input) =
        cx.update(|_, cx| {
            let view = view.read(cx);
            let tab = view
                .controller
                .state()
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .expect("design tab");
            let TabKind::CreateTable(create) = &tab.kind else {
                panic!("design tab kind");
            };
            (
                create.columns.clone(),
                tab.dirty,
                create_table_provider(create.database_kind).design_statements(create),
            )
        });
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
            assert!(matches!(create.mode, CreateTableMode::Design { .. }));
            assert_eq!(
                create.columns, columns_before_disabled_input,
                "disabled input must not edit any column"
            );
            assert_eq!(
                tab.dirty, dirty_before_disabled_input,
                "disabled array parameters must not change draft dirtiness"
            );
            assert_eq!(
                create_table_provider(create.database_kind).design_statements(create),
                statements_before_disabled_input,
                "disabled array parameters must not change generated statements"
            );
        });
    }
}

#[gpui::test]
fn mysql_enum_members_fit_and_can_be_added_in_options_panel(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent, CreateTableState, TabKind};
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
        id: ConnectionId(2),
        name: "MySQL designer UI only".into(),
        kind: DatabaseKind::MySql,
        endpoint: Endpoint::Tcp {
            host: "mysql-designer.invalid".into(),
            port: 1,
            database: None,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        mongo_profile: None,
        sqlserver_profile: None,
    };
    controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
    let object = ObjectPath {
        connection_id: config.id,
        database: Some("app".into()),
        schema: None,
        name: "tasks".into(),
        kind: ObjectKind::Table,
    };
    let column = |name: &str, type_name: &str| CompletionColumn {
        database: Some("app".into()),
        schema: None,
        table: "tasks".into(),
        name: name.into(),
        type_name: Some(type_name.into()),
        nullable: true,
        primary_key: false,
        comment: None,
        stable: None,
    };
    let create = CreateTableState::design(
        object,
        DatabaseKind::MySql,
        vec![
            column("status", "enum('New','Done')"),
            column("tags", "set('a','b')"),
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
    let enum_row = cx
        .debug_bounds("create-table-row-0")
        .expect("enum row bounds");
    cx.simulate_click(enum_row.center(), Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    let editor = cx
        .debug_bounds("create-table-editor")
        .expect("create table editor bounds");
    let options = cx
        .debug_bounds("create-table-column-options")
        .expect("column options bounds");
    let options_scroll = cx
        .debug_bounds("create-table-enum-members-scroll")
        .expect("column options scroll bounds");
    let enum_editor = cx
        .debug_bounds("create-table-enum-editor-1")
        .expect("enum editor bounds");
    let first_member = cx
        .debug_bounds("create-table-enum-value-row-1-0")
        .expect("first enum member bounds");
    let second_member = cx
        .debug_bounds("create-table-enum-value-row-1-1")
        .expect("second enum member bounds");
    let add_member = cx
        .debug_bounds("create-table-enum-add-1")
        .expect("add enum member action");
    let options_bottom = options.origin.y + options.size.height;
    let viewport_height =
        cx.update(|window, _| f32::from(window.viewport_size().height));
    assert!(
        options.size.height >= gpui::px(74.),
        "enum options must keep a visible minimum height: {options:?}"
    );
    assert!(
        options.size.height <= gpui::px(viewport_height * 0.3) + gpui::px(0.5),
        "enum options must stay within 30% of the window height: editor={editor:?}, options={options:?}"
    );
    for (label, bounds) in [
        ("first member", first_member),
        ("second member", second_member),
        ("add member", add_member),
    ] {
        assert!(
            bounds.origin.y >= options.origin.y
                && bounds.origin.y + bounds.size.height <= options_bottom + gpui::px(0.5),
            "{label} must be visible without nested clipping: options={options:?}, bounds={bounds:?}"
        );
    }
    cx.simulate_click(add_member.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("create-table-enum-value-row-1-2").is_some(),
        "adding an enum member must render a new inline row"
    );
    let new_member_input = cx
        .debug_bounds("create-table-enum-value-input-1-2")
        .expect("new enum member input");
    cx.simulate_click(new_member_input.center(), Modifiers::default());
    cx.simulate_input("Archived");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = view.read(cx);
        let tab = view
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .expect("design tab");
        let TabKind::CreateTable(create) = &tab.kind else {
            panic!("design tab kind");
        };
        assert_eq!(create.columns[0].enum_values.len(), 3);
        assert_eq!(create.columns[0].enum_values[2], "Archived");
        assert!(tab.dirty, "adding an enum member must dirty the draft");
        assert_eq!(create.validation_error(), None);
        assert!(
            create
                .sql_preview()
                .is_ok_and(|sql| sql.contains("enum('New','Done','Archived')")),
            "edited enum members must be reflected in generated SQL"
        );
    });

    view.update(cx, |this, cx| {
        for _ in 0..10 {
            this.dispatch(
                AppCommand::AddCreateTableColumnEnumValue {
                    tab_id,
                    column_id: 1,
                },
                cx,
            );
        }
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let scroll_offset_initial = cx.update(|_, cx| {
        view.read(cx)
            .create_table_enum_scrolls
            .get(&(tab_id, 1))
            .expect("enum member scroll handle")
            .offset()
            .y
    });
    let add_member_initial = cx
        .debug_bounds("create-table-enum-add-1")
        .expect("pinned add member action")
        .origin
        .y;
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: options_scroll.center(),
        delta: gpui::ScrollDelta::Lines(gpui::point(0., -8.)),
        ..Default::default()
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let scroll_offset_scrolled = cx.update(|_, cx| {
        view.read(cx)
            .create_table_enum_scrolls
            .get(&(tab_id, 1))
            .expect("enum member scroll handle")
            .offset()
            .y
    });
    let add_member_scrolled = cx
        .debug_bounds("create-table-enum-add-1")
        .expect("scrolled add member action");
    assert!(
        scroll_offset_scrolled != scroll_offset_initial,
        "the member list must scroll when enum values exceed 30% height: initial={scroll_offset_initial}, scrolled={scroll_offset_scrolled}, options={options:?}, scroll={options_scroll:?}, enum={enum_editor:?}, editor={editor:?}"
    );
    assert_eq!(
        add_member_scrolled.origin.y, add_member_initial,
        "the add-member action must stay pinned while members scroll"
    );
    cx.simulate_click(add_member_scrolled.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let auto_scrolled_member = cx
        .debug_bounds("create-table-enum-value-input-1-13")
        .expect("new member after a long list");
    let members_scroll_after_add = cx
        .debug_bounds("create-table-enum-members-scroll")
        .expect("scroll bounds after adding to a long list");
    assert!(
        auto_scrolled_member.origin.y >= members_scroll_after_add.origin.y
            && auto_scrolled_member.bottom() <= members_scroll_after_add.bottom(),
        "adding a member to a long list must bring the new row into view: input={auto_scrolled_member:?}, scroll={members_scroll_after_add:?}"
    );
    cx.update(|_, cx| {
        let view = view.read(cx);
        let tab = view
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .expect("design tab");
        let TabKind::CreateTable(create) = &tab.kind else {
            panic!("design tab kind");
        };
        assert_eq!(create.columns[0].enum_values.len(), 14);
        assert_eq!(create.columns[0].enum_values[2], "Archived");
        assert_eq!(
            create.validation_error(),
            Some("ENUM/SET 成员不能为空"),
            "newly added empty members must still be validated"
        );
    });
}
