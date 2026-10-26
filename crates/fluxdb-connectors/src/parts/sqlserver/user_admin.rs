use fluxdb_core::{
    SqlServerDatabaseRole, SqlServerDatabaseUser, SqlServerLogin, SqlServerPermission,
    SqlServerPermissionState, SqlServerPermissionTarget, SqlServerPrincipalKind,
    SqlServerPrincipalRef, SqlServerRoleMembership, SqlServerRoleScope, SqlServerSecurityChange,
    SqlServerSecurityPlan, SqlServerSecuritySnapshot, SqlServerServerRole,
};

fn sqlserver_security_snapshot(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
) -> fluxdb_core::Result<SqlServerSecuritySnapshot> {
    if config.id != connection_id {
        return Err(Error::new(
            ErrorKind::Connection,
            "SQL Server 连接上下文不匹配",
        ));
    }
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(
            ErrorKind::Query,
            "SQL Server 安全页缺少数据库上下文",
        ));
    }
    let mut client = sqlserver_connect_database(config, Some(database))?;
    let batch = sqlserver_block_on(async {
        let batch = sqlserver_run_batch(
            &mut client,
            "SELECT SUSER_SNAME() AS current_login, USER_NAME() AS [current_user]; \
             SELECT sp.name, sp.type_desc, sp.is_disabled, sp.default_database_name, \
                    sp.default_language_name, CONVERT(nvarchar(33), sp.create_date, 126) \
             FROM sys.server_principals sp \
             WHERE sp.type IN ('S','U','G') AND sp.name NOT LIKE '##%' \
             ORDER BY sp.name; \
             SELECT dp.name, dp.type_desc, sp.name AS login_name, dp.default_schema_name, \
                    CONVERT(nvarchar(33), dp.create_date, 126) \
             FROM sys.database_principals dp \
             LEFT JOIN sys.server_principals sp ON sp.sid = dp.sid \
             WHERE dp.type IN ('S','U','G','E','X') \
               AND dp.principal_id > 0 \
               AND dp.name NOT IN ('guest','INFORMATION_SCHEMA','sys') \
               AND dp.name NOT LIKE '##%' \
             ORDER BY dp.name; \
             SELECT sp.name, sp.is_fixed_role \
             FROM sys.server_principals sp \
             WHERE sp.type = 'R' AND sp.name NOT LIKE '##%' \
             ORDER BY sp.name; \
             SELECT dp.name, USER_NAME(dp.owning_principal_id) AS owner, dp.is_fixed_role, \
                    CONVERT(nvarchar(33), dp.create_date, 126) \
             FROM sys.database_principals dp \
             WHERE dp.type = 'R' AND dp.name NOT LIKE '##%' \
             ORDER BY dp.name; \
             SELECT 'SERVER' AS scope, r.name AS role_name, m.name AS member_name \
             FROM sys.server_role_members srm \
             JOIN sys.server_principals r ON r.principal_id = srm.role_principal_id \
             JOIN sys.server_principals m ON m.principal_id = srm.member_principal_id \
             WHERE r.name NOT LIKE '##%' \
             UNION ALL \
             SELECT 'DATABASE', r.name, m.name \
             FROM sys.database_role_members drm \
             JOIN sys.database_principals r ON r.principal_id = drm.role_principal_id \
             JOIN sys.database_principals m ON m.principal_id = drm.member_principal_id \
             WHERE r.name NOT LIKE '##%' \
             ORDER BY scope, role_name, member_name; \
             SELECT name FROM sys.schemas WHERE name NOT IN ('guest','INFORMATION_SCHEMA','sys') \
               AND name NOT LIKE 'db[_]%' ORDER BY name",
        )
        .await?;
        sqlserver_close(client).await;
        Ok(batch)
    })?;
    let sets = &batch.result_sets;
    let current = sets
        .first()
        .and_then(|set| set.rows.first())
        .ok_or_else(|| Error::new(ErrorKind::Query, "SQL Server 当前登录信息为空"))?;
    let current_login = current
        .values
        .first()
        .and_then(cell_text)
        .unwrap_or_default();
    let current_user = current
        .values
        .get(1)
        .and_then(cell_text)
        .unwrap_or_default();
    let logins = sets
        .get(1)
        .map(|set| {
            set.rows
                .iter()
                .filter_map(|row| {
                    Some(SqlServerLogin {
                        name: row.values.first().and_then(cell_text)?,
                        login_type: row.values.get(1).and_then(cell_text).unwrap_or_default(),
                        is_disabled: row.values.get(2).and_then(cell_bool).unwrap_or(false),
                        default_database: row.values.get(3).and_then(cell_text),
                        default_language: row.values.get(4).and_then(cell_text),
                        create_date: row.values.get(5).and_then(cell_text),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let users = sets
        .get(2)
        .map(|set| {
            set.rows
                .iter()
                .filter_map(|row| {
                    Some(SqlServerDatabaseUser {
                        name: row.values.first().and_then(cell_text)?,
                        user_type: row.values.get(1).and_then(cell_text).unwrap_or_default(),
                        login_name: row.values.get(2).and_then(cell_text),
                        default_schema: row.values.get(3).and_then(cell_text),
                        create_date: row.values.get(4).and_then(cell_text),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let server_roles = sets
        .get(3)
        .map(|set| {
            set.rows
                .iter()
                .filter_map(|row| {
                    Some(SqlServerServerRole {
                        name: row.values.first().and_then(cell_text)?,
                        is_fixed: row.values.get(1).and_then(cell_bool).unwrap_or(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let database_roles = sets
        .get(4)
        .map(|set| {
            set.rows
                .iter()
                .filter_map(|row| {
                    Some(SqlServerDatabaseRole {
                        name: row.values.first().and_then(cell_text)?,
                        owner: row.values.get(1).and_then(cell_text),
                        is_fixed: row.values.get(2).and_then(cell_bool).unwrap_or(true),
                        create_date: row.values.get(3).and_then(cell_text),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let memberships = sets
        .get(5)
        .map(|set| {
            set.rows
                .iter()
                .filter_map(|row| {
                    let scope = match row.values.first().and_then(cell_text)?.as_str() {
                        "SERVER" => SqlServerRoleScope::Server,
                        "DATABASE" => SqlServerRoleScope::Database,
                        _ => return None,
                    };
                    Some(SqlServerRoleMembership {
                        scope,
                        role: row.values.get(1).and_then(cell_text)?,
                        member: row.values.get(2).and_then(cell_text)?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let schemas = sqlserver_first_rows(
        &SqlServerBatchResult {
            result_sets: batch.result_sets.into_iter().skip(6).collect(),
            truncated: batch.truncated,
        },
        "SQL Server Schema 列表返回类型异常",
    )?
    .iter()
    .filter_map(|row| row.values.first().and_then(cell_text))
    .collect();
    Ok(SqlServerSecuritySnapshot {
        schemas,
        database: database.to_string(),
        current_login,
        current_user,
        logins,
        users,
        server_roles,
        database_roles,
        memberships,
    })
}

fn sqlserver_principal_permissions(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
    principal: &SqlServerPrincipalRef,
) -> fluxdb_core::Result<Vec<SqlServerPermission>> {
    if config.id != connection_id {
        return Err(Error::new(
            ErrorKind::Connection,
            "SQL Server 连接上下文不匹配",
        ));
    }
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(
            ErrorKind::Query,
            "SQL Server 权限页缺少数据库上下文",
        ));
    }
    let mut client = sqlserver_connect_database(config, Some(database))?;
    let batch = sqlserver_block_on(async {
        let batch = match principal.kind {
            SqlServerPrincipalKind::Login => {
                sqlserver_run_query(
                    &mut client,
                    "SELECT p.permission_name, p.state, grantor.name \
                     FROM sys.server_permissions p \
                     JOIN sys.server_principals grantee ON grantee.principal_id = p.grantee_principal_id \
                     LEFT JOIN sys.server_principals grantor ON grantor.principal_id = p.grantor_principal_id \
                     WHERE grantee.name = @P1 \
                     ORDER BY p.permission_name",
                    &[fluxdb_core::SqlParameter::Text(principal.name.clone())],
                )
                .await?
            }
            SqlServerPrincipalKind::User | SqlServerPrincipalKind::Role => {
                sqlserver_run_query(
                    &mut client,
                    "SELECT p.permission_name, p.state, p.class_desc, \
                            COALESCE(obj_schema.name, target_schema.name, '') AS schema_name, \
                            COALESCE(o.name, '') AS object_name, \
                            USER_NAME(p.grantor_principal_id) AS grantor_name \
                     FROM sys.database_permissions p \
                     LEFT JOIN sys.objects o \
                       ON p.class = 1 AND o.object_id = p.major_id \
                     LEFT JOIN sys.schemas obj_schema ON obj_schema.schema_id = o.schema_id \
                     LEFT JOIN sys.schemas target_schema \
                       ON p.class = 3 AND target_schema.schema_id = p.major_id \
                     WHERE p.grantee_principal_id = USER_ID(@P1) \
                       AND p.class IN (0,1,3) \
                       AND (p.class <> 1 OR p.minor_id = 0) \
                       AND (p.class <> 1 OR o.is_ms_shipped = 0) \
                     ORDER BY p.permission_name, p.class_desc",
                    &[fluxdb_core::SqlParameter::Text(principal.name.clone())],
                )
                .await?
            }
        };
        sqlserver_close(client).await;
        Ok(batch)
    })?;
    let rows = sqlserver_first_rows(&batch, "SQL Server 权限列表返回类型异常")?;
    let mut permissions = Vec::new();
    for row in rows {
        let permission = row.values.first().and_then(cell_text).unwrap_or_default();
        if permission.is_empty() {
            continue;
        }
        let state = match row.values.get(1).and_then(cell_text).as_deref() {
            Some("W") => SqlServerPermissionState::GrantWithGrantOption,
            Some("D") => SqlServerPermissionState::Deny,
            _ => SqlServerPermissionState::Grant,
        };
        let (target, grantor) = match principal.kind {
            SqlServerPrincipalKind::Login => (
                SqlServerPermissionTarget::Server,
                row.values.get(2).and_then(cell_text),
            ),
            SqlServerPrincipalKind::User | SqlServerPrincipalKind::Role => {
                let class = row.values.get(2).and_then(cell_text).unwrap_or_default();
                let schema = row.values.get(3).and_then(cell_text).unwrap_or_default();
                let object = row.values.get(4).and_then(cell_text).unwrap_or_default();
                let target = match class.as_str() {
                    "DATABASE" => SqlServerPermissionTarget::Database,
                    "SCHEMA" if !schema.is_empty() => SqlServerPermissionTarget::Schema { schema },
                    "OBJECT_OR_COLUMN" if !schema.is_empty() && !object.is_empty() => {
                        SqlServerPermissionTarget::Object { schema, object }
                    }
                    _ => continue,
                };
                (target, row.values.get(5).and_then(cell_text))
            }
        };
        permissions.push(SqlServerPermission {
            target,
            permission,
            state,
            grantor,
        });
    }
    Ok(permissions)
}

fn render_sqlserver_security_plan(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    plan: &SqlServerSecurityPlan,
) -> fluxdb_core::Result<Vec<String>> {
    if config.id != connection_id {
        return Err(Error::new(
            ErrorKind::Connection,
            "SQL Server 连接上下文不匹配",
        ));
    }
    fluxdb_core::render_sqlserver_security_plan(plan, true)
}

fn apply_sqlserver_security_plan(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    plan: &SqlServerSecurityPlan,
) -> fluxdb_core::Result<Vec<String>> {
    if config.id != connection_id {
        return Err(Error::new(
            ErrorKind::Connection,
            "SQL Server 连接上下文不匹配",
        ));
    }
    if plan.is_empty() {
        return Err(Error::new(ErrorKind::Query, "SQL Server 安全变更计划为空"));
    }
    let statements = fluxdb_core::render_sqlserver_security_plan(plan, false)?;
    let mut client = sqlserver_connect_database(config, Some(&plan.database))?;
    let result = sqlserver_block_on(async {
        sqlserver_validate_security_plan(&mut client, plan).await?;
        sqlserver_exec_transaction(&mut client, "BEGIN TRANSACTION").await?;
        let mut current_scope = None;
        for (change, statement) in plan.ordered_changes().into_iter().zip(&statements) {
            let scope = if sqlserver_change_uses_master(change) {
                "master"
            } else {
                plan.database.as_str()
            };
            if current_scope != Some(scope) {
                let use_sql = format!(
                    "USE {}",
                    fluxdb_core::SqlServerDialect.quote_identifier(scope)
                );
                if let Err(error) = sqlserver_exec_transaction(&mut client, &use_sql).await {
                    let _ = sqlserver_exec_transaction(
                        &mut client,
                        "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION",
                    )
                    .await;
                    return Err(error);
                }
                current_scope = Some(scope);
            }
            if let Err(error) = sqlserver_run_batch(&mut client, statement).await {
                if let Err(rollback_error) = sqlserver_exec_transaction(
                    &mut client,
                    "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION",
                )
                .await
                {
                    tracing::warn!(
                        target: "fluxdb_connectors",
                        message = %rollback_error.message,
                        "SQL Server 安全变更回滚失败"
                    );
                }
                return Err(error);
            }
        }
        // 提交途中断连：账号/权限可能已生效，标成结果未知让上层核实。
        sqlserver_exec_transaction(&mut client, "COMMIT TRANSACTION")
            .await
            .map_err(|error| error.mark_outcome_unknown("提交 SQL Server 安全变更"))?;
        fluxdb_core::render_sqlserver_security_plan(plan, true)
    });
    sqlserver_block_on(sqlserver_close(client));
    result
}

fn sqlserver_change_uses_master(change: &SqlServerSecurityChange) -> bool {
    match change {
        SqlServerSecurityChange::CreateLogin { .. }
        | SqlServerSecurityChange::SetLoginEnabled { .. }
        | SqlServerSecurityChange::SetLoginPassword { .. }
        | SqlServerSecurityChange::SetLoginDefaultDatabase { .. }
        | SqlServerSecurityChange::DropLogin { .. }
        | SqlServerSecurityChange::AddRoleMember {
            scope: SqlServerRoleScope::Server,
            ..
        }
        | SqlServerSecurityChange::DropRoleMember {
            scope: SqlServerRoleScope::Server,
            ..
        }
        | SqlServerSecurityChange::GrantPermission {
            target: SqlServerPermissionTarget::Server,
            ..
        }
        | SqlServerSecurityChange::RevokePermission {
            target: SqlServerPermissionTarget::Server,
            ..
        }
        | SqlServerSecurityChange::RevokeGrantOption {
            target: SqlServerPermissionTarget::Server,
            ..
        } => true,
        _ => false,
    }
}

async fn sqlserver_validate_security_plan(
    client: &mut SqlServerClient,
    plan: &SqlServerSecurityPlan,
) -> fluxdb_core::Result<()> {
    let batch = sqlserver_run_batch(
        client,
        "SELECT SUSER_SNAME() AS current_login, USER_NAME() AS [current_user], \
                IS_SRVROLEMEMBER('sysadmin') AS is_sysadmin, \
                IS_ROLEMEMBER('db_owner') AS is_db_owner",
    )
    .await?;
    let row = batch
        .result_sets
        .first()
        .and_then(|set| set.rows.first())
        .ok_or_else(|| Error::new(ErrorKind::Query, "无法读取 SQL Server 当前安全上下文"))?;
    let current_login = row.values.first().and_then(cell_text).unwrap_or_default();
    let current_user = row.values.get(1).and_then(cell_text).unwrap_or_default();
    let is_sysadmin = row.values.get(2).and_then(cell_bool).unwrap_or(false);
    for change in plan.ordered_changes() {
        if let SqlServerSecurityChange::AddRoleMember { role, member, .. }
        | SqlServerSecurityChange::DropRoleMember { role, member, .. } = change
        {
            if role.eq_ignore_ascii_case("public") {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能修改 SQL Server public 角色的成员关系",
                ));
            }
            if role.eq_ignore_ascii_case(member) {
                return Err(Error::new(
                    ErrorKind::Query,
                    "SQL Server 角色不能成为自己的成员",
                ));
            }
        }
        match change {
            SqlServerSecurityChange::SetLoginEnabled {
                name,
                enabled: false,
            }
            | SqlServerSecurityChange::DropLogin { name }
                if name.eq_ignore_ascii_case(&current_login) =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能禁用或删除当前 SQL Server 登录名",
                ));
            }
            SqlServerSecurityChange::DropLogin { name }
                if name.eq_ignore_ascii_case("sa") || name.starts_with("##") =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能删除 SQL Server 系统登录名",
                ));
            }
            SqlServerSecurityChange::DropUser { name }
                if name.eq_ignore_ascii_case(&current_user) =>
            {
                return Err(Error::new(ErrorKind::Permission, "不能删除当前数据库用户"));
            }
            SqlServerSecurityChange::DropDatabaseRole { name }
                if name.to_ascii_lowercase().starts_with("db_") =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能删除 SQL Server 固定数据库角色",
                ));
            }
            SqlServerSecurityChange::DropRoleMember {
                scope,
                role,
                member,
            } if (role.eq_ignore_ascii_case("sysadmin")
                && member.eq_ignore_ascii_case(&current_login))
                || (role.eq_ignore_ascii_case("db_owner")
                    && member.eq_ignore_ascii_case(&current_user)) =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能移除当前主体的 sysadmin/db_owner 角色",
                ));
            }
            SqlServerSecurityChange::GrantPermission {
                grantee,
                permission,
                state,
                ..
            } if grantee.eq_ignore_ascii_case(&current_login)
                && (permission.eq_ignore_ascii_case("CONTROL SERVER")
                    || matches!(state, SqlServerPermissionState::Deny)) =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能向当前登录名授予 DENY 或 CONTROL SERVER",
                ));
            }
            SqlServerSecurityChange::GrantPermission {
                grantee,
                permission,
                state,
                ..
            } if grantee.eq_ignore_ascii_case(&current_user)
                && (permission.eq_ignore_ascii_case("CONTROL")
                    || matches!(state, SqlServerPermissionState::Deny)) =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能向当前数据库用户授予 DENY 或 CONTROL",
                ));
            }
            SqlServerSecurityChange::RevokePermission {
                grantee,
                permission,
                ..
            }
            | SqlServerSecurityChange::RevokeGrantOption {
                grantee,
                permission,
                ..
            } if (grantee.eq_ignore_ascii_case(&current_login)
                && (permission.eq_ignore_ascii_case("CONTROL SERVER")
                    || permission.eq_ignore_ascii_case("CONNECT SQL")))
                || (grantee.eq_ignore_ascii_case(&current_user)
                    && (permission.eq_ignore_ascii_case("CONTROL")
                        || permission.eq_ignore_ascii_case("CONNECT"))) =>
            {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "不能撤销当前主体的登录连接或 CONTROL 权限",
                ));
            }
            _ => {}
        }
        if let Some(member) = sqlserver_sysadmin_member_removed(change) {
            let still_sysadmin = sqlserver_query_scalar_i64(
                client,
                "SELECT CONVERT(int, IS_SRVROLEMEMBER('sysadmin', @P1))",
                member,
            )
            .await?
                != 0;
            if still_sysadmin {
                let other_admins = sqlserver_query_scalar_i64(
                    client,
                    "SELECT COUNT_BIG(*) \
                     FROM sys.server_role_members rm \
                     JOIN sys.server_principals r ON r.principal_id = rm.role_principal_id \
                     JOIN sys.server_principals m ON m.principal_id = rm.member_principal_id \
                     WHERE r.name = N'sysadmin' AND m.is_disabled = 0 \
                       AND m.type IN ('S','U','G') AND m.name <> @P1",
                    member,
                )
                .await?;
                if other_admins == 0 {
                    return Err(Error::new(
                        ErrorKind::Permission,
                        "不能删除、禁用或移除最后一个启用的 sysadmin 成员",
                    ));
                }
            }
        }
        if let Some(member) = sqlserver_db_owner_member_removed(change) {
            let still_db_owner = sqlserver_query_scalar_i64(
                client,
                "SELECT CONVERT(int, IS_ROLEMEMBER('db_owner', @P1))",
                member,
            )
            .await?
                != 0;
            if still_db_owner {
                let other_owners = sqlserver_query_scalar_i64(
                    client,
                    "SELECT COUNT_BIG(*) \
                     FROM sys.database_role_members rm \
                     JOIN sys.database_principals r ON r.principal_id = rm.role_principal_id \
                     JOIN sys.database_principals m ON m.principal_id = rm.member_principal_id \
                     WHERE r.name = N'db_owner' AND m.name <> @P1",
                    member,
                )
                .await?;
                if other_owners == 0 && !is_sysadmin {
                    return Err(Error::new(
                        ErrorKind::Permission,
                        "不能移除最后一个 db_owner 成员（当前主体也不是 sysadmin）",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn sqlserver_sysadmin_member_removed(change: &SqlServerSecurityChange) -> Option<&str> {
    match change {
        SqlServerSecurityChange::SetLoginEnabled {
            name,
            enabled: false,
        }
        | SqlServerSecurityChange::DropLogin { name } => Some(name),
        SqlServerSecurityChange::DropRoleMember {
            scope: SqlServerRoleScope::Server,
            role,
            member,
        } if role.eq_ignore_ascii_case("sysadmin") => Some(member),
        _ => None,
    }
}

fn sqlserver_db_owner_member_removed(change: &SqlServerSecurityChange) -> Option<&str> {
    match change {
        SqlServerSecurityChange::DropUser { name } => Some(name),
        SqlServerSecurityChange::DropRoleMember {
            scope: SqlServerRoleScope::Database,
            role,
            member,
        } if role.eq_ignore_ascii_case("db_owner") => Some(member),
        _ => None,
    }
}

async fn sqlserver_query_scalar_i64(
    client: &mut SqlServerClient,
    sql: &str,
    value: &str,
) -> fluxdb_core::Result<i64> {
    let batch = sqlserver_run_query(
        client,
        sql,
        &[fluxdb_core::SqlParameter::Text(value.to_string())],
    )
    .await?;
    Ok(batch
        .result_sets
        .first()
        .and_then(|set| set.rows.first())
        .and_then(|row| row.values.first())
        .and_then(cell_i64)
        .unwrap_or(0))
}
