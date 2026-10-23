#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_procedure_tree_opens_read_only_ddl(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use fluxdb_app::{AppCommand, AppEvent};
    use fluxdb_core::{ObjectKind, ObjectPath, QueryExecutionOptions};

    let config = sqlserver_e2e_config();
    let database = config
        .sqlserver_profile
        .as_ref()
        .expect("SQL Server profile")
        .basic
        .database
        .clone();
    let procedure = format!(
        "FLUXDB_PROC_UI_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
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
    };
    execute(format!(
        "CREATE OR ALTER PROCEDURE [dbo].[{procedure}] AS SELECT 1 AS [V]"
    ));
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database.clone()),
        schema: Some("dbo".into()),
        name: procedure.clone(),
        kind: ObjectKind::Procedure,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = locale_guard();
        cx.background_executor.allow_parking();
        let temp = tempfile::tempdir().expect("temp profile");
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
                schema: Some("dbo".into()),
                name: "dbo".into(),
                ..object.clone()
            }),
        ] {
            let event = controller.dispatch(command);
            assert!(matches!(event, AppEvent::ObjectsLoaded(_, _)), "{event:?}");
        }
        let filter = procedure.clone();
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
        let selector = Box::leak(format!("routine-tree-row:{procedure}").into_boxed_str());
        let row = cx.debug_bounds(selector).expect("procedure row");
        cx.simulate_click(row.center(), Modifiers::default());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            cx.background_executor
                .advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            let loaded = cx.update(|_, cx| {
                let form = view
                    .read(cx)
                    .pending_routine_ddl
                    .as_ref()
                    .expect("routine DDL modal");
                if let Some(error) = &form.error {
                    panic!("routine DDL failed: {error}");
                }
                form.ddl.as_deref().is_some_and(|ddl| ddl.contains("CREATE"))
            });
            if loaded {
                break;
            }
            assert!(Instant::now() < deadline, "routine DDL timeout");
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(
            cx.debug_bounds("routine-ddl-content").is_some(),
            "read-only DDL editor must render"
        );
        cx.update(|_, cx| {
            let ddl = view
                .read(cx)
                .pending_routine_ddl
                .as_ref()
                .and_then(|form| form.ddl.as_deref())
                .expect("loaded DDL");
            assert!(ddl.contains(&procedure), "{ddl}");
        });
    }));
    execute(format!("DROP PROCEDURE IF EXISTS [dbo].[{procedure}]"));
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
