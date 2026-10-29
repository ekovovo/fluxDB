// Oracle 用户与权限页的状态与变更草稿。
//
// 与 SQL Server 不同，Oracle 没有「服务器登录 / 数据库用户」两层，因此这里以账号为主体，
// 权限页直接区分三类：角色、系统权限、对象权限。每条草稿都记录它是否来自继承
// （`inherited`），继承来的授权只读，不能生成 REVOKE。
use fluxdb_core::{
    OracleObjectPrivilege, OracleRoleGrant, OracleSystemPrivilege, OracleUserAdminChange,
    OracleUserAdminPlan, OracleUserAdminSnapshot,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OracleGrantKind {
    #[default]
    Role,
    System,
    Object,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleGrantDraftField {
    Name,
    Owner,
    Object,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleCreateDraftField {
    Name,
    Password,
    ConfirmPassword,
    DefaultTablespace,
    TemporaryTablespace,
    Profile,
    QuotaTablespace,
    QuotaBytes,
}

/// 授权编辑器草稿。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleGrantDraft {
    pub kind: OracleGrantKind,
    /// 角色名 / 系统权限名 / 对象权限名。
    pub name: String,
    pub owner: String,
    pub object: String,
    /// 角色与系统权限用 `WITH ADMIN OPTION`，对象权限用 `WITH GRANT OPTION`。
    pub with_option: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleUserAdminState {
    /// 账号 DDL 方言：达梦复用本面板，但建号子句与登录权限不同。
    pub dialect: UserAdminSqlDialect,
    pub snapshot: Option<OracleUserAdminSnapshot>,
    pub selected: Option<String>,
    pub loading: bool,
    pub error: Option<UserFacingError>,
    pub grant: OracleGrantDraft,
    /// 已加入待执行计划的授权草稿。授权编辑器一次只放一条，加入这里才能在同一次保存里
    /// 带上多条（角色 / 系统权限 / 对象权限可以混排）。
    pub pending_grants: Vec<OracleGrantDraft>,
    /// 新建账号草稿；`None` 表示不在创建流程。
    pub creating: Option<OracleCreateDraft>,
    pub pending_changes: Vec<OracleUserAdminChange>,
    /// 保存时把草稿合成的完整计划。预览弹窗展示它、执行也用它，
    /// 避免「预览看到的」和「实际跑的」是两份不同的语句。
    pub pending_plan: Option<OracleUserAdminPlan>,
    /// 当前账号的新口令。只存在于内存，订阅输入框写入；不进预览、日志或历史。
    pub new_password: String,
    pub applying: bool,
    pub apply_error: Option<UserFacingError>,
    pub pending_delete: Option<String>,
    /// 删除账号时是否带 `CASCADE`（同时删除该账号下的对象）。
    pub delete_cascade: bool,
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct OracleCreateDraft {
    pub name: String,
    pub password: String,
    pub confirm_password: String,
    pub default_tablespace: String,
    pub temporary_tablespace: String,
    pub profile: String,
    pub quota_tablespace: String,
    /// 空字符串表示不写配额；`unlimited` 表示不限额。
    pub quota_bytes: String,
    pub grant_connect_role: bool,
}

impl std::fmt::Debug for OracleCreateDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 口令只以掩码出现，避免状态快照/日志把明文带出去。
        f.debug_struct("OracleCreateDraft")
            .field("name", &self.name)
            .field("password", &"********")
            .field("confirm_password", &"********")
            .field("default_tablespace", &self.default_tablespace)
            .field("temporary_tablespace", &self.temporary_tablespace)
            .field("profile", &self.profile)
            .field("quota_tablespace", &self.quota_tablespace)
            .field("quota_bytes", &self.quota_bytes)
            .field("grant_connect_role", &self.grant_connect_role)
            .finish()
    }
}

impl OracleCreateDraft {
    fn is_valid(&self) -> bool {
        let name = self.name.trim();
        !name.is_empty()
            && !self.password.is_empty()
            && self.password == self.confirm_password
            && fluxdb_core::validate_oracle_principal_name(name).is_ok()
    }

    fn quota(&self) -> Option<(String, Option<u64>)> {
        let tablespace = self.quota_tablespace.trim();
        if tablespace.is_empty() {
            return None;
        }
        let raw = self.quota_bytes.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.eq_ignore_ascii_case("unlimited") {
            return Some((tablespace.to_string(), None));
        }
        raw.parse::<u64>().ok().map(|bytes| {
            (
                tablespace.to_string(),
                // 界面按 MB 输入，Oracle 需要字节数。
                Some(bytes.saturating_mul(1024 * 1024)),
            )
        })
    }
}

impl OracleUserAdminState {
    pub fn has_changes(&self) -> bool {
        !self.pending_changes.is_empty()
            || !self.pending_grants.is_empty()
            || self.creating.is_some()
            || !self.new_password.is_empty()
            || !self.grant.name.trim().is_empty()
    }

    /// 把授权编辑器里填好的一条加入待执行列表，编辑器清空以便继续填下一条。
    /// 校验提前到加入时做，避免保存时才发现对象权限只填了一半。
    pub fn queue_grant_draft(&mut self) -> fluxdb_core::Result<()> {
        // 用临时副本走一次渲染前的校验，失败就不入列、也不清空编辑器。
        self.grant_change()?
            .ok_or_else(|| Error::new(ErrorKind::Query, "请先填写授权名称".to_string()))?;
        let draft = std::mem::take(&mut self.grant);
        // 类型保留：连续加多条时通常都是同一类授权。
        self.grant.kind = draft.kind;
        self.pending_grants.push(draft);
        Ok(())
    }

    pub fn remove_pending_grant(&mut self, index: usize) {
        if index < self.pending_grants.len() {
            self.pending_grants.remove(index);
        }
    }

    /// 把当前草稿合成一份计划：保存时的预览与执行都必须用它，保证「看到的就是跑的」。
    ///
    /// 顺序：新建账号（含建号即授权）→ 勾选 → 锁定/改密 → 撤销 → 删除，由
    /// `OracleUserAdminPlan::ordered_changes` 统一排定创建先行、删除最后。
    pub fn compose_plan(&self) -> fluxdb_core::Result<OracleUserAdminPlan> {
        let mut changes = Vec::new();
        if self.creating.is_some() {
            changes.push(self.create_change()?);
        }
        changes.extend(self.pending_changes.iter().cloned());
        for draft in &self.pending_grants {
            if let Some(change) = self.grant_change_for(draft)? {
                changes.push(change);
            }
        }
        if let Some(change) = self.grant_change()? {
            changes.push(change);
        }
        if let Some(change) = self.password_change()? {
            changes.push(change);
        }
        Ok(OracleUserAdminPlan { changes })
    }

    /// 预览语句：与 MySQL / PostgreSQL 一致，口令按草稿原样显示。
    ///
    /// 口令只出现在本地内存渲染结果里，不回写日志、不进历史；执行侧仍单独渲染
    /// 一份脱敏副本用于回显（见连接器的 `apply_oracle_user_admin_plan`）。
    pub fn compose_preview(&self) -> fluxdb_core::Result<Vec<String>> {
        let plan = self.compose_plan()?;
        fluxdb_core::render_user_admin_plan(&plan, false, self.dialect)
    }

    fn create_change(&self) -> fluxdb_core::Result<OracleUserAdminChange> {
        let draft = self
            .creating
            .clone()
            .ok_or_else(|| Error::new(ErrorKind::Query, "没有待创建的账号草稿".to_string()))?;
        if !draft.is_valid() {
            return Err(Error::new(
                ErrorKind::Query,
                if draft.password != draft.confirm_password {
                    "两次输入的密码不一致".to_string()
                } else {
                    "账号名称与密码必填".to_string()
                },
            ));
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.contains_user(draft.name.trim()))
        {
            return Err(Error::new(
                ErrorKind::Query,
                format!("Oracle 已存在账号 {}", draft.name.trim()),
            ));
        }
        Ok(OracleUserAdminChange::CreateUser {
            name: draft.name.trim().to_ascii_uppercase(),
            password: draft.password.clone(),
            default_tablespace: optional(draft.default_tablespace.clone()),
            temporary_tablespace: optional(draft.temporary_tablespace.clone()),
            profile: optional(draft.profile.clone()),
            quota: draft.quota(),
            grant_connect_role: draft.grant_connect_role,
        })
    }

    /// 授权草稿：名称为空视为「本次不授权」。
    fn grant_change(&self) -> fluxdb_core::Result<Option<OracleUserAdminChange>> {
        self.grant_change_for(&self.grant)
    }

    fn grant_change_for(
        &self,
        draft: &OracleGrantDraft,
    ) -> fluxdb_core::Result<Option<OracleUserAdminChange>> {
        let name = draft.name.trim().to_string();
        if name.is_empty() {
            return Ok(None);
        }
        // 被授权人：新建流程里必须是即将创建的那个账号，否则授权会打到上一个选中的账号上；
        // 非新建流程才用当前选中账号。建号语句会把名字折叠为大写，授权必须用同一个名字，
        // 否则 `GRANT ... TO "new_user"` 打不到刚建的 `"NEW_USER"`（Oracle 报用户不存在）。
        let grantee = self
            .creating
            .as_ref()
            .map(|draft| draft.name.trim().to_ascii_uppercase())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                self.selected
                    .clone()
                    .filter(|name| !name.trim().is_empty())
            })
            .ok_or_else(|| Error::new(ErrorKind::Query, "请先选择 Oracle 账号".to_string()))?;
        Ok(Some(match draft.kind {
            OracleGrantKind::Role => OracleUserAdminChange::GrantRole {
                role: name,
                grantee,
                admin_option: draft.with_option,
            },
            OracleGrantKind::System => OracleUserAdminChange::GrantSystemPrivilege {
                privilege: name,
                grantee,
                admin_option: draft.with_option,
            },
            OracleGrantKind::Object => {
                if draft.owner.trim().is_empty() || draft.object.trim().is_empty() {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "对象权限必须同时填写 Owner 与对象名".to_string(),
                    ));
                }
                OracleUserAdminChange::GrantObjectPrivilege {
                    privilege: name,
                    owner: draft.owner.trim().to_ascii_uppercase(),
                    object: draft.object.trim().to_ascii_uppercase(),
                    grantee,
                    grant_option: draft.with_option,
                }
            }
        }))
    }

    /// 当前账号的新口令：空表示不改。
    fn password_change(&self) -> fluxdb_core::Result<Option<OracleUserAdminChange>> {
        if self.new_password.is_empty() {
            return Ok(None);
        }
        let name = self
            .selected
            .clone()
            .ok_or_else(|| Error::new(ErrorKind::Query, "请先选择 Oracle 账号".to_string()))?;
        Ok(Some(OracleUserAdminChange::SetPassword {
            name,
            password: self.new_password.clone(),
        }))
    }

    pub fn selected_detail(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// 只有「当前账号自己」的授权能从这里读出来，其它账号显示为不可读。
    pub fn selected_is_self(&self) -> bool {
        match (&self.snapshot, self.selected.as_deref()) {
            (Some(snapshot), Some(selected)) => {
                snapshot.current_user.eq_ignore_ascii_case(selected)
            }
            _ => false,
        }
    }

    /// 授权编辑器里可选的名称候选：只是输入提示，不是白名单。
    ///
    /// 目录读不到或当前账号没有 ADMIN OPTION 时返回空列表，输入框照常接受手填。
    pub fn grant_name_candidates(&self, kind: OracleGrantKind) -> &[String] {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return &[];
        };
        match kind {
            OracleGrantKind::Role => &snapshot.role_candidates,
            OracleGrantKind::System | OracleGrantKind::Object => {
                &snapshot.system_privilege_candidates
            }
        }
    }

    /// 可直接撤销的角色（非继承）。继承来的角色必须走「撤销其在角色链上的授予」。
    pub fn revocable_roles(&self) -> Vec<&OracleRoleGrant> {
        if !self.selected_is_self() {
            return Vec::new();
        }
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.roles.iter().filter(|role| !role.inherited).collect())
            .unwrap_or_default()
    }

    pub fn revocable_system_privileges(&self) -> Vec<&OracleSystemPrivilege> {
        if !self.selected_is_self() {
            return Vec::new();
        }
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .system_privileges
                    .iter()
                    .filter(|privilege| !privilege.inherited)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn revocable_object_privileges(&self) -> Vec<&OracleObjectPrivilege> {
        if !self.selected_is_self() {
            return Vec::new();
        }
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .object_privileges
                    .iter()
                    .filter(|privilege| !privilege.inherited)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn queue_revoke_role(&mut self, role: &str) -> fluxdb_core::Result<()> {
        if role.trim().is_empty() {
            return Err(Error::new(ErrorKind::Query, "角色名称为空".to_string()));
        }
        let grantee = self
            .selected
            .clone()
            .unwrap_or_else(|| self.snapshot_current_user());
        self.pending_changes
            .push(OracleUserAdminChange::RevokeRole {
                role: role.trim().to_string(),
                grantee,
            });
        Ok(())
    }

    pub fn queue_revoke_system_privilege(&mut self, privilege: &str) -> fluxdb_core::Result<()> {
        let grantee = self
            .selected
            .clone()
            .unwrap_or_else(|| self.snapshot_current_user());
        self.pending_changes
            .push(OracleUserAdminChange::RevokeSystemPrivilege {
                privilege: privilege.trim().to_string(),
                grantee,
            });
        Ok(())
    }

    pub fn queue_revoke_object_privilege(
        &mut self,
        owner: &str,
        object: &str,
        privilege: &str,
    ) -> fluxdb_core::Result<()> {
        // 继承来的权限必须由界面过滤后再调用；这里再兜一次，避免任何路径把继承项当成可撤销。
        if self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot.object_privileges.iter().find(|row| {
                    row.owner.eq_ignore_ascii_case(owner)
                        && row.object.eq_ignore_ascii_case(object)
                        && row.privilege.eq_ignore_ascii_case(privilege)
                })
            })
            .is_some_and(|row| row.inherited)
        {
            return Err(Error::new(ErrorKind::Query, "该权限由角色继承而来，只能撤销角色或修改角色授权".to_string()));
        }
        let grantee = self
            .selected
            .clone()
            .unwrap_or_else(|| self.snapshot_current_user());
        self.pending_changes
            .push(OracleUserAdminChange::RevokeObjectPrivilege {
                privilege: privilege.to_string(),
                owner: owner.to_string(),
                object: object.to_string(),
                grantee,
            });
        Ok(())
    }

    pub fn queue_account_locked(&mut self, locked: bool) -> fluxdb_core::Result<()> {
        let name = self
            .selected
            .clone()
            .ok_or_else(|| Error::new(ErrorKind::Query, "请先选择 Oracle 账号".to_string()))?;
        self.pending_changes
            .push(OracleUserAdminChange::SetAccountLocked { name, locked });
        Ok(())
    }

    pub fn queue_delete(&mut self) -> fluxdb_core::Result<()> {
        let name = self
            .selected
            .clone()
            .ok_or_else(|| Error::new(ErrorKind::Query, "请先选择 Oracle 账号".to_string()))?;
        self.pending_changes.push(OracleUserAdminChange::DropUser {
            name,
            // 账号下是否有对象无法从可见目录判定，CASCADE 由界面勾选决定，默认不删对象。
            cascade: self.delete_cascade,
        });
        self.pending_delete = None;
        Ok(())
    }

    pub fn discard_changes(&mut self) {
        self.pending_changes.clear();
        self.pending_grants.clear();
        self.grant = OracleGrantDraft::default();
        self.creating = None;
        self.pending_plan = None;
        self.new_password.clear();
        self.apply_error = None;
    }

    pub fn set_grant_field(&mut self, field: OracleGrantDraftField, value: String) -> bool {
        let target = match field {
            OracleGrantDraftField::Name => &mut self.grant.name,
            OracleGrantDraftField::Owner => &mut self.grant.owner,
            OracleGrantDraftField::Object => &mut self.grant.object,
        };
        if *target == value {
            return false;
        }
        *target = value;
        true
    }

    pub fn set_create_field(&mut self, field: OracleCreateDraftField, value: String) -> bool {
        let Some(draft) = self.creating.as_mut() else {
            return false;
        };
        let target = match field {
            OracleCreateDraftField::Name => &mut draft.name,
            OracleCreateDraftField::Password => &mut draft.password,
            OracleCreateDraftField::ConfirmPassword => &mut draft.confirm_password,
            OracleCreateDraftField::DefaultTablespace => &mut draft.default_tablespace,
            OracleCreateDraftField::TemporaryTablespace => &mut draft.temporary_tablespace,
            OracleCreateDraftField::Profile => &mut draft.profile,
            OracleCreateDraftField::QuotaTablespace => &mut draft.quota_tablespace,
            OracleCreateDraftField::QuotaBytes => &mut draft.quota_bytes,
        };
        if *target == value {
            return false;
        }
        *target = value;
        true
    }

    pub fn set_create_flag(&mut self, grant_connect_role: bool) -> bool {
        let Some(draft) = self.creating.as_mut() else {
            return false;
        };
        if draft.grant_connect_role == grant_connect_role {
            return false;
        }
        draft.grant_connect_role = grant_connect_role;
        true
    }

    fn snapshot_current_user(&self) -> String {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.current_user.clone())
            .unwrap_or_default()
    }

}

fn optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod oracle_user_admin_state_tests {
    use super::*;

    fn state_with_current(name: &str) -> OracleUserAdminState {
        let mut state = OracleUserAdminState::default();
        state.snapshot = Some(OracleUserAdminSnapshot {
            current_user: name.into(),
            users: vec![fluxdb_core::OracleUser {
                name: name.into(),
                ..Default::default()
            }],
            roles: vec![
                OracleRoleGrant {
                    role: "CONNECT".into(),
                    admin_option: false,
                    default_role: true,
                    inherited: false,
                },
                OracleRoleGrant {
                    role: "DB_DEVELOPER_ROLE".into(),
                    admin_option: false,
                    default_role: false,
                    inherited: true,
                },
            ],
            ..Default::default()
        });
        state.selected = Some(name.into());
        state
    }

    #[test]
    fn inherited_grants_are_not_revocable() {
        let state = state_with_current("FLUXDB");
        let revocable = state
            .revocable_roles()
            .iter()
            .map(|role| role.role.clone())
            .collect::<Vec<_>>();
        assert_eq!(revocable, vec!["CONNECT".to_string()]);
    }

    #[test]
    fn object_privilege_revoke_rejects_inherited() {
        let mut state = state_with_current("FLUXDB");
        state.snapshot.as_mut().expect("snapshot").object_privileges = vec![
            OracleObjectPrivilege {
                owner: "SYS".into(),
                object: "V_$PARAMETER".into(),
                privilege: "READ".into(),
                grantable: false,
                inherited: true,
                grantor: None,
            },
            OracleObjectPrivilege {
                owner: "SALES".into(),
                object: "ORDERS".into(),
                privilege: "SELECT".into(),
                grantable: false,
                inherited: false,
                grantor: Some("SALES".into()),
            },
        ];
        let error = state
            .queue_revoke_object_privilege("SYS", "V_$PARAMETER", "READ")
            .expect_err("inherited must be refused");
        assert!(error.message.contains("继承"));
        // 直接授权可以撤销，且生成的是两段名。
        state
            .queue_revoke_object_privilege("SALES", "ORDERS", "SELECT")
            .expect("direct grant revocable");
        let rendered = state.compose_preview().expect("render");
        assert_eq!(
            rendered,
            vec![r#"REVOKE SELECT ON "SALES"."ORDERS" FROM "FLUXDB";"#]
        );
        // 继承项不进可撤销列表；直接授权项此时仍在快照里（撤销只是排队，未刷新目录）。
        let revocable = state.revocable_object_privileges();
        assert!(!revocable.iter().any(|privilege| privilege.inherited));
        assert_eq!(revocable.len(), 1);
        assert_eq!(revocable[0].object, "ORDERS");
    }

    #[test]
    fn grant_draft_requires_selection_and_name() {
        // 名称留空 = 本次不授权，没必要为了让用户点一次「加入变更」而报错。
        let mut state = OracleUserAdminState::default();
        assert!(state.compose_preview().expect("empty draft").is_empty());
        state.grant = OracleGrantDraft {
            kind: OracleGrantKind::Role,
            name: "app_role".into(),
            ..Default::default()
        };
        // 有名称但没选账号：无法确定被授权人，必须拒绝。
        assert!(state.compose_plan().is_err());

        state.snapshot = Some(OracleUserAdminSnapshot {
            current_user: "FLUXDB".into(),
            ..Default::default()
        });
        state.selected = Some("FLUXDB".into());
        let rendered = state.compose_preview().expect("render");
        assert_eq!(rendered, vec![r#"GRANT "app_role" TO "FLUXDB";"#]);
    }

    #[test]
    fn object_grant_uses_grant_option_and_two_part_name() {
        let mut state = state_with_current("FLUXDB");
        state.grant = OracleGrantDraft {
            kind: OracleGrantKind::Object,
            name: "select".into(),
            owner: "sales".into(),
            object: "orders".into(),
            with_option: true,
        };
        let rendered = state.compose_preview().expect("render");
        assert_eq!(
            rendered,
            vec![r#"GRANT select ON "SALES"."ORDERS" TO "FLUXDB" WITH GRANT OPTION;"#]
        );
    }

    #[test]
    fn create_requires_matching_passwords_and_rejects_existing() {
        let mut state = state_with_current("FLUXDB");
        state.creating = Some(OracleCreateDraft {
            name: "fluxdb".into(),
            password: "aA1!".into(),
            confirm_password: "aA1!".into(),
            ..Default::default()
        });
        let error = state.compose_plan().expect_err("existing user must fail");
        assert!(error.message.contains("已存在"));

        state.creating = Some(OracleCreateDraft {
            name: "new_user".into(),
            password: "aA1!".into(),
            confirm_password: "different".into(),
            ..Default::default()
        });
        let error = state.compose_plan().expect_err("mismatch must fail");
        assert!(error.message.contains("密码"));

        state.creating = Some(OracleCreateDraft {
            name: "new_user".into(),
            password: "aA1!".into(),
            confirm_password: "aA1!".into(),
            quota_tablespace: "USERS".into(),
            quota_bytes: "512".into(),
            grant_connect_role: true,
            ..Default::default()
        });
        let rendered = state.compose_preview().expect("render");
        assert!(rendered[0].contains("QUOTA 512M ON \"USERS\""));
        // 账号名统一转大写：未加引号的 Oracle 标识符会折叠为大写。
        assert!(rendered[0].contains("GRANT CONNECT TO \"NEW_USER\""));
        // 与 MySQL / PostgreSQL 一致：预览按草稿原样显示口令（执行回执另有脱敏副本）。
        assert!(rendered[0].contains("aA1!"));
    }

    #[test]
    fn other_user_grants_are_hidden_as_read_only() {
        let mut state = state_with_current("FLUXDB");
        state.snapshot.as_mut().expect("snapshot").users.push(
            fluxdb_core::OracleUser {
                name: "OTHER".into(),
                ..Default::default()
            },
        );
        state.selected = Some("OTHER".into());
        assert!(state.revocable_roles().is_empty());
        assert!(state.revocable_object_privileges().is_empty());
    }

    /// 一次保存要能带多条授权：新建账号 + 角色 + 系统权限 + 对象权限全部进同一份计划。
    #[test]
    fn multiple_pending_grants_render_in_one_plan() {
        let mut state = state_with_current("FLUXDB");
        state.creating = Some(OracleCreateDraft {
            name: "new_user".into(),
            password: "aA1!".into(),
            confirm_password: "aA1!".into(),
            grant_connect_role: true,
            ..Default::default()
        });
        state.grant = OracleGrantDraft {
            kind: OracleGrantKind::Role,
            name: "app_role".into(),
            ..Default::default()
        };
        state.queue_grant_draft().expect("queue role");
        // 加入后编辑器清空，但授权类型保留。
        assert!(state.grant.name.is_empty());
        assert_eq!(state.grant.kind, OracleGrantKind::Role);
        state.grant = OracleGrantDraft {
            kind: OracleGrantKind::System,
            name: "CREATE TABLE".into(),
            ..Default::default()
        };
        state.queue_grant_draft().expect("queue system privilege");
        assert_eq!(state.pending_grants.len(), 2);

        let rendered = state.compose_preview().expect("render");
        // 建号语句在最前（其中含登录授权），随后是两条授权。
        assert!(rendered[0].contains("CREATE USER \"NEW_USER\""));
        assert!(rendered[0].contains("GRANT CONNECT TO \"NEW_USER\""));
        assert!(rendered.iter().any(|sql| sql == r#"GRANT "app_role" TO "NEW_USER";"#));
        assert!(rendered.iter().any(|sql| sql == r#"GRANT CREATE TABLE TO "NEW_USER";"#));

        state.remove_pending_grant(0);
        assert_eq!(state.pending_grants.len(), 1);
        assert!(!state.compose_preview().expect("render").iter().any(|sql| sql.contains("APP_ROLE")));
    }

    /// 对象权限只填一半时拒绝入列，且不清空编辑器（否则用户输入白丢）。
    #[test]
    fn incomplete_object_grant_is_rejected_without_clearing_editor() {
        let mut state = state_with_current("FLUXDB");
        state.grant = OracleGrantDraft {
            kind: OracleGrantKind::Object,
            name: "SELECT".into(),
            owner: "SALES".into(),
            ..Default::default()
        };
        let error = state.queue_grant_draft().expect_err("owner-only object grant");
        assert!(error.message.contains("Owner"));
        assert!(state.pending_grants.is_empty());
        assert_eq!(state.grant.name, "SELECT");
    }

    #[test]
    fn drop_is_ordered_last() {        let mut state = state_with_current("FLUXDB");
        state.pending_delete = Some("FLUXDB".into());
        state.queue_delete().expect("queued");
        state
            .pending_changes
            .insert(0, OracleUserAdminChange::RevokeRole {
                role: "CONNECT".into(),
                grantee: "FLUXDB".into(),
            });
        let rendered = state.compose_preview().expect("render");
        assert!(rendered[0].starts_with("REVOKE"));
        assert!(rendered[1].starts_with("DROP USER"));
    }
}
