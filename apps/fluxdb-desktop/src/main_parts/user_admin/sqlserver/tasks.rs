impl NavicatMain {
    fn start_sqlserver_security_load(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        self.start_sqlserver_security_load_inner(tab_id, false, cx);
    }

    /// 保存成功后的强制回读：服务端已提交，忽略本地“仍有草稿”的门禁。
    fn refresh_sqlserver_security_after_apply(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) {
        self.start_sqlserver_security_load_inner(tab_id, true, cx);
    }

    fn start_sqlserver_security_load_inner(
        &mut self,
        tab_id: TabId,
        after_apply: bool,
        cx: &mut Context<Self>,
    ) {
        if self._sqlserver_security_load_task.is_some() {
            return;
        }
        let Some(security) = self
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.sqlserver)
        else {
            return;
        };
        if security.applying || security.preview_loading {
            return;
        }
        if !after_apply
            && security.has_changes()
            && security.save_status != SqlServerSecuritySaveStatus::NeedsVerify
        {
            return;
        }
        let load_snapshot = !security.database.is_empty();
        self.dispatch(AppCommand::StartSqlServerSecurityLoad(tab_id), cx);
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let (databases, snapshot) = cx
                .background_spawn(async move {
                    let databases = match controller
                        .dispatch(AppCommand::LoadSqlServerSecurityDatabases(tab_id))
                    {
                        AppEvent::SqlServerSecurityDatabasesLoaded(_, result) => result,
                        AppEvent::Failed(error) => Err(error),
                        _ => Err(fluxdb_core::UserFacingError {
                            title: tr("app.sqlserver_security.load_failed").to_string(),
                            message: tr("app.sqlserver_security.load_empty").to_string(),
                            detail: None,
                            retryable: true,
                        }),
                    };
                    let snapshot = if load_snapshot && databases.is_ok() {
                        Some(
                            match controller.dispatch(AppCommand::LoadSqlServerSecurity(tab_id)) {
                                AppEvent::UserAdminSqlServerLoaded(_, result) => result,
                                AppEvent::Failed(error) => Err(error),
                                _ => Err(fluxdb_core::UserFacingError {
                                    title: tr("app.sqlserver_security.load_failed").to_string(),
                                    message: tr("app.sqlserver_security.load_empty").to_string(),
                                    detail: None,
                                    retryable: true,
                                }),
                            },
                        )
                    } else {
                        None
                    };
                    (databases, snapshot)
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._sqlserver_security_load_task = None;
                    this.dispatch(
                        AppCommand::FinishSqlServerSecurityDatabases {
                            tab_id,
                            result: databases,
                        },
                        cx,
                    );
                    if let Some(result) = snapshot {
                        this.dispatch(
                            AppCommand::FinishSqlServerSecurityLoad { tab_id, result },
                            cx,
                        );
                    }
                });
            });
        });
        self._sqlserver_security_load_task = Some(task);
    }

    fn start_sqlserver_permissions_load(
        &mut self,
        tab_id: TabId,
        principal: fluxdb_core::SqlServerPrincipalRef,
        cx: &mut Context<Self>,
    ) {
        if self._sqlserver_security_permissions_task.is_some() {
            return;
        }
        self.dispatch(
            AppCommand::StartSqlServerPermissionsLoad {
                tab_id,
                principal: principal.clone(),
            },
            cx,
        );
        // 待创建主体尚未存在于服务端，查询只会覆盖本地权限草稿并显示空/错误。
        if self
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.sqlserver)
            .is_some_and(|state| state.pending_creations().contains(&principal))
        {
            return;
        }
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match controller.dispatch(AppCommand::LoadSqlServerPermissions {
                        tab_id,
                        principal: principal.clone(),
                    }) {
                        AppEvent::UserAdminSqlServerPermissionsLoaded {
                            principal, result, ..
                        } => (principal, result),
                        AppEvent::Failed(error) => (principal, Err(error)),
                        _ => (
                            principal,
                            Err(fluxdb_core::UserFacingError {
                                title: tr("app.sqlserver_security.load_permissions_failed")
                                    .to_string(),
                                message: tr("app.sqlserver_security.load_permissions_empty")
                                    .to_string(),
                                detail: None,
                                retryable: true,
                            }),
                        ),
                    }
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._sqlserver_security_permissions_task = None;
                    let (principal, result) = result;
                    this.dispatch(
                        AppCommand::FinishSqlServerPermissionsLoad {
                            tab_id,
                            principal,
                            result,
                        },
                        cx,
                    );
                });
            });
        });
        self._sqlserver_security_permissions_task = Some(task);
    }

    fn start_sqlserver_security_preview(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._sqlserver_security_preview_task.is_some() {
            return;
        }
        if !self.prepare_sqlserver_security_preview(tab_id, cx) {
            return;
        }
        let Some(revision) = self
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.sqlserver)
            .map(|state| state.revision)
        else {
            return;
        };
        if matches!(
            self.dispatch(AppCommand::StartSqlServerSecurityPreview(tab_id), cx),
            AppEvent::Failed(_)
        ) {
            return;
        }
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match controller.dispatch(AppCommand::LoadSqlServerSecurityPreview(tab_id)) {
                        AppEvent::UserAdminSqlServerPreview { result, .. } => result,
                        AppEvent::Failed(error) => Err(error),
                        _ => Err(fluxdb_core::UserFacingError {
                            title: tr("app.sqlserver_security.preview_failed").to_string(),
                            message: tr("app.sqlserver_security.preview_empty").to_string(),
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
                    this._sqlserver_security_preview_task = None;
                    this.dispatch(
                        AppCommand::FinishSqlServerSecurityPreview {
                            tab_id,
                            revision,
                            result,
                        },
                        cx,
                    );
                });
            });
        });
        self._sqlserver_security_preview_task = Some(task);
    }

    /// 「保存」：先把表单落进待执行变更，再生成预览 SQL 弹确认框。
    ///
    /// 预览要过连接器渲染（读服务端版本），只能异步生成；预览完成时
    /// `FinishSqlServerSecurityPreview` 据此弹框。用户点「执行」才提交。
    fn start_sqlserver_security_apply(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._sqlserver_security_apply_task.is_some()
            || self._sqlserver_security_preview_task.is_some()
        {
            return;
        }
        if !self.prepare_sqlserver_security_preview(tab_id, cx) {
            return;
        }
        self.dispatch(AppCommand::BeginUserAdminSqlConfirm(tab_id), cx);
        self.start_sqlserver_security_preview(tab_id, cx);
    }

    /// 确认框点「执行」：提交已经核实的变更计划。
    fn execute_sqlserver_security_plan(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._sqlserver_security_apply_task.is_some() {
            return;
        }
        if matches!(
            self.dispatch(AppCommand::StartSqlServerSecurityApply(tab_id), cx),
            AppEvent::Failed(_)
        ) {
            return;
        }
        let mut controller = self.controller.clone();
        let task = cx.spawn(async move |view, cx| {
            let (principal, result) = cx
                .background_spawn(async move {
                    match controller.dispatch(AppCommand::ApplySqlServerSecurityPlan(tab_id)) {
                        AppEvent::UserAdminSqlServerApplied {
                            principal, result, ..
                        } => (principal, result),
                        AppEvent::Failed(error) => (String::new(), Err(error)),
                        _ => (
                            String::new(),
                            Err(fluxdb_core::UserFacingError {
                                title: tr("app.sqlserver_security.apply_failed").to_string(),
                                message: tr("app.sqlserver_security.apply_empty").to_string(),
                                detail: None,
                                retryable: true,
                            }),
                        ),
                    }
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._sqlserver_security_apply_task = None;
                    let success = result.is_ok();
                    this.dispatch(
                        AppCommand::FinishSqlServerSecurityApply {
                            tab_id,
                            principal,
                            result,
                        },
                        cx,
                    );
                    if success {
                        this.show_message(
                            tr("app.sqlserver_security.apply_success"),
                            AppMessageKind::Success,
                            cx,
                        );
                        this.refresh_sqlserver_security_after_apply(tab_id, cx);
                    }
                });
            });
        });
        self._sqlserver_security_apply_task = Some(task);
    }

    fn sync_sqlserver_security_inputs(
        &mut self,
        state: &SqlServerSecurityState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_sqlserver_security_selects(state, window, cx);
        sync_input_value(
            &self.sqlserver_security_name_input,
            &state.form.name,
            window,
            cx,
        );
        sync_input_value(
            &self.sqlserver_security_password_input,
            &state.form.password,
            window,
            cx,
        );
        sync_input_value(
            &self.sqlserver_security_confirm_password_input,
            &state.form.confirm_password,
            window,
            cx,
        );
        sync_input_value(
            &self.sqlserver_security_permission_input,
            &state.permission_form.permission,
            window,
            cx,
        );
        sync_input_value(
            &self.sqlserver_security_permission_target_input,
            &state.permission_form.permission_target,
            window,
            cx,
        );
    }
}
