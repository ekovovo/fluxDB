// Oracle 用户与权限的编排：读取快照、合成计划、执行计划。
//
// 与 SQL Server 的差别：没有数据库层可选，一切以连接当前账号为中心；执行不是单事务，
// 失败时保留已执行语句的脱敏记录，避免把「半完成」说成「已回滚」。

impl AppController {
    fn oracle_user_admin_state_mut(
        &mut self,
        tab_id: TabId,
    ) -> Option<&mut OracleUserAdminState> {
        self.user_admin_state_mut(tab_id)?.oracle.as_mut()
    }

    fn oracle_user_admin_connection(
        &self,
        tab_id: TabId,
    ) -> fluxdb_core::Result<(ConnectionId, ConnectionConfig)> {
        let admin = self
            .user_admin_state(tab_id)
            .ok_or_else(|| Error::new(ErrorKind::Internal, "用户与权限标签页不存在"))?;
        let config = self
            .connection_config(admin.connection_id)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        // 达梦复用同一套账号面板，连接器侧各自实现 `oracle_user_admin_*`。
        if !matches!(config.kind, DatabaseKind::Oracle | DatabaseKind::Dameng) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "该连接不是 Oracle 或达梦",
            ));
        }
        Ok((admin.connection_id, config))
    }

    fn load_oracle_user_admin(&self, tab_id: TabId) -> fluxdb_core::Result<OracleUserAdminSnapshot> {
        let (connection_id, config) = self.oracle_user_admin_connection(tab_id)?;
        role_operation_for_connection(&config, |connector| {
            connector.oracle_user_admin_snapshot(connection_id)
        })
    }

    /// 执行计划。返回脱敏语句列表；错误信息里也不得包含口令。
    fn apply_oracle_user_admin_plan(
        &self,
        tab_id: TabId,
    ) -> fluxdb_core::Result<Vec<String>> {
        let (connection_id, config) = self.oracle_user_admin_connection(tab_id)?;
        let state = self
            .user_admin_state(tab_id)
            .and_then(|admin| admin.oracle.as_ref())
            .ok_or_else(|| Error::new(ErrorKind::Internal, "Oracle 用户与权限状态不存在"))?;
        // `applying` 是界面进度标志（StartOracleApply 会先置位），不是重入锁：
        // 并发双击由界面的执行任务挡掉，这里不能拿它拦下本次执行。
        let Some(plan) = state.pending_plan.clone() else {
            return Err(Error::new(ErrorKind::Query, "没有待执行的 Oracle 变更"));
        };
        if plan.is_empty() {
            return Err(Error::new(ErrorKind::Query, "没有待执行的 Oracle 变更"));
        }
        role_operation_for_connection(&config, |connector| {
            connector.apply_oracle_user_admin_plan(connection_id, &plan)
        })
    }

    /// Oracle 账号列表加载完成回填；保留当前选择（选中项消失时才清空）。
    fn finish_oracle_users_load(
        &mut self,
        tab_id: TabId,
        result: std::result::Result<OracleUserAdminSnapshot, UserFacingError>,
    ) -> AppEvent {
        let Some(state) = self.oracle_user_admin_state_mut(tab_id) else {
            return self.fail(Error::new(ErrorKind::Internal, "Oracle 用户与权限标签页不存在"));
        };
        state.loading = false;
        match result {
            Ok(snapshot) => {
                let keep = state
                    .selected
                    .clone()
                    .filter(|name| snapshot.contains_user(name));
                state.selected = keep.or_else(|| Some(snapshot.current_user.clone()));
                state.error = None;
                // 目录快照必须落到状态里：面板的账号列表、授权与配额全部从这里读。
                state.snapshot = Some(snapshot.clone());
                AppEvent::OracleUsersLoaded(tab_id, snapshot)
            }
            Err(error) => {
                state.error = Some(error.clone());
                self.state.last_error = Some(error.clone());
                AppEvent::Failed(error)
            }
        }
    }

    fn oracle_select_user(&mut self, tab_id: TabId, name: String) -> AppEvent {
        let Some(state) = self.oracle_user_admin_state_mut(tab_id) else {
            return self.fail(Error::new(ErrorKind::Internal, "Oracle 用户与权限标签页不存在"));
        };
        state.selected = Some(name.to_ascii_uppercase());
        // 换账号后授权与改密草稿作废，否则下一条 GRANT / 改密会打到错误的账号。
        state.grant = OracleGrantDraft::default();
        state.new_password.clear();
        state.pending_plan = None;
        AppEvent::TabActivated(tab_id)
    }

    fn oracle_finish_apply(
        &mut self,
        tab_id: TabId,
        result: std::result::Result<Vec<String>, UserFacingError>,
    ) -> AppEvent {
        // 确认框已用完：成功失败都关掉，错误留在 `apply_error` 里给用户看。
        if let Some(admin) = self.user_admin_state_mut(tab_id) {
            admin.pending_sql = None;
            admin.pending_sql_awaits_preview = false;
        }
        let Some(state) = self.oracle_user_admin_state_mut(tab_id) else {
            return self.fail(Error::new(ErrorKind::Internal, "Oracle 用户与权限标签页不存在"));
        };
        state.applying = false;
        match result {
            Ok(statements) => {
                state.apply_error = None;
                state.pending_changes.clear();
                // 待执行授权也是草稿：不清掉会在下一次保存里重复 GRANT。
                state.pending_grants.clear();
                state.creating = None;
                state.new_password.clear();
                state.pending_plan = None;
                state.grant = OracleGrantDraft::default();
                AppEvent::OraclePlanApplied(tab_id, statements)
            }
            Err(error) => {
                // 失败时计划保留：Oracle 已隐式提交的语句无法撤销，必须由用户确认后重跑剩余部分。
                state.apply_error = Some(error.clone());
                self.state.last_error = Some(error.clone());
                AppEvent::Failed(error)
            }
        }
    }
}
