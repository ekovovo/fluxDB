impl NavicatMain {
    /// 在控制器快照上执行命令，仅把事件交回仍存活的主视图，不合并快照状态。
    /// 调用方持有 Task，并负责请求去重、结果身份检查和 Finish 命令；回调后统一通知重绘。
    fn spawn_controller_command(
        &self,
        command: AppCommand,
        finish: impl FnOnce(&mut Self, AppEvent, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let mut controller = self.controller.clone();
        cx.spawn(async move |view, cx| {
            let event = cx
                .background_spawn(async move { controller.dispatch(command) })
                .await;
            let _ = cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    tracing::debug!("后台命令完成时主视图已关闭，丢弃回写");
                    return;
                };
                view.update(cx, |this, cx| {
                    finish(this, event, cx);
                    cx.notify();
                });
            });
        })
    }

    fn spawn_design_table_details(
        &mut self,
        tab_id: TabId,
        object: ObjectPath,
        cx: &mut Context<Self>,
    ) {
        if self._design_table_detail_tasks.contains_key(&tab_id) {
            return;
        }
        let task = self.spawn_controller_command(
            AppCommand::LoadDesignTableDetails(object),
            move |this, event, cx| {
                this._design_table_detail_tasks.remove(&tab_id);
                match event {
                    AppEvent::DesignTableDetailsLoaded(metadata) => {
                        let event = this
                            .controller
                            .dispatch(AppCommand::FinishDesignTableDetails {
                                tab_id,
                                metadata,
                            });
                        this.apply_app_event(&event, cx);
                        cx.notify();
                    }
                    AppEvent::Failed(error) => {
                        this.show_message(error.message, AppMessageKind::Error, cx);
                    }
                    _ => {
                        this.show_message(
                            tr("app.create_table.error.load_design_failed").as_ref(),
                            AppMessageKind::Error,
                            cx,
                        );
                    }
                }
            },
            cx,
        );
        self._design_table_detail_tasks.insert(tab_id, task);
    }
}
