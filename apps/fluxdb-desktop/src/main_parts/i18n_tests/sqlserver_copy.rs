#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_copy_table_from_context_menu_preserves_source_and_data(cx: &mut TestAppContext) {
    run_sqlserver_copy_table_ui(cx, None, true);
}

#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_copy_all_types_structure_from_context_menu(cx: &mut TestAppContext) {
    // 对应 app.all_types 的 29 列类型，使用隔离表复现，不依赖或写入用户数据库。
    run_sqlserver_copy_table_ui(cx, Some(SQLSERVER_ALL_TYPES_COLUMNS), false);
}

const SQLSERVER_ALL_TYPES_COLUMNS: &str = r#"
        [id] int IDENTITY(1,1) NOT NULL PRIMARY KEY,
        [c_tinyint] tinyint, [c_smallint] smallint, [c_int] int, [c_bigint] bigint,
        [c_bit] bit, [c_decimal] decimal(38,10), [c_numeric] numeric(18,4),
        [c_smallmoney] smallmoney, [c_money] money, [c_float] float, [c_real] real,
        [c_date] date, [c_time] time(7), [c_datetime] datetime,
        [c_datetime2] datetime2(7), [c_smalldt] smalldatetime,
        [c_dtoffset] datetimeoffset(7), [c_char] char(10), [c_varchar] varchar(256),
        [c_nchar] nchar(10), [c_nvarchar] nvarchar(256), [c_max] nvarchar(max),
        [c_binary] binary(8), [c_varbinary] varbinary(256), [c_imageish] varbinary(max),
        [c_uniqueid] uniqueidentifier, [c_xml] xml, [c_variant] sql_variant
    "#;

fn run_sqlserver_copy_table_ui(cx: &mut TestAppContext, columns: Option<&str>, copy_data: bool) {
    use fluxdb_app::{AppCommand, AppEvent};
    use fluxdb_core::{ObjectKind, ObjectPath, QueryExecutionOptions};
    use std::time::{Duration, Instant};

    let config = sqlserver_e2e_config();
    let database = config
        .sqlserver_profile
        .as_ref()
        .expect("SQL Server profile")
        .basic
        .database
        .clone();
    let source = format!(
        "FLUXDB_COPY_UI_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let target = format!("{source}_copy");
    eprintln!("copy UI fixture: {source}");
    let mut fixture = AppController::new();
    fixture.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
    let execute = |text: String| {
        let result = fixture
            .execute_query_text_for_scope_with_progress(
                config.id,
                Some(database.clone()),
                Some("dbo".into()),
                text,
                QueryExecutionOptions {
                    continue_on_error: false,
                    ..Default::default()
                },
                &mut |_| {},
                &|| false,
            )
            .expect("fixture SQL");
        assert!(
            result.summaries.iter().all(|summary| summary.success),
            "{:?}",
            result.summaries
        );
        result.results
    };
    if let Some(columns) = columns {
        execute(format!("CREATE TABLE [dbo].[{source}] ({columns});"));
    } else {
        execute(format!(
            "CREATE TABLE [dbo].[{source}] ([ID] int IDENTITY PRIMARY KEY, [NAME] nvarchar(40) NOT NULL); INSERT INTO [dbo].[{source}] ([NAME]) VALUES (N'copy sample');"
        ));
    }
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database.clone()),
        schema: Some("dbo".into()),
        name: source.clone(),
        kind: ObjectKind::Table,
    };
    let before = execute(format!("SELECT * FROM [dbo].[{source}]")).remove(0);
    assert_eq!(before.rows.len(), usize::from(copy_data));
    if !copy_data {
        assert_eq!(before.columns.len(), 29);
    }
    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "en");
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let mut controller = AppController::new();
    controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
    for command in [
        AppCommand::OpenConnection(config.id),
        AppCommand::LoadObjectChildren(ObjectPath {
            kind: ObjectKind::Database,
            schema: None,
            name: database.clone(),
            ..object.clone()
        }),
        AppCommand::LoadObjectChildren(ObjectPath {
            kind: ObjectKind::Schema,
            name: "dbo".into(),
            ..object.clone()
        }),
    ] {
        let event = controller.dispatch(command);
        assert!(matches!(event, AppEvent::ObjectsLoaded(_, _)), "{event:?}");
    }
    let preview_state = std::rc::Rc::new(std::cell::RefCell::new(None));
    let preview_capture = preview_state.clone();
    let preview_target = target.clone();
    let _preview_subscription = cx.update(|cx| {
        cx.observe_new::<gpui_component::input::TextareaState>(move |state, _, cx| {
            if state.value().contains(&preview_target) {
                *preview_capture.borrow_mut() = Some(cx.entity().downgrade());
            }
        })
    });
    let filter = source.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut app = navicat_main_new(
                window,
                cx,
                controller,
                storage,
                gpui_component::ThemeMode::Light,
            );
            app.sidebar_search = filter.clone();
            app.sidebar_search_input.update(cx, |input, cx| {
                input.set_value(filter, window, cx);
            });
            app
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
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let selector = Box::leak(format!("table-tree-row:{source}").into_boxed_str());
    let row = cx.debug_bounds(selector).expect("source tree row");
    cx.simulate_mouse_down(row.center(), gpui::MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(row.center(), gpui::MouseButton::Right, Modifiers::default());
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let menu = cx
        .debug_bounds("table-menu-item:Copy table")
        .expect("copy menu");
    cx.simulate_click(menu.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        cx.background_executor
            .advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let ready = cx.update(|_, cx| {
            view.read(cx)
                .pending_copy_table
                .as_ref()
                .is_some_and(|form| {
                    if let Some(Err(error)) = &form.source_ddl {
                        panic!("source DDL: {error}");
                    }
                    form.source_ddl.is_some()
                })
        });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "copy source DDL timeout");
        std::thread::sleep(Duration::from_millis(30));
    }
    cx.update(|_, cx| {
        let expected = {
            let app = view.read(cx);
            app.copy_table_sql_for_form(app.pending_copy_table.as_ref().expect("copy form"))
                .expect("preview SQL")
        };
        assert!(expected.contains('\n'), "DDL fixture must be multiline");
        let preview = preview_state
            .borrow()
            .as_ref()
            .and_then(|state| state.upgrade())
            .expect("production preview state");
        assert_eq!(
            preview.read(cx).value().as_ref(),
            expected,
            "preview must preserve the SQL executed by CopyTable"
        );
    });
    let sql_box = cx
        .debug_bounds("copy-table-sql-preview")
        .expect("SQL preview");
    cx.simulate_click(sql_box.center(), Modifiers::default());
    let before_input = cx.update(|_, cx| {
        preview_state
            .borrow()
            .as_ref()
            .and_then(|state| state.upgrade())
            .expect("preview")
            .read(cx)
            .value()
    });
    cx.simulate_input("must not replace preview SQL");
    cx.update(|_, cx| {
        let preview = preview_state
            .borrow()
            .as_ref()
            .and_then(|state| state.upgrade())
            .expect("preview");
        assert_eq!(
            preview.read(cx).value(),
            before_input,
            "SQL preview must be read-only"
        );
    });
    assert!(cx.debug_bounds("CopyTableModal-confirm").is_some());
    if copy_data {
        let mode = cx
            .debug_bounds("copy-table-mode:true")
            .expect("structure and data");
        cx.simulate_click(mode.center(), Modifiers::default());
    }
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let confirm = cx
        .debug_bounds("CopyTableModal-confirm")
        .expect("copy confirm");
    cx.simulate_click(confirm.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.background_executor
            .advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let done = cx.update(|_, cx| {
            let app = view.read(cx);
            if let Some(error) = app
                .pending_copy_table
                .as_ref()
                .and_then(|form| form.error.as_ref())
            {
                panic!("copy failed: {error}");
            }
            app.pending_copy_table.is_none() && app._copy_table_task.is_none()
        });
        if done {
            break;
        }
        assert!(Instant::now() < deadline, "copy operation timeout");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(cx.debug_bounds("CopyTableModal-confirm").is_none());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        cx.background_executor.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.update(|window, cx| { window.draw(cx).clear(cx); });
        let visible = cx.update(|_, cx| {
            view.read(cx).controller.state().connections[0].objects.iter().any(|object| {
                object.path.database.as_deref() == Some(database.as_str())
                    && object.path.schema.as_deref() == Some("dbo")
                    && object.path.name == target
                    && object.path.kind == ObjectKind::Table
            })
        });
        if visible { break; }
        assert!(Instant::now() < deadline, "copied table did not refresh into the object tree");
        std::thread::sleep(Duration::from_millis(30));
    }
    let table_selector = Box::leak(format!("table-tree-row:{target}").into_boxed_str());
    assert!(cx.debug_bounds(table_selector).is_some(), "copied table row must render");
    let copied = execute(format!("SELECT * FROM [dbo].[{target}]")).remove(0);
    assert_eq!(copied.rows, before.rows);
    assert_eq!(copied.columns, before.columns);
    assert_eq!(
        execute(format!("SELECT * FROM [dbo].[{source}]"))
            .remove(0)
            .rows,
        before.rows
    );
    execute(format!(
        "DROP TABLE [dbo].[{target}]; DROP TABLE [dbo].[{source}];"
    ));
}
