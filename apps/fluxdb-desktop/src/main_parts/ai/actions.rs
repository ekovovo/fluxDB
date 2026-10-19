impl NavicatMain {
    fn open_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_chat.open = true;
        self.ai_chat.history = false;
        self.ai_chat.follow_reply = true;
        self.ai_chat.new_reply = false;
        if !self.ai_chat.recovery_failed {
            self.ai_chat.error = None;
        }
        if self.ai_chat.loaded {
            self.mount_ai_chat_tab(cx);
            self.ai_chat.input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        } else {
            self.ai_chat_request(
                fluxdb_app::AiConversationCommand::Load { session_id: None },
                window,
                cx,
            );
        }
    }
    fn ai_chat_ui_command(&self, cx: &App) -> fluxdb_app::AiConversationCommand {
        fluxdb_app::AiConversationCommand::SaveUi {
            draft: self.ai_chat.input.read(cx).value().to_string(),
            expanded_parts: self.controller.state().ai_conversation.expanded_parts.clone(),
            mode: self.ai_chat.mode,
            drawer_width: self.ai_chat.drawer,
            embedded_width: self.ai_chat.embedded,
        }
    }
    fn save_ai_chat_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai_chat_request(self.ai_chat_ui_command(cx), window, cx);
    }
    fn close_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reject_pending_ai_chat_approval(window, cx);
        self.save_ai_chat_ui(window, cx);
        self.ai_chat.open = false;
        self.ai_chat.history = false;
        if let Some(tab_id) = self.ai_chat.tab_id.take() {
            self.dispatch(AppCommand::CloseTab(tab_id), cx);
        }
        // 关闭不取消正在执行的任务、不清消息；待审批调用则明确拒绝本次。
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
    fn new_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controller.state().ai_conversation.session_id.is_none()
            && !self.ai_chat.input.read(cx).value().is_empty()
        {
            self.show_message(
                tr("app.ai.chat.action.unsent_draft").as_ref(),
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        self.save_ai_chat_ui(window, cx);
        self.ai_chat.epoch += 1;
        self.ai_chat_request(fluxdb_app::AiConversationCommand::New, window, cx);
    }
    /// 「用 AI 修复」：把失败 SQL 与错误报文交给 AI；会话未加载时待加载完成后自动发送。
    fn ai_fix_query_error(
        &mut self,
        summary: QueryExecutionSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = format!(
            "下面这条 SQL 执行失败，请帮我修复。\n\n错误信息：\n{}\n\n失败的 SQL：\n```sql\n{}\n```\n\n请只给出修复后的完整 SQL 和简短原因，不要调用数据库执行工具。",
            summary.message.trim(),
            summary.sql.trim()
        );
        if self.ai_chat.loaded {
            self.ai_chat
                .input
                .update(cx, |input, cx| input.set_value(prompt, window, cx));
            self.open_ai_chat(window, cx);
            self.send_ai_chat(window, cx);
        } else {
            self.ai_chat.pending_fix_prompt = Some(prompt);
            self.open_ai_chat(window, cx);
        }
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
        let approval = match &command {
            fluxdb_app::AiConversationCommand::DecideApproval { approval_id, approved, .. } =>
                Some((approval_id.clone(), *approved)),
            _ => None,
        };
        self.ai_chat.deciding_approval = approval.is_some();
        if update_input || recovering {
            self.ai_chat.loading = true;
        }
        let epoch = self.ai_chat.epoch;
        let mut controller = self.controller.clone();
        cx.notify();
        self.ai_chat._io = Some(cx.spawn_in(window, async move |view, cx| {
            let background_approval = approval.clone();
            let (event, resume, reference_errors) = cx
                .background_spawn(async move {
                    let event = controller.dispatch(AppCommand::AiConversation(command));
                    // 决定与恢复准备都读写 SQLite，在后台完成；只有决定成功才准备恢复。
                    let resume = background_approval
                        .filter(|(_, approved)| *approved && matches!(event, AppEvent::AiConversationLoaded(_)))
                        .map(|(id, _)| controller.prepare_ai_resume_job(&id));
                    let reference_errors = controller.ai_reference_errors();
                    (event, resume, reference_errors)
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.ai_chat._io = None;
                this.ai_chat.deciding_approval = false;
                this.ai_chat.loading = false;
                if epoch == this.ai_chat.epoch {
                    match event {
                        AppEvent::AiConversationLoaded(view) => {
                            this.ai_chat.reference_errors = reference_errors;
                            if recovering {
                                this.ai_chat.recovery_failed = false;
                            }
                            if !this.ai_chat.recovery_failed {
                                this.ai_chat.error = None;
                            }
                            if update_input {
                                this.ai_chat.loaded = true;
                                this.ai_chat.follow_reply = true;
                                this.ai_chat.new_reply = false;
                                this.ai_chat.mode = view.preferences.preferred_mode;
                                this.ai_chat.drawer = view.preferences.drawer_width_dp;
                                this.ai_chat.embedded = view.preferences.embedded_width_dp;
                                this.ai_chat.input.update(cx, |input, cx| {
                                    input.set_value(view.draft.clone(), window, cx)
                                });
                                this.ai_chat.dirty = false;
                                if this.ai_chat.open {
                                    this.mount_ai_chat_tab(cx);
                                }
                                if let Some(prompt) = this.ai_chat.pending_fix_prompt.take() {
                                    this.ai_chat.input.update(cx, |input, cx| {
                                        input.set_value(prompt, window, cx)
                                    });
                                    this.ai_chat.dirty = false;
                                    this.send_ai_chat(window, cx);
                                }
                            }
                            if !recovering {
                                this.controller.dispatch(AppCommand::AiConversation(
                                    fluxdb_app::AiConversationCommand::Apply { view },
                                ));
                            }
                            // 切会话/新建：清掉旧会话的流式正文与揭示任务。
                            if update_input || recovering {
                                this.ai_chat.stream.reset();
                                this.ai_chat.stream_reveal = None;
                                if let Some(view) = this.ai_chat.stream_view.clone() {
                                    view.update(cx, |state, cx| state.set_text("", cx));
                                }
                            }
                            if update_input {
                                // ScrollHandle 在下次布局时使用新会话内容的高度定位。
                                this.ai_chat.scroll.scroll_to_bottom();
                            }
                        }
                        AppEvent::AiSettingsError(message) => {
                            if approval.is_some() {
                                this.show_message(
                                    tr("app.ai.chat.action.approval_changed").as_ref(),
                                    AppMessageKind::Warning, cx,
                                );
                            } else {
                                if recovering {
                                    this.ai_chat.recovery_failed = true;
                                }
                                this.ai_chat.error = Some(message);
                            }
                        }
                        _ => {}
                    }
                    if let (Some((id, _)), Some(resume)) = (approval, resume) {
                        match resume {
                            Ok(job) => this.start_ai_chat_job_with(job, Some(id), cx),
                            Err(error) => this.show_message(
                                tr("app.ai.chat.action.resume_failed")
                                    .replace("%{message}", &error.message)
                                    .as_str(),
                                AppMessageKind::Warning, cx,
                            ),
                        }
                    }
                }
                this.ai_chat_process(window, cx);
                cx.notify();
            });
        }));
    }
    fn send_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_chat.attachment_loading
            || self.ai_chat.recovery_pending
            || self.ai_chat.recovery_failed
            || self.ai_chat.session_running(self.controller.state().ai_conversation.session_id.as_deref())
            || self.ai_chat.loading
            || self.ai_chat._io.is_some()
        {
            return;
        }
        let prompt = self.ai_chat.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let page = Some(self.capture_ai_page_context(cx, true));
        let context = page.as_ref().map(|page| page.snapshot.clone())
            .unwrap_or_else(|| fluxdb_app::capture(&fluxdb_app::CapturedTab::NoTab {
                connection_id: None, database: None, schema: None, last_object: None,
            }, fluxdb_app::ContextLimits::default()));
        // debug 覆盖优先：GUI E2E 需要固定目标；生产永远是真实界面捕获。
        let pin = self
            .ai_debug_target_override()
            .unwrap_or_else(|| fluxdb_app::pin_target(&context));
        let mut controller = self.controller.clone();
        let epoch = self.ai_chat.epoch;
        self.ai_chat.loading = true;
        self.ai_chat.error = None;
        cx.notify();
        self.ai_chat._io = Some(cx.spawn_in(window, async move |view, cx| {
            let (result, chat) = cx
                .background_spawn(async move {
                    let result = controller.prepare_ai_conversation_page(
                        &prompt,
                        &pin,
                        page,
                    );
                    (result, controller.state().ai_conversation.clone())
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.ai_chat.loading = false;
                this.ai_chat._io = None;
                match result {
                    Ok(job) => {
                        if epoch != this.ai_chat.epoch {
                            fluxdb_app::clear_current_page_grants(&job.turn_id);
                            this.ai_chat.error = Some(
                                tr("app.ai.chat.action.session_switched").to_string(),
                            );
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
        // 新一轮开始：丢弃上一轮遗留的流缓冲与揭示任务，避免串行内容。
        self.ai_chat.stream.reset();
        self.ai_chat.stream_reveal = None;
        if self.ai_chat.runtime.is_none() {
            match fluxdb_app::AiRuntimeHandle::new(4) {
                Ok(runtime) => self.ai_chat.runtime = Some(runtime),
                Err(error) => {
                    self.ai_chat_record_start_failure(storage, job, error.to_string(), cx);
                    return;
                }
            }
        }
        let debug_scripted = cfg!(debug_assertions) && self.ai_chat.scripted_fixture;
        let stream_fixture = self.ai_chat.scripted_stream_fixture;
        // 仅传递非敏感连接配置；目标授权、目录装配和凭据读取由应用层完成。
        let connection_configs: Vec<fluxdb_core::ConnectionConfig> = self
            .controller
            .state()
            .connections
            .iter()
            .map(|c| c.config.clone())
            .collect();
        // ER 关系能力与 ER 页面共用同一份缓存与目录服务：AI 建议与用户确认看到的是同一处。
        let relationships = self.controller.er_relationship_source();
        let runtime = self.ai_chat.runtime.as_ref().expect("runtime 已创建");
        let submitted = if debug_scripted {
                runtime.submit_conversation_source(storage.clone(), job.clone(), cancel.clone(), move |cancel, emit| {
                    if !matches!(stream_fixture, AiChatStreamFixture::Default) {
                        let text = "## 流式验证\n\n**内容必须连续显示。**\n\n```sql\nSELECT 1;\n```\n";
                        emit(&fluxdb_core::ModelEvent::TextDelta(text.into()));
                        if matches!(stream_fixture, AiChatStreamFixture::Burst | AiChatStreamFixture::Error) {
                            std::thread::sleep(std::time::Duration::from_millis(120));
                        }
                        if matches!(stream_fixture, AiChatStreamFixture::Error) {
                            return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "离线流式错误夹具"));
                        }
                        if matches!(stream_fixture, AiChatStreamFixture::Burst) && !cancel.is_cancelled() {
                            for ch in "突发分片不能丢失，也不能在结束时一次展开。\n".repeat(100).chars() {
                                emit(&fluxdb_core::ModelEvent::TextDelta(ch.to_string()));
                            }
                        }
                        let event = fluxdb_core::ModelEvent::Finished(if cancel.is_cancelled() {
                            fluxdb_core::FinishReason::Cancelled
                        } else { fluxdb_core::FinishReason::Stop });
                        emit(&event);
                        return Ok(vec![event]);
                    }
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
                })
        } else {
            runtime.submit_conversation(storage.clone(), job.clone(), connection_configs, relationships, resume_approval, cancel)
        };
        let rx = match submitted {
            Ok(receiver) => receiver,
            Err(error) => {
                self.ai_chat_record_start_failure(storage, job, error.to_string(), cx);
                return;
            }
        };
        self.ai_chat._pump = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let mut done = false;
                let mut updates = Vec::new();
                let mut disconnected = false;
                for _ in 0..8 {
                    match rx.try_recv() {
                        Ok(update) => updates.push(update),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                if !updates.is_empty() {
                    let result = view.update(cx, |this, cx| {
                        for update in updates {
                            done |= update.terminal;
                            this.ai_chat_consume_update(update, cx);
                        }
                    });
                    if result.is_err() { break; }
                }
                if done {
                    break;
                }
                if disconnected {
                    let _ = view.update(cx, |this, cx| {
                        this.ai_chat.error = Some(
                            tr("app.ai.chat.action.channel_closed_error").to_string(),
                        );
                        this.ai_chat_finish_stream();
                        this.show_message(
                            tr("app.ai.chat.action.channel_closed_warning").as_ref(),
                            AppMessageKind::Warning,
                            cx,
                        );
                        cx.notify();
                    });
                    break;
                }
                if view.upgrade().is_none() {
                    break;
                }
            }
        }));
    }
    fn ai_chat_record_start_failure(
        &mut self,
        storage: FileStorage,
        job: fluxdb_app::AiConversationJob,
        message: String,
        cx: &mut Context<Self>,
    ) {
        fluxdb_app::clear_current_page_grants(&job.turn_id);
        self.ai_chat.error = Some(message.clone());
        self.ai_chat._pump = Some(cx.spawn(async move |view, cx| {
            let update = cx.background_spawn(async move {
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                fluxdb_app::run_ai_conversation_failure(
                    &storage,
                    &job,
                    &sender,
                    fluxdb_app::FailureStage::Prepare,
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message),
                );
                receiver.try_recv().ok()
            }).await;
            if let Some(update) = update {
                let _ = view.update(cx, |this, cx| {
                    this.ai_chat_consume_update(update, cx);
                    cx.notify();
                });
            }
        }));
        cx.notify();
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
        if self.ai_chat.deciding_approval
            || self.ai_chat.loading
            || self
                .ai_chat
                .session_running(self.controller.state().ai_conversation.session_id.as_deref())
        {
            return;
        }
        self.ai_chat.deciding_approval = true;
        self.ai_chat_request(
            fluxdb_app::AiConversationCommand::DecideApproval { approval_id, approved, expected_revision },
            window, cx,
        );
    }

    /// 关闭/退出审批通过同一 CAS 决定链路拒绝；不把运行中或已决定的记录当作待审批。
    fn reject_pending_ai_chat_approval(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let pending = self.controller.state().ai_conversation.messages.iter()
            .flat_map(|message| &message.parts)
            .filter(|part| part.kind == "notice" && part.status == "pending")
            .find_map(|part| {
                let payload = serde_json::from_str::<serde_json::Value>(&part.payload_json).ok()?;
                if payload.get("decision").and_then(serde_json::Value::as_str).is_some() {
                    return None;
                }
                Some((
                    payload.get("approval_id")?.as_str()?.to_owned(),
                    payload.get("approval_revision").and_then(serde_json::Value::as_i64).unwrap_or(1),
                ))
            });
        let Some((id, revision)) = pending else { return false };
        self.decide_ai_chat_approval(id, false, revision, window, cx);
        true
    }
    /// 调试用目标覆盖（仅 debug 构建）：GUI E2E 需要确定性 target，避免依赖"当前标签页"状态。
    /// 生产构建不读取该环境变量，永远使用真实界面捕获。
    fn ai_debug_target_override(&self) -> Option<fluxdb_app::TurnTargetPin> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let raw = std::env::var("FLUXDB_DEBUG_AI_TARGET").ok()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let target = fluxdb_app::TargetRef {
            connection_id: value.get("connection_id").and_then(|v| v.as_i64()),
            database: value.get("database").and_then(|v| v.as_str()).map(str::to_owned),
            schema: value.get("schema").and_then(|v| v.as_str()).map(str::to_owned),
            object: value.get("object").and_then(|v| v.as_str()).map(str::to_owned),
            redis_db: value.get("redis_db").and_then(|v| v.as_u64()).map(|v| v as u32),
        };
        Some(fluxdb_app::TurnTargetPin {
            target_json: target.to_json(),
            target,
            captured_tab_id: None,
            captured_tab_revision: 0,
        })
    }

}
