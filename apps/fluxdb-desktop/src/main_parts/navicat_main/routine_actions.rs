impl NavicatMain {
    fn open_routine_ddl(&mut self, object_path: ObjectPath, cx: &mut Context<Self>) {
        self.pending_routine_ddl = Some(PendingRoutineDdl {
            object_path: object_path.clone(),
            ddl: None,
            error: None,
        });
        let controller = self.controller.clone();
        let task_path = object_path.clone();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { controller.load_table_ddl(&task_path) })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._routine_ddl_task = None;
                    if let Some(form) = &mut this.pending_routine_ddl
                        && form.object_path == object_path
                    {
                        match result {
                            Ok(ddl) => form.ddl = Some(ddl),
                            Err(error) => form.error = Some(error.message),
                        }
                        cx.notify();
                    }
                });
            });
        });
        self._routine_ddl_task = Some(task);
        cx.notify();
    }

    fn close_routine_ddl(&mut self, cx: &mut Context<Self>) {
        self.pending_routine_ddl = None;
        self._routine_ddl_task = None;
        cx.notify();
    }
}
