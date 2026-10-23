#[gpui::test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server；仅查询安全目录，不写入"]
fn sqlserver_security_selects_database_without_connection_default(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent};
    use std::time::{Duration, Instant};

    let _guard = locale_guard();
    cx.background_executor.allow_parking();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(&app_assets_base_path().join("locales"), temp.path(), "en");
    let mut config = sqlserver_e2e_config();
    let database = config.sqlserver_profile.as_ref().expect("profile").basic.database.clone();
    assert_ne!(database, "master", "test needs a separate target database");
    config.sqlserver_profile.as_mut().expect("profile").basic.database.clear();
    if let fluxdb_core::Endpoint::Tcp { database, .. } = &mut config.endpoint {
        *database = None;
    }
    let connection_id = config.id;
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(window, cx, AppController::new(), storage, gpui_component::ThemeMode::Light)
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(1000.)));
    let view = cx.update(|window, cx| {
        window.root::<Root>().expect("root").expect("typed root")
            .read(cx).view().clone().downcast::<NavicatMain>().expect("production view")
    });
    cx.run_until_parked();
    let tab_id = cx.update(|_, cx| {
        view.update(cx, |this, cx| {
            this.controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
            let event = this.controller.dispatch(AppCommand::OpenUserAdmin(connection_id));
            cx.notify();
            match event {
                AppEvent::TabOpened(id) | AppEvent::TabActivated(id) => id,
                _ => panic!("security tab did not open"),
            }
        })
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let refresh = cx.debug_bounds("sqlserver-security-refresh").expect("refresh");
    cx.simulate_click(refresh.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        let ready = cx.update(|_, cx| {
            let admin = view.read(cx).user_admin_state_for(tab_id).expect("admin");
            let state = admin.sqlserver.expect("security");
            assert!(state.error.is_none(), "{:?}", state.error);
            state.databases.contains(&database) && !state.loading
        });
        if ready { break; }
        assert!(Instant::now() < deadline, "database discovery timed out");
        std::thread::sleep(Duration::from_millis(30));
    }
    cx.update(|_, cx| {
        let admin = view.read(cx).user_admin_state_for(tab_id).expect("admin");
        let state = admin.sqlserver.expect("security");
        assert!(state.database.is_empty(), "no database should be selected implicitly");
        assert!(state.snapshot.is_none());
    });
    let add = cx.debug_bounds("user-admin-add-user").expect("add button");
    cx.simulate_click(add.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("sqlserver-security-form-name").is_none(),
        "+ must stay disabled until a target database is selected"
    );

    // 通过生产 Select 的搜索与键盘确认，验证跨库切换后的真实目录归属。
    for target in [database.as_str(), "master"] {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let dropdown = cx.debug_bounds("sqlserver-security-database").expect("database selector");
        cx.simulate_click(dropdown.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_input(target);
        cx.run_until_parked();
        cx.simulate_keystrokes("down enter");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, cx| {
                let admin = view.read(cx).user_admin_state_for(tab_id).expect("admin");
                let state = admin.sqlserver.expect("security");
                assert!(state.error.is_none(), "{:?}", state.error);
                state.database == target && !state.loading
                    && state.snapshot.as_ref().is_some_and(|snapshot| snapshot.database == target)
            });
            if ready { break; }
            assert!(Instant::now() < deadline, "database selection did not load {target}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    cx.update(|_, cx| {
        let config = &view.read(cx).controller.state().connections[0].config;
        assert!(config.sqlserver_profile.as_ref().expect("profile").basic.database.is_empty());
    });
}
