// 数据库切换保持在 AppCommand 边界内，不从视图直接读取系统目录。
impl NavicatMain {
    fn select_sqlserver_security_database(
        &mut self,
        tab_id: TabId,
        database: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self
            .user_admin_state_for(tab_id)
            .and_then(|admin| admin.sqlserver)
        else {
            return;
        };
        if state.database == database {
            return;
        }
        let event = self.dispatch(
            AppCommand::SelectSqlServerSecurityDatabase { tab_id, database },
            cx,
        );
        if !matches!(event, AppEvent::Failed(_)) {
            self.start_sqlserver_security_load(tab_id, cx);
        }
    }
}

fn sqlserver_security_database_control(
    state: &SqlServerSecurityState,
    this: &mut NavicatMain,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    if this.sqlserver_security_database_items != state.databases {
        this.sqlserver_security_database_items = state.databases.clone();
        this.sqlserver_security_database_select
            .update(cx, |select, cx| {
                select.set_items(SearchableVec::new(state.databases.clone()), window, cx);
            });
    }
    sync_select_value(
        &this.sqlserver_security_database_select,
        &state.database,
        window,
        cx,
    );
    div()
        .w(px(240.))
        .debug_selector(|| "sqlserver-security-database".into())
        .child(
            Select::new(&this.sqlserver_security_database_select)
                .placeholder(tr("app.sqlserver_security.choose_database"))
                .disabled(!state.can_switch_database())
                .w_full()
                .h(px(30.)),
        )
}
