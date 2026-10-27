// Oracle 用户与权限页的后台任务：加载 / 保存时出预览 / 执行。
//
// 与 SQL Server 面板一致：dispatch 出 Start 命令置位，`background_spawn` 里做真实
// 数据库往返，回主线程再 Finish；面板永不阻塞 UI 线程。

impl NavicatMain {
    fn start_oracle_security_load(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._oracle_security_load_task.is_some() {
            return;
        }
        if self
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.oracle)
            .is_some_and(|state| state.applying || state.has_changes())
        {
            // 有未执行的变更时不覆盖草稿；先让用户预览或放弃。
            return;
        }
        self.dispatch(AppCommand::StartOracleUsersLoad(tab_id), cx);
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match controller.dispatch(AppCommand::LoadOracleUsers(tab_id)) {
                        AppEvent::OracleUsersLoaded(_, snapshot) => Ok(snapshot),
                        AppEvent::Failed(error) => Err(error),
                        _ => Err(fluxdb_core::UserFacingError {
                            title: tr("app.oracle_security.load_failed").to_string(),
                            message: tr("app.oracle_security.load_empty").to_string(),
                            detail: None,
                            retryable: true,
                        }),
                    }
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._oracle_security_load_task = None;
                    this.dispatch(AppCommand::FinishOracleUsersLoad { tab_id, result }, cx);
                });
            });
        });
        self._oracle_security_load_task = Some(task);
    }

    /// 「保存」：把草稿渲染成预览 SQL 弹确认框，用户点「执行」才真正提交。
    fn start_oracle_security_apply(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._oracle_security_apply_task.is_some() {
            return;
        }
        // 合成失败（如两次口令不一致、账号已存在）时返回 Failed，错误由状态里的
        // `apply_error` 渲染，这里不需要额外处理。
        let _ = self.dispatch(AppCommand::PreviewOracleApply(tab_id), cx);
    }

    /// 确认框点「执行」：合成计划后逐条提交。
    fn execute_oracle_security_plan(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._oracle_security_apply_task.is_some() {
            return;
        }
        if matches!(
            self.dispatch(AppCommand::StartOracleApply(tab_id), cx),
            AppEvent::Failed(_)
        ) {
            return;
        }
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match controller.dispatch(AppCommand::ApplyOraclePlan(tab_id)) {
                        AppEvent::OraclePlanApplied(_, statements) => Ok(statements),
                        AppEvent::Failed(error) => Err(error),
                        _ => Err(fluxdb_core::UserFacingError {
                            title: tr("app.oracle_security.apply_failed").to_string(),
                            message: tr("app.oracle_security.apply_empty").to_string(),
                            detail: None,
                            retryable: true,
                        }),
                    }
                })
                .await;
            let success = result.is_ok();
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._oracle_security_apply_task = None;
                    this.dispatch(AppCommand::FinishOracleApply { tab_id, result }, cx);
                    if success {
                        this.show_message(
                            tr("app.oracle_security.apply_success"),
                            AppMessageKind::Success,
                            cx,
                        );
                        // 成功后强制回读目录：已提交的语句必须反映到界面上。
                        this.start_oracle_security_load(tab_id, cx);
                    } else {
                        this.show_message(
                            tr("app.oracle_security.apply_partial_warning"),
                            AppMessageKind::Error,
                            cx,
                        );
                    }
                });
            });
        });
        self._oracle_security_apply_task = Some(task);
    }

    fn sync_oracle_security_inputs(
        &mut self,
        state: &OracleUserAdminState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 口令输入只在草稿存在时同步值；执行成功后草稿清空，输入框随之清空。
        let create = state.creating.clone().unwrap_or_default();
        sync_input_value(&self.oracle_security_name_input, &create.name, window, cx);
        sync_input_value(
            &self.oracle_security_password_input,
            &create.password,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_confirm_password_input,
            &create.confirm_password,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_default_ts_input,
            &create.default_tablespace,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_temp_ts_input,
            &create.temporary_tablespace,
            window,
            cx,
        );
        sync_input_value(&self.oracle_security_profile_input, &create.profile, window, cx);
        sync_input_value(
            &self.oracle_security_quota_ts_input,
            &create.quota_tablespace,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_quota_bytes_input,
            &create.quota_bytes,
            window,
            cx,
        );
        // 改密口令同理：以状态里的草稿为准，执行成功后被清空。
        sync_input_value(
            &self.oracle_security_new_password_input,
            &state.new_password,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_grant_name_input,
            &state.grant.name,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_grant_owner_input,
            &state.grant.owner,
            window,
            cx,
        );
        sync_input_value(
            &self.oracle_security_grant_object_input,
            &state.grant.object,
            window,
            cx,
        );
    }
}
