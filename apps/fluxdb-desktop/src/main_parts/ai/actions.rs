impl NavicatMain {
    fn open_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_chat.open = true;
        self.ai_chat.history = false;
        if !self.ai_chat.recovery_failed {
            self.ai_chat.error = None;
        }
        self.ai_chat_request(
            fluxdb_app::AiConversationCommand::Load { session_id: None },
            window,
            cx,
        );
    }
    fn save_ai_chat_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_chat_request(
            fluxdb_app::AiConversationCommand::SaveUi {
                draft: self.ai_chat.input.read(cx).value().to_string(),
                expanded_parts: self
                    .controller
                    .state()
                    .ai_conversation
                    .expanded_parts
                    .clone(),
                mode: self.ai_chat.mode,
                drawer_width: self.ai_chat.drawer,
                embedded_width: self.ai_chat.embedded,
            },
            window,
            cx,
        );
    }
    fn close_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_ai_chat_ui(window, cx);
        self.ai_chat.open = false;
        self.ai_chat.history = false;
        // 关闭只隐藏面板，不取消任务、不清消息。返回输入焦点给宿主工作区。
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
    fn new_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controller.state().ai_conversation.session_id.is_none()
            && !self.ai_chat.input.read(cx).value().is_empty()
        {
            self.show_message(
                "新草稿尚未发送；请先发送或清空内容再新建",
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        self.save_ai_chat_ui(window, cx);
        self.ai_chat.epoch += 1;
        self.ai_chat_request(fluxdb_app::AiConversationCommand::New, window, cx);
    }
    fn ai_chat_request(
        &mut self,
        command: fluxdb_app::AiConversationCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ai_chat.pending.push_back(command);
        self.ai_chat_process(window, cx);
    }
    fn ai_chat_process(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_chat._io.is_some() {
            return;
        }
        let Some(command) = self.ai_chat.pending.pop_front() else {
            return;
        };
        let recovering = matches!(command, fluxdb_app::AiConversationCommand::RecoverOnStartup);
        let update_input = matches!(
            command,
            fluxdb_app::AiConversationCommand::Load { .. } | fluxdb_app::AiConversationCommand::New
        );
        if update_input || recovering {
            self.ai_chat.loading = true;
        }
        let epoch = self.ai_chat.epoch;
        let mut controller = self.controller.clone();
        cx.notify();
        self.ai_chat._io = Some(cx.spawn_in(window, async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch(AppCommand::AiConversation(command))
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.ai_chat._io = None;
                this.ai_chat.loading = false;
                if epoch == this.ai_chat.epoch {
                    match event {
                        AppEvent::AiConversationLoaded(view) => {
                            if recovering {
                                this.ai_chat.recovery_failed = false;
                            }
                            if !this.ai_chat.recovery_failed {
                                this.ai_chat.error = None;
                            }
                            if update_input {
                                this.ai_chat.mode = view.preferences.preferred_mode;
                                this.ai_chat.drawer = view.preferences.drawer_width_dp;
                                this.ai_chat.embedded = view.preferences.embedded_width_dp;
                                this.ai_chat.input.update(cx, |input, cx| {
                                    input.set_value(view.draft.clone(), window, cx)
                                });
                                this.ai_chat.dirty = false;
                            }
                            if !recovering {
                                this.controller.dispatch(AppCommand::AiConversation(
                                    fluxdb_app::AiConversationCommand::Apply { view },
                                ));
                            }
                        }
                        AppEvent::AiSettingsError(message) => {
                            if recovering {
                                this.ai_chat.recovery_failed = true;
                            }
                            this.ai_chat.error = Some(message);
                        }
                        _ => {}
                    }
                }
                this.ai_chat_process(window, cx);
                cx.notify();
            });
        }));
    }
    fn send_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_chat.recovery_pending
            || self.ai_chat.recovery_failed
            || self.ai_chat.running.is_some()
            || self.ai_chat.loading
            || self.ai_chat._io.is_some()
        {
            return;
        }
        let prompt = self.ai_chat.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let context = self.capture_ai_active_context(cx);
        let pin = fluxdb_app::pin_target(&context);
        let context_text = context
            .selected_text
            .or_else(|| context.excerpt.map(|e| e.text))
            .or(context.summary)
            .unwrap_or_default();
        let mut controller = self.controller.clone();
        let epoch = self.ai_chat.epoch;
        self.ai_chat.loading = true;
        self.ai_chat.error = None;
        cx.notify();
        self.ai_chat._io = Some(cx.spawn_in(window, async move |view, cx| {
            let (result, chat) = cx
                .background_spawn(async move {
                    let result =
                        controller.prepare_ai_conversation_context(&prompt, &pin, &context_text);
                    (result, controller.state().ai_conversation.clone())
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.ai_chat.loading = false;
                this.ai_chat._io = None;
                match result {
                    Ok(job) => {
                        if epoch != this.ai_chat.epoch {
                            this.ai_chat.error =
                                Some("会话已切换，发送准备已中断；请从历史查看任务".into());
                            cx.notify();
                            return;
                        }
                        this.controller.dispatch(AppCommand::AiConversation(
                            fluxdb_app::AiConversationCommand::Apply { view: chat },
                        ));
                        this.ai_chat
                            .input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.ai_chat.dirty = false;
                        this.ai_chat.follow_reply = true;
                        this.ai_chat.new_reply = false;
                        this.ai_chat.scroll.scroll_to_bottom();
                        this.start_ai_chat_job(job, cx);
                    }
                    Err(e) => this.ai_chat.error = Some(e.message),
                }
                cx.notify();
            });
        }));
    }
    fn start_ai_chat_job(&mut self, job: fluxdb_app::AiConversationJob, cx: &mut Context<Self>) {
        self.start_ai_chat_job_with(job, None, cx);
    }
    /// `resume_approval` 为 Some 时表示"审批已通过，恢复执行那个被暂停的工具调用"。
    fn start_ai_chat_job_with(
        &mut self,
        job: fluxdb_app::AiConversationJob,
        resume_approval: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(storage) = self.controller.ai_storage() else {
            return;
        };
        let cancel = fluxdb_core::CancelFlag::new();
        self.ai_chat.cancel = Some(cancel.clone());
        self.ai_chat.running = Some(job.clone());
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let debug_scripted = cfg!(debug_assertions) && self.ai_chat.scripted_fixture;
        // 工具执行只读借用当前已配置连接；连接配置不含凭据（凭据在 connector 层解密）。
        let connection_configs: Vec<fluxdb_core::ConnectionConfig> = self
            .controller
            .state()
            .connections
            .iter()
            .map(|c| c.config.clone())
            .collect();
        std::thread::spawn(move || {
            if debug_scripted {
                fluxdb_app::run_ai_conversation_source(&storage, &job, &cancel, &tx, |emit| {
                    for text in [
                        "已读取当前目标。\n",
                        "以下是 SQL 解释示例，不表示已执行查询。\n",
                        "SELECT COUNT(*) FROM orders;",
                    ] {
                        if cancel.is_cancelled() {
                            let e = fluxdb_core::ModelEvent::Finished(
                                fluxdb_core::FinishReason::Cancelled,
                            );
                            emit(&e);
                            return Ok(vec![e]);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(340));
                        emit(&fluxdb_core::ModelEvent::TextDelta(text.into()));
                    }
                    let e = fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop);
                    emit(&e);
                    Ok(vec![e])
                });
            } else {
                match fluxdb_app::AiConversationProvider::from_storage(
                    &storage,
                    &job.model_profile_id,
                ) {
                    Ok(provider) => {
                        // 工具只暴露**本轮固定目标连接**：模型不能借 conn:<其它 id> 越权访问别的连接。
                        let (connection_id, agent_allowed_tools) =
                            fluxdb_app::ai_turn_security_context(&storage, &job.turn_id)
                                .unwrap_or((None, Vec::new()));
                        let pinned = connection_id.and_then(|id| {
                            connection_configs
                                .iter()
                                .find(|config| config.id.0 as i64 == id)
                                .cloned()
                        });
                        let include_write = pinned
                            .as_ref()
                            .is_some_and(|config| !fluxdb_app::connection_is_read_only(config));
                        let resolver = std::sync::Arc::new(fluxdb_app::MapSqlResolver::new(
                            pinned.iter().cloned().collect::<Vec<_>>(),
                        ));
                        match fluxdb_app::standard_sql_catalog(resolver, include_write) {
                            Ok(catalog) => {
                                // 写权限来自目标连接（只读连接连写工具都不注册），且 authorize 对 Write 强制逐次审批。
                                let permission = fluxdb_app::TurnPermissionContext {
                                    agent_allowed_tools,
                                    policy: fluxdb_app::kernel_policy_for_connection(pinned.as_ref()),
                                    plugin_digest: None,
                                };
                                match resume_approval.as_deref() {
                                    Some(approval_id) => {
                                        fluxdb_app::run_ai_conversation_resume_approval(
                                            &storage,
                                            &job,
                                            &cancel,
                                            &tx,
                                            &provider,
                                            &catalog,
                                            &fluxdb_app::KernelAuditHook,
                                            permission,
                                            approval_id,
                                        )
                                    }
                                    None => fluxdb_app::run_ai_conversation_turn(
                                        &storage,
                                        &job,
                                        &cancel,
                                        &tx,
                                        &provider,
                                        &catalog,
                                        &fluxdb_app::KernelAuditHook,
                                        permission,
                                    ),
                                }
                            }
                            Err(error) => {
                                fluxdb_app::run_ai_conversation_failure(
                                    &storage,
                                    &job,
                                    &tx,
                                    format!("工具目录不可用：{}", error.message),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        fluxdb_app::run_ai_conversation_failure(&storage, &job, &tx, error.message);
                    }
                }
            }
        });
        self.ai_chat._pump = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(60))
                    .await;
                let mut done = false;
                while let Ok(update) = rx.try_recv() {
                    let terminal = update.terminal;
                    let _ = view.update(cx, |this, cx| {
                        if this
                            .ai_chat
                            .running
                            .as_ref()
                            .is_some_and(|j| j.turn_id == update.job.turn_id)
                        {
                            if this
                                .controller
                                .state()
                                .ai_conversation
                                .session_id
                                .as_deref()
                                == Some(update.job.session_id.as_str())
                            {
                                // 草稿由独立输入 Entity 持有，stream checkpoint 不覆盖当前未发送内容。
                                this.controller.dispatch(AppCommand::AiConversation(
                                    fluxdb_app::AiConversationCommand::Apply { view: update.view },
                                ));
                                if this.ai_chat.follow_reply {
                                    this.ai_chat.scroll.scroll_to_bottom();
                                } else {
                                    this.ai_chat.new_reply = true;
                                }
                            }
                            if terminal {
                                this.ai_chat.running = None;
                                this.ai_chat.cancel = None;
                            }
                        }
                        cx.notify();
                    });
                    done |= terminal;
                }
                if done {
                    break;
                }
                if view.upgrade().is_none() {
                    break;
                }
            }
        }));
    }
    /// 审批决定：先做对象级 CAS 决定，批准后再恢复执行该工具调用。
    fn decide_ai_chat_approval(
        &mut self,
        approval_id: String,
        approved: bool,
        expected_revision: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let event = self.controller.dispatch(AppCommand::AiConversation(
            fluxdb_app::AiConversationCommand::DecideApproval {
                approval_id: approval_id.clone(),
                approved,
                expected_revision,
            },
        ));
        if matches!(event, AppEvent::AiSettingsError(_)) {
            self.show_message(
                "审批状态已变化（可能已处理或失效），请刷新后重试",
                AppMessageKind::Warning,
                cx,
            );
            cx.notify();
            return;
        }
        if !approved {
            cx.notify();
            return;
        }
        let _ = window;
        match self.controller.prepare_ai_resume_job(&approval_id) {
            Ok(job) => self.start_ai_chat_job_with(job, Some(approval_id), cx),
            Err(error) => self.show_message(
                &format!("无法恢复任务：{}", error.message),
                AppMessageKind::Warning,
                cx,
            ),
        }
        cx.notify();
    }
    fn capture_ai_active_context(&self, cx: &App) -> fluxdb_app::UiContextSnapshot {
        let state = self.controller.state();
        let tab = state.active_tab();
        let snapshot = match tab.map(|t| &t.kind) {
            Some(TabKind::QueryEditor(q)) => fluxdb_app::CapturedTab::SqlEditor {
                tab_id: tab.unwrap().id.0,
                revision: 0,
                connection_id: q.connection_id.0 as i64,
                database: q.database.clone(),
                schema: q.schema.clone(),
                selection: self
                    .query_editors
                    .get(&tab.unwrap().id)
                    .and_then(|e| e.read(cx).selected_text()),
                full_text: q.text.clone(),
                statement_at_cursor: None,
            },
            Some(TabKind::DataEditor(d)) => fluxdb_app::CapturedTab::TableData {
                tab_id: tab.unwrap().id.0,
                revision: 0,
                connection_id: d.object.connection_id.0 as i64,
                database: d.object.database.clone(),
                schema: d.object.schema.clone(),
                object_name: d.object.name.clone(),
                columns: d
                    .page
                    .as_ref()
                    .map(|p| p.columns.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default(),
                filter: None,
                sort: None,
                sample_rows: Vec::new(),
                result_ref: None,
            },
            Some(TabKind::RedisWorkbench(r)) => fluxdb_app::CapturedTab::Design {
                tab_id: tab.unwrap().id.0,
                revision: 0,
                connection_id: r.connection_id.0 as i64,
                database: Some(r.database.to_string()),
                schema: None,
                object_path: "Redis 命令工作台".into(),
                summary: r.text.clone(),
            },
            _ => fluxdb_app::CapturedTab::NoTab {
                connection_id: None,
                database: None,
                schema: None,
                last_object: None,
            },
        };
        fluxdb_app::capture(&snapshot, fluxdb_app::ContextLimits::default())
    }
}
