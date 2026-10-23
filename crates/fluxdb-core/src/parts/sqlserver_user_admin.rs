/// SQL Server 第一阶段用户与权限管理的结构化模型。
///
/// SQL Server 的 Login 属于服务器层，User/Role 属于数据库层；对象权限还区分
/// DATABASE、SCHEMA 与 OBJECT。UI 只提交结构化变更，SQL 渲染集中在此文件，
/// 连接器负责在同一事务内执行。

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SqlServerPrincipalKind {
    Login,
    User,
    Role,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SqlServerPrincipalRef {
    pub kind: SqlServerPrincipalKind,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerLogin {
    pub name: String,
    pub login_type: String,
    pub is_disabled: bool,
    pub default_database: Option<String>,
    pub default_language: Option<String>,
    pub create_date: Option<String>,
}

impl SqlServerLogin {
    pub fn is_sql_login(&self) -> bool {
        self.login_type.eq_ignore_ascii_case("SQL_LOGIN")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerDatabaseUser {
    pub name: String,
    pub user_type: String,
    pub login_name: Option<String>,
    pub default_schema: Option<String>,
    pub create_date: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerDatabaseRole {
    pub name: String,
    pub owner: Option<String>,
    pub is_fixed: bool,
    pub create_date: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerServerRole {
    pub name: String,
    pub is_fixed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlServerRoleScope {
    Server,
    Database,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerRoleMembership {
    pub scope: SqlServerRoleScope,
    pub role: String,
    pub member: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlServerPermissionState {
    Grant,
    GrantWithGrantOption,
    Deny,
}

impl SqlServerPermissionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Grant => "GRANT",
            Self::GrantWithGrantOption => "GRANT WITH GRANT OPTION",
            Self::Deny => "DENY",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqlServerPermissionTarget {
    Server,
    Database,
    Schema { schema: String },
    Object { schema: String, object: String },
}

impl SqlServerPermissionTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Server => "SERVER".to_string(),
            Self::Database => "DATABASE".to_string(),
            Self::Schema { schema } => format!("SCHEMA::{schema}"),
            Self::Object { schema, object } => format!("OBJECT::{schema}.{object}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerPermission {
    pub target: SqlServerPermissionTarget,
    pub permission: String,
    pub state: SqlServerPermissionState,
    pub grantor: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlServerSecuritySnapshot {
    pub schemas: Vec<String>,
    pub database: String,
    pub current_login: String,
    pub current_user: String,
    pub logins: Vec<SqlServerLogin>,
    pub users: Vec<SqlServerDatabaseUser>,
    pub server_roles: Vec<SqlServerServerRole>,
    pub database_roles: Vec<SqlServerDatabaseRole>,
    pub memberships: Vec<SqlServerRoleMembership>,
}

impl SqlServerSecuritySnapshot {
    pub fn contains_principal(&self, principal: &SqlServerPrincipalRef) -> bool {
        match principal.kind {
            SqlServerPrincipalKind::Login => self
                .logins
                .iter()
                .any(|login| login.name.eq_ignore_ascii_case(&principal.name)),
            SqlServerPrincipalKind::User => self
                .users
                .iter()
                .any(|user| user.name.eq_ignore_ascii_case(&principal.name)),
            SqlServerPrincipalKind::Role => self
                .database_roles
                .iter()
                .any(|role| role.name.eq_ignore_ascii_case(&principal.name)),
        }
    }

    pub fn has_server_role(&self, role: &str, login: &str) -> bool {
        self.memberships.iter().any(|membership| {
            membership.scope == SqlServerRoleScope::Server
                && membership.role.eq_ignore_ascii_case(role)
                && membership.member.eq_ignore_ascii_case(login)
        })
    }

    pub fn has_database_role(&self, role: &str, member: &str) -> bool {
        self.memberships.iter().any(|membership| {
            membership.scope == SqlServerRoleScope::Database
                && membership.role.eq_ignore_ascii_case(role)
                && membership.member.eq_ignore_ascii_case(member)
        })
    }
}

/// 单条安全变更。执行顺序由计划列表决定；同一目标的后置变更会覆盖前置语义。
#[derive(Clone, PartialEq, Eq)]
pub enum SqlServerSecurityChange {
    CreateLogin {
        name: String,
        password: String,
        default_database: Option<String>,
        disabled: bool,
    },
    SetLoginEnabled {
        name: String,
        enabled: bool,
    },
    SetLoginPassword {
        name: String,
        password: String,
    },
    SetLoginDefaultDatabase {
        name: String,
        database: Option<String>,
    },
    DropLogin {
        name: String,
    },
    CreateUser {
        name: String,
        login: Option<String>,
        default_schema: Option<String>,
    },
    SetUserLogin {
        name: String,
        login: Option<String>,
    },
    SetUserDefaultSchema {
        name: String,
        schema: Option<String>,
    },
    DropUser {
        name: String,
    },
    CreateDatabaseRole {
        name: String,
    },
    DropDatabaseRole {
        name: String,
    },
    AddRoleMember {
        scope: SqlServerRoleScope,
        role: String,
        member: String,
    },
    DropRoleMember {
        scope: SqlServerRoleScope,
        role: String,
        member: String,
    },
    GrantPermission {
        grantee: String,
        target: SqlServerPermissionTarget,
        permission: String,
        state: SqlServerPermissionState,
        grant_option: bool,
        /// 同一目标已有显式 GRANT/DENY 时，先 REVOKE 再写入新状态。
        replace_existing: bool,
        /// 替换带 GRANT OPTION 的旧权限时，撤销必须带 CASCADE。
        cascade_revoke: bool,
    },
    RevokePermission {
        grantee: String,
        target: SqlServerPermissionTarget,
        permission: String,
        cascade: bool,
    },
    RevokeGrantOption {
        grantee: String,
        target: SqlServerPermissionTarget,
        permission: String,
        cascade: bool,
    },
}

impl std::fmt::Debug for SqlServerSecurityChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateLogin {
                name,
                default_database,
                disabled,
                ..
            } => f
                .debug_struct("CreateLogin")
                .field("name", name)
                .field("password", &"********")
                .field("default_database", default_database)
                .field("disabled", disabled)
                .finish(),
            Self::SetLoginPassword { name, .. } => f
                .debug_struct("SetLoginPassword")
                .field("name", name)
                .field("password", &"********")
                .finish(),
            other => {
                let value = match other {
                    Self::SetLoginEnabled { name, enabled } => {
                        format!("SetLoginEnabled {{ name: {name:?}, enabled: {enabled} }}")
                    }
                    Self::SetLoginDefaultDatabase { name, database } => {
                        format!("SetLoginDefaultDatabase {{ name: {name:?}, database: {database:?} }}")
                    }
                    Self::DropLogin { name } => format!("DropLogin {{ name: {name:?} }}"),
                    Self::CreateUser {
                        name,
                        login,
                        default_schema,
                    } => format!(
                        "CreateUser {{ name: {name:?}, login: {login:?}, default_schema: {default_schema:?} }}"
                    ),
                    Self::SetUserLogin { name, login } => {
                        format!("SetUserLogin {{ name: {name:?}, login: {login:?} }}")
                    }
                    Self::SetUserDefaultSchema { name, schema } => {
                        format!("SetUserDefaultSchema {{ name: {name:?}, schema: {schema:?} }}")
                    }
                    Self::DropUser { name } => format!("DropUser {{ name: {name:?} }}"),
                    Self::CreateDatabaseRole { name } => {
                        format!("CreateDatabaseRole {{ name: {name:?} }}")
                    }
                    Self::DropDatabaseRole { name } => {
                        format!("DropDatabaseRole {{ name: {name:?} }}")
                    }
                    Self::AddRoleMember { scope, role, member } => {
                        format!("AddRoleMember {{ scope: {scope:?}, role: {role:?}, member: {member:?} }}")
                    }
                    Self::DropRoleMember { scope, role, member } => {
                        format!("DropRoleMember {{ scope: {scope:?}, role: {role:?}, member: {member:?} }}")
                    }
                    Self::GrantPermission {
                        grantee,
                        target,
                        permission,
                        state,
                        grant_option,
                        replace_existing,
                        cascade_revoke,
                    } => format!(
                        "GrantPermission {{ grantee: {grantee:?}, target: {target:?}, permission: {permission:?}, state: {state:?}, grant_option: {grant_option}, replace_existing: {replace_existing}, cascade_revoke: {cascade_revoke} }}"
                    ),
                    Self::RevokePermission {
                        grantee,
                        target,
                        permission,
                        cascade,
                    } => format!(
                        "RevokePermission {{ grantee: {grantee:?}, target: {target:?}, permission: {permission:?}, cascade: {cascade} }}"
                    ),
                    Self::RevokeGrantOption {
                        grantee,
                        target,
                        permission,
                        cascade,
                    } => format!(
                        "RevokeGrantOption {{ grantee: {grantee:?}, target: {target:?}, permission: {permission:?}, cascade: {cascade} }}"
                    ),
                    Self::CreateLogin { .. } | Self::SetLoginPassword { .. } => unreachable!(),
                };
                f.write_str(&value)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlServerSecurityPlan {
    pub database: String,
    pub principal: String,
    pub changes: Vec<SqlServerSecurityChange>,
}

impl SqlServerSecurityPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// 按 SQL Server 对象依赖顺序返回计划；排序稳定，同一阶段保留用户操作顺序。
    ///
    /// 主体必须先于其角色成员和权限存在；同一对象上的撤销必须先于重新授予，
    /// 删除必须放在所有引用清理之后。
    pub fn ordered_changes(&self) -> Vec<&SqlServerSecurityChange> {
        let mut creates = Vec::new();
        let mut remaining = Vec::new();
        for change in &self.changes {
            if matches!(
                change,
                SqlServerSecurityChange::CreateLogin { .. }
                    | SqlServerSecurityChange::CreateUser { .. }
                    | SqlServerSecurityChange::CreateDatabaseRole { .. }
            ) {
                creates.push(change);
            } else {
                remaining.push(change);
            }
        }
        creates.extend(remaining);
        creates
    }
}

pub const SQLSERVER_SERVER_PERMISSIONS: &[&str] = &[
    "ALTER ANY CONNECTION",
    "ALTER ANY DATABASE",
    "ALTER ANY LOGIN",
    "ALTER ANY SERVER ROLE",
    "AUTHENTICATE SERVER",
    "CONNECT SQL",
    "CONTROL SERVER",
    "CREATE ANY DATABASE",
    "VIEW ANY DATABASE",
    "VIEW ANY DEFINITION",
    "VIEW SERVER STATE",
];

pub const SQLSERVER_DATABASE_PERMISSIONS: &[&str] = &[
    "ALTER",
    "CONNECT",
    "CONTROL",
    "CREATE FUNCTION",
    "CREATE PROCEDURE",
    "CREATE SCHEMA",
    "CREATE TABLE",
    "CREATE VIEW",
    "DELETE",
    "EXECUTE",
    "INSERT",
    "REFERENCES",
    "SELECT",
    "TAKE OWNERSHIP",
    "UPDATE",
    "VIEW DEFINITION",
];

pub const SQLSERVER_SCHEMA_PERMISSIONS: &[&str] = &[
    "ALTER",
    "CONTROL",
    "DELETE",
    "EXECUTE",
    "INSERT",
    "REFERENCES",
    "SELECT",
    "TAKE OWNERSHIP",
    "UPDATE",
    "VIEW DEFINITION",
];

pub const SQLSERVER_OBJECT_PERMISSIONS: &[&str] = &[
    "ALTER",
    "CONTROL",
    "DELETE",
    "EXECUTE",
    "INSERT",
    "REFERENCES",
    "SELECT",
    "TAKE OWNERSHIP",
    "UPDATE",
    "VIEW DEFINITION",
];

pub fn sqlserver_permissions_for_target(target: &SqlServerPermissionTarget) -> &'static [&'static str] {
    match target {
        SqlServerPermissionTarget::Server => SQLSERVER_SERVER_PERMISSIONS,
        SqlServerPermissionTarget::Database => SQLSERVER_DATABASE_PERMISSIONS,
        SqlServerPermissionTarget::Schema { .. } => SQLSERVER_SCHEMA_PERMISSIONS,
        SqlServerPermissionTarget::Object { .. } => SQLSERVER_OBJECT_PERMISSIONS,
    }
}

pub fn validate_sqlserver_principal_name(name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::new(ErrorKind::Query, "SQL Server 主体名称不能为空"));
    }
    if name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(Error::new(
            ErrorKind::Query,
            "SQL Server 主体名称最长 128 个字符且不能包含控制字符",
        ));
    }
    Ok(())
}

pub fn validate_sqlserver_permission(
    target: &SqlServerPermissionTarget,
    permission: &str,
) -> Result<()> {
    let permission = permission.trim().to_ascii_uppercase();
    if sqlserver_permissions_for_target(target).contains(&permission.as_str()) {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Query,
        "该权限不属于所选 SQL Server 权限范围",
    ))
}

pub fn render_sqlserver_security_plan(
    plan: &SqlServerSecurityPlan,
    mask_secrets: bool,
) -> Result<Vec<String>> {
    if plan.database.trim().is_empty() {
        return Err(Error::new(ErrorKind::Query, "SQL Server 安全计划缺少数据库"));
    }
    let ordered = plan.ordered_changes();
    let mut statements = Vec::with_capacity(ordered.len());
    for change in ordered {
        statements.push(render_sqlserver_security_change(
            plan,
            change,
            mask_secrets,
        )?);
    }
    Ok(statements)
}

fn render_sqlserver_security_change(
    plan: &SqlServerSecurityPlan,
    change: &SqlServerSecurityChange,
    mask_secrets: bool,
) -> Result<String> {
    let dialect = SqlServerDialect;
    let quote = |value: &str| dialect.quote_identifier(value);
    match change {
        SqlServerSecurityChange::CreateLogin {
            name,
            password,
            default_database,
            disabled,
        } => {
            validate_sqlserver_principal_name(name)?;
            if password.is_empty() {
                return Err(Error::new(ErrorKind::Query, "SQL Server 登录密码不能为空"));
            }
            if password.chars().count() > 128 {
                return Err(Error::new(
                    ErrorKind::Query,
                    "SQL Server 登录密码不能超过 128 个字符",
                ));
            }
            let mut options = vec![format!(
                "PASSWORD = {}",
                sqlserver_string_literal(if mask_secrets { "********" } else { password })
            )];
            if let Some(database) = default_database
                .as_deref()
                .map(str::trim)
                .filter(|database| !database.is_empty())
            {
                options.push(format!("DEFAULT_DATABASE = {}", quote(database)));
            }
            options.push("CHECK_POLICY = ON".to_string());
            let mut sql = format!(
                "CREATE LOGIN {} WITH {}",
                quote(name.trim()),
                options.join(", ")
            );
            if *disabled {
                sql.push_str(";\nALTER LOGIN ");
                sql.push_str(&quote(name.trim()));
                sql.push_str(" DISABLE");
            }
            sql.push(';');
            Ok(sql)
        }
        SqlServerSecurityChange::SetLoginEnabled { name, enabled } => {
            validate_sqlserver_principal_name(name)?;
            Ok(format!(
                "ALTER LOGIN {} {};",
                quote(name.trim()),
                if *enabled { "ENABLE" } else { "DISABLE" }
            ))
        }
        SqlServerSecurityChange::SetLoginPassword { name, password } => {
            validate_sqlserver_principal_name(name)?;
            if password.is_empty() {
                return Err(Error::new(ErrorKind::Query, "SQL Server 登录密码不能为空"));
            }
            if password.chars().count() > 128 {
                return Err(Error::new(
                    ErrorKind::Query,
                    "SQL Server 登录密码不能超过 128 个字符",
                ));
            }
            Ok(format!(
                "ALTER LOGIN {} WITH PASSWORD = {};",
                quote(name.trim()),
                sqlserver_string_literal(if mask_secrets { "********" } else { password })
            ))
        }
        SqlServerSecurityChange::SetLoginDefaultDatabase { name, database } => {
            validate_sqlserver_principal_name(name)?;
            let database = database
                .as_deref()
                .map(str::trim)
                .filter(|database| !database.is_empty())
                .ok_or_else(|| Error::new(ErrorKind::Query, "默认数据库不能为空"))?;
            Ok(format!(
                "ALTER LOGIN {} WITH DEFAULT_DATABASE = {};",
                quote(name.trim()),
                quote(database)
            ))
        }
        SqlServerSecurityChange::DropLogin { name } => {
            validate_sqlserver_principal_name(name)?;
            Ok(format!("DROP LOGIN {};", quote(name.trim())))
        }
        SqlServerSecurityChange::CreateUser {
            name,
            login,
            default_schema,
        } => {
            validate_sqlserver_principal_name(name)?;
            let mut sql = format!("CREATE USER {}", quote(name.trim()));
            match login.as_deref().map(str::trim).filter(|login| !login.is_empty()) {
                Some(login) => {
                    validate_sqlserver_principal_name(login)?;
                    sql.push_str(&format!(" FOR LOGIN {}", quote(login)));
                }
                None => sql.push_str(" WITHOUT LOGIN"),
            }
            if let Some(schema) = default_schema
                .as_deref()
                .map(str::trim)
                .filter(|schema| !schema.is_empty())
            {
                sql.push_str(&format!(" WITH DEFAULT_SCHEMA = {}", quote(schema)));
            }
            sql.push(';');
            Ok(sql)
        }
        SqlServerSecurityChange::SetUserLogin { name, login } => {
            validate_sqlserver_principal_name(name)?;
            let login = login
                .as_deref()
                .map(str::trim)
                .filter(|login| !login.is_empty())
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Unsupported,
                        "第一阶段不支持把已有 SQL Server 用户改为 WITHOUT LOGIN",
                    )
                })?;
            validate_sqlserver_principal_name(login)?;
            Ok(format!(
                "ALTER USER {} WITH LOGIN = {};",
                quote(name.trim()),
                quote(login)
            ))
        }
        SqlServerSecurityChange::SetUserDefaultSchema { name, schema } => {
            validate_sqlserver_principal_name(name)?;
            let schema = schema
                .as_deref()
                .map(str::trim)
                .filter(|schema| !schema.is_empty())
                .ok_or_else(|| Error::new(ErrorKind::Query, "默认 Schema 不能为空"))?;
            Ok(format!(
                "ALTER USER {} WITH DEFAULT_SCHEMA = {};",
                quote(name.trim()),
                quote(schema)
            ))
        }
        SqlServerSecurityChange::DropUser { name } => {
            validate_sqlserver_principal_name(name)?;
            Ok(format!("DROP USER {};", quote(name.trim())))
        }
        SqlServerSecurityChange::CreateDatabaseRole { name } => {
            validate_sqlserver_principal_name(name)?;
            Ok(format!("CREATE ROLE {};", quote(name.trim())))
        }
        SqlServerSecurityChange::DropDatabaseRole { name } => {
            validate_sqlserver_principal_name(name)?;
            Ok(format!("DROP ROLE {};", quote(name.trim())))
        }
        SqlServerSecurityChange::AddRoleMember { scope, role, member } => {
            validate_sqlserver_principal_name(role)?;
            validate_sqlserver_principal_name(member)?;
            Ok(format!(
                "{} {} ADD MEMBER {};",
                match scope {
                    SqlServerRoleScope::Server => "ALTER SERVER ROLE",
                    SqlServerRoleScope::Database => "ALTER ROLE",
                },
                quote(role.trim()),
                quote(member.trim())
            ))
        }
        SqlServerSecurityChange::DropRoleMember { scope, role, member } => {
            validate_sqlserver_principal_name(role)?;
            validate_sqlserver_principal_name(member)?;
            Ok(format!(
                "{} {} DROP MEMBER {};",
                match scope {
                    SqlServerRoleScope::Server => "ALTER SERVER ROLE",
                    SqlServerRoleScope::Database => "ALTER ROLE",
                },
                quote(role.trim()),
                quote(member.trim())
            ))
        }
        SqlServerSecurityChange::GrantPermission {
            grantee,
            target,
            permission,
            state,
            grant_option,
            replace_existing,
            cascade_revoke,
        } => {
            validate_sqlserver_principal_name(grantee)?;
            validate_sqlserver_permission(target, permission)?;
            let permission = permission.trim().to_ascii_uppercase();
            let target = render_sqlserver_permission_target(plan, target)?;
            let grant_option = *grant_option
                || matches!(state, SqlServerPermissionState::GrantWithGrantOption);
            let state_keyword = match state {
                SqlServerPermissionState::Grant | SqlServerPermissionState::GrantWithGrantOption => {
                    "GRANT"
                }
                SqlServerPermissionState::Deny => "DENY",
            };
            let grant_option = if grant_option {
                " WITH GRANT OPTION"
            } else {
                ""
            };
            let grant = format!(
                "{state_keyword} {permission}{target} TO {}{grant_option};",
                quote(grantee.trim())
            );
            if *replace_existing {
                Ok(format!(
                    "REVOKE {permission}{target} FROM {}{};\n{grant}",
                    quote(grantee.trim()),
                    if *cascade_revoke { " CASCADE" } else { "" }
                ))
            } else {
                Ok(grant)
            }
        }
        SqlServerSecurityChange::RevokePermission {
            grantee,
            target,
            permission,
            cascade,
        } => {
            validate_sqlserver_principal_name(grantee)?;
            validate_sqlserver_permission(target, permission)?;
            Ok(format!(
                "REVOKE {}{} FROM {}{};",
                permission.trim().to_ascii_uppercase(),
                render_sqlserver_permission_target(plan, target)?,
                quote(grantee.trim()),
                if *cascade { " CASCADE" } else { "" }
            ))
        }
        SqlServerSecurityChange::RevokeGrantOption {
            grantee,
            target,
            permission,
            cascade,
        } => {
            validate_sqlserver_principal_name(grantee)?;
            validate_sqlserver_permission(target, permission)?;
            Ok(format!(
                "REVOKE GRANT OPTION FOR {}{} FROM {}{};",
                permission.trim().to_ascii_uppercase(),
                render_sqlserver_permission_target(plan, target)?,
                quote(grantee.trim()),
                if *cascade { " CASCADE" } else { "" }
            ))
        }
    }
}

fn render_sqlserver_permission_target(
    plan: &SqlServerSecurityPlan,
    target: &SqlServerPermissionTarget,
) -> Result<String> {
    let dialect = SqlServerDialect;
    Ok(match target {
        SqlServerPermissionTarget::Server => String::new(),
        SqlServerPermissionTarget::Database => {
            format!(
                " ON DATABASE::{}",
                dialect.quote_identifier(plan.database.trim())
            )
        }
        SqlServerPermissionTarget::Schema { schema } => {
            validate_sqlserver_principal_name(schema)?;
            format!(" ON SCHEMA::{}", dialect.quote_identifier(schema.trim()))
        }
        SqlServerPermissionTarget::Object { schema, object } => {
            validate_sqlserver_principal_name(schema)?;
            validate_sqlserver_principal_name(object)?;
            format!(
                " ON OBJECT::{}.{}",
                dialect.quote_identifier(schema.trim()),
                dialect.quote_identifier(object.trim())
            )
        }
    })
}

/// 渲染 SQL Server 字符串字面量，包含 `N` 前缀与单引号转义。
pub fn sqlserver_string_literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod sqlserver_user_admin_tests {
    use super::*;

    fn plan(changes: Vec<SqlServerSecurityChange>) -> SqlServerSecurityPlan {
        SqlServerSecurityPlan {
            database: "fluxdb_demo".into(),
            principal: "demo".into(),
            changes,
        }
    }

    #[test]
    fn renders_login_and_user_management_with_quoted_identifiers() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![
                SqlServerSecurityChange::CreateLogin {
                    name: "demo]login".into(),
                    password: "p'ass".into(),
                    default_database: Some("fluxdb_demo".into()),
                    disabled: true,
                },
                SqlServerSecurityChange::CreateUser {
                    name: "demo_user".into(),
                    login: Some("demo]login".into()),
                    default_schema: Some("app".into()),
                },
                SqlServerSecurityChange::AddRoleMember {
                    scope: SqlServerRoleScope::Database,
                    role: "db_datareader".into(),
                    member: "demo_user".into(),
                },
            ]),
            false,
        )
        .expect("render plan");

        assert!(statements[0].contains("[demo]]login]"));
        assert!(statements[0].contains("N'p''ass'"));
        assert!(statements[0].contains("ALTER LOGIN [demo]]login] DISABLE"));
        assert!(statements[1].contains("FOR LOGIN [demo]]login]"));
        assert_eq!(
            statements[2],
            "ALTER ROLE [db_datareader] ADD MEMBER [demo_user];"
        );
    }

    #[test]
    fn create_statements_precede_permissions_and_role_membership() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![
                SqlServerSecurityChange::GrantPermission {
                    grantee: "test2".into(),
                    target: SqlServerPermissionTarget::Server,
                    permission: "CONTROL SERVER".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::CreateLogin {
                    name: "test2".into(),
                    password: "Fsw!123".into(),
                    default_database: Some("fluxdb_demo".into()),
                    disabled: false,
                },
                SqlServerSecurityChange::AddRoleMember {
                    scope: SqlServerRoleScope::Server,
                    role: "sysadmin".into(),
                    member: "test2".into(),
                },
            ]),
            false,
        )
        .expect("render ordered plan");

        assert!(statements[0].starts_with("CREATE LOGIN [test2]"));
        assert_eq!(statements[1], "GRANT CONTROL SERVER TO [test2];");
        assert_eq!(statements[2], "ALTER SERVER ROLE [sysadmin] ADD MEMBER [test2];");
    }

    #[test]
    fn renders_server_database_schema_and_object_permissions() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![
                SqlServerSecurityChange::GrantPermission {
                    grantee: "demo_login".into(),
                    target: SqlServerPermissionTarget::Server,
                    permission: "CONNECT SQL".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: "demo_user".into(),
                    target: SqlServerPermissionTarget::Database,
                    permission: "CONNECT".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: "demo_user".into(),
                    target: SqlServerPermissionTarget::Schema {
                        schema: "app".into(),
                    },
                    permission: "SELECT".into(),
                    state: SqlServerPermissionState::Deny,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: "demo_user".into(),
                    target: SqlServerPermissionTarget::Object {
                        schema: "app".into(),
                        object: "customers".into(),
                    },
                    permission: "UPDATE".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: true,
                    replace_existing: false,
                    cascade_revoke: false,
                },
            ]),
            false,
        )
        .expect("render permissions");

        assert_eq!(statements[0], "GRANT CONNECT SQL TO [demo_login];");
        assert_eq!(
            statements[1],
            "GRANT CONNECT ON DATABASE::[fluxdb_demo] TO [demo_user];"
        );
        assert_eq!(
            statements[2],
            "DENY SELECT ON SCHEMA::[app] TO [demo_user];"
        );
        assert_eq!(
            statements[3],
            "GRANT UPDATE ON OBJECT::[app].[customers] TO [demo_user] WITH GRANT OPTION;"
        );
    }

    #[test]
    fn rejects_permissions_outside_the_target_scope() {
        let error = render_sqlserver_security_plan(
            &plan(vec![SqlServerSecurityChange::GrantPermission {
                grantee: "demo_user".into(),
                target: SqlServerPermissionTarget::Database,
                permission: "CONTROL SERVER".into(),
                state: SqlServerPermissionState::Grant,
                grant_option: false,
                replace_existing: false,
                cascade_revoke: false,
            }]),
            false,
        )
        .expect_err("server permission must not be accepted for database target");

        assert_eq!(error.kind, ErrorKind::Query);
    }

    #[test]
    fn masks_passwords_in_preview() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![
                SqlServerSecurityChange::CreateLogin {
                    name: "demo_login".into(),
                    password: "secret".into(),
                    default_database: None,
                    disabled: false,
                },
                SqlServerSecurityChange::SetLoginPassword {
                    name: "demo_login".into(),
                    password: "another-secret".into(),
                },
            ]),
            true,
        )
        .expect("render masked plan");

        assert!(statements.iter().all(|statement| !statement.contains("secret")));
        assert!(statements[0].contains("N'********'"));
        assert!(statements[1].contains("N'********'"));
    }

    #[test]
    fn replacing_an_explicit_permission_revokes_before_granting() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![SqlServerSecurityChange::GrantPermission {
                grantee: "demo_user".into(),
                target: SqlServerPermissionTarget::Database,
                permission: "CONNECT".into(),
                state: SqlServerPermissionState::Grant,
                grant_option: false,
                replace_existing: true,
                cascade_revoke: false,
            }]),
            false,
        )
        .expect("replace permission");

        assert_eq!(
            statements[0],
            "REVOKE CONNECT ON DATABASE::[fluxdb_demo] FROM [demo_user];\nGRANT CONNECT ON DATABASE::[fluxdb_demo] TO [demo_user];"
        );
    }

    #[test]
    fn downgrading_grant_option_keeps_the_base_grant() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![SqlServerSecurityChange::RevokeGrantOption {
                grantee: "demo_user".into(),
                target: SqlServerPermissionTarget::Object {
                    schema: "app".into(),
                    object: "customers".into(),
                },
                permission: "SELECT".into(),
                cascade: true,
            }]),
            false,
        )
        .expect("revoke grant option");

        assert_eq!(
            statements[0],
            "REVOKE GRANT OPTION FOR SELECT ON OBJECT::[app].[customers] FROM [demo_user] CASCADE;"
        );
    }

    #[test]
    fn replacing_a_grantable_permission_uses_cascade() {
        let statements = render_sqlserver_security_plan(
            &plan(vec![SqlServerSecurityChange::GrantPermission {
                grantee: "demo_user".into(),
                target: SqlServerPermissionTarget::Object {
                    schema: "app".into(),
                    object: "customers".into(),
                },
                permission: "SELECT".into(),
                state: SqlServerPermissionState::Deny,
                grant_option: false,
                replace_existing: true,
                cascade_revoke: true,
            }]),
            false,
        )
        .expect("replace grantable permission");

        assert_eq!(
            statements[0],
            "REVOKE SELECT ON OBJECT::[app].[customers] FROM [demo_user] CASCADE;\nDENY SELECT ON OBJECT::[app].[customers] TO [demo_user];"
        );
    }

    #[test]
    fn debug_redacts_login_passwords() {
        let change = SqlServerSecurityChange::SetLoginPassword {
            name: "demo_login".into(),
            password: "never-log-this".into(),
        };

        let debug = format!("{change:?}");
        assert!(!debug.contains("never-log-this"));
        assert!(debug.contains("********"));
    }

    #[test]
    fn sqlserver_uses_dedicated_security_state_not_mysql_provider() {
        assert!(supports_database_user_admin(DatabaseKind::SqlServer));
        assert!(database_user_admin_provider(DatabaseKind::SqlServer).is_none());
    }
}
