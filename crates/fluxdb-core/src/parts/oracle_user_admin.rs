// Oracle 用户与权限的结构化模型。
//
// 与 SQL Server / PostgreSQL 的模型不能互相套用：
// - Oracle 的「用户」同时就是 schema，没有独立的数据库层；
// - 权限分系统权限与对象权限，角色授权另有一套；`WITH ADMIN OPTION`（系统权限/角色）
//   与 `WITH GRANT OPTION`（对象权限）语义不同，撤销规则也不同；
// - 目录视图里 `INHERITED = YES` 的行是通过角色间接得到的，不能当成可直接撤销的授权；
// - 普通账号能看的目录有限：只能读自己的授权明细，别人的账号只列名字。
//
// 另一条硬约束：Oracle 的用户管理 DDL 同样会隐式提交，任何「整批回滚」的承诺都不成立。

/// 一个 Oracle 账号（`ALL_USERS` 的一行）。详情只在能读到目录时才有。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleUser {
    pub name: String,
    /// `ALL_USERS.ORACLE_MAINTAINED`：SYS 自带账号，不应当在这里改。
    pub oracle_maintained: bool,
    /// `ALL_USERS.COMMON`：CDB 根容器里的公共用户。
    pub common: bool,
    pub created: Option<String>,
}

/// 当前账号的详情（`USER_USERS`）。Oracle 只允许读到自己的这一行。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleUserDetail {
    pub name: String,
    pub account_status: String,
    pub lock_date: Option<String>,
    pub expiry_date: Option<String>,
    pub default_tablespace: Option<String>,
    pub temporary_tablespace: Option<String>,
    pub created: Option<String>,
}

impl OracleUserDetail {
    pub fn is_locked(&self) -> bool {
        self.account_status.eq_ignore_ascii_case("LOCKED")
            || self.account_status.eq_ignore_ascii_case("LOCKED(TIMED)")
    }
}

/// 角色授权（`USER_ROLE_PRIVS` / 目标用户自身可见的行）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleRoleGrant {
    pub role: String,
    pub admin_option: bool,
    pub default_role: bool,
    /// 通过其它角色间接获得；这类行只能看，不能直接 REVOKE。
    pub inherited: bool,
}

/// 系统权限（`USER_SYS_PRIVS`）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleSystemPrivilege {
    pub privilege: String,
    pub admin_option: bool,
    pub inherited: bool,
}

/// 对象权限（`ALL_TAB_PRIVS`）。`owner` 是对象所在 schema，不是被授权人。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleObjectPrivilege {
    pub owner: String,
    pub object: String,
    pub privilege: String,
    pub grantable: bool,
    pub inherited: bool,
    pub grantor: Option<String>,
}

/// 表空间配额（`USER_TS_QUOTAS`）。`None` 表示无上限（`MAX_BYTES = -1`）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleTablespaceQuota {
    pub tablespace: String,
    pub max_bytes: Option<u64>,
}

/// Profile 限制（`USER_RESOURCE_LIMITS`）。`UNLIMITED` 用 `None` 表示。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleProfileLimit {
    pub resource: String,
    pub limit: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleUserAdminSnapshot {
    pub current_user: String,
    pub users: Vec<OracleUser>,
    /// 当前账号自己的详情；读不到时为 `None`。
    pub own: Option<OracleUserDetail>,
    pub roles: Vec<OracleRoleGrant>,
    pub system_privileges: Vec<OracleSystemPrivilege>,
    pub object_privileges: Vec<OracleObjectPrivilege>,
    pub quotas: Vec<OracleTablespaceQuota>,
    pub limits: Vec<OracleProfileLimit>,
    /// 可授予给别人的角色候选（`USER_ROLE_PRIVS` 中带 `ADMIN_OPTION` 的行）。
    ///
    /// 只是输入提示：普通账号看不到完整角色目录，列表为空时仍允许手工填写。
    pub role_candidates: Vec<String>,
    /// 可授予给别人的系统权限候选（`USER_SYS_PRIVS` 中带 `ADMIN_OPTION` 的行）。
    pub system_privilege_candidates: Vec<String>,
}

impl OracleUserAdminSnapshot {
    pub fn contains_user(&self, name: &str) -> bool {
        self.users
            .iter()
            .any(|user| user.name.eq_ignore_ascii_case(name))
    }

    pub fn user(&self, name: &str) -> Option<&OracleUser> {
        self.users
            .iter()
            .find(|user| user.name.eq_ignore_ascii_case(name))
    }
}

/// 单条安全变更。密码字段不派生 `Debug`，避免调试输出把明文带出来。
#[derive(Clone, PartialEq, Eq)]
pub enum OracleUserAdminChange {
    CreateUser {
        name: String,
        password: String,
        default_tablespace: Option<String>,
        temporary_tablespace: Option<String>,
        profile: Option<String>,
        /// `(tablespace, bytes)`；`None` 表示不写配额子句。
        quota: Option<(String, Option<u64>)>,
        /// 新建账号默认没有 `CREATE SESSION`，要能登录必须显式授角色/权限。
        grant_connect_role: bool,
    },
    DropUser {
        name: String,
        /// 账号下有对象时必须 `CASCADE`，否则 Oracle 直接拒绝。
        cascade: bool,
    },
    SetPassword {
        name: String,
        password: String,
    },
    SetAccountLocked {
        name: String,
        locked: bool,
    },
    SetDefaultTablespace {
        name: String,
        tablespace: String,
    },
    SetQuota {
        name: String,
        tablespace: String,
        bytes: Option<u64>,
    },
    GrantRole {
        role: String,
        grantee: String,
        admin_option: bool,
    },
    RevokeRole {
        role: String,
        grantee: String,
    },
    GrantSystemPrivilege {
        privilege: String,
        grantee: String,
        admin_option: bool,
    },
    RevokeSystemPrivilege {
        privilege: String,
        grantee: String,
    },
    GrantObjectPrivilege {
        privilege: String,
        owner: String,
        object: String,
        grantee: String,
        grant_option: bool,
    },
    RevokeObjectPrivilege {
        privilege: String,
        owner: String,
        object: String,
        grantee: String,
    },
}

impl std::fmt::Debug for OracleUserAdminChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 密码只以掩码形式进入调试输出，避免日志/错误回执泄漏。
        match self {
            Self::CreateUser {
                name,
                default_tablespace,
                temporary_tablespace,
                profile,
                quota,
                grant_connect_role,
                ..
            } => f
                .debug_struct("CreateUser")
                .field("name", name)
                .field("password", &"********")
                .field("default_tablespace", default_tablespace)
                .field("temporary_tablespace", temporary_tablespace)
                .field("profile", profile)
                .field("quota", quota)
                .field("grant_connect_role", grant_connect_role)
                .finish(),
            Self::SetPassword { name, .. } => f
                .debug_struct("SetPassword")
                .field("name", name)
                .field("password", &"********")
                .finish(),
            other => f.write_str(&other.describe()),
        }
    }
}

impl OracleUserAdminChange {
    fn describe(&self) -> String {
        match self {
            Self::CreateUser { name, .. } => format!("CreateUser {{ name: {name:?} }}"),
            Self::DropUser { name, cascade } => {
                format!("DropUser {{ name: {name:?}, cascade: {cascade} }}")
            }
            Self::SetPassword { name, .. } => format!("SetPassword {{ name: {name:?} }}"),
            Self::SetAccountLocked { name, locked } => {
                format!("SetAccountLocked {{ name: {name:?}, locked: {locked} }}")
            }
            Self::SetDefaultTablespace { name, tablespace } => format!(
                "SetDefaultTablespace {{ name: {name:?}, tablespace: {tablespace:?} }}"
            ),
            Self::SetQuota {
                name,
                tablespace,
                bytes,
            } => format!(
                "SetQuota {{ name: {name:?}, tablespace: {tablespace:?}, bytes: {bytes:?} }}"
            ),
            Self::GrantRole {
                role,
                grantee,
                admin_option,
            } => format!(
                "GrantRole {{ role: {role:?}, grantee: {grantee:?}, admin_option: {admin_option} }}"
            ),
            Self::RevokeRole { role, grantee } => {
                format!("RevokeRole {{ role: {role:?}, grantee: {grantee:?} }}")
            }
            Self::GrantSystemPrivilege {
                privilege,
                grantee,
                admin_option,
            } => format!(
                "GrantSystemPrivilege {{ privilege: {privilege:?}, grantee: {grantee:?}, admin_option: {admin_option} }}"
            ),
            Self::RevokeSystemPrivilege { privilege, grantee } => format!(
                "RevokeSystemPrivilege {{ privilege: {privilege:?}, grantee: {grantee:?} }}"
            ),
            Self::GrantObjectPrivilege {
                privilege,
                owner,
                object,
                grantee,
                grant_option,
            } => format!(
                "GrantObjectPrivilege {{ privilege: {privilege:?}, owner: {owner:?}, object: {object:?}, grantee: {grantee:?}, grant_option: {grant_option} }}"
            ),
            Self::RevokeObjectPrivilege {
                privilege,
                owner,
                object,
                grantee,
            } => format!(
                "RevokeObjectPrivilege {{ privilege: {privilege:?}, owner: {owner:?}, object: {object:?}, grantee: {grantee:?} }}"
            ),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleUserAdminPlan {
    pub changes: Vec<OracleUserAdminChange>,
}

impl OracleUserAdminPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// 创建必须先于授权，删除必须晚于撤销；其余保持用户操作顺序。
    pub fn ordered_changes(&self) -> Vec<&OracleUserAdminChange> {
        let (creates, rest): (Vec<_>, Vec<_>) = self.changes.iter().partition(|change| {
            matches!(change, OracleUserAdminChange::CreateUser { .. })
        });
        let (drops, middle): (Vec<_>, Vec<_>) = rest.into_iter().partition(|change| {
            matches!(change, OracleUserAdminChange::DropUser { .. })
        });
        creates.into_iter().chain(middle).chain(drops).collect()
    }
}

/// 常用的 Oracle 系统权限。只作为输入建议，最终仍由服务端校验。
pub const ORACLE_SYSTEM_PRIVILEGES: &[&str] = &[
    "CREATE SESSION",
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE SEQUENCE",
    "CREATE PROCEDURE",
    "CREATE TRIGGER",
    "CREATE ANY TABLE",
    "CREATE ANY VIEW",
    "ALTER ANY TABLE",
    "DROP ANY TABLE",
    "INSERT ANY TABLE",
    "UPDATE ANY TABLE",
    "DELETE ANY TABLE",
    "SELECT ANY TABLE",
    "SELECT ANY DICTIONARY",
    "GRANT ANY PRIVILEGE",
    "GRANT ANY ROLE",
    "UNLIMITED TABLESPACE",
];

/// 常用的对象权限。
pub const ORACLE_OBJECT_PRIVILEGES: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "ALTER", "INDEX", "REFERENCES", "EXECUTE", "READ",
    "DEBUG",
];

/// Oracle 标识符校验：引用时总带双引号，因此禁止双引号与控制字符即可。
pub fn validate_oracle_principal_name(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::new(ErrorKind::Query, "Oracle 账号名称不能为空"));
    }
    if name.chars().count() > 128 {
        return Err(Error::new(
            ErrorKind::Query,
            "Oracle 账号名称不能超过 128 个字符",
        ));
    }
    if name.chars().any(|ch| ch == '"' || ch.is_control()) {
        return Err(Error::new(
            ErrorKind::Query,
            "Oracle 账号名称不能包含双引号或控制字符",
        ));
    }
    Ok(())
}

fn validate_oracle_password(password: &str) -> Result<()> {
    if password.is_empty() {
        return Err(Error::new(ErrorKind::Query, "Oracle 密码不能为空"));
    }
    if password.chars().count() > 128 || password.chars().any(char::is_control) {
        return Err(Error::new(
            ErrorKind::Query,
            "Oracle 密码不能超过 128 个字符且不能包含控制字符",
        ));
    }
    Ok(())
}

/// Oracle 口令里的双引号要双写，否则会截断 `IDENTIFIED BY "..."`。
fn oracle_password_literal(password: &str) -> String {
    format!("\"{}\"", password.replace('"', "\"\""))
}

/// 只用于目录视图里已经出现过的名称（表空间、角色、权限名），不做语法猜测。
fn validate_oracle_catalog_name(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::new(ErrorKind::Query, format!("{label}不能为空")));
    }
    if value.chars().any(|ch| ch == '"' || ch.is_control()) {
        return Err(Error::new(
            ErrorKind::Query,
            format!("{label}不能包含双引号或控制字符"),
        ));
    }
    Ok(value.to_string())
}

/// 渲染变更计划。`mask_secrets = true` 时密码以 `********` 出现在预览里，
/// 无论哪种情况返回值都不包含可复用的真实口令副本之外的其它形态。
pub fn render_oracle_user_admin_plan(
    plan: &OracleUserAdminPlan,
    mask_secrets: bool,
) -> Result<Vec<String>> {
    render_user_admin_plan(plan, mask_secrets, UserAdminSqlDialect::Oracle)
}

/// 渲染目标库。达梦账号 DDL 与 Oracle 高度一致，差异都收敛在这里：
/// - 达梦没有 `QUOTA ... ON ...` 子句，配额留空；
/// - 达梦 `CREATE USER` 不接受 `PROFILE`（只能 `ALTER USER ... PROFILE`）；
/// - 达梦的登录权限是系统权限 `CREATE SESSION`，没有 `CONNECT` 角色。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UserAdminSqlDialect {
    #[default]
    Oracle,
    Dameng,
}

pub fn render_user_admin_plan(
    plan: &OracleUserAdminPlan,
    mask_secrets: bool,
    dialect: UserAdminSqlDialect,
) -> Result<Vec<String>> {
    let mut statements = Vec::with_capacity(plan.changes.len());
    for change in plan.ordered_changes() {
        statements.push(render_user_admin_change(change, mask_secrets, dialect)?);
    }
    Ok(statements)
}

fn render_user_admin_change(
    change: &OracleUserAdminChange,
    mask_secrets: bool,
    dialect: UserAdminSqlDialect,
) -> Result<String> {
    let quote = |value: &str| OracleDialect.quote_identifier(value);
    match change {
        OracleUserAdminChange::CreateUser {
            name,
            password,
            default_tablespace,
            temporary_tablespace,
            profile,
            quota,
            grant_connect_role,
        } => {
            validate_oracle_principal_name(name)?;
            validate_oracle_password(password)?;
            let mut clauses = vec![format!(
                "IDENTIFIED BY {}",
                oracle_password_literal(if mask_secrets { "********" } else { password })
            )];
            if let Some(tablespace) = default_tablespace
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                clauses.push(format!(
                    "DEFAULT TABLESPACE {}",
                    quote(&validate_oracle_catalog_name(tablespace, "默认表空间")?)
                ));
            }
            if let Some(tablespace) = temporary_tablespace
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                clauses.push(format!(
                    "TEMPORARY TABLESPACE {}",
                    quote(&validate_oracle_catalog_name(tablespace, "临时表空间")?)
                ));
            }
            // 达梦 CREATE USER 不接受 PROFILE，改由随后的 ALTER USER 补上。
            if dialect == UserAdminSqlDialect::Oracle
                && let Some(profile) = profile.as_deref().filter(|value| !value.trim().is_empty())
            {
                clauses.push(format!(
                    "PROFILE {}",
                    quote(&validate_oracle_catalog_name(profile, "Profile")?)
                ));
            }
            // 达梦没有 `QUOTA ... ON ...` 子句。
            if dialect == UserAdminSqlDialect::Oracle
                && let Some((tablespace, bytes)) = quota
            {
                clauses.push(format!(
                    "QUOTA {} ON {}",
                    oracle_quota_clause(*bytes),
                    quote(&validate_oracle_catalog_name(tablespace, "配额表空间")?)
                ));
            }
            let mut sql = format!("CREATE USER {} {}", quote(name.trim()), clauses.join(" "));
            if *grant_connect_role {
                // 新账号默认不能登录；显式授登录权限是一步可解释的动作。
                // 达梦没有 CONNECT 角色，对应的是系统权限 CREATE SESSION。
                let login_privilege = match dialect {
                    UserAdminSqlDialect::Oracle => "CONNECT",
                    UserAdminSqlDialect::Dameng => "CREATE SESSION",
                };
                sql.push_str(";\nGRANT ");
                sql.push_str(login_privilege);
                sql.push_str(" TO ");
                sql.push_str(&quote(name.trim()));
            }
            sql.push(';');
            // 达梦：PROFILE 只能在建号之后设置。
            if dialect == UserAdminSqlDialect::Dameng
                && let Some(profile) = profile.as_deref().filter(|value| !value.trim().is_empty())
            {
                sql.push_str(&format!(
                    "\nALTER USER {} PROFILE {};",
                    quote(name.trim()),
                    quote(&validate_oracle_catalog_name(profile, "Profile")?)
                ));
            }
            Ok(sql)
        }
        OracleUserAdminChange::DropUser { name, cascade } => {
            validate_oracle_principal_name(name)?;
            Ok(format!(
                "DROP USER {}{};",
                quote(name.trim()),
                if *cascade { " CASCADE" } else { "" }
            ))
        }
        OracleUserAdminChange::SetPassword { name, password } => {
            validate_oracle_principal_name(name)?;
            validate_oracle_password(password)?;
            Ok(format!(
                "ALTER USER {} IDENTIFIED BY {};",
                quote(name.trim()),
                oracle_password_literal(if mask_secrets { "********" } else { password })
            ))
        }
        OracleUserAdminChange::SetAccountLocked { name, locked } => {
            validate_oracle_principal_name(name)?;
            Ok(format!(
                "ALTER USER {} ACCOUNT {};",
                quote(name.trim()),
                if *locked { "LOCK" } else { "UNLOCK" }
            ))
        }
        OracleUserAdminChange::SetDefaultTablespace { name, tablespace } => {
            validate_oracle_principal_name(name)?;
            Ok(format!(
                "ALTER USER {} DEFAULT TABLESPACE {};",
                quote(name.trim()),
                quote(&validate_oracle_catalog_name(tablespace, "默认表空间")?)
            ))
        }
        OracleUserAdminChange::SetQuota {
            name,
            tablespace,
            bytes,
        } => {
            validate_oracle_principal_name(name)?;
            Ok(format!(
                "ALTER USER {} QUOTA {} ON {};",
                quote(name.trim()),
                oracle_quota_clause(*bytes),
                quote(&validate_oracle_catalog_name(tablespace, "配额表空间")?)
            ))
        }
        OracleUserAdminChange::GrantRole {
            role,
            grantee,
            admin_option,
        } => {
            validate_oracle_principal_name(grantee)?;
            let role = validate_oracle_catalog_name(role, "角色")?;
            Ok(format!(
                "GRANT {} TO {}{};",
                quote(&role),
                quote(grantee.trim()),
                if *admin_option { " WITH ADMIN OPTION" } else { "" }
            ))
        }
        OracleUserAdminChange::RevokeRole { role, grantee } => {
            validate_oracle_principal_name(grantee)?;
            let role = validate_oracle_catalog_name(role, "角色")?;
            Ok(format!(
                "REVOKE {} FROM {};",
                quote(&role),
                quote(grantee.trim())
            ))
        }
        OracleUserAdminChange::GrantSystemPrivilege {
            privilege,
            grantee,
            admin_option,
        } => {
            validate_oracle_principal_name(grantee)?;
            let privilege = validate_oracle_catalog_name(privilege, "系统权限")?;
            Ok(format!(
                "GRANT {} TO {}{};",
                privilege,
                quote(grantee.trim()),
                if *admin_option { " WITH ADMIN OPTION" } else { "" }
            ))
        }
        OracleUserAdminChange::RevokeSystemPrivilege { privilege, grantee } => {
            validate_oracle_principal_name(grantee)?;
            let privilege = validate_oracle_catalog_name(privilege, "系统权限")?;
            Ok(format!(
                "REVOKE {} FROM {};",
                privilege,
                quote(grantee.trim())
            ))
        }
        OracleUserAdminChange::GrantObjectPrivilege {
            privilege,
            owner,
            object,
            grantee,
            grant_option,
        } => {
            validate_oracle_principal_name(grantee)?;
            Ok(format!(
                "GRANT {} ON {}.{} TO {}{};",
                validate_oracle_catalog_name(privilege, "对象权限")?,
                quote(&validate_oracle_catalog_name(owner, "对象 Owner")?),
                quote(&validate_oracle_catalog_name(object, "对象名")?),
                quote(grantee.trim()),
                if *grant_option {
                    " WITH GRANT OPTION"
                } else {
                    ""
                }
            ))
        }
        OracleUserAdminChange::RevokeObjectPrivilege {
            privilege,
            owner,
            object,
            grantee,
        } => {
            validate_oracle_principal_name(grantee)?;
            Ok(format!(
                "REVOKE {} ON {}.{} FROM {};",
                validate_oracle_catalog_name(privilege, "对象权限")?,
                quote(&validate_oracle_catalog_name(owner, "对象 Owner")?),
                quote(&validate_oracle_catalog_name(object, "对象名")?),
                quote(grantee.trim())
            ))
        }
    }
}

/// `None`（无上限）对应 `UNLIMITED`，否则按字节换算成 `K`/`M` 便于阅读。
fn oracle_quota_clause(bytes: Option<u64>) -> String {
    match bytes {
        None => "UNLIMITED".to_string(),
        Some(bytes) if bytes % (1024 * 1024) == 0 => format!("{}M", bytes / (1024 * 1024)),
        Some(bytes) if bytes % 1024 == 0 => format!("{}K", bytes / 1024),
        Some(bytes) => bytes.to_string(),
    }
}

#[cfg(test)]
mod oracle_user_admin_tests {
    use super::*;

    fn plan(changes: Vec<OracleUserAdminChange>) -> OracleUserAdminPlan {
        OracleUserAdminPlan { changes }
    }

    #[test]
    fn create_user_preview_masks_password_and_states_login_role() {
        let plan = plan(vec![OracleUserAdminChange::CreateUser {
            name: "app_user".into(),
            password: "S3cret\"x".into(),
            default_tablespace: Some("USERS".into()),
            temporary_tablespace: Some("TEMP".into()),
            profile: Some("DEFAULT".into()),
            quota: Some(("USERS".into(), Some(64 * 1024 * 1024))),
            grant_connect_role: true,
        }]);
        let masked = render_oracle_user_admin_plan(&plan, true).expect("render");
        assert!(!masked.iter().any(|sql| sql.contains("S3cret")));
        assert!(masked[0].contains(r#"IDENTIFIED BY "********""#));
        assert!(masked[0].contains("QUOTA 64M ON \"USERS\""));
        assert!(masked[0].contains("GRANT CONNECT TO \"app_user\""));

        let real = render_oracle_user_admin_plan(&plan, false).expect("render");
        // 口令里的双引号必须双写，否则会提前结束引用。
        assert!(real[0].contains(r#"IDENTIFIED BY "S3cret""x""#));
    }

    #[test]
    fn drop_user_runs_after_grants() {
        let plan = plan(vec![
            OracleUserAdminChange::DropUser {
                name: "old".into(),
                cascade: true,
            },
            OracleUserAdminChange::GrantRole {
                role: "CONNECT".into(),
                grantee: "new".into(),
                admin_option: false,
            },
            OracleUserAdminChange::CreateUser {
                name: "new".into(),
                password: "pw".into(),
                default_tablespace: None,
                temporary_tablespace: None,
                profile: None,
                quota: None,
                grant_connect_role: false,
            },
        ]);
        let sql = render_oracle_user_admin_plan(&plan, true).expect("render");
        assert!(sql[0].starts_with("CREATE USER"));
        assert!(sql[1].starts_with("GRANT \"CONNECT\""));
        assert_eq!(sql[2], "DROP USER \"old\" CASCADE;");
    }

    #[test]
    fn quota_and_privilege_syntax_follows_oracle_option_names() {
        let plan = plan(vec![
            OracleUserAdminChange::SetQuota {
                name: "app".into(),
                tablespace: "USERS".into(),
                bytes: None,
            },
            OracleUserAdminChange::GrantSystemPrivilege {
                privilege: "CREATE SESSION".into(),
                grantee: "app".into(),
                admin_option: true,
            },
            OracleUserAdminChange::GrantObjectPrivilege {
                privilege: "SELECT".into(),
                owner: "SALES".into(),
                object: "ORDERS".into(),
                grantee: "app".into(),
                grant_option: true,
            },
        ]);
        let sql = render_oracle_user_admin_plan(&plan, false).expect("render");
        assert_eq!(sql[0], "ALTER USER \"app\" QUOTA UNLIMITED ON \"USERS\";");
        // 系统权限/角色用 WITH ADMIN OPTION。
        assert_eq!(
            sql[1],
            "GRANT CREATE SESSION TO \"app\" WITH ADMIN OPTION;"
        );
        // 对象权限用 WITH GRANT OPTION。
        assert_eq!(
            sql[2],
            "GRANT SELECT ON \"SALES\".\"ORDERS\" TO \"app\" WITH GRANT OPTION;"
        );
    }

    #[test]
    fn identifier_with_quote_is_rejected() {
        let plan = plan(vec![OracleUserAdminChange::DropUser {
            name: "bad\"name".into(),
            cascade: false,
        }]);
        assert!(render_oracle_user_admin_plan(&plan, false).is_err());
    }

    #[test]
    fn change_debug_never_contains_password() {
        let change = OracleUserAdminChange::SetPassword {
            name: "app".into(),
            password: "PlainText1".into(),
        };
        let text = format!("{change:?}");
        assert!(!text.contains("PlainText1"));
        assert!(text.contains("********"));
    }
}
