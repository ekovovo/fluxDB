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
