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
            oracle_profile: None,
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

/// 「保存」必须先弹确认框展示本次 SQL，确认后才执行。
///
/// 这条曾经失守：某一轮把五库的二次确认都去掉了，保存直接落库，而当时的用例
/// （SQL Server 断言「Save executes the staged plan directly」）恰好把错误行为写成了预期。
/// 这里锁住共同契约：点保存 → 出现 `user-admin-sql-preview-modal` 且带 SQL → 未执行；
/// 点取消 → 弹框消失且仍未执行。
#[gpui::test]
fn user_admin_save_opens_sql_confirmation_modal(cx: &mut TestAppContext) {
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

    let config = ConnectionConfig {
        id: ConnectionId(931),
        name: "MySQL confirm".into(),
        kind: DatabaseKind::MySql,
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
        sqlserver_profile: None,
        oracle_profile: None,
        mongo_profile: None,
    };
    let tab_id = cx.update(|_, cx| {
        view.update(cx, |this, _cx| {
            this.controller
                .dispatch(AppCommand::ReplaceConnections(vec![config]));
            match this
                .controller
                .dispatch(AppCommand::OpenUserAdmin(ConnectionId(931)))
            {
                AppEvent::TabOpened(tab_id) | AppEvent::TabActivated(tab_id) => tab_id,
                event => panic!("user-admin did not open: {event:?}"),
            }
        })
    });
    cx.update(|_, cx| {
        view.update(cx, |this, _cx| {
            this.controller
                .dispatch(AppCommand::FinishUserAdminUsersLoad {
                    tab_id,
                    result: Ok(vec![DatabaseUserIdentity {
                        user: "app_user".into(),
                        host: "%".into(),
                        plugin: Some("caching_sha2_password".into()),
                    }]),
                });
            // 一次保存的草稿：建号 + 口令，不给「加入变更」之类的中间步骤。
            this.controller
                .dispatch(AppCommand::BeginUserAdminCreateUser(tab_id));
            this.controller.dispatch(AppCommand::SetUserAdminCreateUser {
                tab_id,
                user: "confirm_user".into(),
            });
            // 新建表单的两个口令框都必须有值且一致，否则通用表单校验不过、不会生成 SQL。
            for command in [
                AppCommand::SetUserAdminCreatePassword {
                    tab_id,
                    password: "FluxDB!123".into(),
                },
                AppCommand::SetUserAdminNewPassword {
                    tab_id,
                    password: "FluxDB!123".into(),
                },
            ] {
                this.controller.dispatch(command);
            }
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(
        cx.debug_bounds("user-admin-sql-preview-modal").is_none(),
        "the confirmation modal must not be open before Save"
    );
    let save = cx
        .debug_bounds("user-admin-save")
        .expect("MySQL save button");
    cx.simulate_click(save.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("user-admin-sql-preview-modal").is_some(),
        "Save must open the SQL confirmation modal"
    );
    let (sql, applying) = cx.update(|_, cx| {
        let admin = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .expect("admin state");
        (
            admin.pending_sql.as_ref().map(|pending| pending.sql.clone()),
            admin.applying,
        )
    });
    let sql = sql.expect("confirmation modal must carry the SQL");
    assert!(
        sql.contains("CREATE USER") && sql.contains("confirm_user"),
        "modal must show the same statements as the SQL preview page, got: {sql}"
    );
    assert!(!applying, "Save must not execute before confirmation");

    let cancel = cx
        .debug_bounds("user-admin-sql-preview-cancel")
        .expect("modal cancel button");
    cx.simulate_click(cancel.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.debug_bounds("user-admin-sql-preview-modal").is_none(),
        "Cancel must close the modal"
    );
    cx.update(|_, cx| {
        let admin = view
            .read(cx)
            .user_admin_state_for(tab_id)
            .expect("admin state");
        assert!(admin.pending_sql.is_none(), "cancel must drop the pending SQL");
        assert!(!admin.applying, "cancel must not execute");
        // 草稿还在：取消弹框不等于丢弃编辑。
        assert_eq!(admin.create_user, "confirm_user");
    });
}

/// 四个库的用户与权限页签条必须同高、同宽（等分），否则切换库时页签会跳。
///
/// 这条曾经失败：MySQL 用 28px + 按文字撑开，PG 用 36px + 按文字撑开，
/// SQL Server / Oracle 用 36px + `flex_1` 等分，同一功能四种外观。
#[gpui::test]
fn user_admin_tab_strips_share_the_same_metrics(cx: &mut TestAppContext) {
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

    for (kind, label, prefixes) in [
        (
            DatabaseKind::MySql,
            "MySQL",
            [
                "user-admin-tab-general",
                "user-admin-tab-member-of",
                "user-admin-tab-privileges",
                "user-admin-tab-sql-preview",
            ],
        ),
        (
            DatabaseKind::Postgres,
            "PostgreSQL",
            [
                "pg-user-admin-tab-general",
                "pg-user-admin-tab-member-of",
                "pg-user-admin-tab-privileges",
                "pg-user-admin-tab-sql-preview",
            ],
        ),
        (
            DatabaseKind::SqlServer,
            "SQL Server",
            [
                "sqlserver-security-tab-general",
                "sqlserver-security-tab-roles",
                "sqlserver-security-tab-permissions",
                "sqlserver-security-tab-sql-preview",
            ],
        ),
    ] {
        let connection_id = ConnectionId(800 + prefixes[0].len() as u64);
        let config = ConnectionConfig {
            id: connection_id,
            name: format!("{label} tabs"),
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
            sqlserver_profile: None,
            oracle_profile: None,
            mongo_profile: None,
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
                    DatabaseKind::SqlServer => {
                        this.controller.dispatch(AppCommand::FinishSqlServerSecurityLoad {
                            tab_id,
                            result: Ok(SqlServerSecuritySnapshot::default()),
                        });
                        this.controller.dispatch(AppCommand::SelectUserAdminDetailTab {
                            tab_id,
                            detail_tab: fluxdb_app::UserAdminDetailTab::MySql(fluxdb_app::UserAdminMySqlTab::General),
                        })
                    }
                    _ => {
                        this.controller.dispatch(AppCommand::FinishUserAdminUsersLoad {
                            tab_id,
                            result: Ok(vec![DatabaseUserIdentity {
                                user: "app_user".into(),
                                host: "%".into(),
                                plugin: Some("caching_sha2_password".into()),
                            }]),
                        });
                        this.controller.dispatch(AppCommand::SelectUserAdminDetailTab {
                            tab_id,
                            detail_tab: fluxdb_app::UserAdminDetailTab::MySql(fluxdb_app::UserAdminMySqlTab::General),
                        })
                    }
                };
                assert!(
                    !matches!(event, AppEvent::Failed(_)),
                    "{label} tab fixture failed: {event:?}"
                );
                assert!(
                    this.controller
                        .state()
                        .tabs
                        .iter()
                        .any(|tab| tab.id == tab_id && matches!(tab.kind, TabKind::UserAdmin(_)))
                );
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // 各库都有的四页：常规 / 角色（成员关系）/ 权限 / SQL 预览。
        let mut widths = Vec::new();
        for selector in prefixes {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{label} tab strip missing {selector}"));
            widths.push(f32::from(bounds.size.width));
            assert!(
                (f32::from(bounds.size.height) - 36.).abs() < 0.5,
                "{label} tab {selector} must use the shared 36px height: {bounds:?}"
            );
        }
        // 等分是亚像素布局，除不尽时最后一格会差半个像素。
        let first = widths[0];
        for (index, width) in widths.iter().enumerate() {
            assert!(
                (width - first).abs() <= 1.,
                "{label} tab {index} must share the equal split width {first}: {widths:?}"
            );
        }
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
            oracle_profile: None,
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
