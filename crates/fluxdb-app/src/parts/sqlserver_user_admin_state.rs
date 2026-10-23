#[cfg(test)]
use fluxdb_core::SqlServerRoleMembership;
use fluxdb_core::{
    SqlServerPermission, SqlServerPermissionState, SqlServerPermissionTarget,
    SqlServerPrincipalKind, SqlServerPrincipalRef, SqlServerRoleScope, SqlServerSecurityChange,
    SqlServerSecurityPlan, SqlServerSecuritySnapshot,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SqlServerSecurityListFilter {
    #[default]
    All,
    Logins,
    Users,
    Roles,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SqlServerSecurityFormKind {
    #[default]
    None,
    CreateLogin,
    CreateUser,
    CreateRole,
    EditLogin,
    EditUser,
    GrantPermission,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlServerSecurityFormField {
    Name,
    Password,
    ConfirmPassword,
    Login,
    DefaultValue,
    PermissionScope,
    PermissionTarget,
    Permission,
    PermissionState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlServerSecurityFormFlag {
    GrantOption,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct SqlServerSecurityForm {
    pub kind: SqlServerSecurityFormKind,
    pub name: String,
    pub password: String,
    pub confirm_password: String,
    pub login: String,
    pub default_value: String,
    pub permission_scope: String,
    pub permission_target: String,
    pub permission: String,
    pub permission_state: String,
    pub grant_option: bool,
}

impl std::fmt::Debug for SqlServerSecurityForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlServerSecurityForm")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("password", &"********")
            .field("confirm_password", &"********")
            .field("login", &self.login)
            .field("default_value", &self.default_value)
            .field("permission_scope", &self.permission_scope)
            .field("permission_target", &self.permission_target)
            .field("permission", &self.permission)
            .field("permission_state", &self.permission_state)
            .field("grant_option", &self.grant_option)
            .finish()
    }
}

impl SqlServerSecurityForm {
    pub fn open(kind: SqlServerSecurityFormKind) -> Self {
        let mut form = Self {
            kind,
            permission_scope: "DATABASE".to_string(),
            permission_state: "GRANT".to_string(),
            ..Self::default()
        };
        match kind {
            SqlServerSecurityFormKind::CreateLogin => {
                form.permission_scope = "SERVER".to_string();
            }
            SqlServerSecurityFormKind::CreateUser
            | SqlServerSecurityFormKind::CreateRole
            | SqlServerSecurityFormKind::GrantPermission
            | SqlServerSecurityFormKind::EditUser => {}
            _ => {}
        }
        form
    }

    pub fn is_open(&self) -> bool {
        self.kind != SqlServerSecurityFormKind::None
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn set_field(&mut self, field: SqlServerSecurityFormField, value: String) -> bool {
        let target = match field {
            SqlServerSecurityFormField::Name => &mut self.name,
            SqlServerSecurityFormField::Password => &mut self.password,
            SqlServerSecurityFormField::ConfirmPassword => &mut self.confirm_password,
            SqlServerSecurityFormField::Login => &mut self.login,
            SqlServerSecurityFormField::DefaultValue => &mut self.default_value,
            SqlServerSecurityFormField::PermissionScope => &mut self.permission_scope,
            SqlServerSecurityFormField::PermissionTarget => &mut self.permission_target,
            SqlServerSecurityFormField::Permission => &mut self.permission,
            SqlServerSecurityFormField::PermissionState => &mut self.permission_state,
        };
        if *target == value {
            return false;
        }
        *target = value;
        true
    }

    pub fn set_flag(&mut self, flag: SqlServerSecurityFormFlag, value: bool) -> bool {
        match flag {
            SqlServerSecurityFormFlag::GrantOption => {
                if self.grant_option == value {
                    return false;
                }
                self.grant_option = value;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SqlServerSecuritySaveStatus {
    #[default]
    Idle,
    Saving,
    NeedsVerify,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlServerSecurityState {
    pub revision: u64,
    pub database: String,
    pub databases: Vec<String>,
    pub snapshot: Option<SqlServerSecuritySnapshot>,
    pub selected: Option<SqlServerPrincipalRef>,
    pub permissions: Vec<SqlServerPermission>,
    pub permissions_loaded_for: Option<SqlServerPrincipalRef>,
    pub loading: bool,
    pub loading_permissions: bool,
    pub error: Option<UserFacingError>,
    pub permissions_error: Option<UserFacingError>,
    pub list_filter: SqlServerSecurityListFilter,
    /// 主体常规/成员等编辑表单；权限编辑器独立保存，避免打开权限页覆盖主体表单。
    pub form: SqlServerSecurityForm,
    /// 权限页内联编辑器的草稿；kind=GrantPermission 表示面板已展开。
    pub permission_form: SqlServerSecurityForm,
    pub pending_changes: Vec<SqlServerSecurityChange>,
    pub preview: Option<Vec<String>>,
    pub preview_loading: bool,
    pub preview_error: Option<UserFacingError>,
    pub applying: bool,
    pub apply_error: Option<UserFacingError>,
    pub save_status: SqlServerSecuritySaveStatus,
    pub pending_delete: Option<SqlServerPrincipalRef>,
}

impl SqlServerSecurityState {
    pub fn invalidate_preview(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.preview = None;
        self.preview_error = None;
    }
    pub fn has_form_changes(&self) -> bool {
        let form = &self.form;
        match form.kind {
            SqlServerSecurityFormKind::None => false,
            SqlServerSecurityFormKind::CreateLogin => {
                if let Some(SqlServerSecurityChange::CreateLogin {
                    password,
                    default_database,
                    ..
                }) = self.pending_creation_change()
                {
                    let default_value = form.default_value.trim();
                    let default_value = (!default_value.is_empty()).then_some(default_value);
                    return form.password != *password
                        || form.confirm_password != *password
                        || default_value != default_database.as_deref();
                }
                !form.name.is_empty()
                    || !form.password.is_empty()
                    || !form.confirm_password.is_empty()
                    || form.default_value != self.database
            }
            SqlServerSecurityFormKind::CreateUser => {
                if let Some(SqlServerSecurityChange::CreateUser {
                    login,
                    default_schema,
                    ..
                }) = self.pending_creation_change()
                {
                    let form_login = form.login.trim();
                    let form_login = (!form_login.is_empty()).then_some(form_login);
                    let form_schema = form.default_value.trim();
                    let form_schema = (!form_schema.is_empty()).then_some(form_schema);
                    return form_login != login.as_deref()
                        || form_schema != default_schema.as_deref();
                }
                !form.name.is_empty() || !form.login.is_empty() || form.default_value != "dbo"
            }
            SqlServerSecurityFormKind::CreateRole => {
                if self.pending_creation_change().is_some() {
                    return false;
                }
                !form.name.is_empty()
            }
            SqlServerSecurityFormKind::EditLogin => self.selected_login().is_some_and(|login| {
                !form.password.is_empty()
                    || !form.confirm_password.is_empty()
                    || form.default_value
                        != self
                            .effective_login_default_database(
                                &login.name,
                                login.default_database.as_deref(),
                            )
                            .unwrap_or("")
            }),
            SqlServerSecurityFormKind::EditUser => self.selected_user().is_some_and(|user| {
                form.login
                    != self
                        .effective_user_login(&user.name, user.login_name.as_deref())
                        .unwrap_or("")
                    || form.default_value
                        != self
                            .effective_user_default_schema(
                                &user.name,
                                user.default_schema.as_deref(),
                            )
                            .unwrap_or("")
            }),
            // 权限编辑器独立存放于 permission_form，不能覆盖主体表单的脏状态。
            SqlServerSecurityFormKind::GrantPermission => false,
        }
    }

    pub fn has_changes(&self) -> bool {
        self.has_pending_changes()
            || self.has_form_changes()
            || self.has_permission_form_changes()
    }

    pub fn has_permission_form_changes(&self) -> bool {
        self.permission_form.kind == SqlServerSecurityFormKind::GrantPermission
            && !self.permission_form.permission.trim().is_empty()
    }

    fn pending_creation_change(&self) -> Option<&SqlServerSecurityChange> {
        let name = self.form.name.trim();
        if name.is_empty() {
            return None;
        }
        self.pending_changes.iter().rev().find(|change| match change {
            SqlServerSecurityChange::CreateLogin { name: pending, .. } => {
                self.form.kind == SqlServerSecurityFormKind::CreateLogin && pending == name
            }
            SqlServerSecurityChange::CreateUser { name: pending, .. } => {
                self.form.kind == SqlServerSecurityFormKind::CreateUser && pending == name
            }
            SqlServerSecurityChange::CreateDatabaseRole { name: pending } => {
                self.form.kind == SqlServerSecurityFormKind::CreateRole && pending == name
            }
            _ => false,
        })
    }

    pub fn form_matches_pending_creation(&self) -> bool {
        self.pending_creation_change().is_some()
    }

    pub fn pending_creations(&self) -> Vec<SqlServerPrincipalRef> {
        self.pending_changes
            .iter()
            .filter_map(|change| match change {
                SqlServerSecurityChange::CreateLogin { name, .. } => Some(SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::Login,
                    name: name.clone(),
                }),
                SqlServerSecurityChange::CreateUser { name, .. } => Some(SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: name.clone(),
                }),
                SqlServerSecurityChange::CreateDatabaseRole { name } => Some(SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::Role,
                    name: name.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    pub fn edit_selected(&mut self) {
        if let Some(change) = self.pending_changes.iter().rev().find(|change| {
            self.selected.as_ref().is_some_and(|principal| match (change, principal.kind) {
                (
                    SqlServerSecurityChange::CreateLogin { name, .. },
                    SqlServerPrincipalKind::Login,
                )
                | (
                    SqlServerSecurityChange::CreateUser { name, .. },
                    SqlServerPrincipalKind::User,
                )
                | (
                    SqlServerSecurityChange::CreateDatabaseRole { name },
                    SqlServerPrincipalKind::Role,
                ) => name.eq_ignore_ascii_case(&principal.name),
                _ => false,
            })
        }) {
            self.form = match change {
                SqlServerSecurityChange::CreateLogin {
                    name,
                    password,
                    default_database,
                    ..
                } => SqlServerSecurityForm {
                    kind: SqlServerSecurityFormKind::CreateLogin,
                    name: name.clone(),
                    password: password.clone(),
                    confirm_password: password.clone(),
                    default_value: default_database.clone().unwrap_or_default(),
                    ..Default::default()
                },
                SqlServerSecurityChange::CreateUser {
                    name,
                    login,
                    default_schema,
                } => SqlServerSecurityForm {
                    kind: SqlServerSecurityFormKind::CreateUser,
                    name: name.clone(),
                    login: login.clone().unwrap_or_default(),
                    default_value: default_schema.clone().unwrap_or_default(),
                    ..Default::default()
                },
                SqlServerSecurityChange::CreateDatabaseRole { name } => SqlServerSecurityForm {
                    kind: SqlServerSecurityFormKind::CreateRole,
                    name: name.clone(),
                    ..Default::default()
                },
                _ => unreachable!(),
            };
            return;
        }
        let form = match self.selected_kind() {
            Some(SqlServerPrincipalKind::Login) => self
                .selected_login()
                .filter(|login| login.is_sql_login())
                .map(|login| SqlServerSecurityForm {
                    kind: SqlServerSecurityFormKind::EditLogin,
                    name: login.name.clone(),
                    default_value: self
                        .effective_login_default_database(
                            &login.name,
                            login.default_database.as_deref(),
                        )
                        .unwrap_or("")
                        .into(),
                    ..Default::default()
                }),
            Some(SqlServerPrincipalKind::User) if self.selected_user_is_editable() => {
                self.selected_user().map(|user| SqlServerSecurityForm {
                    kind: SqlServerSecurityFormKind::EditUser,
                    name: user.name.clone(),
                    login: self
                        .effective_user_login(&user.name, user.login_name.as_deref())
                        .unwrap_or("")
                        .into(),
                    default_value: self
                        .effective_user_default_schema(&user.name, user.default_schema.as_deref())
                        .unwrap_or("")
                        .into(),
                    ..Default::default()
                })
            }
            _ => None,
        };
        self.form = form.unwrap_or_default();
    }

    pub fn discard_changes(&mut self) -> fluxdb_core::Result<()> {
        if self.loading
            || self.loading_permissions
            || self.applying
            || self.preview_loading
            || self.save_status == SqlServerSecuritySaveStatus::NeedsVerify
        {
            return Err(Error::new(
                ErrorKind::Query,
                "请先刷新核实 SQL Server 提交结果",
            ));
        }
        self.pending_changes.clear();
        self.permission_form.clear();
        self.invalidate_preview();
        self.preview_error = None;
        self.apply_error = None;
        self.pending_delete = None;
        self.selected = self.selected.take().filter(|principal| {
            self.snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.contains_principal(principal))
        });
        self.edit_selected();
        Ok(())
    }

    pub fn restore_failed_form(&mut self) {
        let creation = self.pending_changes.iter().find_map(|change| match change {
            SqlServerSecurityChange::CreateLogin {
                name,
                password,
                default_database,
                ..
            } => Some(SqlServerSecurityForm {
                kind: SqlServerSecurityFormKind::CreateLogin,
                name: name.clone(),
                password: password.clone(),
                confirm_password: password.clone(),
                default_value: default_database.clone().unwrap_or_default(),
                ..Default::default()
            }),
            SqlServerSecurityChange::CreateUser {
                name,
                login,
                default_schema,
            } => Some(SqlServerSecurityForm {
                kind: SqlServerSecurityFormKind::CreateUser,
                name: name.clone(),
                login: login.clone().unwrap_or_default(),
                default_value: default_schema.clone().unwrap_or_default(),
                ..Default::default()
            }),
            SqlServerSecurityChange::CreateDatabaseRole { name } => Some(SqlServerSecurityForm {
                kind: SqlServerSecurityFormKind::CreateRole,
                name: name.clone(),
                ..Default::default()
            }),
            _ => None,
        });
        if let Some(form) = creation {
            self.form = form;
            return;
        }
        self.edit_selected();
        if self.form.kind == SqlServerSecurityFormKind::EditLogin
            && let Some(password) = self.pending_changes.iter().find_map(|change| match change {
                SqlServerSecurityChange::SetLoginPassword { name, password }
                    if name == &self.form.name =>
                {
                    Some(password)
                }
                _ => None,
            })
        {
            self.form.password = password.clone();
            self.form.confirm_password = password.clone();
        }
    }

    /// 失败重试时改名：判断当前创建表单是否正在修正某个失败计划里的同名主体。
    fn failed_retry_original(&self, new_name: &str) -> Option<SqlServerPrincipalRef> {
        let failed = self
            .pending_changes
            .iter()
            .find_map(|change| match change {
                SqlServerSecurityChange::CreateLogin { name, .. } => {
                    Some(SqlServerPrincipalRef {
                        kind: SqlServerPrincipalKind::Login,
                        name: name.clone(),
                    })
                }
                SqlServerSecurityChange::CreateUser { name, .. } => Some(SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: name.clone(),
                }),
                SqlServerSecurityChange::CreateDatabaseRole { name } => {
                    Some(SqlServerPrincipalRef {
                        kind: SqlServerPrincipalKind::Role,
                        name: name.clone(),
                    })
                }
                _ => None,
            })?;
        // 当前表单来自失败恢复（名字仍匹配原计划）时改名，需要按原身份替换计划。
        (self.selected.as_ref() == Some(&failed) || failed.name.eq_ignore_ascii_case(new_name))
            .then_some(failed)
    }

    fn replace_creation_identity(
        &mut self,
        original: &SqlServerPrincipalRef,
        replacement: &SqlServerPrincipalRef,
    ) {
        // 只更新原主体的身份引用；角色/授权成员等依赖项一起迁移到新名字。
        for change in &mut self.pending_changes {
            match change {
                SqlServerSecurityChange::CreateLogin { name, .. }
                | SqlServerSecurityChange::SetLoginEnabled { name, .. }
                | SqlServerSecurityChange::SetLoginPassword { name, .. }
                | SqlServerSecurityChange::SetLoginDefaultDatabase { name, .. }
                    if original.kind == SqlServerPrincipalKind::Login
                        && name.eq_ignore_ascii_case(&original.name) =>
                {
                    *name = replacement.name.clone();
                }
                SqlServerSecurityChange::CreateUser { name, .. }
                | SqlServerSecurityChange::SetUserLogin { name, .. }
                | SqlServerSecurityChange::SetUserDefaultSchema { name, .. }
                    if original.kind == SqlServerPrincipalKind::User
                        && name.eq_ignore_ascii_case(&original.name) =>
                {
                    *name = replacement.name.clone();
                }
                SqlServerSecurityChange::CreateDatabaseRole { name }
                    if original.kind == SqlServerPrincipalKind::Role
                        && name.eq_ignore_ascii_case(&original.name) =>
                {
                    *name = replacement.name.clone();
                }
                SqlServerSecurityChange::AddRoleMember { member, .. }
                | SqlServerSecurityChange::DropRoleMember { member, .. }
                    if member.eq_ignore_ascii_case(&original.name) =>
                {
                    *member = replacement.name.clone();
                }
                SqlServerSecurityChange::GrantPermission { grantee, .. }
                | SqlServerSecurityChange::RevokePermission { grantee, .. }
                | SqlServerSecurityChange::RevokeGrantOption { grantee, .. }
                    if grantee.eq_ignore_ascii_case(&original.name) =>
                {
                    *grantee = replacement.name.clone();
                }
                _ => {}
            }
        }
        self.selected = Some(replacement.clone());
    }

    pub fn can_switch_database(&self) -> bool {
        !self.loading
            && !self.loading_permissions
            && !self.preview_loading
            && !self.applying
            && self.save_status == SqlServerSecuritySaveStatus::Idle
            && !self.has_changes()
            && self.pending_delete.is_none()
    }

    fn select_database(&mut self, database: String) -> fluxdb_core::Result<()> {
        if self.database == database {
            return Ok(());
        }
        if !self.can_switch_database() {
            return Err(Error::new(
                ErrorKind::Query,
                "请先完成加载、保存或取消当前 SQL Server 安全变更",
            ));
        }
        if !self.databases.contains(&database) {
            return Err(Error::new(
                ErrorKind::Query,
                "请选择列表中的 SQL Server 数据库",
            ));
        }
        self.reset_session();
        self.snapshot = None;
        self.error = None;
        self.database = database;
        Ok(())
    }

    pub fn selected_login(&self) -> Option<&fluxdb_core::SqlServerLogin> {
        let snapshot = self.snapshot.as_ref()?;
        let name = self.selected.as_ref()?.name.as_str();
        snapshot
            .logins
            .iter()
            .find(|login| login.name.eq_ignore_ascii_case(name))
    }

    pub fn selected_user(&self) -> Option<&fluxdb_core::SqlServerDatabaseUser> {
        let snapshot = self.snapshot.as_ref()?;
        let name = self.selected.as_ref()?.name.as_str();
        snapshot
            .users
            .iter()
            .find(|user| user.name.eq_ignore_ascii_case(name))
    }

    pub fn selected_role(&self) -> Option<&fluxdb_core::SqlServerDatabaseRole> {
        let snapshot = self.snapshot.as_ref()?;
        let name = self.selected.as_ref()?.name.as_str();
        snapshot
            .database_roles
            .iter()
            .find(|role| role.name.eq_ignore_ascii_case(name))
    }

    pub fn selected_kind(&self) -> Option<SqlServerPrincipalKind> {
        self.selected.as_ref().map(|principal| principal.kind)
    }

    pub fn selected_login_is_editable(&self) -> bool {
        self.selected_login().is_some_and(|login| login.is_sql_login())
            || self.pending_changes.iter().any(|change| matches!(
                change,
                SqlServerSecurityChange::CreateLogin { name, .. }
                    if self.selected.as_ref().is_some_and(|principal| {
                        principal.kind == SqlServerPrincipalKind::Login
                            && principal.name.eq_ignore_ascii_case(name)
                    })
            ))
    }

    pub fn sql_login_names(&self) -> Vec<String> {
        let mut names = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .logins
                    .iter()
                    .filter(|login| login.is_sql_login())
                    .map(|login| login.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for change in &self.pending_changes {
            if let SqlServerSecurityChange::CreateLogin { name, .. } = change
                && !names.iter().any(|candidate| candidate.eq_ignore_ascii_case(name))
            {
                names.push(name.clone());
            }
        }
        names.sort();
        names
    }

    pub fn is_known_sql_login(&self, name: &str) -> bool {
        self.sql_login_names()
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    }

    pub fn selected_user_is_editable(&self) -> bool {
        let Some(user) = self.selected_user() else {
            return self.pending_changes.iter().any(|change| {
                matches!(
                    change,
                    SqlServerSecurityChange::CreateUser { name, login, .. }
                        if self.selected.as_ref().is_some_and(|principal| {
                            principal.kind == SqlServerPrincipalKind::User
                                && principal.name.eq_ignore_ascii_case(name)
                        }) && login.as_deref().is_some_and(|login| self.is_known_sql_login(login))
                )
            });
        };
        if matches!(
            user.name.to_ascii_lowercase().as_str(),
            "dbo" | "guest" | "sys" | "information_schema"
        ) || !user.user_type.eq_ignore_ascii_case("SQL_USER")
        {
            return false;
        }
        let Some(login_name) = user.login_name.as_deref() else {
            return false;
        };
        self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .logins
                .iter()
                .any(|login| login.name.eq_ignore_ascii_case(login_name) && login.is_sql_login())
        })
    }

    pub fn has_pending_changes(&self) -> bool {
        !self.pending_changes.is_empty()
    }

    pub fn reset_session(&mut self) {
        self.invalidate_preview();
        self.selected = None;
        self.permissions.clear();
        self.permissions_loaded_for = None;
        self.loading_permissions = false;
        self.permissions_error = None;
        self.form.clear();
        self.permission_form.clear();
        self.pending_changes.clear();
        self.preview = None;
        self.preview_loading = false;
        self.preview_error = None;
        self.applying = false;
        self.apply_error = None;
        self.save_status = SqlServerSecuritySaveStatus::Idle;
        self.pending_delete = None;
    }

    pub fn effective_login_enabled(&self, name: &str, base: bool) -> bool {
        self.pending_changes
            .iter()
            .rev()
            .find_map(|change| match change {
                SqlServerSecurityChange::SetLoginEnabled {
                    name: target,
                    enabled,
                } if target.eq_ignore_ascii_case(name) => Some(*enabled),
                _ => None,
            })
            .unwrap_or(base)
    }

    pub fn effective_login_default_database<'a>(
        &'a self,
        name: &str,
        base: Option<&'a str>,
    ) -> Option<&'a str> {
        self.pending_changes
            .iter()
            .rev()
            .find_map(|change| match change {
                SqlServerSecurityChange::SetLoginDefaultDatabase {
                    name: target,
                    database,
                } if target.eq_ignore_ascii_case(name) => Some(database.as_deref()),
                _ => None,
            })
            .unwrap_or(base)
    }

    pub fn effective_user_login<'a>(
        &'a self,
        name: &str,
        base: Option<&'a str>,
    ) -> Option<&'a str> {
        self.pending_changes
            .iter()
            .rev()
            .find_map(|change| match change {
                SqlServerSecurityChange::SetUserLogin {
                    name: target,
                    login,
                } if target.eq_ignore_ascii_case(name) => Some(login.as_deref()),
                _ => None,
            })
            .unwrap_or(base)
    }

    pub fn effective_user_default_schema<'a>(
        &'a self,
        name: &str,
        base: Option<&'a str>,
    ) -> Option<&'a str> {
        self.pending_changes
            .iter()
            .rev()
            .find_map(|change| match change {
                SqlServerSecurityChange::SetUserDefaultSchema {
                    name: target,
                    schema,
                } if target.eq_ignore_ascii_case(name) => Some(schema.as_deref()),
                _ => None,
            })
            .unwrap_or(base)
    }

    pub fn effective_role_membership(
        &self,
        scope: SqlServerRoleScope,
        role: &str,
        member: &str,
    ) -> bool {
        let base = self.snapshot.as_ref().is_some_and(|snapshot| match scope {
            SqlServerRoleScope::Server => snapshot.has_server_role(role, member),
            SqlServerRoleScope::Database => snapshot.has_database_role(role, member),
        });
        self.pending_changes
            .iter()
            .rev()
            .find_map(|change| match change {
                SqlServerSecurityChange::AddRoleMember {
                    scope: target_scope,
                    role: target_role,
                    member: target_member,
                } if *target_scope == scope
                    && target_role.eq_ignore_ascii_case(role)
                    && target_member.eq_ignore_ascii_case(member) =>
                {
                    Some(true)
                }
                SqlServerSecurityChange::DropRoleMember {
                    scope: target_scope,
                    role: target_role,
                    member: target_member,
                } if *target_scope == scope
                    && target_role.eq_ignore_ascii_case(role)
                    && target_member.eq_ignore_ascii_case(member) =>
                {
                    Some(false)
                }
                _ => None,
            })
            .unwrap_or(base)
    }

    pub fn queue_change(&mut self, change: SqlServerSecurityChange) {
        let same_target = |existing: &SqlServerSecurityChange| match (existing, &change) {
            (
                SqlServerSecurityChange::CreateLogin { name: left, .. },
                SqlServerSecurityChange::CreateLogin { name: right, .. },
            )
            | (
                SqlServerSecurityChange::CreateUser { name: left, .. },
                SqlServerSecurityChange::CreateUser { name: right, .. },
            )
            | (
                SqlServerSecurityChange::CreateDatabaseRole { name: left },
                SqlServerSecurityChange::CreateDatabaseRole { name: right },
            ) => left == right,
            (
                SqlServerSecurityChange::SetLoginEnabled { name: left, .. },
                SqlServerSecurityChange::SetLoginEnabled { name: right, .. },
            )
            | (
                SqlServerSecurityChange::SetLoginPassword { name: left, .. },
                SqlServerSecurityChange::SetLoginPassword { name: right, .. },
            )
            | (
                SqlServerSecurityChange::SetLoginDefaultDatabase { name: left, .. },
                SqlServerSecurityChange::SetLoginDefaultDatabase { name: right, .. },
            )
            | (
                SqlServerSecurityChange::SetUserLogin { name: left, .. },
                SqlServerSecurityChange::SetUserLogin { name: right, .. },
            )
            | (
                SqlServerSecurityChange::SetUserDefaultSchema { name: left, .. },
                SqlServerSecurityChange::SetUserDefaultSchema { name: right, .. },
            ) => left.eq_ignore_ascii_case(right),
            (
                SqlServerSecurityChange::AddRoleMember {
                    scope: left_scope,
                    role: left_role,
                    member: left_member,
                }
                | SqlServerSecurityChange::DropRoleMember {
                    scope: left_scope,
                    role: left_role,
                    member: left_member,
                },
                SqlServerSecurityChange::AddRoleMember {
                    scope: right_scope,
                    role: right_role,
                    member: right_member,
                }
                | SqlServerSecurityChange::DropRoleMember {
                    scope: right_scope,
                    role: right_role,
                    member: right_member,
                },
            ) => {
                left_scope == right_scope
                    && left_role.eq_ignore_ascii_case(right_role)
                    && left_member.eq_ignore_ascii_case(right_member)
            }
            (
                SqlServerSecurityChange::GrantPermission {
                    grantee: left_grantee,
                    target: left_target,
                    permission: left_permission,
                    ..
                }
                | SqlServerSecurityChange::RevokePermission {
                    grantee: left_grantee,
                    target: left_target,
                    permission: left_permission,
                    ..
                }
                | SqlServerSecurityChange::RevokeGrantOption {
                    grantee: left_grantee,
                    target: left_target,
                    permission: left_permission,
                    ..
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: right_grantee,
                    target: right_target,
                    permission: right_permission,
                    ..
                }
                | SqlServerSecurityChange::RevokePermission {
                    grantee: right_grantee,
                    target: right_target,
                    permission: right_permission,
                    ..
                }
                | SqlServerSecurityChange::RevokeGrantOption {
                    grantee: right_grantee,
                    target: right_target,
                    permission: right_permission,
                    ..
                },
            ) => {
                left_grantee.eq_ignore_ascii_case(right_grantee)
                    && sqlserver_permission_targets_equal(left_target, right_target)
                    && left_permission.eq_ignore_ascii_case(right_permission)
            }
            _ => false,
        };
        self.pending_changes
            .retain(|existing| !same_target(existing));
        self.pending_changes.push(change);
        self.invalidate_preview();
        self.preview_error = None;
        self.apply_error = None;
    }

    pub fn queue_role_toggle(&mut self, scope: SqlServerRoleScope, role: &str, member: &str) {
        let base = self.snapshot.as_ref().is_some_and(|snapshot| match scope {
            SqlServerRoleScope::Server => snapshot.has_server_role(role, member),
            SqlServerRoleScope::Database => snapshot.has_database_role(role, member),
        });
        let current = self.effective_role_membership(scope, role, member);
        self.pending_changes.retain(|change| {
            !matches!(
                change,
                SqlServerSecurityChange::AddRoleMember {
                    scope: target_scope,
                    role: target_role,
                    member: target_member,
                } | SqlServerSecurityChange::DropRoleMember {
                    scope: target_scope,
                    role: target_role,
                    member: target_member,
                } if *target_scope == scope
                    && target_role.eq_ignore_ascii_case(role)
                    && target_member.eq_ignore_ascii_case(member)
            )
        });
        let desired = !current;
        if desired != base {
            self.pending_changes.push(if desired {
                SqlServerSecurityChange::AddRoleMember {
                    scope,
                    role: role.to_string(),
                    member: member.to_string(),
                }
            } else {
                SqlServerSecurityChange::DropRoleMember {
                    scope,
                    role: role.to_string(),
                    member: member.to_string(),
                }
            });
        }
        self.invalidate_preview();
        self.preview_error = None;
        self.apply_error = None;
    }

    pub fn queue_permission_revoke(&mut self, permission: &SqlServerPermission) {
        let Some(principal) = self.selected.as_ref() else {
            return;
        };
        let pending_grant = self.pending_changes.iter().rev().find(|change| {
            matches!(
                change,
                SqlServerSecurityChange::GrantPermission {
                    grantee,
                    target,
                    permission: pending,
                    ..
                } if grantee.eq_ignore_ascii_case(&principal.name)
                    && sqlserver_permission_targets_equal(target, &permission.target)
                    && pending.eq_ignore_ascii_case(&permission.permission)
            )
        });
        // 撤销尚未提交的权限只回退草稿；若覆盖了服务端基线，移除草稿即恢复基线。
        if pending_grant.is_some() {
            self.pending_changes.retain(|change| {
                !matches!(
                    change,
                    SqlServerSecurityChange::GrantPermission {
                        grantee,
                        target,
                        permission: pending,
                        ..
                    } if grantee.eq_ignore_ascii_case(&principal.name)
                        && sqlserver_permission_targets_equal(target, &permission.target)
                        && pending.eq_ignore_ascii_case(&permission.permission)
                )
            });
            self.invalidate_preview();
            self.preview_error = None;
            self.apply_error = None;
            return;
        }
        self.queue_change(SqlServerSecurityChange::RevokePermission {
            grantee: principal.name.clone(),
            target: permission.target.clone(),
            permission: permission.permission.clone(),
            cascade: permission.state == SqlServerPermissionState::GrantWithGrantOption,
        });
    }

    /// 服务端已有显式权限叠加当前待保存变更后的列表。
    pub fn effective_permissions(&self) -> Vec<SqlServerPermission> {
        let Some(principal) = self.selected.as_ref() else {
            return self.permissions.clone();
        };
        let mut permissions = self.permissions.clone();
        for change in &self.pending_changes {
            match change {
                SqlServerSecurityChange::GrantPermission {
                    grantee,
                    target,
                    permission,
                    state,
                    grant_option,
                    ..
                } if grantee.eq_ignore_ascii_case(&principal.name) => {
                    permissions.retain(|existing| {
                        !(sqlserver_permission_targets_equal(&existing.target, target)
                            && existing.permission.eq_ignore_ascii_case(permission))
                    });
                    permissions.push(SqlServerPermission {
                        target: target.clone(),
                        permission: permission.clone(),
                        state: match state {
                            SqlServerPermissionState::Deny => SqlServerPermissionState::Deny,
                            _ if *grant_option => SqlServerPermissionState::GrantWithGrantOption,
                            _ => SqlServerPermissionState::Grant,
                        },
                        grantor: None,
                    });
                }
                SqlServerSecurityChange::RevokePermission {
                    grantee,
                    target,
                    permission,
                    ..
                } if grantee.eq_ignore_ascii_case(&principal.name) => {
                    permissions.retain(|existing| {
                        !(sqlserver_permission_targets_equal(&existing.target, target)
                            && existing.permission.eq_ignore_ascii_case(permission))
                    });
                }
                SqlServerSecurityChange::RevokeGrantOption {
                    grantee,
                    target,
                    permission,
                    ..
                } if grantee.eq_ignore_ascii_case(&principal.name) => {
                    if let Some(existing) = permissions.iter_mut().find(|existing| {
                        sqlserver_permission_targets_equal(&existing.target, target)
                            && existing.permission.eq_ignore_ascii_case(permission)
                    }) {
                        existing.state = SqlServerPermissionState::Grant;
                    }
                }
                _ => {}
            }
        }
        permissions
    }

    pub fn permission_is_pending(&self, permission: &SqlServerPermission) -> bool {
        !self.permissions.iter().any(|existing| {
            sqlserver_permission_targets_equal(&existing.target, &permission.target)
                && existing.permission.eq_ignore_ascii_case(&permission.permission)
                && existing.state == permission.state
        })
    }

    /// 预览事件里保留脱敏占位符；只在本地绘制时还原草稿密码。
    pub fn preview_for_display(&self) -> Vec<String> {
        let Some(preview) = self.preview.as_ref() else {
            return Vec::new();
        };
        let mut passwords = self.pending_changes.iter().filter_map(|change| match change {
            SqlServerSecurityChange::CreateLogin { password, .. }
            | SqlServerSecurityChange::SetLoginPassword { password, .. } => Some(password),
            _ => None,
        });
        preview
            .iter()
            .map(|sql| {
                if sql.contains("N'********'")
                    && let Some(password) = passwords.next()
                {
                    return sql.replacen(
                        "N'********'",
                        &fluxdb_core::sqlserver_string_literal(password),
                        1,
                    );
                }
                sql.clone()
            })
            .collect()
    }

    pub fn build_plan(&self) -> fluxdb_core::Result<SqlServerSecurityPlan> {
        let snapshot = self.snapshot.as_ref().ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, "SQL Server 安全数据尚未加载")
        })?;
        if self.pending_changes.is_empty() {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Query,
                "没有待保存的 SQL Server 安全变更",
            ));
        }
        Ok(SqlServerSecurityPlan {
            database: snapshot.database.clone(),
            principal: self
                .selected
                .as_ref()
                .map(|principal| principal.name.clone())
                .unwrap_or_else(|| "security".to_string()),
            changes: self.pending_changes.clone(),
        })
    }

    pub fn permission_state_label(state: SqlServerPermissionState) -> &'static str {
        match state {
            SqlServerPermissionState::Grant => "GRANT",
            SqlServerPermissionState::GrantWithGrantOption => "GRANT OPTION",
            SqlServerPermissionState::Deny => "DENY",
        }
    }

    pub fn permission_target_label(target: &SqlServerPermissionTarget) -> String {
        target.label()
    }
}

fn sqlserver_permission_targets_equal(
    left: &SqlServerPermissionTarget,
    right: &SqlServerPermissionTarget,
) -> bool {
    match (left, right) {
        (SqlServerPermissionTarget::Server, SqlServerPermissionTarget::Server)
        | (SqlServerPermissionTarget::Database, SqlServerPermissionTarget::Database) => true,
        (
            SqlServerPermissionTarget::Schema { schema: left },
            SqlServerPermissionTarget::Schema { schema: right },
        ) => left.eq_ignore_ascii_case(right),
        (
            SqlServerPermissionTarget::Object {
                schema: left_schema,
                object: left_object,
            },
            SqlServerPermissionTarget::Object {
                schema: right_schema,
                object: right_object,
            },
        ) => {
            left_schema.eq_ignore_ascii_case(right_schema)
                && left_object.eq_ignore_ascii_case(right_object)
        }
        _ => false,
    }
}

#[cfg(test)]
mod sqlserver_user_admin_state_tests {
    include!("sqlserver_user_admin_state_tests.rs");
}
