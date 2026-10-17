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
}
