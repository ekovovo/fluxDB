impl NavicatMain {
    /// 消费一个后台回包：视图回包走 Apply（画面来源仍是数据库），
    /// 增量回包只更新内存流缓冲。切会话后的迟到回包按 turn/session 身份拒绝。
    fn ai_chat_consume_update(
        &mut self,
        update: fluxdb_app::AiConversationUpdate,
        cx: &mut Context<Self>,
    ) {
        if !self
            .ai_chat
            .running
            .as_ref()
            .is_some_and(|j| j.turn_id == update.job.turn_id)
        {
            return;
        }
        if self
            .controller
            .state()
            .ai_conversation
            .session_id
            .as_deref()
            != Some(update.job.session_id.as_str())
        {
            if update.terminal {
                self.ai_chat_finish_stream();
            }
            return;
        }
        let terminal = update.terminal;
        let structural_change = update.view.is_some() || update.tool_progress.is_some() || terminal;
        let had_new_reply = self.ai_chat.new_reply;
        let changed = update.delta.is_some() || update.view.is_some() || update.tool_progress.is_some();
        if let Some(delta) = update.delta {
            self.ai_chat_apply_delta(delta, cx);
        }
        if let Some(progress) = update.tool_progress {
            // 工具执行中的实时进度：按 call_id 记录，供工具卡就地显示；终态回执到达后清理。
            if progress.success || progress.returned_rows > 0 || progress.affected_rows > 0 {
                self.ai_chat.tool_progress.insert(progress.call_id.clone(), progress);
            }
        }
        if let Some(view) = update.view {
            // 工具回执（终态）到达：对应调用的实时进度不再需要，即时清理避免陈旧数值残留。
            for message in &view.messages {
                for part in &message.parts {
                    // 工具卡payload 用 `call_id`；结果引用若有 `tool_call_id` 也一并认。
                    if let Ok(payload) =
                        serde_json::from_str::<serde_json::Value>(&part.payload_json)
                    {
                        let call_id = payload
                            .get("call_id")
                            .or_else(|| payload.get("tool_call_id"))
                            .and_then(serde_json::Value::as_str);
                        if let Some(call_id) = call_id
                            && (part.kind == "tool_result_ref"
                                || (part.kind == "tool_call_ref" && part.status != "streaming"))
                        {
                            self.ai_chat.tool_progress.remove(call_id);
                        }
                    }
                }
            }
            // 最终快照可能是首个成功送达的回包，也必须从空正文平滑显示。
            if !self.ai_chat.stream.is_active() {
                if let Some(part) = view.messages.iter()
                    .find(|message| message.id == update.job.assistant_message_id)
                    .and_then(|message| message.parts.iter().find(|part| part.kind == "text")) {
                    let text = ai_chat_part_text(part);
                    if !text.is_empty() {
                        let baseline = self.ai_chat_stream_baseline(&update.job.assistant_message_id);
                        self.ai_chat_begin_stream(&update.job.assistant_message_id, &baseline, cx);
                    }
                }
            }
            // 草稿由独立输入 Entity 持有，stream checkpoint 不覆盖当前未发送内容。
            self.controller.dispatch(AppCommand::AiConversation(
                fluxdb_app::AiConversationCommand::Apply { view },
            ));
            if let Some(message_id) = self.ai_chat.stream.message_id().map(str::to_owned) {
                self.ai_chat_sync_stream_from_view(&message_id);
            }
            self.ai_chat_ensure_reveal_task(cx);
        }
        if self.ai_chat.follow_reply && structural_change {
            self.ai_chat.scroll.scroll_to_bottom();
        } else if changed {
            self.ai_chat.new_reply |= !self.ai_chat.follow_reply;
        }
        if terminal {
            self.ai_chat.stream.finish();
            // 结束/失败立即冲刷最新尾片，不让 Markdown 节流留下额外的“打字动画”。
            if let Some(chunk) = self.ai_chat.stream.reveal_next()
                && let Some(view) = self.ai_chat.stream_view.clone()
            {
                view.update(cx, |state, cx| state.push_str(&chunk, cx));
            }
            self.ai_chat_finish_stream();
        }
        // 正文增量只更新缓冲，重绘交给 Markdown 节拍；不能让 16ms 的回桥泵
        // 又把整个聊天列表重绘一遍。工具/终态和“有新回复”标记仍即时刷新。
        if structural_change || had_new_reply != self.ai_chat.new_reply {
            cx.notify();
        }
    }
    /// 把一段未落盘增量并入流缓冲，必要时绑定正文实体并启动揭示任务。
    fn ai_chat_apply_delta(
        &mut self,
        delta: fluxdb_app::AiConversationStreamDelta,
        cx: &mut Context<Self>,
    ) {
        // 只有助手正文流式渲染；公开摘要仍走折叠卡，不进入正文实体。
        if !matches!(delta.kind, fluxdb_app::AiConversationDeltaKind::Text) {
            return;
        }
        if !self.ai_chat.stream.target_matches(&delta.message_id) {
            let baseline = self.ai_chat_stream_baseline(&delta.message_id);
            self.ai_chat_begin_stream(&delta.message_id, &baseline, cx);
        }
        self.ai_chat.stream.push(delta.start_offset, &delta.delta);
        self.ai_chat_ensure_reveal_task(cx);
    }

    fn ai_chat_begin_stream(&mut self, message_id: &str, baseline: &str, cx: &mut Context<Self>) {
        self.ai_chat.stream.begin(message_id, baseline);
        let view = self.ai_chat.stream_view.get_or_insert_with(|| {
            cx.new(|cx| gpui_component::text::TextViewState::markdown(baseline, cx))
        }).clone();
        view.update(cx, |state, cx| state.set_text(baseline, cx));
    }

    fn ai_chat_finish_stream(&mut self) {
        self.ai_chat.stream.reset();
        self.ai_chat.running = None;
        self.ai_chat.cancel = None;
        self.ai_chat.tool_progress.clear();
    }
    /// 末端助手正文已 checkpoint 的正文，作为流式渲染基线（避免重打历史内容）。
    fn ai_chat_stream_baseline(&self, message_id: &str) -> String {
        let part_id = format!("{message_id}-text");
        self.controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .and_then(|message| message.parts.iter().find(|part| part.id == part_id))
            .and_then(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
            .and_then(|payload| payload.get("text").and_then(|v| v.as_str()).map(str::to_owned))
            .unwrap_or_default()
    }
    /// checkpoint 视图回来后，用权威正文校正流缓冲（只补不回退）。
    fn ai_chat_sync_stream_from_view(&mut self, message_id: &str) {
        let payload = self.ai_chat_stream_baseline(message_id);
        self.ai_chat.stream.reconcile(&payload);
    }
    /// 启动/保持流式渲染任务；按 50ms（长正文 250ms）合并 Markdown 更新。
    ///
    /// 任务只在*有待揭示文本*时存活；一旦排空且没有新 delta 就自然结束，
    /// 不留下空转定时器（下一个 delta 会重新拉起）。已结束的句柄由
    /// `is_ready` 识别并重建。
    fn ai_chat_ensure_reveal_task(&mut self, cx: &mut Context<Self>) {
        if self
            .ai_chat
            .stream_reveal
            .as_ref()
            .is_some_and(|task| !task.is_ready())
            || !self.ai_chat.stream.has_pending()
        {
            return;
        }
        self.ai_chat.stream_reveal = Some(cx.spawn(async move |view, cx| loop {
            let interval = view.read_with(cx, |this, _| this.ai_chat.stream.render_interval())
                .unwrap_or(std::time::Duration::from_millis(STREAM_RENDER_INTERVAL_MS));
            cx.background_executor().timer(interval).await;
            let keep = view
                .update(cx, |this, cx| {
                    if !this.ai_chat.stream.is_active() {
                        return false;
                    }
                    if this.ai_chat.stream.is_finished()
                        && this.ai_chat.cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled()) {
                        this.ai_chat_finish_stream();
                        cx.notify();
                        return false;
                    }
                    if let Some(chunk) = this.ai_chat.stream.reveal_next() {
                        if let Some(view) = this.ai_chat.stream_view.clone() {
                            view.update(cx, |state, cx| state.push_str(&chunk, cx));
                        }
                        if this.ai_chat.follow_reply {
                            this.ai_chat.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                    }
                    let pending = this.ai_chat.stream.has_pending();
                    if !pending && this.ai_chat.stream.is_finished() {
                        this.ai_chat_finish_stream();
                        cx.notify();
                    }
                    pending
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        }));
    }
}
