impl AppController {
    fn load_sqlserver_security_databases(&self, tab_id: TabId) -> fluxdb_core::Result<Vec<String>> {
        let admin = self
            .user_admin_state(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let config = self
            .connection_config(admin.connection_id)
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        let mut databases = list_objects_for_connection(config, None)?
            .into_iter()
            .filter(|object| object.path.kind == ObjectKind::Database)
            .map(|object| object.path.name)
            .collect::<Vec<_>>();
        databases.sort();
        databases.dedup();
        Ok(databases)
    }

    fn sqlserver_security_database(
        &self,
        tab_id: TabId,
    ) -> fluxdb_core::Result<(ConnectionId, String)> {
        let admin = self
            .user_admin_state(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let state = admin
            .sqlserver
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Internal, "SQL Server 安全状态不存在"))?;
        let database = state.database.trim();
        if database.is_empty() {
            return Err(Error::new(
                ErrorKind::Query,
                "SQL Server 用户与权限需要先选择目标数据库",
            ));
        }
        let database = database.to_string();
        Ok((admin.connection_id, database))
    }

    fn load_sqlserver_security(
        &self,
        tab_id: TabId,
    ) -> fluxdb_core::Result<fluxdb_core::SqlServerSecuritySnapshot> {
        let (connection_id, database) = self.sqlserver_security_database(tab_id)?;
        let config = self
            .connection_config(connection_id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        role_operation_for_connection(&config, |connector| {
            connector.sqlserver_security_snapshot(connection_id, &database)
        })
    }

    fn load_sqlserver_permissions(
        &self,
        tab_id: TabId,
        principal: &fluxdb_core::SqlServerPrincipalRef,
    ) -> fluxdb_core::Result<Vec<fluxdb_core::SqlServerPermission>> {
        let (connection_id, database) = self.sqlserver_security_database(tab_id)?;
        let config = self
            .connection_config(connection_id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        role_operation_for_connection(&config, |connector| {
            connector.sqlserver_principal_permissions(connection_id, &database, principal)
        })
    }

    fn preview_sqlserver_security_plan(&self, tab_id: TabId) -> fluxdb_core::Result<Vec<String>> {
        let admin = self
            .user_admin_state(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let state = admin
            .sqlserver
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Internal, "SQL Server 安全状态不存在"))?;
        let plan = state.build_plan()?;
        let config = self
            .connection_config(admin.connection_id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        role_operation_for_connection(&config, |connector| {
            connector.render_sqlserver_security_plan(admin.connection_id, &plan)
        })
    }

    fn apply_sqlserver_security_plan(
        &self,
        tab_id: TabId,
    ) -> fluxdb_core::Result<(String, Vec<String>)> {
        let admin = self
            .user_admin_state(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let state = admin
            .sqlserver
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Internal, "SQL Server 安全状态不存在"))?;
        if state.save_status == SqlServerSecuritySaveStatus::NeedsVerify {
            return Err(Error::new(
                ErrorKind::Permission,
                "请先刷新核实 SQL Server 提交结果",
            ));
        }
        if !state.has_pending_changes()
            || state.has_form_changes()
            || state.loading
            || state.loading_permissions
            || state.preview_loading
        {
            return Err(Error::new(
                ErrorKind::Query,
                "请先预览或刷新核实 SQL Server 安全变更",
            ));
        }
        let plan = state.build_plan()?;
        let config = self
            .connection_config(admin.connection_id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        let statements = role_operation_for_connection(&config, |connector| {
            connector.apply_sqlserver_security_plan(admin.connection_id, &plan)
        })?;
        Ok((plan.principal, statements))
    }

    fn submit_sqlserver_security_form(&mut self, tab_id: TabId) -> fluxdb_core::Result<()> {
        let admin = self
            .user_admin_state_mut(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let state = admin
            .sqlserver
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::Internal, "SQL Server 安全状态不存在"))?;
        let form = if state.permission_form.kind == SqlServerSecurityFormKind::GrantPermission {
            state.permission_form.clone()
        } else {
            state.form.clone()
        };
        let selected = state.selected.clone();
        let retry_original = matches!(
            form.kind,
            SqlServerSecurityFormKind::CreateLogin
                | SqlServerSecurityFormKind::CreateUser
                | SqlServerSecurityFormKind::CreateRole
        )
        .then(|| state.failed_retry_original(form.name.trim()))
        .flatten();
        if form.password.chars().count() > 128 {
            return Err(Error::new(
                ErrorKind::Query,
                "SQL Server 登录密码不能超过 128 个字符",
            ));
        }
        match form.kind {
            SqlServerSecurityFormKind::None => {
                return Err(Error::new(ErrorKind::Query, "没有待提交的 SQL Server 表单"));
            }
            SqlServerSecurityFormKind::CreateLogin => {
                fluxdb_core::validate_sqlserver_principal_name(&form.name)?;
                if form.password.is_empty() || form.password != form.confirm_password {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "SQL Server 登录密码不能为空且两次输入必须一致",
                    ));
                }
                let name = form.name.trim().to_string();
                if let Some(original) = retry_original.as_ref()
                    && !original.name.eq_ignore_ascii_case(&name)
                {
                    state.replace_creation_identity(
                        original,
                        &SqlServerPrincipalRef {
                            kind: SqlServerPrincipalKind::Login,
                            name: name.clone(),
                        },
                    );
                }
                state.queue_change(fluxdb_core::SqlServerSecurityChange::CreateLogin {
                    name,
                    password: form.password.clone(),
                    default_database: non_empty(form.default_value.clone()),
                    disabled: false,
                });
            }
            SqlServerSecurityFormKind::CreateUser => {
                fluxdb_core::validate_sqlserver_principal_name(&form.name)?;
                let login = non_empty(form.login.clone()).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Query,
                        "SQL Server 数据库用户必须映射到 SQL 登录名",
                    )
                })?;
                let login_is_sql = state.is_known_sql_login(&login);
                if !login_is_sql {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "第一阶段只能把数据库用户映射到已有 SQL 认证登录名",
                    ));
                }
                let name = form.name.trim().to_string();
                if let Some(original) = retry_original.as_ref()
                    && !original.name.eq_ignore_ascii_case(&name)
                {
                    state.replace_creation_identity(
                        original,
                        &SqlServerPrincipalRef {
                            kind: SqlServerPrincipalKind::User,
                            name: name.clone(),
                        },
                    );
                }
                state.queue_change(fluxdb_core::SqlServerSecurityChange::CreateUser {
                    name,
                    login: Some(login),
                    default_schema: non_empty(form.default_value.clone()),
                });
            }
            SqlServerSecurityFormKind::CreateRole => {
                fluxdb_core::validate_sqlserver_principal_name(&form.name)?;
                let name = form.name.trim().to_string();
                if let Some(original) = retry_original.as_ref()
                    && !original.name.eq_ignore_ascii_case(&name)
                {
                    state.replace_creation_identity(
                        original,
                        &SqlServerPrincipalRef {
                            kind: SqlServerPrincipalKind::Role,
                            name: name.clone(),
                        },
                    );
                }
                state.queue_change(fluxdb_core::SqlServerSecurityChange::CreateDatabaseRole {
                    name,
                });
            }
            SqlServerSecurityFormKind::EditLogin => {
                let login = state
                    .selected_login()
                    .filter(|login| login.is_sql_login())
                    .ok_or_else(|| {
                        Error::new(ErrorKind::Permission, "第一阶段只能编辑 SQL 认证登录名")
                    })?;
                if form.password != form.confirm_password {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "SQL Server 登录密码不能为空且两次输入必须一致",
                    ));
                }
                let mut changes = Vec::new();
                if !form.password.is_empty() {
                    changes.push(SqlServerSecurityChange::SetLoginPassword {
                        name: login.name.clone(),
                        password: form.password.clone(),
                    });
                }
                let current = state
                    .effective_login_default_database(
                        &login.name,
                        login.default_database.as_deref(),
                    )
                    .unwrap_or("");
                if form.default_value != current {
                    if !state.databases.contains(&form.default_value) {
                        return Err(Error::new(
                            ErrorKind::Query,
                            "请选择列表中的 SQL Server 数据库",
                        ));
                    }
                    changes.push(SqlServerSecurityChange::SetLoginDefaultDatabase {
                        name: login.name.clone(),
                        database: Some(form.default_value.clone()),
                    });
                }
                for change in changes {
                    state.queue_change(change);
                }
            }
            SqlServerSecurityFormKind::EditUser => {
                if !state.selected_user_is_editable() {
                    return Err(Error::new(
                        ErrorKind::Permission,
                        "该数据库用户不是第一阶段可编辑的 SQL 登录映射用户",
                    ));
                }
                let user = state.selected_user().ok_or_else(|| {
                    Error::new(ErrorKind::Query, "请先选择一个 SQL Server 数据库用户")
                })?;
                let mut changes = Vec::new();
                if form.login
                    != state
                        .effective_user_login(&user.name, user.login_name.as_deref())
                        .unwrap_or("")
                {
                    if !state.is_known_sql_login(&form.login) {
                        return Err(Error::new(
                            ErrorKind::Query,
                            "第一阶段只能把数据库用户映射到已有 SQL 认证登录名",
                        ));
                    }
                    changes.push(SqlServerSecurityChange::SetUserLogin {
                        name: user.name.clone(),
                        login: Some(form.login.clone()),
                    });
                }
                if form.default_value
                    != state
                        .effective_user_default_schema(&user.name, user.default_schema.as_deref())
                        .unwrap_or("")
                {
                    if !state
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.schemas.contains(&form.default_value))
                    {
                        return Err(Error::new(
                            ErrorKind::Query,
                            "请选择列表中的 SQL Server Schema",
                        ));
                    }
                    changes.push(SqlServerSecurityChange::SetUserDefaultSchema {
                        name: user.name.clone(),
                        schema: Some(form.default_value.clone()),
                    });
                }
                for change in changes {
                    state.queue_change(change);
                }
            }
            SqlServerSecurityFormKind::GrantPermission => {
                let principal = selected.ok_or_else(|| {
                    Error::new(ErrorKind::Query, "请先选择 SQL Server 登录名、用户或角色")
                })?;
                let target = sqlserver_permission_target_from_form(&form, principal.kind)?;
                let permission = form.permission.trim().to_ascii_uppercase();
                fluxdb_core::validate_sqlserver_permission(&target, &permission)?;
                let state_value = match form.permission_state.trim().to_ascii_uppercase().as_str() {
                    "DENY" => fluxdb_core::SqlServerPermissionState::Deny,
                    _ => fluxdb_core::SqlServerPermissionState::Grant,
                };
                let existing = state.permissions.iter().find(|existing| {
                    sqlserver_permission_targets_equal(&existing.target, &target)
                        && existing.permission.eq_ignore_ascii_case(&permission)
                });
                if existing.is_some_and(|existing| {
                    matches!(
                        (existing.state, state_value, form.grant_option),
                        (
                            fluxdb_core::SqlServerPermissionState::Deny,
                            fluxdb_core::SqlServerPermissionState::Deny,
                            _
                        ) | (
                            fluxdb_core::SqlServerPermissionState::Grant,
                            fluxdb_core::SqlServerPermissionState::Grant,
                            false
                        ) | (
                            fluxdb_core::SqlServerPermissionState::GrantWithGrantOption,
                            fluxdb_core::SqlServerPermissionState::Grant,
                            true
                        )
                    )
                }) {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "该 SQL Server 显式权限没有变化",
                    ));
                }
                if existing.is_some_and(|existing| {
                    existing.state == fluxdb_core::SqlServerPermissionState::GrantWithGrantOption
                        && state_value == fluxdb_core::SqlServerPermissionState::Grant
                        && !form.grant_option
                }) {
                    state.queue_change(fluxdb_core::SqlServerSecurityChange::RevokeGrantOption {
                        grantee: principal.name,
                        target,
                        permission,
                        cascade: true,
                    });
                } else {
                    let replace_existing = existing.is_some_and(|existing| {
                        existing.state != state_value
                            && !(existing.state == fluxdb_core::SqlServerPermissionState::Grant
                                && state_value == fluxdb_core::SqlServerPermissionState::Grant)
                    });
                    state.queue_change(fluxdb_core::SqlServerSecurityChange::GrantPermission {
                        grantee: principal.name,
                        target,
                        permission,
                        state: state_value,
                        grant_option: state_value != fluxdb_core::SqlServerPermissionState::Deny
                            && form.grant_option,
                        replace_existing,
                        cascade_revoke: replace_existing
                            && existing.is_some_and(|existing| {
                                existing.state
                                    == fluxdb_core::SqlServerPermissionState::GrantWithGrantOption
                            }),
                    });
                }
                // 权限编辑器保持打开并清空本次选择，便于连续添加；列表立即刷新。
                state.permission_form =
                    SqlServerSecurityForm::open(SqlServerSecurityFormKind::GrantPermission);
                if principal.kind == SqlServerPrincipalKind::Login {
                    state.permission_form.permission_scope = "SERVER".into();
                }
                state.permission_form.grant_option = false;
                return Ok(());
            }
        }
        match form.kind {
            SqlServerSecurityFormKind::CreateLogin
            | SqlServerSecurityFormKind::CreateUser
            | SqlServerSecurityFormKind::CreateRole => {
                state.selected = Some(SqlServerPrincipalRef {
                    kind: match form.kind {
                        SqlServerSecurityFormKind::CreateLogin => SqlServerPrincipalKind::Login,
                        SqlServerSecurityFormKind::CreateUser => SqlServerPrincipalKind::User,
                        _ => SqlServerPrincipalKind::Role,
                    },
                    name: form.name.trim().to_string(),
                });
                // 保留已提交的创建表单，切回常规页时仍能看到刚填写的账号信息。
            }
            _ => state.edit_selected(),
        }
        admin.active_detail_tab = admin.active_detail_tab.general();
        Ok(())
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn sqlserver_permission_target_from_form(
    form: &SqlServerSecurityForm,
    kind: SqlServerPrincipalKind,
) -> fluxdb_core::Result<fluxdb_core::SqlServerPermissionTarget> {
    let scope = form.permission_scope.trim().to_ascii_uppercase();
    let target = form.permission_target.trim();
    match (kind, scope.as_str()) {
        (SqlServerPrincipalKind::Login, "SERVER") => {
            Ok(fluxdb_core::SqlServerPermissionTarget::Server)
        }
        (SqlServerPrincipalKind::Login, _) => Err(Error::new(
            ErrorKind::Query,
            "SQL Server 登录名只能配置服务器级权限",
        )),
        (SqlServerPrincipalKind::User | SqlServerPrincipalKind::Role, "DATABASE") => {
            Ok(fluxdb_core::SqlServerPermissionTarget::Database)
        }
        (SqlServerPrincipalKind::User | SqlServerPrincipalKind::Role, "SCHEMA") => {
            fluxdb_core::validate_sqlserver_principal_name(target)?;
            Ok(fluxdb_core::SqlServerPermissionTarget::Schema {
                schema: target.to_string(),
            })
        }
        (SqlServerPrincipalKind::User | SqlServerPrincipalKind::Role, "OBJECT") => {
            let (schema, object) = target.split_once('.').ok_or_else(|| {
                Error::new(ErrorKind::Query, "对象权限目标必须使用 schema.object 格式")
            })?;
            fluxdb_core::validate_sqlserver_principal_name(schema)?;
            fluxdb_core::validate_sqlserver_principal_name(object)?;
            Ok(fluxdb_core::SqlServerPermissionTarget::Object {
                schema: schema.trim().to_string(),
                object: object.trim().to_string(),
            })
        }
        _ => Err(Error::new(ErrorKind::Query, "SQL Server 权限范围不合法")),
    }
}
