// T028 会话加载/草稿/模型事件保存。同步存储由 UI 在后台执行，普通打开不发模型请求。
fn ai_chat_now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default() }
fn ai_chat_project(data: fluxdb_storage::ai_platform::conversation::ConversationData) -> AiConversationView {
    let draft=data.ui_state.as_ref().and_then(|ui|ui.draft_json.as_deref())
        .or(data.preferences.pending_draft_json.as_deref()).and_then(|raw|serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|v|v.get("text").and_then(|s|s.as_str()).map(str::to_owned)).unwrap_or_default();
    let expanded_parts=data.ui_state.as_ref().and_then(|ui|serde_json::from_str(&ui.expanded_part_ids_json).ok()).unwrap_or_default();
    AiConversationView {
        session_id:data.session.as_ref().map(|s|s.id.clone()), title:data.session.as_ref().map(|s|s.title.clone()).unwrap_or_else(||"新会话".into()),
        sessions:data.sessions.into_iter().filter(|s|s.status==fluxdb_storage::ai_platform::sessions::SessionStatus::Active).map(|s| AiConversationSummary{id:s.id,title:s.title,agent_id:s.agent_id}).collect(),
        messages:data.messages.into_iter().map(|m|AiConversationMessage{id:m.id.clone(),role:m.role,parts:data.parts.iter().filter(|p|p.message_id==m.id).map(|p|AiConversationPart{id:p.id.clone(),kind:p.kind.as_str().into(),status:p.status.as_str().into(),payload_json:p.payload_json.clone()}).collect()}).collect(),
        target_json:data.turns.last().map(|t|t.target_json.clone()),
        preferences:data.preferences, draft, expanded_parts,
        active_turn_id:data.turns.iter().find(|t| !t.status.is_terminal()).map(|t|t.id.clone()),error:None,
    }
}
impl AppController {
    fn dispatch_ai_conversation(&mut self, command:AiConversationCommand) -> AppEvent {
        if let AiConversationCommand::Apply{view}=command {self.state.ai_conversation=view; return AppEvent::AiConversationLoaded(self.state.ai_conversation.clone());}
        let result=self.ai_storage().and_then(|storage|{
            with_ai_storage(&storage,|conn| {
                match command {
                    AiConversationCommand::Load{ref session_id}=>{
                        let mut prefs=fluxdb_storage::ai_platform::settings::load_panel_preferences(conn)?;
                        if let Some(id)=session_id {
                            let _=fluxdb_storage::ai_platform::sessions::load_session(conn,id)?;
                            let rev=prefs.revision;prefs.last_session_id=Some(id.clone());
                            fluxdb_storage::ai_platform::settings::save_panel_preferences(conn,ai_chat_now(),&prefs,rev)?;
                        }
                    },
                    AiConversationCommand::New=>{
                        let mut prefs=fluxdb_storage::ai_platform::settings::load_panel_preferences(conn)?;
                        let rev=prefs.revision;prefs.last_session_id=None;prefs.pending_draft_json=None;
                        fluxdb_storage::ai_platform::settings::save_panel_preferences(conn,ai_chat_now(),&prefs,rev)?;
                    },
                    AiConversationCommand::SaveUi{ref draft,ref expanded_parts,mode,drawer_width,embedded_width}=>{
                        if draft.len()>64*1024 {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"草稿超过 64 KiB"));}
                        let tx=conn.unchecked_transaction().map_err(|e|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,e.to_string()))?;
                        let mut prefs=fluxdb_storage::ai_platform::settings::load_panel_preferences(&tx)?;
                        let rev=prefs.revision;prefs.preferred_mode=mode;prefs.drawer_width_dp=drawer_width;prefs.embedded_width_dp=embedded_width;
                        let draft_json=(!draft.is_empty()).then(||serde_json::json!({"text":draft}).to_string());
                        if let Some(ref id)=prefs.last_session_id {
                            let ui=fluxdb_storage::ai_platform::sessions::load_ui_state(&tx,id)?;
                            fluxdb_storage::ai_platform::sessions::save_ui_state(&tx,ai_chat_now(),&fluxdb_storage::ai_platform::sessions::SessionUiStateEdit{
                                session_id:id.clone(), selected_turn_id:ui.selected_turn_id,expanded_part_ids_json:serde_json::json!(expanded_parts).to_string(),scroll_anchor_message_id:ui.scroll_anchor_message_id,ui_options_json:ui.ui_options_json,draft_json,
                            },ui.revision)?;
                        } else {prefs.pending_draft_json=draft_json;}
                        fluxdb_storage::ai_platform::settings::save_panel_preferences(&tx,ai_chat_now(),&prefs,rev)?;
                        tx.commit().map_err(|e|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,e.to_string()))?;
                    },
                    AiConversationCommand::Apply{..}=>unreachable!(),
                }
                fluxdb_storage::ai_platform::conversation::load(conn,None).map(ai_chat_project)
            })
        });
        match result {Ok(view)=>{self.state.ai_conversation=view.clone();AppEvent::AiConversationLoaded(view)},Err(e)=>AppEvent::AiSettingsError(e.message)}
    }

    /// 正式发送前读取现有模型/Agent 配置。无可用配置明确失败，不能静默启动模拟回答。
    pub fn prepare_ai_conversation(&mut self, prompt:&str, target: &TurnTargetPin) -> fluxdb_core::Result<AiConversationJob> {
        self.prepare_ai_conversation_context(prompt,target,"")
    }
    pub fn prepare_ai_conversation_context(&mut self,prompt:&str,target:&TurnTargetPin,context:&str)->fluxdb_core::Result<AiConversationJob> {
        if context.len()>64*1024 {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"当前页面摘录超限"));}
        let storage=self.ai_storage()?;
        let (job,view)=with_ai_storage(&storage,|conn| {
            let runtime=fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?;
            let current=self.state.ai_conversation.session_id.as_deref().map(|id|fluxdb_storage::ai_platform::sessions::load_session(conn,id)).transpose()?;
            let agents=fluxdb_storage::ai_platform::agents::list_agents(conn)?;
            let agent_id=current.as_ref().map(|s|s.agent_id.as_str()).or(runtime.default_agent_id.as_deref());
            let agent=match agent_id {Some(id)=>agents.iter().find(|a|a.id==id),None=>agents.iter().filter(|a|a.enabled).next()}.ok_or_else(||fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"请先在 Agent 配置中新建并启用一个 Agent"))?;
            if !agent.enabled {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"当前 Agent 已停用"));}
            let model_id=agent.default_model_profile_id.as_deref().or(runtime.default_model_profile_id.as_deref()).ok_or_else(||fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"请给 Agent 绑定模型，或设置默认主模型"))?;
            let model=fluxdb_storage::ai_platform::models::load_model(conn,model_id)?;
            if !model.enabled || model.kind!=fluxdb_storage::ai_platform::models::ModelKind::Generation {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"当前生成模型不可用"));}
            let session_id=current.as_ref().map(|s|s.id.clone()).unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            let turn_id=uuid::Uuid::new_v4().to_string();
            fluxdb_storage::ai_platform::conversation::send(conn,ai_chat_now(),current.is_none(),&session_id,&agent.id,&turn_id,&uuid::Uuid::new_v4().to_string(),prompt,&target.target_json,&serde_json::json!({"model_profile_id":model_id,"agent_id":agent.id,"agent_revision":agent.revision,"system_prompt":agent.system_prompt,"ui_context":context}).to_string())?;
            let job=AiConversationJob{session_id:session_id.clone(),turn_id,assistant_message_id:uuid::Uuid::new_v4().to_string(),model_profile_id:model_id.into()};
            let view=ai_chat_project(fluxdb_storage::ai_platform::conversation::load(conn,Some(&session_id))?);
            Ok((job,view))
        })?;
        self.state.ai_conversation=view;
        Ok(job)
    }
}

/// 事件桥持久化：每 300ms checkpoint；结束强制保存，不按 token 建行。
pub fn persist_ai_conversation_event(storage:&fluxdb_storage::FileStorage,job:&AiConversationJob,text:&str,summary:&str,status:Option<fluxdb_storage::ai_platform::sessions::TurnStatus>,error:Option<&str>) -> fluxdb_core::Result<AiConversationView> {
    with_ai_storage(storage,|conn|{
        fluxdb_storage::ai_platform::conversation::checkpoint(conn,ai_chat_now(),&job.session_id,&job.turn_id,&job.assistant_message_id,text,summary,status,error)?;
        let mut view=ai_chat_project(fluxdb_storage::ai_platform::conversation::load(conn,Some(&job.session_id))?);
        view.error=error.map(str::to_owned);Ok(view)
    })
}

/// 仅主线程消费的回桥包；身份用于拒绝切会话后的迟到回调。
#[derive(Clone, Debug)]
pub struct AiConversationUpdate { pub job:AiConversationJob, pub view:AiConversationView, pub terminal:bool }

/// 本次先提供有时间/输出限额保护的单轮解释/问答；数据库工具执行需 T020/T023 接线后启用。
/// 不是另一个 Agent 循环，不执行未经审计的工具请求。
pub fn run_ai_conversation(storage:fluxdb_storage::FileStorage,job:AiConversationJob,cancel:fluxdb_core::CancelFlag,sender:std::sync::mpsc::SyncSender<AiConversationUpdate>) {
    let setup=with_ai_storage(&storage,|conn|{
        let model=fluxdb_storage::ai_platform::models::load_model(conn,&job.model_profile_id)?;
        let data=fluxdb_storage::ai_platform::conversation::load(conn,Some(&job.session_id))?;
        let turn=fluxdb_storage::ai_platform::sessions::load_turn(conn,&job.turn_id)?;
        let sealed=fluxdb_storage::ai_platform::models::load_encrypted_secret(conn,&model.id)?;
        Ok((model,data,turn,sealed))
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
            match protocol{ChatProtocol::OpenAiCompatible=>TransportAuth::Bearer(key),ChatProtocol::AnthropicMessages=>TransportAuth::Header{name:"x-api-key",value:key}}
        };
        let base=model.endpoint.as_deref().unwrap_or(if protocol==ChatProtocol::OpenAiCompatible {"https://api.openai.com/v1"} else {"https://api.anthropic.com/v1"});
        let endpoint=ai_request_url(base,&model.api_protocol,model.endpoint_is_full_url)?;
        let headers:&[(&str,&str)]=if protocol==ChatProtocol::AnthropicMessages{&[("anthropic-version","2023-06-01")]}else{&[]};
        stream_chat_emit(TransportLimits{max_total_bytes:512*1024,read_timeout:std::time::Duration::from_secs(30),..Default::default()},protocol,&endpoint,&auth,&body.to_string(),headers,&cancel,emit).map_err(TransportError::to_core_error)
    };
    run_ai_conversation_source(&storage,&job,&cancel,&sender,run);
}

/// 可替换 source 只决定模型事件，保存与回桥仍经过同一个生产路径。fixture 不伪造数据库结果。
pub fn run_ai_conversation_source(
    storage:&fluxdb_storage::FileStorage,job:&AiConversationJob,cancel:&fluxdb_core::CancelFlag,
    sender:&std::sync::mpsc::SyncSender<AiConversationUpdate>,
    source:impl FnOnce(&mut dyn FnMut(&fluxdb_core::ModelEvent))->fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>,
) {
    let mut text=String::new();let mut summary=String::new();let mut saved=std::time::Instant::now();let mut finish=None;let mut persist_error=None;
    let mut emit=|event:&fluxdb_core::ModelEvent|{
        match event {fluxdb_core::ModelEvent::TextDelta(delta)=>text.push_str(delta),fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta)=>summary.push_str(delta),fluxdb_core::ModelEvent::Finished(reason)=>finish=Some(*reason),_=>{}}
        if saved.elapsed()>=std::time::Duration::from_millis(300) && persist_error.is_none() {
            match persist_ai_conversation_event(storage,job,&text,&summary,None,None){Ok(view)=>{let _=sender.try_send(AiConversationUpdate{job:job.clone(),view,terminal:false});},Err(e)=>persist_error=Some(e.message)};
            saved=std::time::Instant::now();
        }
    };
    let response=source(&mut emit);drop(emit);
    let error=persist_error.or_else(||response.err().map(|e|e.message)).or_else(||if finish.is_none()&&!cancel.is_cancelled(){Some("模型连接提前结束，未收到完整结束事件".into())}else{None});
    let status=if cancel.is_cancelled() || finish==Some(fluxdb_core::FinishReason::Cancelled){fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled}else if error.is_some() || finish==Some(fluxdb_core::FinishReason::Error){fluxdb_storage::ai_platform::sessions::TurnStatus::Failed}else{fluxdb_storage::ai_platform::sessions::TurnStatus::Completed};
    if finish==Some(fluxdb_core::FinishReason::Length) {text.push_str("\n[模型输出达到本轮上限，内容可能不完整。]");}
    // 即使没有回复文字也写出可见状态，空响应不能伪装成功。
    if text.is_empty() {text=error.clone().unwrap_or_else(||if status==fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled{"已停止后续生成。".into()}else{"模型未返回正文。".into()});}
    match persist_ai_conversation_event(storage,job,&text,&summary,Some(status),error.as_deref()) {
        Ok(view)=>{let _=sender.send(AiConversationUpdate{job:job.clone(),view,terminal:true});},
        Err(e)=>{tracing::warn!(turn_id=%job.turn_id,"AI 回复保存失败");let mut view=with_ai_storage(storage,|conn|fluxdb_storage::ai_platform::conversation::load(conn,Some(&job.session_id))).map(ai_chat_project).unwrap_or_default();view.error=Some(format!("回复保存失败：{}",e.message));view.session_id=Some(job.session_id.clone());let _=sender.send(AiConversationUpdate{job:job.clone(),view,terminal:true});}
    }
}

/// 真实 Provider：把既有 transport 包成 `GenerationProvider`（设计 §17/T017）。
///
/// 只负责"模型事件"；工具 schema 校验、审计、授权、执行与审批全部由 `TurnRunner`/运行时负责。
/// 凭据只以运行时明文存在于本结构内：不实现 `Debug`，不进日志/事件/持久化。
pub struct AiConversationProvider {
    protocol: ChatProtocol,
    endpoint: String,
    auth: TransportAuth,
    model: String,
    max_output_tokens: Option<i64>,
    capabilities: fluxdb_core::ModelCapabilities,
}

impl AiConversationProvider {
    /// 从落盘模型配置构造；凭据在本函数内 AES 解密后直接交给 transport，不返回给调用方。
    pub fn from_storage(
        storage: &fluxdb_storage::FileStorage,
        model_profile_id: &str,
    ) -> fluxdb_core::Result<Self> {
        let (model, sealed) = with_ai_storage(storage, |conn| {
            let model = fluxdb_storage::ai_platform::models::load_model(conn, model_profile_id)?;
            let sealed = fluxdb_storage::ai_platform::models::load_encrypted_secret(conn, &model.id)?;
            Ok((model, sealed))
        })?;
        let protocol = match model.api_protocol.as_str() {
            "openai-compatible" => ChatProtocol::OpenAiCompatible,
            "anthropic-messages" => ChatProtocol::AnthropicMessages,
            _ => {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "该 Provider 暂不支持对话",
                ))
            }
        };
        let auth = if model.auth_mode == fluxdb_storage::ai_platform::models::AuthMode::None {
            TransportAuth::None
        } else {
            let sealed = sealed.ok_or_else(|| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型缺少 API Key")
            })?;
            let (cipher, _) = fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir())?;
            let key = fluxdb_storage::ai_platform::model_secret::open(&cipher, &model.id, &sealed)
                .map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型凭据不可用")
                })?;
            match protocol {
                ChatProtocol::OpenAiCompatible => TransportAuth::Bearer(key),
                ChatProtocol::AnthropicMessages => TransportAuth::Header {
                    name: "x-api-key",
                    value: key,
                },
            }
        };
        let base = model.endpoint.as_deref().unwrap_or(
            if protocol == ChatProtocol::OpenAiCompatible {
                "https://api.openai.com/v1"
            } else {
                "https://api.anthropic.com/v1"
            },
        );
        let endpoint = ai_request_url(base, &model.api_protocol, model.endpoint_is_full_url)?;
        Ok(Self {
            protocol,
            endpoint,
            auth,
            model: model.model.clone(),
            max_output_tokens: Some(model.max_output_tokens.unwrap_or(1024).min(1024)),
            capabilities: fluxdb_core::ModelCapabilities {
                streaming: true,
                // 工具交给模型前仍要经过内核授权；这里只声明协议能力。
                tool_calling: true,
                reasoning_summary: false,
            },
        })
    }

    /// 供测试/离线夹具构造（不含凭据）。
    pub fn for_test(
        protocol: ChatProtocol,
        endpoint: String,
        model: String,
        capabilities: fluxdb_core::ModelCapabilities,
    ) -> Self {
        Self {
            protocol,
            endpoint,
            auth: TransportAuth::None,
            model,
            max_output_tokens: Some(1024),
            capabilities,
        }
    }
}

impl fluxdb_core::GenerationProvider for AiConversationProvider {
    fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
        self.capabilities
    }

    fn stream(
        &self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            let mapper = ToolNameMapper::new(&request.tools)?;
            let body = build_chat_body(
                self.protocol,
                &self.model,
                &request,
                &mapper,
                self.capabilities,
                self.max_output_tokens,
            );
            let headers: &[(&str, &str)] = if self.protocol == ChatProtocol::AnthropicMessages {
                &[("anthropic-version", "2023-06-01")]
            } else {
                &[]
            };
            let mut events: Vec<fluxdb_core::ModelEvent> = Vec::new();
            let mut emit = |event: &fluxdb_core::ModelEvent| events.push(event.clone());
            stream_chat_emit(
                TransportLimits {
                    max_total_bytes: 512 * 1024,
                    read_timeout: std::time::Duration::from_secs(30),
                    ..Default::default()
                },
                self.protocol,
                &self.endpoint,
                &self.auth,
                &body.to_string(),
                headers,
                &cancel,
                &mut emit,
            )
            .map_err(TransportError::to_core_error)?;
            Ok(events)
        })
    }
}

/// 组装本轮公开消息：系统提示 + 界面摘录 + 历史文本片段（不含工具输出，避免把上次执行结果当新上下文）。
fn ai_conversation_turn_messages(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::Result<(String, Vec<fluxdb_core::ModelMessage>)> {
    with_ai_storage(storage, |conn| {
        let data =
            fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))?;
        let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)?;
        let snapshot: serde_json::Value = serde_json::from_str(&turn.runtime_snapshot_json)
            .map_err(|_| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "运行快照损坏"))?;
        let system_prompt = snapshot
            .get("system_prompt")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let mut messages = vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::System,
            format!(
                "{system_prompt}\n当前执行目标：{}。你可以调用被授权的工具；只能依据工具真实回执陈述执行结果，不得臆造。",
                turn.target_json
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
                .filter_map(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
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


/// 参数脱敏：保留工具与 SQL 结构，去掉疑似凭据字段后入库（审计可读，秘密不入库）。
fn ai_conversation_redact_arguments(arguments_json:&str)->String {
    let Ok(mut value)=serde_json::from_str::<serde_json::Value>(arguments_json) else {return "{}".to_string();};
    ai_conversation_redact_value(&mut value);
    value.to_string()
}

/// 递归脱敏：嵌套对象/数组里的疑似凭据同样要删掉，不能只过滤顶层键。
fn ai_conversation_redact_value(value:&mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object)=>{
            object.retain(|key,_| !ai_conversation_is_secret_key(key));
            for child in object.values_mut() { ai_conversation_redact_value(child); }
        }
        serde_json::Value::Array(items)=>{ for item in items { ai_conversation_redact_value(item); } }
        _=>{}
    }
}

fn ai_conversation_is_secret_key(key:&str)->bool {
    let key=key.to_ascii_lowercase();
    ["password","secret","token","api_key","apikey","credential","private_key"].iter().any(|needle|key.contains(needle))
}

/// 记录"工具开始执行"：写 `ai_tool_calls` 事实行 + 工具卡片段（外键要求先有事实行）。
fn ai_conversation_record_tool_start(
    storage:&fluxdb_storage::FileStorage, job:&AiConversationJob, ordinal:usize,
    call:&fluxdb_core::ToolCallRequest, plan_hash:&str, tool_version:u32, target_json:&str,
) -> fluxdb_core::Result<()> {
    let call_row=format!("{}-toolcall-{}",job.turn_id,call.provider_call_id);
    let payload=serde_json::json!({"tool":call.tool_id,"call_id":call.provider_call_id,"plan_hash":plan_hash}).to_string();
    with_ai_storage(storage,|conn|{
        fluxdb_storage::ai_platform::sessions::start_tool_call(conn,ai_chat_now(),&call_row,&job.session_id,&job.turn_id,
            ordinal as i64+1,ordinal as i64,&call.provider_call_id,&call.tool_id,tool_version,None,
            &ai_conversation_redact_arguments(&call.arguments_json),target_json,plan_hash)?;
        fluxdb_storage::ai_platform::conversation::upsert_event_part(conn,ai_chat_now(),&job.session_id,&job.turn_id,
            &job.assistant_message_id,&format!("{}-tool-{}",job.assistant_message_id,call.provider_call_id),
            fluxdb_storage::ai_platform::sessions::PartKind::ToolCallRef,
            fluxdb_storage::ai_platform::sessions::PartStatus::Streaming,&payload,Some(&call_row),None)
    })
}

/// 记录工具真实回执：写 `ai_results` 句柄 + 结果卡；回执原文只进 receipt 列，不被投影改写。
fn ai_conversation_record_tool_result(
    storage:&fluxdb_storage::FileStorage, job:&AiConversationJob, receipt:&fluxdb_core::ToolReceipt,
) -> fluxdb_core::Result<()> {
    let call_row=format!("{}-toolcall-{}",job.turn_id,receipt.call_id);
    let result_row=format!("{}-result-{}",job.turn_id,receipt.call_id);
    let call_status=match receipt.status {
        fluxdb_core::ToolReceiptStatus::Succeeded=>"succeeded",
        fluxdb_core::ToolReceiptStatus::Failed=>"failed",
        fluxdb_core::ToolReceiptStatus::Denied=>"denied",
        fluxdb_core::ToolReceiptStatus::Cancelled=>"cancelled",
        fluxdb_core::ToolReceiptStatus::Unknown=>"unknown",
    };
    let part_status=match receipt.status {
        fluxdb_core::ToolReceiptStatus::Succeeded=>fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
        fluxdb_core::ToolReceiptStatus::Cancelled=>fluxdb_storage::ai_platform::sessions::PartStatus::Interrupted,
        _=>fluxdb_storage::ai_platform::sessions::PartStatus::Error,
    };
    let payload=serde_json::json!({
        "tool":receipt.tool_id,
        "status":call_status,
        "error_code":receipt.error_code,
        "affected_rows":receipt.affected_rows,
        "truncated":false,
    }).to_string();
    let byte_count=receipt.result_json.len() as i64;
    with_ai_storage(storage,|conn|{
        fluxdb_storage::ai_platform::sessions::finish_tool_call(conn,&call_row,call_status,Some(&receipt.result_json),
            Some(receipt.started_at_ms),Some(receipt.finished_at_ms))?;
        fluxdb_storage::ai_platform::sessions::insert_result(conn,ai_chat_now(),&result_row,&job.session_id,&job.turn_id,
            &call_row,&serde_json::json!({"tool":receipt.tool_id,"call_id":receipt.call_id}).to_string(),"available",byte_count)?;
        fluxdb_storage::ai_platform::conversation::upsert_event_part(conn,ai_chat_now(),&job.session_id,&job.turn_id,
            &job.assistant_message_id,&format!("{}-result-{}",job.assistant_message_id,receipt.call_id),
            fluxdb_storage::ai_platform::sessions::PartKind::ToolResultRef,part_status,&payload,Some(&call_row),Some(&result_row))
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
    let (target_json, messages) = match ai_conversation_turn_messages(storage, job) {
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

    let mut text = String::new();
    let mut summary = String::new();
    let mut saved = std::time::Instant::now();
    let mut persist_error: Option<String> = None;
    let mut paused_reason: Option<String> = None;
    let mut notice_count: usize = 0;
    let mut tool_index: usize = 0;
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
            LoopEvent::ToolStarted { call, plan_hash, tool_version } => {
                let saved_part = ai_conversation_record_tool_start(storage, job, tool_index, &call, &plan_hash, tool_version, &target_json_for_parts);
                if let Err(error) = &saved_part {
                    part_error.get_or_insert_with(|| error.message.clone());
                }
                tool_index += 1;
            }
            // 工具真实回执：写 ai_tool_calls/ai_results 事实行 + 结果卡。
            LoopEvent::ToolFinished { receipt } => {
                if !receipt.is_success() {
                    tool_error = Some(format!("工具 {} 未成功：{}",receipt.tool_id,receipt.error_code.clone().unwrap_or_else(||"unknown".into())));
                }
                let saved = ai_conversation_record_tool_result(storage, job, &receipt);
                if let Err(error) = &saved {
                    part_error.get_or_insert_with(|| error.message.clone());
                }
            }
            LoopEvent::Notice(notice) => {
                let payload = serde_json::json!({"text":notice}).to_string();
                let saved = ai_conversation_save_part(
                    storage, job,
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
            // 审批卡由后续切片（审批持久化 + 决定入口）接通；本轮只把等待原因写进通知片段。
            LoopEvent::ApprovalRequired { .. } => {}
            LoopEvent::IterationStarted { .. } => {}
        }
        if saved.elapsed() >= std::time::Duration::from_millis(300) && persist_error.is_none() {
            match persist_ai_conversation_event(storage, job, &text, &summary, None, None) {
                Ok(view) => {
                    let _ = sender.try_send(AiConversationUpdate { job: job.clone(), view, terminal: false });
                }
                Err(error) => persist_error = Some(error.message),
            }
            saved = std::time::Instant::now();
        }
    });

    let status = match outcome {
        TurnOutcome::Completed { .. } => fluxdb_storage::ai_platform::sessions::TurnStatus::Completed,
        TurnOutcome::Cancelled => fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled,
        TurnOutcome::Failed { message } => {
            persist_error.get_or_insert(message);
            fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
        }
        TurnOutcome::Paused { reason } => {
            paused_reason = Some(reason);
            fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval
        }
    };
    let error = persist_error.clone().or(part_error.clone());
    if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Completed {
        if let Some(message) = tool_error {
            // 工具失败不等于整个回合失败，但必须让用户看得见。
            if let Err(save_error) = ai_conversation_save_part(
                storage, job,
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
            storage, job,
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
            fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled => "已停止后续生成。".into(),
            fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval => paused_reason.clone().unwrap_or_else(|| "等待审批。".into()),
            _ => "模型未返回正文。".into(),
        });
    }
    match persist_ai_conversation_event(storage, job, &text, &summary, Some(status), error.as_deref()) {
        Ok(view) => {
            let _ = sender.send(AiConversationUpdate { job: job.clone(), view, terminal: true });
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
            let _ = sender.send(AiConversationUpdate { job: job.clone(), view, terminal: true });
        }
    }
}

/// 明确失败：在会话里留下可见错误并结束该 Turn（不能静默无响应，也不能伪造成功）。
pub fn run_ai_conversation_failure(
    storage:&fluxdb_storage::FileStorage, job:&AiConversationJob,
    sender:&std::sync::mpsc::SyncSender<AiConversationUpdate>, message:String,
) {
    let text=format!("本轮未能开始：{message}");
    match persist_ai_conversation_event(storage,job,&text,"",Some(fluxdb_storage::ai_platform::sessions::TurnStatus::Failed),Some(&message)) {
        Ok(view)=>{let _=sender.send(AiConversationUpdate{job:job.clone(),view,terminal:true});},
        Err(error)=>{
            let mut view=with_ai_storage(storage,|conn|fluxdb_storage::ai_platform::conversation::load(conn,Some(&job.session_id))).map(ai_chat_project).unwrap_or_default();
            view.error=Some(format!("{message}（保存也失败：{}）",error.message));
            view.session_id=Some(job.session_id.clone());
            let _=sender.send(AiConversationUpdate{job:job.clone(),view,terminal:true});
        }
    }
}
