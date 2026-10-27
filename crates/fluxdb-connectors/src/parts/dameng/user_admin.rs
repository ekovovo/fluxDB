// 达梦用户与权限：账号即 schema，权限模型与 Oracle 同源（角色 / 系统权限 / 对象权限）。
//
// 目录视图差异（DM8 实测）：
// - `USER_ROLE_PRIVS` 没有 `INHERITED` 列，`ALL_TAB_PRIVS` 用 `OWNER` 而不是 `TABLE_SCHEMA`；
// - 没有 `USER_TS_QUOTAS` / `USER_RESOURCE_LIMITS`，配额与 Profile 限制留空；
// - 没有 `CONNECT` 角色，登录权限对应系统权限 `CREATE SESSION`。
//
// 与 Oracle 一样只读当前登录账号自己的授权明细：普通账号读 `DBA_*` 会报 -5504。
// 账号 DDL 逐条执行，失败后已提交的语句无法回滚，错误信息带序号。

fn dm_user_admin_query(
    client: &mut DmClient,
    sql: &str,
    params: &[Param],
) -> fluxdb_core::Result<Vec<Vec<Value>>> {
    match dm_execute(client, sql, params)? {
        ExecuteResult::Query { rows, .. } => Ok(rows),
        _ => Err(Error::new(ErrorKind::Query, "达梦目录查询返回了非结果集")),
    }
}

fn dm_cell(row: &[Value], index: usize) -> Option<String> {
    row.get(index).and_then(dm_value_text)
}

/// 达梦的 YES/NO 与 Y/N 混用：两种都按真处理。
fn dm_flag_true(value: &str) -> bool {
    value.eq_ignore_ascii_case("YES") || value.eq_ignore_ascii_case("Y")
}

/// DM8 内置账号：SYS / SYSDBA / SYSAUDITOR / SYSSSO。不可删除，也不应在这里改名。
fn dm_is_builtin_account(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "SYS" | "SYSDBA" | "SYSAUDITOR" | "SYSSSO"
    )
}

fn dm_flag(row: &[Value], index: usize) -> bool {
    dm_cell(row, index).is_some_and(|value| dm_flag_true(&value))
}

/// 读出账号列表、当前账号详情与当前账号的直接授权。
fn dm_user_admin_snapshot(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
) -> fluxdb_core::Result<fluxdb_core::OracleUserAdminSnapshot> {
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "达梦连接上下文不匹配"));
    }
    let mut client = dm_connect(config)?;
    let current_user = dm_user_admin_query(&mut client, "SELECT USER FROM DUAL", &[])?
        .first()
        .and_then(|row| dm_cell(row, 0))
        .ok_or_else(|| Error::new(ErrorKind::Query, "无法读取当前达梦账号"))?;

    let users = dm_user_admin_query(
        &mut client,
        "SELECT USERNAME, TO_CHAR(CREATED, 'YYYY-MM-DD HH24:MI:SS') \
         FROM ALL_USERS ORDER BY USERNAME",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        let name = dm_cell(row, 0)?;
        // 达梦没有 ORACLE_MAINTAINED 列：四个内置账号按名字识别，
        // 界面据此禁止删除，避免把 DROP USER SYS 发到服务端。
        let oracle_maintained = dm_is_builtin_account(&name);
        Some(fluxdb_core::OracleUser {
            name,
            oracle_maintained,
            common: false,
            created: dm_cell(row, 1),
        })
    })
    .collect();

    let own = dm_user_admin_query(
        &mut client,
        "SELECT USERNAME, ACCOUNT_STATUS, TO_CHAR(LOCK_DATE, 'YYYY-MM-DD HH24:MI:SS'), \
                TO_CHAR(EXPIRY_DATE, 'YYYY-MM-DD HH24:MI:SS'), DEFAULT_TABLESPACE, \
                TEMPORARY_TABLESPACE, TO_CHAR(CREATED, 'YYYY-MM-DD HH24:MI:SS') \
         FROM USER_USERS",
        &[],
    )?
    .first()
    .and_then(|row| {
        Some(fluxdb_core::OracleUserDetail {
            name: dm_cell(row, 0)?,
            account_status: dm_cell(row, 1).unwrap_or_default(),
            lock_date: dm_cell(row, 2),
            expiry_date: dm_cell(row, 3),
            default_tablespace: dm_cell(row, 4),
            temporary_tablespace: dm_cell(row, 5),
            created: dm_cell(row, 6),
        })
    });

    let roles: Vec<fluxdb_core::OracleRoleGrant> = dm_user_admin_query(
        &mut client,
        "SELECT GRANTED_ROLE, ADMIN_OPTION, DEFAULT_ROLE \
         FROM USER_ROLE_PRIVS ORDER BY GRANTED_ROLE",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        Some(fluxdb_core::OracleRoleGrant {
            role: dm_cell(row, 0)?,
            admin_option: dm_flag(row, 1),
            default_role: dm_flag(row, 2),
            // 达梦视图没有 INHERITED：按直接授予处理，能否撤销交给服务端判定。
            inherited: false,
        })
    })
    .collect();

    let system_privileges: Vec<fluxdb_core::OracleSystemPrivilege> = dm_user_admin_query(
        &mut client,
        "SELECT PRIVILEGE, ADMIN_OPTION FROM USER_SYS_PRIVS ORDER BY PRIVILEGE",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        Some(fluxdb_core::OracleSystemPrivilege {
            privilege: dm_cell(row, 0)?,
            admin_option: dm_flag(row, 1),
            inherited: false,
        })
    })
    .collect();

    let object_privileges = dm_user_admin_query(
        &mut client,
        "SELECT OWNER, TABLE_NAME, PRIVILEGE, GRANTABLE, GRANTOR \
         FROM ALL_TAB_PRIVS WHERE GRANTEE = ? \
         ORDER BY OWNER, TABLE_NAME, PRIVILEGE",
        &[Param::Str(current_user.clone())],
    )?
    .iter()
    .filter_map(|row| {
        Some(fluxdb_core::OracleObjectPrivilege {
            owner: dm_cell(row, 0)?,
            object: dm_cell(row, 1)?,
            privilege: dm_cell(row, 2)?,
            grantable: dm_flag(row, 3),
            inherited: false,
            grantor: dm_cell(row, 4),
        })
    })
    .collect();

    client.close();
    // 与 Oracle 同规则：只把带 ADMIN OPTION 的直接授权列为候选，其余仍可手工填写。
    let role_candidates = roles
        .iter()
        .filter(|grant| grant.admin_option)
        .map(|grant| grant.role.clone())
        .collect();
    let system_privilege_candidates = system_privileges
        .iter()
        .filter(|privilege| privilege.admin_option)
        .map(|privilege| privilege.privilege.clone())
        .collect();
    Ok(fluxdb_core::OracleUserAdminSnapshot {
        current_user,
        users,
        own,
        roles,
        system_privileges,
        object_privileges,
        // 达梦没有这两张视图：界面按「无记录」展示，不伪造配额与 Profile 限制。
        quotas: Vec::new(),
        limits: Vec::new(),
        role_candidates,
        system_privilege_candidates,
    })
}

/// 逐条执行账号 DDL，失败即停并说明已提交部分不可回滚。
///
/// 核心渲染把「建号 + 授登录权限 + 补 PROFILE」放在同一条字符串里，
/// 这里按引号外的分号拆成单句，避免驱动一次发送多语句。
fn dm_apply_user_admin_plan(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    plan: &fluxdb_core::OracleUserAdminPlan,
) -> fluxdb_core::Result<Vec<String>> {
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "达梦连接上下文不匹配"));
    }
    if plan.is_empty() {
        return Err(Error::new(ErrorKind::Query, "达梦用户与权限变更计划为空"));
    }
    let statements = fluxdb_core::render_user_admin_plan(plan, false, fluxdb_core::UserAdminSqlDialect::Dameng)?;
    let masked = fluxdb_core::render_user_admin_plan(plan, true, fluxdb_core::UserAdminSqlDialect::Dameng)?;
    let mut client = dm_connect(config)?;
    let mut executed = Vec::with_capacity(statements.len());
    for (index, statement) in statements.iter().enumerate() {
        for single in dm_split_statements(statement) {
            if let Err(error) = dm_execute(&mut client, &single, &[]) {
                client.close();
                return Err(Error::new(
                    error.kind,
                    format!(
                        "达梦用户与权限变更在第 {} 条失败（此前语句已提交，无法整体回滚）：{}",
                        index + 1,
                        error.message
                    ),
                ));
            }
        }
        executed.push(masked[index].clone());
    }
    client.close();
    Ok(executed)
}

/// 按引号外的分号拆分语句；口令里的分号不能当成语句边界。
fn dm_split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in sql.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                current.push(ch);
            }
            ';' if !in_quote => {
                if !current.trim().is_empty() {
                    statements.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        statements.push(current.trim().to_string());
    }
    statements
}

#[cfg(test)]
mod dameng_user_admin_tests {
    use super::*;
    use fluxdb_core::OracleUserAdminChange;

    /// 达梦的登录权限是 `CREATE SESSION`，不能沿用 Oracle 的 `CONNECT` 角色。
    #[test]
    fn create_user_uses_dameng_login_privilege() {
        let plan = fluxdb_core::OracleUserAdminPlan {
            changes: vec![OracleUserAdminChange::CreateUser {
                name: "app_user".into(),
                password: "Abc_123456".into(),
                default_tablespace: Some("MAIN".into()),
                temporary_tablespace: None,
                profile: Some("DEFAULT".into()),
                quota: Some(("MAIN".into(), None)),
                grant_connect_role: true,
            }],
        };
        let sql = fluxdb_core::render_user_admin_plan(&plan, true, fluxdb_core::UserAdminSqlDialect::Dameng)
            .expect("render");
        let joined = sql.join("\n");
        assert!(joined.contains("GRANT CREATE SESSION TO"));
        assert!(!joined.contains("CONNECT"));
        // 达梦不支持建号时写 PROFILE / QUOTA，改到 ALTER USER。
        assert!(!joined.contains("QUOTA"));
        assert!(joined.contains("ALTER USER \"app_user\" PROFILE \"DEFAULT\";"));
        assert!(!joined.contains("Abc_123456"), "预览必须脱敏");
    }

    /// 建号语句里的口令可能自带分号，拆分不能把它截断。
    #[test]
    fn split_keeps_semicolon_inside_password() {
        let sql = "CREATE USER \"app\" IDENTIFIED BY \"a;b\";\nGRANT CREATE SESSION TO \"app\";";
        let parts = dm_split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("\"a;b\""));
    }

    #[test]
    fn dameng_flag_accepts_yes_and_y() {
        assert!(dm_flag_true("YES"));
        assert!(dm_flag_true("Y"));
        assert!(!dm_flag_true("NO"));
        assert!(!dm_flag_true("N"));
    }
}
