#[gpui::test]
fn drop_and_truncate_use_the_aligned_component_select(cx: &mut TestAppContext) {
    use crate::{DangerTableAction, ForeignKeyCheckMode, PendingDangerTableAction};
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
        name: "MySQL danger layout".into(),
        kind: DatabaseKind::MySql,
        endpoint: Endpoint::Tcp {
            host: "danger.invalid".into(),
            port: 3306,
            database: Some("demo".into()),
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        mongo_profile: None,
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
    for action in [DangerTableAction::Drop, DangerTableAction::Truncate] {
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.pending_danger_table_action = Some(PendingDangerTableAction {
                    object_path: ObjectPath {
                        connection_id,
                        database: Some("demo".into()),
                        schema: None,
                        name: "ITEMS".into(),
                        kind: ObjectKind::Table,
                    },
                    action,
                    foreign_key_check: ForeignKeyCheckMode::Default,
                    restart_identity: false,
                    acknowledged: true,
                    error: None,
                });
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let row = cx
            .debug_bounds("danger-table-fk-row")
            .expect("foreign key check row");
        let select = cx
            .debug_bounds("danger-table-fk-select")
            .expect("component select");
        assert!(
            (select.size.height - gpui::px(34.)).abs() < gpui::px(1.),
            "select keeps its component height"
        );
        let row_center = row.origin.y + row.size.height / 2.;
        let select_center = select.origin.y + select.size.height / 2.;
        assert!(
            (row_center - select_center).abs() < gpui::px(1.),
            "select is vertically centered in the labeled row"
        );
    }
}
