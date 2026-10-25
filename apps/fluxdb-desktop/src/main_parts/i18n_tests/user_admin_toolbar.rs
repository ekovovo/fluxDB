#[gpui::test]
fn user_admin_toolbars_share_the_same_actions_and_button_metrics(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent, TabKind};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, DatabaseUserIdentity, Endpoint,
        SqlServerSecuritySnapshot,
    };

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                AppController::new(),
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
    cx.run_until_parked();

    for (kind, label, selectors) in [
        (
            DatabaseKind::MySql,
            "MySQL",
            [
                "user-admin-save",
                "user-admin-cancel",
                "user-admin-refresh",
                "user-admin-delete",
            ],
        ),
        (
            DatabaseKind::Postgres,
            "PostgreSQL",
            [
                "pg-user-admin-save",
                "pg-user-admin-cancel",
                "pg-user-admin-refresh",
                "pg-user-admin-delete",
            ],
        ),
        (
            DatabaseKind::SqlServer,
            "SQL Server",
            [
                "sqlserver-security-save",
                "sqlserver-security-discard",
                "sqlserver-security-refresh",
                "sqlserver-security-delete",
            ],
        ),
    ] {
        let connection_id = ConnectionId(90 + selectors[0].len() as u64);
        let config = ConnectionConfig {
            id: connection_id,
            name: format!("{label} toolbar"),
            kind,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
                database: Some("fluxdb_demo".into()),
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            mongo_profile: None,
            sqlserver_profile: None,
        };
        let tab_id = cx.update(|_, cx| {
            view.update(cx, |this, _cx| {
                this.controller
                    .dispatch(AppCommand::ReplaceConnections(vec![config]));
                match this.controller.dispatch(AppCommand::OpenUserAdmin(connection_id)) {
                    AppEvent::TabOpened(tab_id) | AppEvent::TabActivated(tab_id) => tab_id,
                    event => panic!("{label} user-admin did not open: {event:?}"),
                }
            })
        });
        cx.update(|_, cx| {
            view.update(cx, |this, _cx| {
                let event = match kind {
                    DatabaseKind::MySql => this.controller.dispatch(
                        AppCommand::FinishUserAdminUsersLoad {
                            tab_id,
                            result: Ok(vec![DatabaseUserIdentity {
                                user: "app_user".into(),
                                host: "%".into(),
                                plugin: Some("caching_sha2_password".into()),
                            }]),
                        },
                    ),
                    DatabaseKind::Postgres => {
                        this.controller.dispatch(AppCommand::PgBeginCreateRole(tab_id))
                    }
                    DatabaseKind::SqlServer => {
                        let result = this.controller.dispatch(
                            AppCommand::FinishSqlServerSecurityLoad {
                                tab_id,
                                result: Ok(SqlServerSecuritySnapshot {
                                    database: "fluxdb_demo".into(),
                                    current_login: "sa".into(),
                                    ..Default::default()
                                }),
                            },
                        );
                        this.controller.dispatch(AppCommand::OpenSqlServerSecurityForm {
                            tab_id,
                            kind: fluxdb_app::SqlServerSecurityFormKind::CreateLogin,
                        });
                        result
                    }
                    _ => unreachable!(),
                };
                assert!(
                    !matches!(event, AppEvent::Failed(_)),
                    "{label} toolbar fixture failed: {event:?}"
                );
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|_, cx| {
            assert!(
                view.read(cx)
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .any(|tab| tab.id == tab_id && matches!(tab.kind, TabKind::UserAdmin(_)))
            );
        });

        let mut heights = Vec::new();
        for selector in selectors {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{label} toolbar button missing: {selector}"));
            heights.push(f32::from(bounds.size.height));
            assert!(
                f32::from(bounds.size.height) >= 29. && f32::from(bounds.size.height) <= 31.,
                "{label} button {selector} must use the shared 30px height: {bounds:?}"
            );
        }
        assert_eq!(
            heights.len(),
            4,
            "{label} toolbar must expose save/cancel/refresh/delete"
        );
    }
}

#[gpui::test]
fn user_admin_cancel_actions_are_wired_for_mysql_and_postgres(cx: &mut TestAppContext) {
    use fluxdb_app::{AppCommand, AppEvent};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, DatabaseUserIdentity, Endpoint,
    };

    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            navicat_main_new(
                window,
                cx,
                AppController::new(),
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
    cx.run_until_parked();

    for (kind, selector, cancel) in [
        (
            DatabaseKind::MySql,
            "user-admin-cancel",
            "mysql",
        ),
        (
            DatabaseKind::Postgres,
            "pg-user-admin-cancel",
            "postgres",
        ),
    ] {
        let connection_id = ConnectionId(if cancel == "mysql" { 201 } else { 202 });
        let config = ConnectionConfig {
            id: connection_id,
            name: format!("{cancel} cancel"),
            kind,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
                database: Some("fluxdb_demo".into()),
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            mongo_profile: None,
            sqlserver_profile: None,
        };
        let tab_id = cx.update(|_, cx| {
            view.update(cx, |this, _cx| {
                this.controller
                    .dispatch(AppCommand::ReplaceConnections(vec![config]));
                match this.controller.dispatch(AppCommand::OpenUserAdmin(connection_id)) {
                    AppEvent::TabOpened(tab_id) | AppEvent::TabActivated(tab_id) => tab_id,
                    event => panic!("{kind:?} user-admin did not open: {event:?}"),
                }
            })
        });
        cx.update(|_, cx| {
            view.update(cx, |this, cx| {
                if cancel == "mysql" {
                    this.controller.dispatch(AppCommand::FinishUserAdminUsersLoad {
                        tab_id,
                        result: Ok(vec![DatabaseUserIdentity {
                            user: "app_user".into(),
                            host: "%".into(),
                            plugin: Some("caching_sha2_password".into()),
                        }]),
                    });
                    this.controller.dispatch(AppCommand::BeginUserAdminCreateUser(tab_id));
                } else {
                    this.controller.dispatch(AppCommand::PgBeginCreateRole(tab_id));
                    this.controller.dispatch(AppCommand::SetPgDraftName {
                        tab_id,
                        name: "cancel_role".into(),
                    });
                }
                cx.notify();
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{kind:?} cancel button missing"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let admin = view
                .read(cx)
                .user_admin_state_for(tab_id)
                .expect("user admin");
            if cancel == "mysql" {
                assert!(!admin.creating_user);
                assert!(admin.new_password.is_empty());
            } else {
                assert!(admin.pg_draft.is_none());
                assert!(admin.pg_selected_role.is_none());
            }
        });
    }
}

#[test]
fn mysql_create_preview_orders_user_before_grants() {
    use crate::UserAdminSqlPreviewDraft;
    use fluxdb_app::UserAdminState;
    use fluxdb_core::{ConnectionId, DatabaseKind, PrivilegeScope};

    let provider =
        fluxdb_core::database_user_admin_provider(DatabaseKind::MySql).expect("MySQL provider");
    let mut admin = UserAdminState::new(
        ConnectionId(211),
        Some("fluxdb_demo".into()),
        PrivilegeScope::MySql,
    );
    admin.creating_user = true;
    admin.create_user = "created_user".into();
    admin.create_host = "%".into();
    admin.new_password = "FluxDB!123".into();
    admin.create_password = admin.new_password.clone();
    admin.privilege_rows = vec![fluxdb_app::UserAdminPrivilegeRow {
        id: 1,
        database: "*".into(),
        privileges: vec!["SELECT".into()],
        grant_option: false,
    }];
    admin.next_privilege_row_id = 2;
    let mut draft = UserAdminSqlPreviewDraft::default();
    if let Some(general) = crate::user_admin_general_sql_preview_for_test(provider, &admin) {
        draft.extend(general);
    }
    draft.extend(crate::user_admin_privileges_sql_preview_for_test(
        provider, &admin,
    ));
    let statements = draft.statements;
    assert!(
        statements
            .first()
            .is_some_and(|statement| statement.starts_with("CREATE USER")),
        "{statements:?}"
    );
    assert!(
        statements
            .iter()
            .any(|statement| statement.starts_with("GRANT SELECT")),
        "{statements:?}"
    );
}
