// Oracle 面板的输入订阅辅助：只有当前激活的是「带 Oracle 状态」的用户与权限标签
// 才发命令，避免在 MySQL/PG/SQL Server 标签页里误发 Oracle 命令。
//
// 改密口令的订阅在 `app_boot.rs`：明文只写进 `OracleUserAdminState::new_password`（内存草稿），
// 保存时随计划一起执行；确认框不订阅状态，只触发重绘做一致性校验。

impl NavicatMain {
    /// 当前激活且带 Oracle 状态的用户与权限标签；其它标签返回 None。
    fn active_oracle_admin_tab_id(&self) -> Option<TabId> {
        let tab_id = self.active_user_admin_tab_id()?;
        self.user_admin_state_for(tab_id)
            .and_then(|admin| admin.oracle)
            .is_some()
            .then_some(tab_id)
    }

    fn oracle_create_field_changed(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        field: fluxdb_app::OracleCreateDraftField,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(tab_id) = self.active_oracle_admin_tab_id() else {
            return;
        };
        let value = input.read(cx).value().to_string();
        self.dispatch(
            AppCommand::SetOracleCreateDraftField { tab_id, field, value },
            cx,
        );
    }

    fn oracle_grant_field_changed(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        field: fluxdb_app::OracleGrantDraftField,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(tab_id) = self.active_oracle_admin_tab_id() else {
            return;
        };
        let value = input.read(cx).value().to_string();
        self.dispatch(
            AppCommand::SetOracleGrantDraftField { tab_id, field, value },
            cx,
        );
    }
}
