// T028 会话加载/草稿/模型事件保存。同步存储由 UI 在后台执行，普通打开不发模型请求。
fn ai_chat_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
fn ai_chat_project(
    data: fluxdb_storage::ai_platform::conversation::ConversationData,
) -> AiConversationView {
    let draft = data
        .ui_state
        .as_ref()
        .and_then(|ui| ui.draft_json.as_deref())
        .or(data.preferences.pending_draft_json.as_deref())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v| v.get("text").and_then(|s| s.as_str()).map(str::to_owned))
        .unwrap_or_default();
    let expanded_parts = data
        .ui_state
        .as_ref()
        .and_then(|ui| serde_json::from_str(&ui.expanded_part_ids_json).ok())
        .unwrap_or_default();
    AiConversationView {
        session_id: data.session.as_ref().map(|s| s.id.clone()),
        title: data
            .session
            .as_ref()
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "新会话".into()),
        sessions: data
            .sessions
            .into_iter()
            .filter(|s| s.status == fluxdb_storage::ai_platform::sessions::SessionStatus::Active)
            .map(|s| AiConversationSummary {
                id: s.id,
                title: s.title,
                agent_id: s.agent_id,
            })
            .collect(),
        messages: data
            .messages
            .into_iter()
            .map(|m| AiConversationMessage {
                id: m.id.clone(),
                role: m.role,
                parts: data
                    .parts
                    .iter()
                    .filter(|p| p.message_id == m.id)
                    .map(|p| AiConversationPart {
                        id: p.id.clone(),
                        kind: p.kind.as_str().into(),
                        status: p.status.as_str().into(),
                        payload_json: p.payload_json.clone(),
                    })
                    .collect(),
            })
            .collect(),
        target_json: data.turns.last().map(|t| t.target_json.clone()),
        preferences: data.preferences,
        draft,
        expanded_parts,
        active_turn_id: data
            .turns
            .iter()
            .find(|t| !t.status.is_terminal())
            .map(|t| t.id.clone()),
        error: None,
    }
}
impl AppController {
    fn dispatch_ai_conversation(&mut self, command: AiConversationCommand) -> AppEvent {
        if let AiConversationCommand::Apply { view } = command {
            self.state.ai_conversation = view;
            return AppEvent::AiConversationLoaded(self.state.ai_conversation.clone());
        }
        if matches!(command, AiConversationCommand::RecoverOnStartup) {
            return self.recover_ai_on_startup();
        }
        if let AiConversationCommand::DecideApproval {
            approval_id,
            approved,
            expected_revision,
        } = &command
        {
            return self.decide_ai_approval_atomic(approval_id, *approved, *expected_revision);
        }
        let result = self.ai_storage().and_then(|storage| {
            with_ai_storage(&storage, |conn| {
                match command {
                    AiConversationCommand::Load { ref session_id } => {
                        let mut prefs =
                            fluxdb_storage::ai_platform::settings::load_panel_preferences(conn)?;
                        if let Some(id) = session_id {
                            let _ = fluxdb_storage::ai_platform::sessions::load_session(conn, id)?;
                            let rev = prefs.revision;
                            prefs.last_session_id = Some(id.clone());
                            fluxdb_storage::ai_platform::settings::save_panel_preferences(
                                conn,
                                ai_chat_now(),
                                &prefs,
                                rev,
                            )?;
                        }
                    }
                    AiConversationCommand::New => {
                        let mut prefs =
                            fluxdb_storage::ai_platform::settings::load_panel_preferences(conn)?;
                        let rev = prefs.revision;
                        prefs.last_session_id = None;
                        prefs.pending_draft_json = None;
                        fluxdb_storage::ai_platform::settings::save_panel_preferences(
                            conn,
                            ai_chat_now(),
                            &prefs,
                            rev,
                        )?;
                    }
                    AiConversationCommand::SaveUi {
                        ref draft,
                        ref expanded_parts,
                        mode,
                        drawer_width,
                        embedded_width,
                    } => {
                        if draft.len() > 64 * 1024 {
                            return Err(fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "草稿超过 64 KiB",
                            ));
                        }
                        let tx = conn.unchecked_transaction().map_err(|e| {
                            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
                        })?;
                        let mut prefs =
                            fluxdb_storage::ai_platform::settings::load_panel_preferences(&tx)?;
                        let rev = prefs.revision;
                        prefs.preferred_mode = mode;
                        prefs.drawer_width_dp = drawer_width;
                        prefs.embedded_width_dp = embedded_width;
                        let draft_json = (!draft.is_empty())
                            .then(|| serde_json::json!({"text":draft}).to_string());
                        if let Some(ref id) = prefs.last_session_id {
                            let ui = fluxdb_storage::ai_platform::sessions::load_ui_state(&tx, id)?;
                            fluxdb_storage::ai_platform::sessions::save_ui_state(
                                &tx,
                                ai_chat_now(),
                                &fluxdb_storage::ai_platform::sessions::SessionUiStateEdit {
                                    session_id: id.clone(),
                                    selected_turn_id: ui.selected_turn_id,
                                    expanded_part_ids_json: serde_json::json!(expanded_parts)
                                        .to_string(),
                                    scroll_anchor_message_id: ui.scroll_anchor_message_id,
                                    ui_options_json: ui.ui_options_json,
                                    draft_json,
                                },
                                ui.revision,
                            )?;
                        } else {
                            prefs.pending_draft_json = draft_json;
                        }
                        fluxdb_storage::ai_platform::settings::save_panel_preferences(
                            &tx,
                            ai_chat_now(),
                            &prefs,
                            rev,
                        )?;
                        tx.commit().map_err(|e| {
                            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
                        })?;
                    }
                    AiConversationCommand::DecideApproval { .. } => unreachable!(),
                    AiConversationCommand::Apply { .. }
                    | AiConversationCommand::RecoverOnStartup => unreachable!(),
                }
                fluxdb_storage::ai_platform::conversation::load(conn, None).map(ai_chat_project)
            })
        });
        match result {
            Ok(view) => {
                self.state.ai_conversation = view.clone();
                AppEvent::AiConversationLoaded(view)
            }
            Err(e) => AppEvent::AiSettingsError(e.message),
        }
    }

    /// 恢复被审批暂停的 Turn：重建同一个会话/轮次/助手消息与原模型（不新建消息、不改目标）。
    pub fn prepare_ai_resume_job(
        &mut self,
        approval_id: &str,
    ) -> fluxdb_core::Result<AiConversationJob> {
        let storage = self.ai_storage()?;
        with_ai_storage(&storage, |conn| {
            let approval = fluxdb_storage::ai_platform::audits::load_approval(conn, approval_id)?;
            let call_id = approval.tool_call_id.clone().ok_or_else(|| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "该审批没有绑定工具调用")
            })?;
            let call = fluxdb_storage::ai_platform::sessions::load_tool_call(conn, &call_id)?;
            let session =
                fluxdb_storage::ai_platform::sessions::load_session(conn, &call.session_id)?;
            let agent = fluxdb_storage::ai_platform::agents::load_agent(conn, &session.agent_id)?;
            if !agent.enabled {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "当前 Agent 已停用，无法恢复任务",
                ));
            }
            let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &call.turn_id)?;
            let snapshot: serde_json::Value = serde_json::from_str(&turn.runtime_snapshot_json)
                .map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "任务配置快照损坏")
                })?;
            let model_id = snapshot
                .get("model_profile_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "任务快照缺少原模型，拒绝改用当前模型",
                    )
                })?
                .to_string();
            let model = fluxdb_storage::ai_platform::models::load_model(conn, &model_id)?;
            if !model.enabled {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "原任务模型已停用，无法恢复任务",
                ));
            }
            let message_id =
                fluxdb_storage::ai_platform::sessions::find_assistant_message_for_turn(
                    conn,
                    &call.turn_id,
                )?
                .ok_or_else(|| {
                    fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "该任务还没有助手消息",
                    )
                })?;
            Ok(AiConversationJob {
                session_id: call.session_id.clone(),
                turn_id: call.turn_id.clone(),
                assistant_message_id: message_id,
                model_profile_id: model_id,
            })
        })
    }

    /// 正式发送前读取现有模型/Agent 配置。无可用配置明确失败，不能静默启动模拟回答。
    pub fn prepare_ai_conversation(
        &mut self,
        prompt: &str,
        target: &TurnTargetPin,
    ) -> fluxdb_core::Result<AiConversationJob> {
        self.prepare_ai_conversation_context(prompt, target, "")
    }
    pub fn prepare_ai_conversation_context(
        &mut self,
        prompt: &str,
        target: &TurnTargetPin,
        context: &str,
    ) -> fluxdb_core::Result<AiConversationJob> {
        if context.len() > 64 * 1024 {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "当前页面摘录超限",
            ));
        }
        let storage = self.ai_storage()?;
        let (job, view) = with_ai_storage(&storage, |conn| {
            let runtime = fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?;
            let current = self
                .state
                .ai_conversation
                .session_id
                .as_deref()
                .map(|id| fluxdb_storage::ai_platform::sessions::load_session(conn, id))
                .transpose()?;
            let agents = fluxdb_storage::ai_platform::agents::list_agents(conn)?;
            let agent_id = current
                .as_ref()
                .map(|s| s.agent_id.as_str())
                .or(runtime.default_agent_id.as_deref());
            let agent = match agent_id {
                Some(id) => agents.iter().find(|a| a.id == id),
                None => agents.iter().filter(|a| a.enabled).next(),
            }
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "请先在 Agent 配置中新建并启用一个 Agent",
                )
            })?;
            if !agent.enabled {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "当前 Agent 已停用",
                ));
            }
            let model_id = agent
                .default_model_profile_id
                .as_deref()
                .or(runtime.default_model_profile_id.as_deref())
                .ok_or_else(|| {
                    fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "请给 Agent 绑定模型，或设置默认主模型",
                    )
                })?;
            let model = fluxdb_storage::ai_platform::models::load_model(conn, model_id)?;
            if !model.enabled
                || model.kind != fluxdb_storage::ai_platform::models::ModelKind::Generation
            {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "当前生成模型不可用",
                ));
            }
            let session_id = current
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let turn_id = uuid::Uuid::new_v4().to_string();
            fluxdb_storage::ai_platform::conversation::send(conn,ai_chat_now(),current.is_none(),&session_id,&agent.id,&turn_id,&uuid::Uuid::new_v4().to_string(),prompt,&target.target_json,&serde_json::json!({"model_profile_id":model_id,"agent_id":agent.id,"agent_revision":agent.revision,"system_prompt":agent.system_prompt,"ui_context":context}).to_string())?;
            let job = AiConversationJob {
                session_id: session_id.clone(),
                turn_id,
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
                model_profile_id: model_id.into(),
            };
            let view = ai_chat_project(fluxdb_storage::ai_platform::conversation::load(
                conn,
                Some(&session_id),
            )?);
            Ok((job, view))
        })?;
        self.state.ai_conversation = view;
        Ok(job)
    }
}

/// 事件桥持久化：每 300ms checkpoint；结束强制保存，不按 token 建行。
pub fn persist_ai_conversation_event(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    text: &str,
    summary: &str,
    status: Option<fluxdb_storage::ai_platform::sessions::TurnStatus>,
    error: Option<&str>,
) -> fluxdb_core::Result<AiConversationView> {
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::conversation::checkpoint(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            text,
            summary,
            status,
            error,
        )?;
        let mut view = ai_chat_project(fluxdb_storage::ai_platform::conversation::load(
            conn,
            Some(&job.session_id),
        )?);
        view.error = error.map(str::to_owned);
        Ok(view)
    })
}

/// 仅主线程消费的回桥包；身份用于拒绝切会话后的迟到回调。
#[derive(Clone, Debug)]
pub struct AiConversationUpdate {
    pub job: AiConversationJob,
    pub view: AiConversationView,
    pub terminal: bool,
}

/// 本次先提供有时间/输出限额保护的单轮解释/问答；数据库工具执行需 T020/T023 接线后启用。
/// 不是另一个 Agent 循环，不执行未经审计的工具请求。
pub fn run_ai_conversation(
    storage: fluxdb_storage::FileStorage,
    job: AiConversationJob,
    cancel: fluxdb_core::CancelFlag,
    sender: std::sync::mpsc::SyncSender<AiConversationUpdate>,
) {
    let setup = with_ai_storage(&storage, |conn| {
        let model = fluxdb_storage::ai_platform::models::load_model(conn, &job.model_profile_id)?;
        let data = fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))?;
        let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)?;
        let sealed = fluxdb_storage::ai_platform::models::load_encrypted_secret(conn, &model.id)?;
        Ok((model, data, turn, sealed))
    });
    let run=|emit:&mut dyn FnMut(&fluxdb_core::ModelEvent)| -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>> {
        if cancel.is_cancelled() {let event=fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Cancelled);emit(&event);return Ok(vec![event]);}
        let (model,data,turn,sealed)=setup?;
        let protocol=match model.api_protocol.as_str(){"openai-compatible"=>ChatProtocol::OpenAiCompatible,"anthropic-messages"=>ChatProtocol::AnthropicMessages,_=>return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"该 Provider 暂不支持对话"))};
        let snapshot:serde_json::Value=serde_json::from_str(&turn.runtime_snapshot_json).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"运行快照损坏"))?;
        let mut messages=vec![fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::System,
            format!("{}\n当前执行目标：{}。只解释与建议；本次未开放执行工具，不得宣称已经执行 SQL 或生成真实报表。",snapshot.get("system_prompt").and_then(|s|s.as_str()).unwrap_or(""),turn.target_json))];
        if let Some(context)=snapshot.get("ui_context").and_then(|s|s.as_str()).filter(|s|!s.is_empty()) {messages.push(fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::User,format!("当前页面的有界摘录：\n{context}")));}
        let view=ai_chat_project(data);
        for message in view.messages {
            let text=message.parts.iter().filter(|p|p.kind=="text").filter_map(|p|serde_json::from_str::<serde_json::Value>(&p.payload_json).ok()).filter_map(|v|v.get("text").and_then(|s|s.as_str()).map(str::to_owned)).collect::<Vec<_>>().join("\n");
            if !text.is_empty(){messages.push(fluxdb_core::ModelMessage::new(if message.role=="user"{fluxdb_core::ModelRole::User}else{fluxdb_core::ModelRole::Assistant},text));}
        }
        if messages.iter().map(|m|m.text.len()).sum::<usize>()>64*1024 {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"上下文超过本轮 64 KiB 保护上限；请新建会话"));}
        let body=build_chat_body(protocol,&model.model,&fluxdb_core::ModelRequest{model:model.model.clone(),messages,tools:Vec::new(),max_output_tokens:Some(model.max_output_tokens.unwrap_or(1024).min(1024))},&ToolNameMapper::new(&[])?,fluxdb_core::ModelCapabilities{streaming:true,..Default::default()},Some(1024));
        let auth=if model.auth_mode==fluxdb_storage::ai_platform::models::AuthMode::None {TransportAuth::None} else {
            let sealed=sealed.ok_or_else(||fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"模型缺少 API Key"))?;
            let (cipher,_)=fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir())?;
            let key=fluxdb_storage::ai_platform::model_secret::open(&cipher,&model.id,&sealed).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"模型凭据不可用"))?;
            match protocol{ChatProtocol::OpenAiCompatible|ChatProtocol::OpenAiResponses=>TransportAuth::Bearer(key),ChatProtocol::AnthropicMessages=>TransportAuth::Header{name:"x-api-key",value:key}}
        };
        let base=model.endpoint.as_deref().unwrap_or(if protocol==ChatProtocol::OpenAiCompatible {"https://api.openai.com/v1"} else {"https://api.anthropic.com/v1"});
        let endpoint=ai_request_url(base,&model.api_protocol,model.endpoint_is_full_url)?;
        let headers:&[(&str,&str)]=if protocol==ChatProtocol::AnthropicMessages{&[("anthropic-version","2023-06-01")]}else{&[]};
        stream_chat_emit(TransportLimits{max_total_bytes:512*1024,read_timeout:std::time::Duration::from_secs(30),..Default::default()},protocol,&endpoint,&auth,&body.to_string(),headers,&cancel,emit).map_err(TransportError::to_core_error)
    };
    run_ai_conversation_source(&storage, &job, &cancel, &sender, run);
}

/// 可替换 source 只决定模型事件，保存与回桥仍经过同一个生产路径。fixture 不伪造数据库结果。
pub fn run_ai_conversation_source(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &std::sync::mpsc::SyncSender<AiConversationUpdate>,
    source: impl FnOnce(
        &mut dyn FnMut(&fluxdb_core::ModelEvent),
    ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>,
) {
    let mut text = String::new();
    let mut summary = String::new();
    let mut saved = std::time::Instant::now();
    let mut finish = None;
    let mut persist_error = None;
    let mut emit = |event: &fluxdb_core::ModelEvent| {
        match event {
            fluxdb_core::ModelEvent::TextDelta(delta) => text.push_str(delta),
            fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta) => summary.push_str(delta),
            fluxdb_core::ModelEvent::Finished(reason) => finish = Some(*reason),
            _ => {}
        }
        if saved.elapsed() >= std::time::Duration::from_millis(300) && persist_error.is_none() {
            match persist_ai_conversation_event(storage, job, &text, &summary, None, None) {
                Ok(view) => {
                    let _ = sender.try_send(AiConversationUpdate {
                        job: job.clone(),
                        view,
                        terminal: false,
                    });
                }
                Err(e) => persist_error = Some(e.message),
            };
            saved = std::time::Instant::now();
        }
    };
    let response = source(&mut emit);
    drop(emit);
    let error = persist_error
        .or_else(|| response.err().map(|e| e.message))
        .or_else(|| {
            if finish.is_none() && !cancel.is_cancelled() {
                Some("模型连接提前结束，未收到完整结束事件".into())
            } else {
                None
            }
        });
    let status = if cancel.is_cancelled() || finish == Some(fluxdb_core::FinishReason::Cancelled) {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled
    } else if error.is_some() || finish == Some(fluxdb_core::FinishReason::Error) {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
    } else {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Completed
    };
    if finish == Some(fluxdb_core::FinishReason::Length) {
        text.push_str("\n[模型输出达到本轮上限，内容可能不完整。]");
    }
    // 即使没有回复文字也写出可见状态，空响应不能伪装成功。
    if text.is_empty() {
        text = error.clone().unwrap_or_else(|| {
            if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled {
                "已停止后续生成。".into()
            } else {
                "模型未返回正文。".into()
            }
        });
    }
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        &summary,
        Some(status),
        error.as_deref(),
    ) {
        Ok(view) => {
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
        Err(e) => {
            tracing::warn!(turn_id=%job.turn_id,"AI 回复保存失败");
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("回复保存失败：{}", e.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
    }
}

/// 组装本轮公开消息：系统提示 + 界面摘录 + 历史文本片段（不含工具输出，避免把上次执行结果当新上下文）。
fn ai_conversation_turn_messages(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::Result<(String, Vec<fluxdb_core::ModelMessage>)> {
    with_ai_storage(storage, |conn| {
        let data = fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))?;
        let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)?;
        let snapshot: serde_json::Value = serde_json::from_str(&turn.runtime_snapshot_json)
            .map_err(|_| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "运行快照损坏")
            })?;
        let system_prompt = snapshot
            .get("system_prompt")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let mut messages = vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::System,
            format!(
                "{system_prompt}\n当前执行目标：{}。工具的 target 参数必须使用句柄：{}。你可以调用被授权的工具；只能依据工具真实回执陈述执行结果，不得臆造。",
                turn.target_json,
                ai_tool_target_handle(&turn.target_json).unwrap_or_else(|| "（无可用句柄）".to_string())
            ),
        )];
        if let Some(context) = snapshot
            .get("ui_context")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
        {
            messages.push(fluxdb_core::ModelMessage::new(
                fluxdb_core::ModelRole::User,
                format!("当前页面的有界摘录：\n{context}"),
            ));
        }
        for message in ai_chat_project(data).messages {
            let text = message
                .parts
                .iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| {
                    serde_json::from_str::<serde_json::Value>(&part.payload_json).ok()
                })
                .filter_map(|value| {
                    value
                        .get("text")
                        .and_then(|text| text.as_str())
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                messages.push(fluxdb_core::ModelMessage::new(
                    if message.role == "user" {
                        fluxdb_core::ModelRole::User
                    } else {
                        fluxdb_core::ModelRole::Assistant
                    },
                    text,
                ));
            }
        }
        if messages.iter().map(|m| m.text.len()).sum::<usize>() > 64 * 1024 {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "上下文超过本轮 64 KiB 保护上限；请新建会话",
            ));
        }
        Ok((turn.target_json.clone(), messages))
    })
}

/// 保存一条事件片段（工具卡/结果/通知）；失败只记错误，不让持久化问题吞掉真实回执。
fn ai_conversation_save_part(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    part_id: &str,
    kind: fluxdb_storage::ai_platform::sessions::PartKind,
    status: fluxdb_storage::ai_platform::sessions::PartStatus,
    payload_json: &str,
    tool_call_id: Option<&str>,
    result_id: Option<&str>,
) -> fluxdb_core::Result<()> {
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::conversation::upsert_event_part(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            part_id,
            kind,
            status,
            payload_json,
            tool_call_id,
            result_id,
        )
    })
}

/// 工具回合：用 `TurnRunner` 跑完整链路（schema → 审计 → 授权 → 执行 → 有界投影）。
///
/// 只有**真实回执**才允许写成工具结果片段；模型文案不会被执行结果替代或伪造。
pub fn run_ai_conversation_turn(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &std::sync::mpsc::SyncSender<AiConversationUpdate>,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
) {
    ai_conversation_run_turn(
        storage,
        job,
        cancel,
        sender,
        provider,
        tools,
        audit,
        permission,
        Vec::new(),
        String::new(),
    );
}

/// 跑一轮工具回合的公共实现；`extra_messages`/`initial_text` 供"审批后恢复"续写同一条助手消息。
#[allow(clippy::too_many_arguments)]
fn ai_conversation_run_turn(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &std::sync::mpsc::SyncSender<AiConversationUpdate>,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
    extra_messages: Vec<fluxdb_core::ModelMessage>,
    initial_text: String,
) {
    let (target_json, mut messages) = match ai_conversation_turn_messages(storage, job) {
        Ok(value) => value,
        Err(error) => {
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(error.message);
            view.session_id = Some(job.session_id.clone());
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
            return;
        }
    };

    messages.extend(extra_messages);
    let mut text = initial_text;
    let mut summary = String::new();
    let mut saved = std::time::Instant::now();
    let mut persist_error: Option<String> = None;
    let mut paused_reason: Option<String> = None;
    let mut notice_count: usize = 0;
    let mut tool_index: usize = 0;
    let mut approval_id: Option<String> = None;
    let mut part_error: Option<String> = None;
    let mut tool_error: Option<String> = None;

    let runner = TurnRunner {
        provider,
        tools,
        limits: LoopLimits::default(),
        permission,
        audit,
    };
    let target_json_for_parts = target_json.clone();
    let state = TurnState {
        model: job.model_profile_id.clone(),
        messages,
        max_output_tokens: Some(1024),
        target_json,
    };
    let mut steering: Vec<String> = Vec::new();
    let outcome = runner.run(state, &mut steering, cancel, &mut |event| {
        match event {
            LoopEvent::TextDelta(delta) => text.push_str(&delta),
            LoopEvent::ReasoningSummaryDelta(delta) => summary.push_str(&delta),
            // 仅表示"模型打算调用"：还没校验/审计/授权，不写任何持久化事实。
            LoopEvent::ToolCallRequested(_call) => {}
            // 工具开始执行：此时已通过 schema 校验、审计与授权，才写下事实行与工具卡。
            LoopEvent::ToolStarted {
                call,
                plan_hash,
                tool_version,
            } => {
                let saved_part = ai_conversation_record_tool_start(
                    storage,
                    job,
                    tool_index,
                    &call,
                    &plan_hash,
                    tool_version,
                    &target_json_for_parts,
                );
                if let Err(error) = &saved_part {
                    part_error.get_or_insert_with(|| error.message.clone());
                }
                tool_index += 1;
            }
            // 工具真实回执：写 ai_tool_calls/ai_results 事实行 + 结果卡。
            LoopEvent::ToolFinished { receipt } => {
                if !receipt.is_success() {
                    tool_error = Some(format!(
                        "工具 {} 未成功：{}",
                        receipt.tool_id,
                        receipt
                            .error_code
                            .clone()
                            .unwrap_or_else(|| "unknown".into())
                    ));
                }
                let saved = ai_conversation_record_tool_result(storage, job, &receipt);
                if let Err(error) = &saved {
                    part_error.get_or_insert_with(|| error.message.clone());
                }
            }
            LoopEvent::Notice(notice) => {
                let payload = serde_json::json!({"text":notice}).to_string();
                let saved = ai_conversation_save_part(
                    storage,
                    job,
                    &format!("{}-notice-{notice_count}", job.assistant_message_id),
                    fluxdb_storage::ai_platform::sessions::PartKind::Notice,
                    fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                    &payload,
                    None,
                    None,
                );
                if let Err(error) = &saved {
                    part_error.get_or_insert_with(|| error.message.clone());
                }
                notice_count += 1;
            }
            // 需要审批：写工具调用事实行（waiting_approval）+ 审批行（pending）+ 可见审批片段。
            LoopEvent::ApprovalRequired {
                call,
                tool_version,
                plan_hash,
                scope,
            } => {
                let saved = ai_conversation_record_approval(
                    storage,
                    job,
                    tool_index,
                    &call,
                    tool_version,
                    &plan_hash,
                    &scope.to_json(),
                    &target_json_for_parts,
                );
                match saved {
                    Ok(id) => approval_id = Some(id),
                    Err(error) => {
                        part_error.get_or_insert_with(|| error.message.clone());
                    }
                }
                tool_index += 1;
            }
            LoopEvent::IterationStarted { .. } => {}
        }
        if saved.elapsed() >= std::time::Duration::from_millis(300) && persist_error.is_none() {
            match persist_ai_conversation_event(storage, job, &text, &summary, None, None) {
                Ok(view) => {
                    let _ = sender.try_send(AiConversationUpdate {
                        job: job.clone(),
                        view,
                        terminal: false,
                    });
                }
                Err(error) => persist_error = Some(error.message),
            }
            saved = std::time::Instant::now();
        }
    });

    let status = match outcome {
        TurnOutcome::Completed { .. } => {
            fluxdb_storage::ai_platform::sessions::TurnStatus::Completed
        }
        TurnOutcome::Cancelled => fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled,
        TurnOutcome::Failed { message } => {
            persist_error.get_or_insert(message);
            fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
        }
        TurnOutcome::Paused { reason } => {
            paused_reason = Some(reason);
            // 只有真的产生了待审批项才算 waiting_approval；达到迭代上限等属于 waiting_user。
            if approval_id.is_some() {
                fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval
            } else {
                fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingUser
            }
        }
    };
    let error = persist_error.clone().or(part_error.clone());
    if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Completed {
        if let Some(message) = tool_error {
            // 工具失败不等于整个回合失败，但必须让用户看得见。
            if let Err(save_error) = ai_conversation_save_part(
                storage,
                job,
                &format!("{}-notice-tool-failure", job.assistant_message_id),
                fluxdb_storage::ai_platform::sessions::PartKind::Notice,
                fluxdb_storage::ai_platform::sessions::PartStatus::Error,
                &serde_json::json!({"text":message}).to_string(),
                None,
                None,
            ) {
                persist_error.get_or_insert(save_error.message);
            }
        }
    }
    if let Some(reason) = &paused_reason {
        let _ = ai_conversation_save_part(
            storage,
            job,
            &format!("{}-notice-paused", job.assistant_message_id),
            fluxdb_storage::ai_platform::sessions::PartKind::Notice,
            fluxdb_storage::ai_platform::sessions::PartStatus::Pending,
            &serde_json::json!({"text":reason}).to_string(),
            None,
            None,
        );
    }
    if text.is_empty() {
        text = error.clone().unwrap_or_else(|| match status {
            fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled => {
                "已停止后续生成。".into()
            }
            fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval => {
                paused_reason.clone().unwrap_or_else(|| "等待审批。".into())
            }
            _ => "模型未返回正文。".into(),
        });
    }
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        &summary,
        Some(status),
        error.as_deref(),
    ) {
        Ok(view) => {
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
        Err(save_error) => {
            tracing::warn!(turn_id=%job.turn_id,"AI 回复保存失败");
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("回复保存失败：{}", save_error.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
    }
}

/// 明确失败：在会话里留下可见错误并结束该 Turn（不能静默无响应，也不能伪造成功）。
pub fn run_ai_conversation_failure(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    sender: &std::sync::mpsc::SyncSender<AiConversationUpdate>,
    message: String,
) {
    let text = format!("本轮未能开始：{message}");
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        "",
        Some(fluxdb_storage::ai_platform::sessions::TurnStatus::Failed),
        Some(&message),
    ) {
        Ok(view) => {
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
        Err(error) => {
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("{message}（保存也失败：{}）", error.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.send(AiConversationUpdate {
                job: job.clone(),
                view,
                terminal: true,
            });
        }
    }
}

/// 读取当前助手消息已保存的正文（恢复续写时不能覆盖已展示内容）。
fn ai_conversation_existing_text(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> String {
    with_ai_storage(storage, |conn| {
        let data = fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))?;
        Ok(ai_chat_project(data)
            .messages
            .into_iter()
            .find(|m| m.id == job.assistant_message_id)
            .and_then(|m| m.parts.into_iter().find(|p| p.kind == "text"))
            .and_then(|p| serde_json::from_str::<serde_json::Value>(&p.payload_json).ok())
            .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))
            .unwrap_or_default())
    })
    .unwrap_or_default()
}

/// 审批通过后恢复执行：按 plan_hash 复核批准（fail closed）→ 执行该工具 → 把真实回执交给模型继续本轮。
///
/// 批准只解除"需要审批"，**不放宽** schema 校验与连接/内核权限：参数会重新校验，工具仍走同一实现。
#[allow(clippy::too_many_arguments)]
pub fn run_ai_conversation_resume_approval(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &std::sync::mpsc::SyncSender<AiConversationUpdate>,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
    approval_id: &str,
) {
    let loaded = claim_ai_approved_call(storage, job, tools, audit, &permission, approval_id);
    let (call, message_id) = match loaded {
        Ok(value) => value,
        Err(error) => {
            run_ai_conversation_failure(storage, job, sender, error.message);
            return;
        }
    };
    let job = AiConversationJob {
        session_id: job.session_id.clone(),
        turn_id: call.turn_id.clone(),
        assistant_message_id: message_id,
        model_profile_id: job.model_profile_id.clone(),
    };

    let prepared = match tools.prepare(&call.tool_id, &call.arguments_redacted_json) {
        Ok(prepared) => prepared,
        Err(error) => {
            run_ai_conversation_failure(
                storage,
                &job,
                sender,
                format!("批准后的参数未通过校验：{}", error.message),
            );
            return;
        }
    };
    let Some(tool) = tools.executable(&call.tool_id) else {
        run_ai_conversation_failure(
            storage,
            &job,
            sender,
            format!("工具 {} 没有可执行实现", call.tool_id),
        );
        return;
    };
    let ctx = fluxdb_core::ToolExecutionContext {
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        iteration: 1,
        call_id: call.provider_call_id.clone(),
        cancel: cancel.clone(),
    };
    let receipt = match block_on_ready(tool.execute(ctx, prepared.to_json_string())) {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(error)) | Err(error) => {
            run_ai_conversation_failure(
                storage,
                &job,
                sender,
                format!("工具执行失败：{}", error.message),
            );
            return;
        }
    };
    if let Err(error) = ai_conversation_record_tool_result(storage, &job, &receipt) {
        // 工具已经真实执行，但回执没能落盘：绝不能让调用停在 running 假装"还在跑"。
        // 先尽力标记 unknown（T029 按未知副作用处理），再把错误显式交给用户。
        tracing::warn!(turn_id=%job.turn_id, error=%error.message, "工具回执保存失败");
        let _ = with_ai_storage(storage, |conn| {
            fluxdb_storage::ai_platform::sessions::finish_tool_call(
                conn,
                &format!("{}-toolcall-{}", job.turn_id, receipt.call_id),
                "unknown",
                Some(&receipt.result_json),
                Some(receipt.started_at_ms),
                Some(receipt.finished_at_ms),
            )
        });
        run_ai_conversation_failure(storage, &job, sender,
            format!("工具已执行但回执保存失败，已标记为结果未知：{}", error.message));
        return;
    }

    let message = fluxdb_core::ModelMessage::new(
        fluxdb_core::ModelRole::Tool,
        format!(
            "{{\"tool\":\"{}\",\"status\":\"{}\",\"result\":{}}}",
            receipt.tool_id,
            if receipt.is_success() {
                "succeeded"
            } else {
                "failed"
            },
            if receipt.result_json.trim().is_empty() {
                "null".to_string()
            } else {
                receipt.result_json.clone()
            }
        ),
    );
    let existing = ai_conversation_existing_text(storage, &job);
    ai_conversation_run_turn(
        storage,
        &job,
        cancel,
        sender,
        provider,
        tools,
        audit,
        permission,
        vec![message],
        existing,
    );
}
