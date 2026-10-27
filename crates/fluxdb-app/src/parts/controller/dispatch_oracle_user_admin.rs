impl AppController {
    // Oracle/达梦用户、授权计划与确认操作独立成帧，避免权限草稿与计划占用无关命令的栈空间。
    fn dispatch_oracle_user_admin_command(&mut self, command: AppCommand) -> AppEvent {
        match command {
            AppCommand::StartOracleUsersLoad(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.loading = true;
                    state.error = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::LoadOracleUsers(tab_id) => match self.load_oracle_user_admin(tab_id) {
                Ok(snapshot) => AppEvent::OracleUsersLoaded(tab_id, snapshot),
                Err(error) => AppEvent::Failed(UserFacingError::from(error)),
            },
            AppCommand::FinishOracleUsersLoad { tab_id, result } => {
                self.finish_oracle_users_load(tab_id, result)
            }
            AppCommand::SelectOracleUser { tab_id, name } => self.oracle_select_user(tab_id, name),
            AppCommand::SetOracleGrantDraftField {
                tab_id,
                field,
                value,
            } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.set_grant_field(field, value);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetOracleGrantDraftOption {
                tab_id,
                with_option,
            } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.grant.with_option = with_option;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            // 授权编辑器一次只放一条草稿：加入待执行列表后清空，便于同一次保存带多条授权。
            // 类型由所在页签决定，先写进草稿再入列，避免沿用上一个页签的类型。
            AppCommand::QueueOracleGrantDraft { tab_id, kind } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.grant.kind = kind;
                }
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_grant_draft())
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            AppCommand::RemoveOraclePendingGrant { tab_id, index } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.remove_pending_grant(index);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::RevokeOracleRole { tab_id, role } => {
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_revoke_role(&role))
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            AppCommand::RevokeOracleSystemPrivilege { tab_id, privilege } => {
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_revoke_system_privilege(&privilege))
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            AppCommand::RevokeOracleObjectPrivilege {
                tab_id,
                owner,
                object,
                privilege,
            } => {
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_revoke_object_privilege(&owner, &object, &privilege))
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            AppCommand::BeginOracleCreateUser(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.creating = Some(OracleCreateDraft::default());
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::CancelOracleCreateUser(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.creating = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetOracleCreateDraftField {
                tab_id,
                field,
                value,
            } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.set_create_field(field, value);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetOracleCreateDraftFlag {
                tab_id,
                grant_connect_role,
            } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.set_create_flag(grant_connect_role);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            // 口令只留在内存草稿里，保存时再进计划；这里不做校验也不落任何记录。
            AppCommand::SetOracleUserPassword { tab_id, password } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.new_password = password;
                }
                AppEvent::TabActivated(tab_id)
            }
            AppCommand::SetOracleUserLocked { tab_id, locked } => {
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_account_locked(locked))
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            AppCommand::DiscardOracleUserChanges(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.discard_changes();
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            // 「保存」第一步：把草稿渲染成预览 SQL 放进确认框，不碰数据库。
            AppCommand::PreviewOracleApply(tab_id) => {
                let Some(state) = self.oracle_user_admin_state_mut(tab_id) else {
                    return self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ));
                };
                match state.compose_preview() {
                    Ok(statements) if !statements.is_empty() => {
                        let danger = statements
                            .iter()
                            .any(|sql| sql.trim_start().starts_with("DROP "));
                        let sql = statements.join("\n");
                        if let Some(admin) = self.user_admin_state_mut(tab_id) {
                            admin.pending_sql = Some(UserAdminPendingSql {
                                sql,
                                danger,
                                kind: UserAdminPendingKind::Oracle,
                            });
                            admin.apply_error = None;
                        }
                        AppEvent::TabActivated(tab_id)
                    }
                    // 空草稿与合成失败都不弹框：没有可执行的语句。
                    Ok(_) => AppEvent::TabActivated(tab_id),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            // 确认框点「执行」后：先合成计划，成功才置位执行中；`ApplyOraclePlan` 直接跑这一份。
            AppCommand::StartOracleApply(tab_id) => {
                let Some(state) = self.oracle_user_admin_state_mut(tab_id) else {
                    return self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ));
                };
                state.apply_error = None;
                match state.compose_plan() {
                    Ok(plan) => {
                        state.pending_plan = Some(plan);
                        state.applying = true;
                        AppEvent::TabActivated(tab_id)
                    }
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::ApplyOraclePlan(tab_id) => {
                match self.apply_oracle_user_admin_plan(tab_id) {
                    Ok(statements) => AppEvent::OraclePlanApplied(tab_id, statements),
                    Err(error) => AppEvent::Failed(UserFacingError::from(error)),
                }
            }
            AppCommand::FinishOracleApply { tab_id, result } => {
                self.oracle_finish_apply(tab_id, result)
            }
            AppCommand::BeginDeleteOracleUser(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    let Some(name) = state.selected.clone() else {
                        return AppEvent::TabActivated(tab_id);
                    };
                    // SYS 等自带账号与当前登录账号不允许从这里删除。
                    let protected = state
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.user(&name))
                        .is_some_and(|user| user.oracle_maintained)
                        || state.snapshot.as_ref().is_some_and(|snapshot| {
                            snapshot.current_user.eq_ignore_ascii_case(&name)
                        });
                    if protected {
                        return AppEvent::Failed(UserFacingError::from(Error::new(
                            ErrorKind::Permission,
                            "该 Oracle 账号受保护（系统账号或当前登录账号），不能删除",
                        )));
                    }
                    state.pending_delete = Some(name);
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::CancelDeleteOracleUser(tab_id) => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.pending_delete = None;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::SetOracleDeleteCascade { tab_id, cascade } => {
                if let Some(state) = self.oracle_user_admin_state_mut(tab_id) {
                    state.delete_cascade = cascade;
                    AppEvent::TabActivated(tab_id)
                } else {
                    self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    ))
                }
            }
            AppCommand::ConfirmDeleteOracleUser(tab_id) => {
                match self
                    .oracle_user_admin_state_mut(tab_id)
                    .map(|state| state.queue_delete())
                {
                    Some(Ok(())) => AppEvent::TabActivated(tab_id),
                    Some(Err(error)) => AppEvent::Failed(UserFacingError::from(error)),
                    None => self.fail(Error::new(
                        ErrorKind::Internal,
                        "Oracle 用户与权限标签页不存在",
                    )),
                }
            }
            // 私有入口仅由 dispatch 的对应变体分组调用。
            _ => unreachable!("仅Oracle/达梦用户、授权计划与确认操作命令进入此分发器"),
        }
    }
}
