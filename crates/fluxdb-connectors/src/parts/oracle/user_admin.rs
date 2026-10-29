// Oracle 用户与权限：目录读取、计划渲染与执行。
//
// 三条硬约束（docs/oracle.md §8）：
// 1. 只走当前账号可见的 `ALL_*` / `USER_*` 视图，不要求 `DBA_*` / `V$*`；
//    授权明细只读当前账号自己（`USER_*` / 以自己为 GRANTEE 的 `ALL_TAB_PRIVS`），
//    别人的账号只列名字，界面对非当前账号一律只读，不伪造其权限为空。
// 2. `INHERITED = YES` 的行是角色间接带来的，不能当成可直接撤销的授权。
// 3. 用户管理 DDL 会隐式提交，执行中断后无法整批回滚；密码不进日志与错误回执。

use fluxdb_core::{
    OracleObjectPrivilege, OracleProfileLimit, OracleRoleGrant, OracleSystemPrivilege,
    OracleTablespaceQuota, OracleUser, OracleUserAdminPlan, OracleUserAdminSnapshot,
    OracleUserDetail,
};

/// 目录里可见的账号列表。只读 `ALL_USERS`，不碰 `DBA_USERS`。
fn oracle_load_user_admin_snapshot(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
) -> fluxdb_core::Result<OracleUserAdminSnapshot> {
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "Oracle 连接上下文不匹配"));
    }
    let connection = oracle_connect(config)?;
    let current_user = oracle_current_user(&connection)?;
    let users = oracle_query(
        &connection,
        "SELECT USERNAME, ORACLE_MAINTAINED, COMMON, TO_CHAR(CREATED, 'YYYY-MM-DD HH24:MI:SS') \
         FROM ALL_USERS ORDER BY USERNAME",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        Some(OracleUser {
            name: oracle_meta_text(row, 0)?,
            oracle_maintained: oracle_meta_text(row, 1)
                .is_some_and(|value| value.eq_ignore_ascii_case("Y")),
            common: oracle_meta_text(row, 2)
                .is_some_and(|value| value.eq_ignore_ascii_case("Y")),
            created: oracle_meta_text(row, 3),
        })
    })
    .collect();

    let own = oracle_load_own_user_detail(&connection)?;
    let roles = oracle_role_grants(&connection)?;
    let system_privileges = oracle_system_privileges(&connection)?;
    let object_privileges = oracle_object_privileges(&connection, &current_user)?;
    let quotas = oracle_tablespace_quotas(&connection)?;
    let limits = oracle_profile_limits(&connection)?;
    // 候选只取「带 ADMIN OPTION 的直接授权」：经角色间接得到的 ADMIN OPTION 不能转授，
    // 列进去会诱导用户填一条注定失败的授权。
    let role_candidates = roles
        .iter()
        .filter(|grant| grant.admin_option && !grant.inherited)
        .map(|grant| grant.role.clone())
        .collect();
    let system_privilege_candidates = system_privileges
        .iter()
        .filter(|privilege| privilege.admin_option && !privilege.inherited)
        .map(|privilege| privilege.privilege.clone())
        .collect();
    Ok(OracleUserAdminSnapshot {
        current_user,
        users,
        own,
        roles,
        system_privileges,
        object_privileges,
        quotas,
        limits,
        role_candidates,
        system_privilege_candidates,
    })
}

fn oracle_current_user(connection: &OracleConnection) -> fluxdb_core::Result<String> {
    let rows = oracle_query(connection, "SELECT USER FROM DUAL", &[])?;
    rows.first()
        .and_then(|row| oracle_meta_text(row, 0))
        .ok_or_else(|| Error::new(ErrorKind::Query, "无法读取当前 Oracle 账号"))
}

fn oracle_load_own_user_detail(
    connection: &OracleConnection,
) -> fluxdb_core::Result<Option<OracleUserDetail>> {
    // `USER_USERS` 只返回当前账号，不会因为缺少权限而返回别人的行。
    let rows = oracle_query(
        connection,
        "SELECT USERNAME, ACCOUNT_STATUS, TO_CHAR(LOCK_DATE, 'YYYY-MM-DD HH24:MI:SS'), \
                TO_CHAR(EXPIRY_DATE, 'YYYY-MM-DD HH24:MI:SS'), DEFAULT_TABLESPACE, \
                TEMPORARY_TABLESPACE, TO_CHAR(CREATED, 'YYYY-MM-DD HH24:MI:SS') \
         FROM USER_USERS",
        &[],
    )?;
    Ok(rows.first().and_then(|row| {
        Some(OracleUserDetail {
            name: oracle_meta_text(row, 0)?,
            account_status: oracle_meta_text(row, 1).unwrap_or_default(),
            lock_date: oracle_meta_text(row, 2),
            expiry_date: oracle_meta_text(row, 3),
            default_tablespace: oracle_meta_text(row, 4),
            temporary_tablespace: oracle_meta_text(row, 5),
            created: oracle_meta_text(row, 6),
        })
    }))
}

fn oracle_role_grants(connection: &OracleConnection) -> fluxdb_core::Result<Vec<OracleRoleGrant>> {
    // `USER_ROLE_PRIVS` 自带 `INHERITED`，能区分直接授予与角色传递。
    Ok(oracle_query(
        connection,
        "SELECT GRANTED_ROLE, ADMIN_OPTION, DEFAULT_ROLE, INHERITED \
         FROM USER_ROLE_PRIVS ORDER BY GRANTED_ROLE",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        Some(OracleRoleGrant {
            role: oracle_meta_text(row, 0)?,
            admin_option: oracle_meta_text(row, 1)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            default_role: oracle_meta_text(row, 2)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            inherited: oracle_meta_text(row, 3)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
        })
    })
    .collect())
}

fn oracle_system_privileges(
    connection: &OracleConnection,
) -> fluxdb_core::Result<Vec<OracleSystemPrivilege>> {
    Ok(oracle_query(
        connection,
        "SELECT PRIVILEGE, ADMIN_OPTION, INHERITED FROM USER_SYS_PRIVS ORDER BY PRIVILEGE",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        Some(OracleSystemPrivilege {
            privilege: oracle_meta_text(row, 0)?,
            admin_option: oracle_meta_text(row, 1)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            inherited: oracle_meta_text(row, 2)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
        })
    })
    .collect())
}

/// 对象权限：`ALL_TAB_PRIVS` 里当前账号作为被授权人的行。
///
/// `INHERITED` 为 YES 的行来自角色，界面上必须显示为继承且不可直接撤销。
fn oracle_object_privileges(
    connection: &OracleConnection,
    current_user: &str,
) -> fluxdb_core::Result<Vec<OracleObjectPrivilege>> {
    Ok(oracle_query(
        connection,
        "SELECT TABLE_SCHEMA, TABLE_NAME, PRIVILEGE, GRANTABLE, INHERITED, GRANTOR \
         FROM ALL_TAB_PRIVS WHERE GRANTEE = :1 \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, PRIVILEGE",
        &[OracleBindValue::Text(current_user.to_string())],
    )?
    .iter()
    .filter_map(|row| {
        Some(OracleObjectPrivilege {
            owner: oracle_meta_text(row, 0)?,
            object: oracle_meta_text(row, 1)?,
            privilege: oracle_meta_text(row, 2)?,
            grantable: oracle_meta_text(row, 3)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            inherited: oracle_meta_text(row, 4)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            grantor: oracle_meta_text(row, 5),
        })
    })
    .collect())
}

fn oracle_tablespace_quotas(
    connection: &OracleConnection,
) -> fluxdb_core::Result<Vec<OracleTablespaceQuota>> {
    // `MAX_BYTES = -1` 表示不限额。
    Ok(oracle_query(
        connection,
        "SELECT TABLESPACE_NAME, MAX_BYTES FROM USER_TS_QUOTAS ORDER BY TABLESPACE_NAME",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        let max_bytes = row
            .get::<Option<oracledb::OracleNumber>>(1)
            .ok()
            .flatten()
            .and_then(|value| value.to_string().parse::<i64>().ok())
            .and_then(|value| u64::try_from(value).ok());
        Some(OracleTablespaceQuota {
            tablespace: oracle_meta_text(row, 0)?,
            max_bytes,
        })
    })
    .collect())
}

fn oracle_profile_limits(
    connection: &OracleConnection,
) -> fluxdb_core::Result<Vec<OracleProfileLimit>> {
    Ok(oracle_query(
        connection,
        "SELECT RESOURCE_NAME, LIMIT FROM USER_RESOURCE_LIMITS ORDER BY RESOURCE_NAME",
        &[],
    )?
    .iter()
    .filter_map(|row| {
        let limit = oracle_meta_text(row, 1);
        Some(OracleProfileLimit {
            resource: oracle_meta_text(row, 0)?,
            // `UNLIMITED` 归一为 `None`，界面不必特判这个词。
            limit: limit.filter(|value| !value.eq_ignore_ascii_case("UNLIMITED")),
        })
    })
    .collect())
}

/// 执行计划。语句逐条提交（Oracle DDL 会隐式提交），失败即停并明说无法回滚。
///
/// 返回值是实际执行过的语句（密码位置已脱敏），供界面回显；真实口令不进入日志。
fn oracle_apply_user_admin_plan(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    plan: &OracleUserAdminPlan,
) -> fluxdb_core::Result<Vec<String>> {
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "Oracle 连接上下文不匹配"));
    }
    if plan.is_empty() {
        return Err(Error::new(ErrorKind::Query, "Oracle 用户与权限变更计划为空"));
    }
    let statements = fluxdb_core::render_oracle_user_admin_plan(plan, false)?;
    let masked = fluxdb_core::render_oracle_user_admin_plan(plan, true)?;
    let connection = oracle_connect(config)?;
    let mut executed = Vec::with_capacity(statements.len());
    for (index, statement) in statements.iter().enumerate() {
        // 渲染出的语句带结尾分号（给预览看的），OCI 不接受分号，会报 ORA-00922。
        let statement = statement.trim().trim_end_matches(';').trim_end();
        if let Err(error) = oracle_execute(&connection, statement, &[]) {
            return Err(Error::new(
                error.kind,
                format!(
                    "Oracle 用户与权限变更在第 {} 条失败（此前语句已隐式提交，无法整体回滚）：{}",
                    index + 1,
                    error.message
                ),
            ));
        }
        executed.push(masked[index].clone());
    }
    Ok(executed)
}

#[cfg(test)]
mod oracle_user_admin_plan_tests {
    use super::*;

    /// 继承来的授权必须在提示里区分，避免界面把它显示成可直接撤销。
    #[test]
    fn inherited_privileges_are_distinguishable() {
        let granted = OracleRoleGrant {
            role: "DBA".into(),
            admin_option: false,
            default_role: true,
            inherited: true,
        };
        assert!(granted.inherited);
        let direct = OracleSystemPrivilege {
            privilege: "CREATE SESSION".into(),
            admin_option: true,
            inherited: false,
        };
        assert!(!direct.inherited);
    }

    #[test]
    fn apply_rejects_empty_plan() {
        // 空计划在连接之前就被拒绝，所以这里不需要可用的连接配置。
        let config = ConnectionConfig {
            id: ConnectionId(1),
            name: "oracle-empty-plan".into(),
            kind: DatabaseKind::Oracle,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 1521,
                database: None,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            sqlserver_profile: None,
            mongo_profile: None,
            oracle_profile: None,
        };
        let error = oracle_apply_user_admin_plan(
            &config,
            config.id,
            &OracleUserAdminPlan::default(),
        )
        .expect_err("empty plan must be rejected");
        assert!(error.message.contains("为空"));
    }
}
