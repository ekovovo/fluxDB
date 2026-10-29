impl AppController {
    // PostgreSQL 角色、成员关系与授权操作独立成帧，避免权限草稿与计划占用无关命令的栈空间。
    fn dispatch_pg_user_admin_command(&mut self, command: AppCommand) -> AppEvent {
        match command {
            AppCommand::LoadPgRoles(connection_id) => {
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.list_roles(connection_id)
                }) {
                    Ok(roles) => AppEvent::PgRolesLoaded(connection_id, roles),
                    Err(error) => self.fail(error),
                }
            }
            AppCommand::CreatePgRole {
                connection_id,
                name,
                can_login,
                password,
            } => {
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.create_role(connection_id, &name, can_login, password.as_deref())
                }) {
                    Ok(()) => AppEvent::PgRoleChanged(connection_id),
                    Err(error) => self.fail(error),
                }
            }
            AppCommand::AlterPgRolePassword {
                connection_id,
                name,
                password,
            } => {
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.alter_role_password(connection_id, &name, &password)
                }) {
                    Ok(()) => AppEvent::PgRoleChanged(connection_id),
                    Err(error) => self.fail(error),
                }
            }
            AppCommand::RenamePgRole {
                connection_id,
                old_name,
                new_name,
            } => {
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.rename_role(connection_id, &old_name, &new_name)
                }) {
                    Ok(()) => AppEvent::PgRoleChanged(connection_id),
                    Err(error) => self.fail(error),
                }
            }
            AppCommand::DropPgRole {
                connection_id,
                name,
            } => {
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.drop_role(connection_id, &name)
                }) {
                    Ok(()) => AppEvent::PgRoleChanged(connection_id),
                    Err(error) => self.fail(error),
                }
            }
            AppCommand::SelectPgRole { tab_id, name } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_selected_role = Some(name.clone());
                    // 切换角色是新的编辑会话：目标选择、成员/授权草稿和预览都不能带到新角色。
                    admin.pg_reset_role_editor_session();
                    // 从基线派生干净草稿：右侧面板始终以草稿渲染；干净草稿不计脏。
                    admin.pg_reset_draft_from_baseline();
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgRoleSwitchPending { tab_id, target } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_pending_switch = Some(target);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgCancelSwitchRole(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_pending_switch = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::DiscardPgDraftAndSelect { tab_id, name } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_selected_role = Some(name.clone());
                    admin.pg_reset_role_editor_session();
                    admin.pg_reset_draft_from_baseline();
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgBeginCreateRole(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    // 新建也是独立编辑会话；旧角色的权限目标/成员状态不能带入新建表单。
                    admin.pg_selected_role = None;
                    admin.pg_reset_role_editor_session();
                    admin.pg_draft = Some(PgRoleDraft::new_create());
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgCancelDraft(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_draft = None;
                    admin.pg_membership_edits.clear();
                    admin.pg_grant_edits.clear();
                    admin.pg_plan_preview = None;
                    admin.pg_plan_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::DiscardPgDraftEdits(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    if admin.pg_save_status == PgRoleSaveStatus::NeedsVerify {
                        return self.fail(Error::new(
                            ErrorKind::Query,
                            "提交结果待核实，请先刷新核实后再取消",
                        ));
                    }
                    admin.discard_pg_edits();
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftName { tab_id, name } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    if let Some(draft) = admin.pg_draft.as_mut() {
                        draft.name = name;
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftCanLogin { tab_id, can_login } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    if let Some(draft) = admin.pg_draft.as_mut() {
                        draft.can_login = can_login;
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftAttr {
                tab_id,
                field,
                value,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(draft) = admin.pg_draft.as_mut()
                {
                    match field {
                        PgDraftAttrField::IsSuperuser => draft.is_superuser = value,
                        PgDraftAttrField::CanCreateDb => draft.can_create_db = value,
                        PgDraftAttrField::CanCreateRole => draft.can_create_role = value,
                        PgDraftAttrField::Inherit => draft.inherit = value,
                        PgDraftAttrField::IsReplication => draft.is_replication = value,
                        PgDraftAttrField::BypassRls => draft.bypass_rls = value,
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftConnectionLimit { tab_id, value } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(draft) = admin.pg_draft.as_mut()
                {
                    draft.connection_limit_text = value;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftValidUntil { tab_id, op } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(draft) = admin.pg_draft.as_mut()
                {
                    draft.valid_until = op;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftPasswordOp { tab_id, op } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(draft) = admin.pg_draft.as_mut()
                {
                    draft.password = op;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgDraftPassword { tab_id, password } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && let Some(draft) = admin.pg_draft.as_mut()
                {
                    draft.password = PgPasswordOp::Set(password);
                }
                AppEvent::TabActivated(tab_id)
            }
            AppCommand::StartUserAdminPgRolesLoad(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_roles_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::LoadUserAdminPgRoles(tab_id) => {
                let Some(connection_id) = self
                    .user_admin_state(tab_id)
                    .map(|admin| admin.connection_id)
                else {
                    return self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"));
                };
                match self.list_pg_roles_for_connection(connection_id) {
                    Ok(roles) => AppEvent::UserAdminPgRolesLoaded(tab_id, roles),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::FinishUserAdminPgRolesLoad { tab_id, result } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    match result {
                        Ok(roles) => {
                            admin.pg_roles = roles;
                            admin.pg_roles_error = None;
                            // 选中角色被删除后回退到第一个角色；无角色则清空选择。
                            let names: Vec<String> = admin
                                .pg_roles
                                .iter()
                                .map(|role| role.name.clone())
                                .collect();
                            let pending_selection = admin.pending_selected_pg_role.take();
                            if pending_selection
                                .as_ref()
                                .is_some_and(|pending| names.iter().any(|name| name == pending))
                            {
                                admin.pg_selected_role = pending_selection;
                            } else if !names
                                .iter()
                                .any(|name| Some(name) == admin.pg_selected_role.as_ref())
                            {
                                admin.pg_selected_role = names.first().cloned();
                            }
                            // 刷新是重新读取服务端基线：右侧会话态与权限目标都回到初始态。
                            admin.pg_reset_role_editor_session();
                            admin.pg_reset_draft_from_baseline();
                        }
                        Err(error) => {
                            // 失败保留旧数据（UI 标记未刷新），显示错误并可重试。
                            admin.pg_roles_error = Some(error.clone());
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::StartPgMembershipsLoad(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_memberships_loaded = false;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::LoadPgMemberships(tab_id) => {
                let Some(connection_id) = self
                    .user_admin_state(tab_id)
                    .map(|admin| admin.connection_id)
                else {
                    return self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"));
                };
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                let member_options_supported =
                    role_operation_for_connection(&config, |connector| {
                        connector.supports_member_options(connection_id)
                    })
                    .unwrap_or(false);
                match self.list_pg_memberships_for_connection(connection_id) {
                    Ok(memberships) => AppEvent::UserAdminPgMembershipsLoaded(
                        tab_id,
                        memberships,
                        member_options_supported,
                    ),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::FinishPgMembershipsLoad {
                tab_id,
                result,
                member_options_supported,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_member_options_supported = Some(member_options_supported);
                    match result {
                        Ok(memberships) => {
                            admin.pg_memberships = memberships;
                            admin.pg_memberships_loaded = true;
                        }
                        Err(error) => {
                            // 失败保留旧数据并标记未加载，不显示为「无成员」。
                            admin.pg_memberships_loaded = false;
                            admin.pg_plan_error = Some(error.clone());
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgMembershipGrant {
                tab_id,
                role,
                member,
                admin: admin_option,
                inherit,
                set,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    let edit = PgRoleChange::GrantMembership {
                        role: role.clone(),
                        member: member.clone(),
                        admin: admin_option,
                        inherit,
                        set,
                    };
                    // 同 (role, member) 只保留一条最新变更，避免重复提交。
                    if let Some(index) = admin.pg_membership_edit_index(&role, &member) {
                        admin.pg_membership_edits[index] = edit;
                    } else {
                        admin.pg_membership_edits.push(edit);
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgMembershipRevoke {
                tab_id,
                role,
                member,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    let edit = PgRoleChange::RevokeMembership {
                        role: role.clone(),
                        member: member.clone(),
                    };
                    if let Some(index) = admin.pg_membership_edit_index(&role, &member) {
                        admin.pg_membership_edits[index] = edit;
                    } else {
                        admin.pg_membership_edits.push(edit);
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgMembershipRemoveEdit { tab_id, index } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && index < admin.pg_membership_edits.len()
                {
                    admin.pg_membership_edits.remove(index);
                    AppEvent::TabActivated(tab_id)
                } else {
                    AppEvent::TabActivated(tab_id)
                }
            }
            AppCommand::SetPgGrantDatabase { tab_id, database } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    // 一期同批只允许一个数据库：切换数据库前 UI 已确认保存或放弃授权草稿；
                    // 目标选择也必须清空，避免旧库的 schema/object 被带到新库。对象种类保留。
                    let kind = admin.pg_grant_kind;
                    admin.pg_reset_grant_target_session();
                    admin.pg_grant_kind = kind;
                    admin.pg_grant_database = database;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::StartPgGrantTargetsLoad(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_loading_targets = true;
                    admin.pg_targets_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::LoadPgGrantTargets { tab_id, database } => {
                let Some(connection_id) = self
                    .user_admin_state(tab_id)
                    .map(|admin| admin.connection_id)
                else {
                    return self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"));
                };
                let Some(config) = self.connection_config(connection_id).cloned() else {
                    return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                };
                match role_operation_for_connection(&config, |connector| {
                    connector.list_grant_targets(connection_id, &database)
                }) {
                    Ok(lists) => AppEvent::UserAdminPgGrantTargetsLoaded(tab_id, lists),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::FinishPgGrantTargetsLoad { tab_id, result } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_loading_targets = false;
                    match result {
                        Ok(lists) => admin.pg_grant_targets = Some(lists),
                        Err(error) => admin.pg_targets_error = Some(error),
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgToggleGrant {
                tab_id,
                privilege,
                scope,
                op,
                grant_option,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    let edit = match op {
                        PgGrantEditOp::Grant => PgRoleChange::GrantObject {
                            privilege: privilege.clone(),
                            scope: scope.clone(),
                            grantee: admin.pg_effective_grantee_name(),
                            grant_option,
                        },
                        PgGrantEditOp::Revoke => PgRoleChange::RevokeObject {
                            privilege: privilege.clone(),
                            scope: scope.clone(),
                            grantee: admin.pg_effective_grantee_name(),
                        },
                        PgGrantEditOp::RevokeGrantOption => PgRoleChange::RevokeGrantOption {
                            privilege: privilege.clone(),
                            scope: scope.clone(),
                            grantee: admin.pg_effective_grantee_name(),
                        },
                    };
                    if let Some(index) = admin.pg_grant_edit_index(&privilege, &scope) {
                        admin.pg_grant_edits[index] = edit;
                    } else {
                        admin.pg_grant_edits.push(edit);
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgRemoveGrantEdit { tab_id, index } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id)
                    && index < admin.pg_grant_edits.len()
                {
                    admin.pg_grant_edits.remove(index);
                    AppEvent::TabActivated(tab_id)
                } else {
                    AppEvent::TabActivated(tab_id)
                }
            }
            AppCommand::StartPgPlanPreview(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_preview_loading = true;
                    admin.pg_plan_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::LoadPgPlanPreview(tab_id) => match self.build_pg_role_plan(tab_id) {
                Ok(plan) => {
                    if plan.is_empty() {
                        AppEvent::UserAdminPgPlanPreview(tab_id, Vec::new())
                    } else {
                        let connection_id = self
                            .user_admin_state(tab_id)
                            .map(|admin| admin.connection_id);
                        let Some(connection_id) = connection_id else {
                            return self
                                .fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"));
                        };
                        let Some(config) = self.connection_config(connection_id).cloned() else {
                            return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                        };
                        // 与 MySQL 预览一致：展示与执行同源的完整语句（含明文口令）。
                        // 口令不落日志、历史或 AI 上下文；执行路径另取脱敏副本做审计。
                        match role_operation_for_connection(&config, |connector| {
                            connector.render_role_plan(connection_id, &plan, false)
                        }) {
                            Ok(stmts) => AppEvent::UserAdminPgPlanPreview(tab_id, stmts),
                            Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                        }
                    }
                }
                Err(error) => AppEvent::Failed(UserFacingError::from(error)),
            },
            AppCommand::FinishPgPlanPreview { tab_id, result } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_preview_loading = false;
                    let confirm = std::mem::take(&mut admin.pending_sql_awaits_preview);
                    match result {
                        Ok(stmts) => {
                            if confirm {
                                // 「保存」触发的预览：直接把它放进确认框，不再等用户切页签。
                                let danger = stmts.iter().any(|sql| sql.starts_with("DROP "));
                                admin.pending_sql = Some(UserAdminPendingSql {
                                    sql: stmts.join("\n"),
                                    danger,
                                    kind: UserAdminPendingKind::Postgres,
                                });
                                admin.apply_error = None;
                            }
                            admin.pg_plan_preview = Some(stmts);
                            admin.pg_plan_error = None;
                        }
                        Err(error) => {
                            admin.pg_plan_preview = None;
                            admin.pg_plan_error = Some(error);
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::StartPgPlanApply(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    if admin.pg_save_status == PgRoleSaveStatus::Saving {
                        return AppEvent::TabActivated(tab_id);
                    }
                    admin.pg_save_status = PgRoleSaveStatus::Saving;
                    admin.pg_plan_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::ApplyPgRolePlan(tab_id) => {
                // 由 App 构建（校验）计划并单事务应用；结果无论成败都带计划回传 Finish。
                match self.build_pg_role_plan(tab_id) {
                    Ok(plan) => {
                        let Some(connection_id) = self
                            .user_admin_state(tab_id)
                            .map(|admin| admin.connection_id)
                        else {
                            return self
                                .fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"));
                        };
                        let Some(config) = self.connection_config(connection_id).cloned() else {
                            return self.fail(Error::new(ErrorKind::Connection, "连接不存在"));
                        };
                        let result = role_operation_for_connection(&config, |connector| {
                            connector.apply_role_plan(connection_id, &plan)
                        });
                        match result {
                            Ok(_) => {
                                AppEvent::UserAdminPgRolePlanFinished(tab_id, plan, Ok(Vec::new()))
                            }
                            Err(error) => AppEvent::UserAdminPgRolePlanFinished(
                                tab_id,
                                plan,
                                Err(UserFacingError::from(error)),
                            ),
                        }
                    }
                    Err(error) => AppEvent::UserAdminPgRolePlanFinished(
                        tab_id,
                        PgRoleSavePlan {
                            database: None,
                            role_name: String::new(),
                            changes: Vec::new(),
                        },
                        Err(UserFacingError::from(error)),
                    ),
                }
            }
            AppCommand::FinishPgRolePlanApply {
                tab_id,
                plan,
                result,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    // 确认框已用完（失败保留草稿，但不重复弹旧 SQL）。
                    admin.pending_sql = None;
                    admin.pending_sql_awaits_preview = false;
                    match result {
                        Ok(_) => {
                            admin.pg_save_status = PgRoleSaveStatus::Idle;
                            admin.pg_draft = None;
                            admin.pg_membership_edits.clear();
                            admin.pg_grant_edits.clear();
                            admin.pg_plan_preview = None;
                            // 应用成功后对象权限基线已变化，标记过期等待重新读取。
                            admin.pg_loaded_target.clear();
                            // 改名后选中角色跟随新身份。
                            admin.pg_selected_role = Some(plan.role_name.clone());
                            admin.pending_selected_pg_role = Some(plan.role_name);
                        }
                        Err(error) => {
                            // 失败保留草稿；结果不确定时（连接中断）标记待核实，不直接重试。
                            admin.pg_save_status = if error.retryable {
                                PgRoleSaveStatus::Idle
                            } else {
                                PgRoleSaveStatus::NeedsVerify
                            };
                            admin.pg_plan_error = Some(error.clone());
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgBeginDeleteRole(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    if let Some(name) = admin
                        .pg_selected_role
                        .clone()
                        .filter(|name| !UserAdminState::pg_is_predefined_role(name))
                    {
                        admin.pg_pending_delete = Some(name);
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::PgCancelDeleteRole(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_pending_delete = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetPgRoleFilter { tab_id, filter } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_role_filter = filter;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::SetUserAdminPgGrantTarget {
                tab_id,
                kind,
                schema,
                object,
                signature,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.pg_grant_kind = kind;
                    admin.pg_grant_schema = schema;
                    admin.pg_grant_object = object;
                    admin.pg_grant_signature = signature;
                    // 目标已变化：旧请求结果不可用；桌面层同时取消旧 Task，避免悬挂 loading。
                    admin.loading_pg_grants = false;
                    admin.pg_grants_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::LoadUserAdminPgObjectGrants(tab_id) => {
                match self.load_pg_object_grants(tab_id) {
                    Ok(result) => AppEvent::UserAdminPgObjectGrantsLoaded(tab_id, Ok(result)),
                    Err(error) => {
                        AppEvent::UserAdminPgObjectGrantsLoaded(tab_id, Err(error.into()))
                    }
                }
            }
            AppCommand::StartUserAdminPgObjectGrantsLoad(tab_id) => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    admin.loading_pg_grants = true;
                    admin.pg_grants_error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            AppCommand::FinishUserAdminPgObjectGrantsLoad {
                tab_id,
                target_fingerprint,
                result,
            } => {
                if let Some(admin) = self.user_admin_state_mut(tab_id) {
                    // 读取期间目标可能已被用户切换；过期结果不能结束新目标的
                    // loading，也不能覆盖新目标状态。
                    if pg_grant_target_fingerprint(admin) == target_fingerprint {
                        admin.loading_pg_grants = false;
                        match result {
                            Ok((grants, effective)) => {
                                admin.pg_object_grants = Some(grants);
                                admin.pg_effective_grants = effective;
                                admin.pg_grants_error = None;
                                // 记录已加载目标指纹，供 UI 判断选择器目标是否已过期需重取。
                                admin.pg_loaded_target = target_fingerprint;
                            }
                            Err(error) => {
                                admin.pg_object_grants = None;
                                admin.pg_effective_grants.clear();
                                admin.pg_grants_error = Some(error);
                            }
                        }
                    }
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))
                }
            }
            // 私有入口仅由 dispatch 的对应变体分组调用。
            _ => unreachable!("仅PostgreSQL 角色、成员关系与授权操作命令进入此分发器"),
        }
    }
}
