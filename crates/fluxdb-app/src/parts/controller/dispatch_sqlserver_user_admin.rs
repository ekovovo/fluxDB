impl AppController {
    // SQL Server 主体、权限计划与确认操作独立成帧，避免权限草稿与计划占用无关命令的栈空间。
    fn dispatch_sqlserver_user_admin_command(&mut self, command: AppCommand) -> AppEvent {
        match command {
            AppCommand::LoadSqlServerSecurityDatabases(tab_id) => {
                AppEvent::SqlServerSecurityDatabasesLoaded(
                    tab_id,
                    self.load_sqlserver_security_databases(tab_id)
                        .map_err(UserFacingError::from),
                )
            }
            AppCommand::FinishSqlServerSecurityDatabases { tab_id, result } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.loading = false;
                    match result {
                        Ok(databases) => {
                            state.databases = databases;
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error),
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SelectSqlServerSecurityDatabase { tab_id, database } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    match state.select_database(database) {
                        Ok(()) => AppEvent::TabActivated(tab_id),
                        Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                    }
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::StartSqlServerSecurityLoad(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.loading = true;
                    state.error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::LoadSqlServerSecurity(tab_id) => match self.load_sqlserver_security(tab_id)
            {
                Ok(snapshot) => AppEvent::UserAdminSqlServerLoaded(tab_id, Ok(snapshot)),
                Err(error) => {
                    AppEvent::UserAdminSqlServerLoaded(tab_id, Err(UserFacingError::from(error)))
                }
            },
            AppCommand::FinishSqlServerSecurityLoad { tab_id, result } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.loading = false;
                    match result {
                        Ok(snapshot) => {
                            if snapshot.database != state.database {
                                return AppEvent::TabActivated(tab_id);
                            }
                            let previous = state.selected.clone();
                            let current_login = (!snapshot.current_login.is_empty()).then(|| {
                                fluxdb_core::SqlServerPrincipalRef {
                                    kind: SqlServerPrincipalKind::Login,
                                    name: snapshot.current_login.clone(),
                                }
                            });
                            state.snapshot = Some(snapshot);
                            state.error = None;
                            state.pending_changes.clear();
                            state.preview = None;
                            state.preview_error = None;
                            state.apply_error = None;
                            state.form.clear();
                            state.pending_delete = None;
                            state.save_status = SqlServerSecuritySaveStatus::Idle;
                            state.selected = previous
                                .filter(|principal| {
                                    state.snapshot.as_ref().is_some_and(|snapshot| {
                                        snapshot.contains_principal(principal)
                                    })
                                })
                                .or(current_login)
                                .or_else(|| {
                                    state.snapshot.as_ref().and_then(|snapshot| {
                                        snapshot.logins.first().map(|login| {
                                            fluxdb_core::SqlServerPrincipalRef {
                                                kind: SqlServerPrincipalKind::Login,
                                                name: login.name.clone(),
                                            }
                                        })
                                    })
                                });
                            state.permissions.clear();
                            state.permissions_loaded_for = None;
                            state.edit_selected();
                        }
                        Err(error) => state.error = Some(error),
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::StartSqlServerPermissionsLoad { tab_id, principal } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.loading_permissions = true;
                    state.permissions_error = None;
                    state.permissions.clear();
                    state.permissions_loaded_for = None;
                    if state.selected.as_ref() != Some(&principal) {
                        state.selected = Some(principal.clone());
                    }
                    if state.pending_creations().contains(&principal) {
                        state.loading_permissions = false;
                        state.permissions_loaded_for = Some(principal);
                        return AppEvent::TabActivated(tab_id);
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::LoadSqlServerPermissions { tab_id, principal } => {
                match self.load_sqlserver_permissions(tab_id, &principal) {
                    Ok(permissions) => AppEvent::UserAdminSqlServerPermissionsLoaded {
                        tab_id,
                        principal,
                        result: Ok(permissions),
                    },
                    Err(error) => AppEvent::UserAdminSqlServerPermissionsLoaded {
                        tab_id,
                        principal,
                        result: Err(UserFacingError::from(error)),
                    },
                }
            }
            AppCommand::FinishSqlServerPermissionsLoad {
                tab_id,
                principal,
                result,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.loading_permissions = false;
                    if state.selected.as_ref() == Some(&principal) {
                        match result {
                            Ok(permissions) => {
                                state.permissions = permissions;
                                state.permissions_loaded_for = Some(principal);
                                state.permissions_error = None;
                            }
                            Err(error) => {
                                state.permissions.clear();
                                state.permissions_loaded_for = None;
                                state.permissions_error = Some(error);
                            }
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SelectSqlServerPrincipal { tab_id, principal } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    if state.selected.as_ref() == Some(&principal) {
                        return AppEvent::TabActivated(tab_id);
                    }
                    if state.has_form_changes()
                        || state.applying
                        || state.save_status == SqlServerSecuritySaveStatus::NeedsVerify
                    {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Query,
                            "请先保存或取消当前 SQL Server 安全变更",
                        )));
                    }
                    state.selected = Some(principal);
                    state.permissions.clear();
                    state.permissions_loaded_for = None;
                    state.permissions_error = None;
                    state.edit_selected();
                    state.preview = None;
                    state.preview_error = None;
                    admin.active_detail_tab = admin.active_detail_tab.general();
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetSqlServerSecurityFilter { tab_id, filter } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.list_filter = filter;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::OpenSqlServerSecurityForm { tab_id, kind } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    if state.has_form_changes()
                        || state.applying
                        || state.save_status == SqlServerSecuritySaveStatus::NeedsVerify
                    {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Query,
                            "请先保存或取消当前 SQL Server 安全变更",
                        )));
                    }
                    let grant_on_login = kind == SqlServerSecurityFormKind::GrantPermission
                        && state.selected_kind() == Some(SqlServerPrincipalKind::Login);
                    let form = if kind == SqlServerSecurityFormKind::GrantPermission {
                        &mut state.permission_form
                    } else {
                        &mut state.form
                    };
                    *form = SqlServerSecurityForm::open(kind);
                    match kind {
                        SqlServerSecurityFormKind::CreateLogin => {
                            form.default_value = state.database.clone()
                        }
                        SqlServerSecurityFormKind::CreateUser => form.default_value = "dbo".into(),
                        SqlServerSecurityFormKind::GrantPermission if grant_on_login => {
                            form.permission_scope = "SERVER".into()
                        }
                        _ => {}
                    }
                    if matches!(
                        kind,
                        SqlServerSecurityFormKind::CreateLogin
                            | SqlServerSecurityFormKind::CreateUser
                            | SqlServerSecurityFormKind::CreateRole
                    ) {
                        state.selected = None;
                        admin.active_detail_tab = admin.active_detail_tab.general();
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::CloseSqlServerPermissionComposer(tab_id) => {
                if let Some(state) = self
                    .user_admin_state_mut(tab_id)
                    .and_then(|admin| admin.sqlserver.as_mut())
                {
                    if state.permission_form.kind == SqlServerSecurityFormKind::GrantPermission {
                        state.permission_form.clear();
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetSqlServerSecurityFormField {
                tab_id,
                field,
                value,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    let form = if matches!(
                        field,
                        SqlServerSecurityFormField::PermissionScope
                            | SqlServerSecurityFormField::PermissionTarget
                            | SqlServerSecurityFormField::Permission
                            | SqlServerSecurityFormField::PermissionState
                    ) {
                        &mut state.permission_form
                    } else {
                        &mut state.form
                    };
                    if form.set_field(field, value) {
                        state.invalidate_preview();
                        if field == SqlServerSecurityFormField::PermissionScope {
                            state.permission_form.permission.clear();
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetSqlServerSecurityFormFlag {
                tab_id,
                flag,
                value,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    if state.permission_form.set_flag(flag, value) {
                        state.invalidate_preview();
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::DiscardSqlServerSecurityChanges(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    match state.discard_changes() {
                        Ok(()) => AppEvent::TabActivated(tab_id),
                        Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                    }
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SubmitSqlServerSecurityForm(tab_id) => {
                match self.submit_sqlserver_security_form(tab_id) {
                    Ok(()) => AppEvent::TabActivated(tab_id),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::ToggleSqlServerRoleMembership {
                tab_id,
                scope,
                role,
                member,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.queue_role_toggle(scope, &role, &member);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetSqlServerSelectedLoginEnabled { tab_id, enabled } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                    && let Some(principal) = state.selected.clone()
                    && principal.kind == SqlServerPrincipalKind::Login
                    && state
                        .selected_login()
                        .is_some_and(|login| login.is_sql_login())
                {
                    state.queue_change(fluxdb_core::SqlServerSecurityChange::SetLoginEnabled {
                        name: principal.name,
                        enabled,
                    });
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "SQL Server 登录名不存在"))
                }
            }
            AppCommand::RevokeSqlServerPermission {
                tab_id,
                permission_index,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                    && let Some(permission) =
                        state.effective_permissions().get(permission_index).cloned()
                {
                    state.queue_permission_revoke(&permission);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "SQL Server 权限不存在"))
                }
            }
            AppCommand::StartSqlServerSecurityPreview(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    if state.pending_changes.is_empty() {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Query,
                            "没有待预览的 SQL Server 安全变更",
                        )));
                    }
                    state.preview_loading = true;
                    state.preview = None;
                    state.preview_error = None;
                    admin.active_detail_tab =
                        UserAdminDetailTab::SqlServer(SqlServerUserAdminTab::SqlPreview);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::LoadSqlServerSecurityPreview(tab_id) => {
                match self.preview_sqlserver_security_plan(tab_id) {
                    Ok(statements) => AppEvent::UserAdminSqlServerPreview {
                        tab_id,
                        result: Ok(statements),
                    },
                    Err(error) => AppEvent::UserAdminSqlServerPreview {
                        tab_id,
                        result: Err(UserFacingError::from(error)),
                    },
                }
            }
            AppCommand::FinishSqlServerSecurityPreview {
                tab_id,
                revision,
                result,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.preview_loading = false;
                    if state.revision != revision {
                        // 草稿已变，这批预览作废；「保存」的弹框意图也一并作废。
                        admin.pending_sql_awaits_preview = false;
                        return AppEvent::TabActivated(tab_id);
                    }
                    let confirm = std::mem::take(&mut admin.pending_sql_awaits_preview);
                    match result {
                        Ok(statements) => {
                            state.preview = Some(statements);
                            state.preview_error = None;
                            if confirm {
                                // 「保存」触发的预览：直接放进确认框。
                                // 连接器返回的是脱敏副本，这里按预览页同一套规则还原口令，
                                // 保证「确认框看到的」与「预览页看到的」逐字一致。
                                let shown = state.preview_for_display();
                                let danger = shown.iter().any(|sql| sql.starts_with("DROP "));
                                admin.pending_sql = Some(UserAdminPendingSql {
                                    sql: shown.join("\n"),
                                    danger,
                                    kind: UserAdminPendingKind::SqlServer,
                                });
                                admin.apply_error = None;
                            }
                        }
                        Err(error) => state.preview_error = Some(error),
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::StartSqlServerSecurityApply(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    if state.save_status == SqlServerSecuritySaveStatus::NeedsVerify
                        || !state.has_pending_changes()
                    {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Query,
                            "请先预览或刷新核实 SQL Server 安全变更",
                        )));
                    }
                    if state.applying {
                        return AppEvent::TabActivated(tab_id);
                    }
                    state.applying = true;
                    state.apply_error = None;
                    state.save_status = SqlServerSecuritySaveStatus::Saving;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::ApplySqlServerSecurityPlan(tab_id) => {
                match self.apply_sqlserver_security_plan(tab_id) {
                    Ok((principal, statements)) => AppEvent::UserAdminSqlServerApplied {
                        tab_id,
                        principal,
                        result: Ok(statements),
                    },
                    Err(error) => AppEvent::UserAdminSqlServerApplied {
                        tab_id,
                        principal: String::new(),
                        result: Err(UserFacingError::from(error)),
                    },
                }
            }
            AppCommand::FinishSqlServerSecurityApply {
                tab_id,
                principal,
                result,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.applying = false;
                    admin.pending_sql = None;
                    match result {
                        Ok(_) => {
                            state.save_status = SqlServerSecuritySaveStatus::Idle;
                            state.pending_changes.clear();
                            state.form.clear();
                            state.permission_form.clear();
                            state.preview = None;
                            state.preview_error = None;
                            state.apply_error = None;
                            state.permissions.clear();
                            state.permissions_loaded_for = None;
                            if !principal.is_empty() {
                                state.selected = Some(fluxdb_core::SqlServerPrincipalRef {
                                    kind: state
                                        .selected
                                        .as_ref()
                                        .map(|principal| principal.kind)
                                        .unwrap_or(SqlServerPrincipalKind::Login),
                                    name: principal,
                                });
                            }
                        }
                        Err(error) => {
                            // 可重试标记在这里仅由传输/取消/超时错误产生；普通服务端拒绝是确定性失败。
                            state.save_status = if error.retryable {
                                SqlServerSecuritySaveStatus::NeedsVerify
                            } else {
                                SqlServerSecuritySaveStatus::Idle
                            };
                            state.preview = None;
                            if state.save_status == SqlServerSecuritySaveStatus::Idle {
                                state.restore_failed_form();
                                admin.active_detail_tab = admin.active_detail_tab.general();
                            }
                            state.apply_error = Some(error);
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::BeginDeleteSqlServerPrincipal(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    let Some(principal) = state.selected.clone() else {
                        return AppEvent::TabActivated(tab_id);
                    };
                    let snapshot = state.snapshot.as_ref();
                    let protected = match principal.kind {
                        SqlServerPrincipalKind::Login => {
                            principal.name.eq_ignore_ascii_case("sa")
                                || principal.name.starts_with("##")
                                || state
                                    .selected_login()
                                    .is_some_and(|login| !login.is_sql_login())
                                || snapshot.is_some_and(|snapshot| {
                                    principal.name.eq_ignore_ascii_case(&snapshot.current_login)
                                })
                        }
                        SqlServerPrincipalKind::User => {
                            snapshot.is_some_and(|snapshot| {
                                principal.name.eq_ignore_ascii_case(&snapshot.current_user)
                            }) || !state.selected_user_is_editable()
                        }
                        SqlServerPrincipalKind::Role => {
                            state.selected_role().is_some_and(|role| role.is_fixed)
                        }
                    };
                    if protected {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Permission,
                            "该 SQL Server 主体受系统保护，不能删除",
                        )));
                    }
                    state.pending_delete = Some(principal);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::ConfirmDeleteSqlServerPrincipal(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                    && let Some(principal) = state.pending_delete.take()
                {
                    state.queue_change(match principal.kind {
                        SqlServerPrincipalKind::Login => {
                            fluxdb_core::SqlServerSecurityChange::DropLogin {
                                name: principal.name,
                            }
                        }
                        SqlServerPrincipalKind::User => {
                            fluxdb_core::SqlServerSecurityChange::DropUser {
                                name: principal.name,
                            }
                        }
                        SqlServerPrincipalKind::Role => {
                            fluxdb_core::SqlServerSecurityChange::DropDatabaseRole {
                                name: principal.name,
                            }
                        }
                    });
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "SQL Server 删除目标不存在"))
                }
            }
            AppCommand::CancelDeleteSqlServerPrincipal(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(state) = admin.sqlserver.as_mut()
                {
                    state.pending_delete = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "SQL Server 用户与权限标签页不存在",
                    ))
                }
            }
            // 私有入口仅由 dispatch 的对应变体分组调用。
            _ => unreachable!("仅SQL Server 主体、权限计划与确认操作命令进入此分发器"),
        }
    }
}
