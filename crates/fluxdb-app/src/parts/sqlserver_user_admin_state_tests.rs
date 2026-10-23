use super::*;

fn editable_login_state() -> SqlServerSecurityState {
    let mut state = state();
    state
        .snapshot
        .as_mut()
        .expect("snapshot")
        .logins
        .push(fluxdb_core::SqlServerLogin {
            name: "app_login".into(),
            login_type: "SQL_LOGIN".into(),
            is_disabled: false,
            default_database: Some("fluxdb_demo".into()),
            default_language: None,
            create_date: None,
        });
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::Login,
        name: "app_login".into(),
    });
    state.edit_selected();
    state
}

fn controller_with_security(state: SqlServerSecurityState) -> (AppController, TabId) {
    let mut controller = AppController::new();
    let tab_id = TabId(101);
    let mut admin = UserAdminState::new(
        ConnectionId(51),
        Some("fluxdb_demo".into()),
        PrivilegeScope::SqlServer,
    );
    admin.sqlserver = Some(state);
    controller.state.tabs.push(TabState {
        id: tab_id,
        title: "security".into(),
        kind: TabKind::UserAdmin(admin),
        dirty: false,
    });
    (controller, tab_id)
}

#[test]
fn opening_clean_forms_does_not_claim_database_changes() {
    let mut state = editable_login_state();
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::EditLogin);
    assert!(!state.has_changes());
    assert!(state.can_switch_database());

    state.form = SqlServerSecurityForm::open(SqlServerSecurityFormKind::CreateLogin);
    state.form.default_value = state.database.clone();
    assert!(!state.has_changes());
    assert!(state.can_switch_database());
}

#[test]
fn security_form_debug_never_exposes_password_or_confirmation() {
    let form = SqlServerSecurityForm {
        password: "NeverLogSecret".into(),
        confirm_password: "NeverLogConfirmation".into(),
        ..Default::default()
    };
    let debug = format!("{form:?}");
    assert!(!debug.contains("NeverLogSecret"));
    assert!(!debug.contains("NeverLogConfirmation"));
    assert!(debug.contains("********"));
}

#[test]
fn inline_login_edit_stages_password_and_database_together_and_cancel_restores_baseline() {
    let mut state = editable_login_state();
    state.form.password = "New!Password123".into();
    state.form.confirm_password = state.form.password.clone();
    state.form.default_value = "master".into();
    assert!(state.has_changes());
    let (mut controller, tab_id) = controller_with_security(state);

    controller
        .submit_sqlserver_security_form(tab_id)
        .expect("stage inline edit");
    let security = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("security");
    assert!(matches!(security.pending_changes.as_slice(), [
            SqlServerSecurityChange::SetLoginPassword { name, .. },
            SqlServerSecurityChange::SetLoginDefaultDatabase { database: Some(database), .. }
        ] if name == "app_login" && database == "master"));
    assert!(security.form.password.is_empty());
    assert_eq!(security.form.kind, SqlServerSecurityFormKind::EditLogin);
    assert!(!security.has_form_changes());

    controller.dispatch(AppCommand::DiscardSqlServerSecurityChanges(tab_id));
    let security = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("security");
    assert!(!security.has_changes());
    assert_eq!(security.form.default_value, "fluxdb_demo");
}

#[test]
fn validation_failure_does_not_stage_part_of_an_inline_edit() {
    let mut state = editable_login_state();
    state.form.password = "New!Password123".into();
    state.form.confirm_password = "does not match".into();
    state.form.default_value = "master".into();
    let (mut controller, tab_id) = controller_with_security(state);
    let error = controller
        .submit_sqlserver_security_form(tab_id)
        .expect_err("password confirmation");
    assert_eq!(error.kind, ErrorKind::Query);
    let security = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("security");
    assert!(security.pending_changes.is_empty());
    assert_eq!(security.form.default_value, "master");
}

#[test]
fn late_preview_cannot_confirm_a_newer_form_revision() {
    let mut state = editable_login_state();
    state.queue_change(SqlServerSecurityChange::SetLoginEnabled {
        name: "app_login".into(),
        enabled: false,
    });
    state.preview_loading = true;
    let revision = state.revision;
    let (mut controller, tab_id) = controller_with_security(state);
    controller.dispatch(AppCommand::SetSqlServerSecurityFormField {
        tab_id,
        field: SqlServerSecurityFormField::Name,
        value: "app_login".into(),
    });
    assert_eq!(
        controller
            .user_admin_state(tab_id)
            .expect("admin")
            .sqlserver
            .as_ref()
            .expect("state")
            .revision,
        revision,
        "synchronizing identical form values must not invalidate a preview"
    );
    controller.dispatch(AppCommand::SetSqlServerSecurityFormField {
        tab_id,
        field: SqlServerSecurityFormField::Password,
        value: "Changed!123".into(),
    });
    controller.dispatch(AppCommand::FinishSqlServerSecurityPreview {
        tab_id,
        revision,
        result: Ok(vec!["ALTER LOGIN [app_login] DISABLE;".into()]),
    });
    let state = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("state");
    assert!(state.preview.is_none());
    assert!(!state.preview_loading);
        assert!(!matches!(
        controller.dispatch(AppCommand::StartSqlServerSecurityApply(tab_id)),
        AppEvent::Failed(_)
        ), "direct Save must be allowed even when the form changed after an older preview");
}

#[test]
fn rejected_new_login_restores_form_and_retry_replaces_instead_of_duplicating_creation() {
    let mut state = editable_login_state();
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::Login,
        name: "new_login".into(),
    });
    state.form.clear();
    state.queue_change(SqlServerSecurityChange::CreateLogin {
        name: "new_login".into(),
        password: "weak".into(),
        default_database: Some("fluxdb_demo".into()),
        disabled: false,
    });
    let (mut controller, tab_id) = controller_with_security(state);
    controller.dispatch(AppCommand::FinishSqlServerSecurityApply {
        tab_id,
        principal: "new_login".into(),
        result: Err(UserFacingError::from(Error::new(
            ErrorKind::Query,
            "password policy rejected",
        ))),
    });
    let state = controller
        .user_admin_state_mut(tab_id)
        .expect("admin")
        .sqlserver
        .as_mut()
        .expect("state");
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::CreateLogin);
    assert_eq!(state.form.name, "new_login");
    state.form.password = "Strong!Replacement123".into();
    state.form.confirm_password = state.form.password.clone();

    controller
        .submit_sqlserver_security_form(tab_id)
        .expect("correct and retry");
    let state = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("state");
    assert!(matches!(state.pending_changes.as_slice(),
            [SqlServerSecurityChange::CreateLogin { password, .. }] if password == "Strong!Replacement123"));
}

#[test]
fn renaming_a_failed_create_replaces_old_identity_and_dependent_plan_items() {
    let mut state = editable_login_state();
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::Login,
        name: "test".into(),
    });
    state.queue_change(SqlServerSecurityChange::CreateLogin {
        name: "test".into(),
        password: "Strong!Password123".into(),
        default_database: Some("fluxdb_demo".into()),
        disabled: false,
    });
    state.queue_change(SqlServerSecurityChange::AddRoleMember {
        scope: SqlServerRoleScope::Server,
        role: "bulkadmin".into(),
        member: "test".into(),
    });
    state.queue_change(SqlServerSecurityChange::GrantPermission {
        grantee: "test".into(),
        target: SqlServerPermissionTarget::Server,
        permission: "CONNECT SQL".into(),
        state: SqlServerPermissionState::Grant,
        grant_option: false,
        replace_existing: false,
        cascade_revoke: false,
    });
    state.form = SqlServerSecurityForm {
        kind: SqlServerSecurityFormKind::CreateLogin,
        name: "test22".into(),
        password: "Strong!Password123".into(),
        confirm_password: "Strong!Password123".into(),
        default_value: "fluxdb_demo".into(),
        ..Default::default()
    };
    let (mut controller, tab_id) = controller_with_security(state);
    controller
        .submit_sqlserver_security_form(tab_id)
        .expect("rename and retry");
    let state = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("state");
    assert_eq!(state.pending_creations().len(), 1);
    assert!(state.pending_creations().iter().any(|principal| {
        principal.kind == SqlServerPrincipalKind::Login && principal.name == "test22"
    }));
    assert!(state.pending_changes.iter().all(|change| match change {
        SqlServerSecurityChange::CreateLogin { name, .. }
        | SqlServerSecurityChange::AddRoleMember { member: name, .. }
        | SqlServerSecurityChange::GrantPermission { grantee: name, .. } => name != "test",
        _ => true,
    }));
    assert!(
        state
            .build_plan()
            .expect("retry plan")
            .changes
            .iter()
            .all(|change| !matches!(
                change,
                SqlServerSecurityChange::CreateLogin { name, .. } if name == "test"
            ))
    );
    assert!(
        state
            .preview_for_display()
            .iter()
            .all(|sql| !sql.contains("[test]"))
    );
}

#[test]
fn permission_composer_is_independent_from_the_login_form() {
    let mut state = editable_login_state();
    state.form = SqlServerSecurityForm {
        kind: SqlServerSecurityFormKind::CreateLogin,
        name: "test22".into(),
        password: "Strong!Password123".into(),
        confirm_password: "Strong!Password123".into(),
        default_value: "fluxdb_demo".into(),
        ..Default::default()
    };
    state.permission_form = SqlServerSecurityForm::open(
        SqlServerSecurityFormKind::GrantPermission,
    );
    state.permission_form.permission_scope = "SERVER".into();
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::CreateLogin);
    assert_eq!(
        state.permission_form.kind,
        SqlServerSecurityFormKind::GrantPermission
    );
    state.permission_form.clear();
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::CreateLogin);
    assert_eq!(state.form.name, "test22");
}

    #[test]
    fn pending_login_can_be_used_by_a_user_in_the_same_plan() {
        let mut state = state();
        state.queue_change(SqlServerSecurityChange::CreateLogin {
            name: "new_login".into(),
            password: "Strong!Password123".into(),
            default_database: Some("fluxdb_demo".into()),
            disabled: false,
        });
        assert!(state.is_known_sql_login("new_login"));
        state.queue_change(SqlServerSecurityChange::CreateUser {
            name: "new_user".into(),
            login: Some("new_login".into()),
            default_schema: Some("dbo".into()),
        });
        state.selected = Some(SqlServerPrincipalRef {
            kind: SqlServerPrincipalKind::User,
            name: "new_user".into(),
        });
        assert!(state.selected_user_is_editable());
        assert_eq!(state.pending_creations().len(), 2);
        let plan = state.build_plan().expect("combined plan");
        assert_eq!(plan.changes.len(), 2);
    }

    #[test]
    fn pending_creation_does_not_block_opening_the_next_creation_form() {
        let mut state = editable_login_state();
        state.queue_change(SqlServerSecurityChange::CreateLogin {
            name: "new_login".into(),
            password: "Strong!Password123".into(),
            default_database: Some("fluxdb_demo".into()),
            disabled: false,
        });
        let (mut controller, tab_id) = controller_with_security(state);
        let event = controller.dispatch(AppCommand::OpenSqlServerSecurityForm {
            tab_id,
            kind: SqlServerSecurityFormKind::CreateUser,
        });
        assert!(!matches!(event, AppEvent::Failed(_)));
        let state = controller.user_admin_state(tab_id).expect("admin").sqlserver.as_ref().expect("state");
        assert_eq!(state.form.kind, SqlServerSecurityFormKind::CreateUser);
        assert_eq!(state.pending_changes.len(), 1);
    }

    #[test]
    fn pending_principal_permissions_tab_does_not_query_nonexistent_server_principal() {
        let mut state = editable_login_state();
        state.queue_change(SqlServerSecurityChange::CreateLogin {
            name: "new_login".into(),
            password: "Strong!Password123".into(),
            default_database: Some("fluxdb_demo".into()),
            disabled: false,
        });
        state.selected = Some(SqlServerPrincipalRef {
            kind: SqlServerPrincipalKind::Login,
            name: "new_login".into(),
        });
        let (mut controller, tab_id) = controller_with_security(state);
        let principal = controller
            .user_admin_state(tab_id)
            .and_then(|admin| admin.sqlserver.clone())
            .and_then(|state| state.selected)
            .expect("pending principal");
        controller.dispatch(AppCommand::StartSqlServerPermissionsLoad {
            tab_id,
            principal: principal.clone(),
        });
        let state = controller.user_admin_state(tab_id).expect("admin").sqlserver.as_ref().expect("state");
        assert!(!state.loading_permissions);
        assert_eq!(state.permissions_loaded_for.as_ref(), Some(&principal));
    }

#[test]
fn pending_permissions_remain_visible_and_can_be_removed_without_server_revoke() {
    let mut state = editable_login_state();
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::Login,
        name: "new_login".into(),
    });
    state.queue_change(SqlServerSecurityChange::CreateLogin {
        name: "new_login".into(),
        password: "Strong!Password123".into(),
        default_database: Some("fluxdb_demo".into()),
        disabled: false,
    });
    state.queue_change(SqlServerSecurityChange::GrantPermission {
        grantee: "new_login".into(),
        target: SqlServerPermissionTarget::Server,
        permission: "CONNECT SQL".into(),
        state: SqlServerPermissionState::Grant,
        grant_option: false,
        replace_existing: false,
        cascade_revoke: false,
    });

    let permissions = state.effective_permissions();
    assert_eq!(permissions.len(), 1);
    assert!(state.permission_is_pending(&permissions[0]));

    state.queue_permission_revoke(&permissions[0]);
    assert!(state.effective_permissions().is_empty());
    assert!(
        state
            .pending_changes
            .iter()
            .all(|change| !matches!(change, SqlServerSecurityChange::RevokePermission { .. })),
        "removing an unsaved permission must not generate a server REVOKE"
    );
}

#[test]
fn permission_composer_is_not_a_change_until_a_permission_is_selected() {
    let mut state = editable_login_state();
    state.permission_form =
        SqlServerSecurityForm::open(SqlServerSecurityFormKind::GrantPermission);
    state.permission_form.permission_scope = "SERVER".into();
    assert!(!state.has_permission_form_changes());

    state.permission_form.permission = "CONNECT SQL".into();
    assert!(state.has_permission_form_changes());
}

#[test]
fn repeated_pending_permission_replaces_the_row_instead_of_duplicating_it() {
    let mut state = editable_login_state();
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::Login,
        name: "app_login".into(),
    });
    for _ in 0..2 {
        state.queue_change(SqlServerSecurityChange::GrantPermission {
            grantee: "app_login".into(),
            target: SqlServerPermissionTarget::Server,
            permission: "CONNECT SQL".into(),
            state: SqlServerPermissionState::Grant,
            grant_option: false,
            replace_existing: false,
            cascade_revoke: false,
        });
    }
    let permissions = state.effective_permissions();
    assert_eq!(permissions.len(), 1);
    assert_eq!(
        state.pending_changes.len(),
        1,
        "the same explicit permission must have one pending change"
    );
}

#[test]
fn preview_display_restores_password_without_changing_the_event_payload() {
    let mut state = state();
    state.queue_change(SqlServerSecurityChange::CreateLogin {
        name: "new_login".into(),
        password: "Se'cret!123".into(),
        default_database: Some("fluxdb_demo".into()),
        disabled: false,
    });
    state.preview = Some(vec![
        "CREATE LOGIN [new_login] WITH PASSWORD = N'********';".into(),
    ]);

    assert!(
        state
            .preview
            .as_ref()
            .expect("stored preview")
            .iter()
            .all(|sql| !sql.contains("Se'cret!123"))
    );
    assert_eq!(
        state.preview_for_display(),
        vec!["CREATE LOGIN [new_login] WITH PASSWORD = N'Se''cret!123';"]
    );
}

#[test]
fn confirmed_failures_keep_edits_but_only_transport_failures_require_verification() {
    for kind in [
        ErrorKind::Query,
        ErrorKind::Permission,
        ErrorKind::Connection,
        ErrorKind::Timeout,
    ] {
        let mut state = editable_login_state();
        state.queue_change(SqlServerSecurityChange::SetLoginPassword {
            name: "app_login".into(),
            password: "New!Password123".into(),
        });
        state.applying = true;
        let plan = state.build_plan().expect("plan");
        let snapshot = state.snapshot.clone().expect("snapshot");
        let (mut controller, tab_id) = controller_with_security(state);
        controller.dispatch(AppCommand::FinishSqlServerSecurityApply {
            tab_id,
            principal: plan.principal,
            result: Err(UserFacingError::from(Error::new(kind, "test rejection"))),
        });
        let security = controller
            .user_admin_state(tab_id)
            .expect("admin")
            .sqlserver
            .as_ref()
            .expect("security");
        let uncertain = matches!(kind, ErrorKind::Connection | ErrorKind::Timeout);
        assert_eq!(
            security.save_status,
            if uncertain {
                SqlServerSecuritySaveStatus::NeedsVerify
            } else {
                SqlServerSecuritySaveStatus::Idle
            }
        );
        assert!(!security.applying);
        assert!(security.has_pending_changes());
        if uncertain {
            let event = controller.dispatch(AppCommand::StartSqlServerSecurityApply(tab_id));
            assert!(
                matches!(event, AppEvent::Failed(_)),
                "cannot replay before verification"
            );
            controller.dispatch(AppCommand::FinishSqlServerSecurityLoad {
                tab_id,
                result: Ok(snapshot),
            });
            let security = controller
                .user_admin_state(tab_id)
                .expect("admin")
                .sqlserver
                .as_ref()
                .expect("security");
            assert_eq!(security.save_status, SqlServerSecuritySaveStatus::Idle);
            assert!(!security.has_changes());
        }
    }
}

#[test]
fn successful_create_apply_clears_form_and_refresh_selects_created_principal() {
    let mut state = state();
    state
        .snapshot
        .as_mut()
        .expect("snapshot")
        .logins
        .push(fluxdb_core::SqlServerLogin {
            name: "sa".into(),
            login_type: "SQL_LOGIN".into(),
            is_disabled: false,
            default_database: Some("master".into()),
            default_language: None,
            create_date: None,
        });
    state.form = SqlServerSecurityForm {
        kind: SqlServerSecurityFormKind::CreateLogin,
        name: "created_login".into(),
        password: "Strong!Password123".into(),
        confirm_password: "Strong!Password123".into(),
        default_value: "fluxdb_demo".into(),
        ..Default::default()
    };
    state.queue_change(SqlServerSecurityChange::CreateLogin {
        name: "created_login".into(),
        password: "Strong!Password123".into(),
        default_database: Some("fluxdb_demo".into()),
        disabled: false,
    });
    state.applying = true;
    let (mut controller, tab_id) = controller_with_security(state);
    controller.dispatch(AppCommand::FinishSqlServerSecurityApply {
        tab_id,
        principal: "created_login".into(),
        result: Ok(vec!["CREATE LOGIN [created_login] ...;".into()]),
    });
    let state = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("state");
    assert!(!state.has_changes());
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::None);
    assert_eq!(
        state.selected,
        Some(SqlServerPrincipalRef {
            kind: SqlServerPrincipalKind::Login,
            name: "created_login".into(),
        })
    );

    let mut snapshot = state.snapshot.clone().expect("snapshot");
    snapshot.logins.push(fluxdb_core::SqlServerLogin {
        name: "created_login".into(),
        login_type: "SQL_LOGIN".into(),
        is_disabled: false,
        default_database: Some("fluxdb_demo".into()),
        default_language: None,
        create_date: None,
    });
    controller.dispatch(AppCommand::FinishSqlServerSecurityLoad {
        tab_id,
        result: Ok(snapshot),
    });
    let state = controller
        .user_admin_state(tab_id)
        .expect("admin")
        .sqlserver
        .as_ref()
        .expect("state");
    assert!(
        state
            .snapshot
            .as_ref()
            .expect("snapshot")
            .contains_principal(&SqlServerPrincipalRef {
                kind: SqlServerPrincipalKind::Login,
                name: "created_login".into(),
            })
    );
    assert_eq!(
        state.selected,
        Some(SqlServerPrincipalRef {
            kind: SqlServerPrincipalKind::Login,
            name: "created_login".into(),
        })
    );
    assert_eq!(state.form.kind, SqlServerSecurityFormKind::EditLogin);
    assert_eq!(state.form.name, "created_login");
}

#[test]
fn mysql_create_apply_marks_created_user_for_the_following_refresh() {
    let mut admin = UserAdminState::new(
        ConnectionId(61),
        Some("fluxdb_demo".into()),
        PrivilegeScope::MySql,
    );
    admin.users = vec![DatabaseUserIdentity {
        user: "existing".into(),
        host: "%".into(),
        plugin: Some("caching_sha2_password".into()),
    }];
    admin.selected_user = admin.users.first().cloned();
    admin.creating_user = true;
    admin.create_user = "created_user".into();
    admin.create_host = "%".into();
    let mut controller = AppController::new();
    let tab_id = TabId(102);
    controller.state.tabs.push(TabState {
        id: tab_id,
        title: "mysql users".into(),
        kind: TabKind::UserAdmin(admin),
        dirty: false,
    });
    controller.dispatch(AppCommand::FinishUserAdminSqlApply {
        tab_id,
        result: Ok(()),
    });
    let admin = controller
        .user_admin_state(tab_id)
        .expect("mysql admin");
    assert_eq!(
        admin.pending_selected_user,
        Some(DatabaseUserIdentity {
            user: "created_user".into(),
            host: "%".into(),
            plugin: Some("caching_sha2_password".into()),
        })
    );
    let refreshed = vec![
        DatabaseUserIdentity {
            user: "existing".into(),
            host: "%".into(),
            plugin: Some("caching_sha2_password".into()),
        },
        DatabaseUserIdentity {
            user: "created_user".into(),
            host: "%".into(),
            plugin: Some("caching_sha2_password".into()),
        },
    ];
    controller.dispatch(AppCommand::FinishUserAdminUsersLoad {
        tab_id,
        result: Ok(refreshed),
    });
    let admin = controller
        .user_admin_state(tab_id)
        .expect("mysql admin");
    assert_eq!(
        admin.selected_user.as_ref().map(|user| user.user.as_str()),
        Some("created_user")
    );
    assert!(admin.pending_selected_user.is_none());
}

#[test]
fn postgres_apply_marks_final_role_for_the_following_refresh() {
    let mut admin = UserAdminState::new(
        ConnectionId(62),
        Some("fluxdb_demo".into()),
        PrivilegeScope::Postgres,
    );
    admin.pg_roles = vec![pg_role("existing")];
    admin.pg_selected_role = Some("existing".into());
    let mut controller = AppController::new();
    let tab_id = TabId(103);
    controller.state.tabs.push(TabState {
        id: tab_id,
        title: "pg roles".into(),
        kind: TabKind::UserAdmin(admin),
        dirty: false,
    });
    controller.dispatch(AppCommand::FinishPgRolePlanApply {
        tab_id,
        plan: fluxdb_core::PgRoleSavePlan {
            database: None,
            role_name: "created_role".into(),
            changes: Vec::new(),
        },
        result: Ok(Vec::new()),
    });
    let admin = controller.user_admin_state(tab_id).expect("pg admin");
    assert_eq!(
        admin.pending_selected_pg_role.as_deref(),
        Some("created_role")
    );
    controller.dispatch(AppCommand::FinishUserAdminPgRolesLoad {
        tab_id,
        result: Ok(vec![
            pg_role("existing"),
            pg_role("created_role"),
        ]),
    });
    let admin = controller.user_admin_state(tab_id).expect("pg admin");
    assert_eq!(admin.pg_selected_role.as_deref(), Some("created_role"));
    assert!(admin.pending_selected_pg_role.is_none());
}

fn pg_role(name: &str) -> fluxdb_core::PgRole {
    fluxdb_core::PgRole {
        name: name.into(),
        can_login: false,
        is_superuser: false,
        can_create_db: false,
        can_create_role: false,
        inherit: true,
        is_replication: false,
        bypass_rls: false,
        connection_limit: -1,
        valid_until: None,
        comment: None,
    }
}

fn state() -> SqlServerSecurityState {
    SqlServerSecurityState {
        database: "fluxdb_demo".into(),
        databases: vec!["fluxdb_demo".into(), "master".into()],
        snapshot: Some(SqlServerSecuritySnapshot {
            schemas: vec!["app".into(), "dbo".into()],
            database: "fluxdb_demo".into(),
            current_login: "sa".into(),
            current_user: "dbo".into(),
            logins: Vec::new(),
            users: Vec::new(),
            server_roles: Vec::new(),
            database_roles: Vec::new(),
            memberships: vec![SqlServerRoleMembership {
                scope: SqlServerRoleScope::Database,
                role: "db_datareader".into(),
                member: "app_user".into(),
            }],
        }),
        ..SqlServerSecurityState::default()
    }
}

#[test]
fn switching_database_clears_old_security_context_and_preserves_candidates() {
    let mut state = state();
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::User,
        name: "old_user".into(),
    });
    state.permissions = vec![SqlServerPermission {
        target: SqlServerPermissionTarget::Database,
        permission: "CONNECT".into(),
        state: SqlServerPermissionState::Grant,
        grantor: None,
    }];

    state
        .select_database("master".into())
        .expect("switch database");

    assert_eq!(state.database, "master");
    assert!(state.snapshot.is_none());
    assert!(state.selected.is_none());
    assert!(state.permissions.is_empty());
    assert_eq!(state.databases, ["fluxdb_demo", "master"]);
}

#[test]
fn database_switch_rejects_dirty_busy_or_unknown_targets_without_losing_drafts() {
    for reason in [
        "draft",
        "form",
        "load",
        "permissions",
        "preview",
        "apply",
        "verify",
        "unknown",
    ] {
        let mut state = state();
        match reason {
            "draft" => state.queue_change(SqlServerSecurityChange::CreateDatabaseRole {
                name: "draft_role".into(),
            }),
            "form" => {
                state.form = SqlServerSecurityForm::open(SqlServerSecurityFormKind::CreateUser);
                state.form.name = "draft_user".into();
            }
            "load" => state.loading = true,
            "permissions" => state.loading_permissions = true,
            "preview" => state.preview_loading = true,
            "apply" => state.applying = true,
            "verify" => state.save_status = SqlServerSecuritySaveStatus::NeedsVerify,
            _ => {}
        }
        let before = state.clone();
        let target = if reason == "unknown" {
            "missing"
        } else {
            "master"
        };
        let error = state.select_database(target.into()).expect_err(reason);
        assert_eq!(error.kind, ErrorKind::Query, "{reason}");
        assert_eq!(state, before, "{reason}");
    }
}

#[test]
fn role_toggle_restores_baseline_without_redundant_sql() {
    let mut state = state();

    state.queue_role_toggle(SqlServerRoleScope::Database, "db_datareader", "app_user");
    assert!(!state.effective_role_membership(
        SqlServerRoleScope::Database,
        "db_datareader",
        "app_user"
    ));
    assert!(matches!(
        state.pending_changes.as_slice(),
        [SqlServerSecurityChange::DropRoleMember { .. }]
    ));

    state.queue_role_toggle(SqlServerRoleScope::Database, "db_datareader", "app_user");
    assert!(state.effective_role_membership(
        SqlServerRoleScope::Database,
        "db_datareader",
        "app_user"
    ));
    assert!(state.pending_changes.is_empty());
}

#[test]
fn repeated_field_edits_keep_only_the_latest_change() {
    let mut state = state();
    state.queue_change(SqlServerSecurityChange::SetLoginEnabled {
        name: "demo_login".into(),
        enabled: false,
    });
    state.queue_change(SqlServerSecurityChange::SetLoginEnabled {
        name: "demo_login".into(),
        enabled: true,
    });

    assert!(matches!(
        state.pending_changes.as_slice(),
        [SqlServerSecurityChange::SetLoginEnabled { enabled: true, .. }]
    ));
}

#[test]
fn only_sql_login_mapped_nonsystem_users_are_editable() {
    let mut state = state();
    state
        .snapshot
        .as_mut()
        .expect("snapshot")
        .logins
        .push(fluxdb_core::SqlServerLogin {
            name: "sql_login".into(),
            login_type: "SQL_LOGIN".into(),
            is_disabled: false,
            default_database: None,
            default_language: None,
            create_date: None,
        });
    state.snapshot.as_mut().expect("snapshot").users.extend([
        fluxdb_core::SqlServerDatabaseUser {
            name: "app_user".into(),
            user_type: "SQL_USER".into(),
            login_name: Some("sql_login".into()),
            default_schema: Some("dbo".into()),
            create_date: None,
        },
        fluxdb_core::SqlServerDatabaseUser {
            name: "dbo".into(),
            user_type: "SQL_USER".into(),
            login_name: Some("sql_login".into()),
            default_schema: Some("dbo".into()),
            create_date: None,
        },
    ]);

    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::User,
        name: "app_user".into(),
    });
    assert!(state.selected_user_is_editable());
    state.selected = Some(SqlServerPrincipalRef {
        kind: SqlServerPrincipalKind::User,
        name: "dbo".into(),
    });
    assert!(!state.selected_user_is_editable());
}
