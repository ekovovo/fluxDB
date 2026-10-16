// 会话命令、发送准备与持久化投影；打开会话不发模型请求，执行任务由 actor 调度。
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
        // 面板历史只列活跃会话：已归档会话在设置页管理，避免把归档指针写成 last_session_id。
        sessions: data
            .sessions
            .into_iter()
            .filter(|s| s.status == fluxdb_storage::ai_platform::sessions::SessionStatus::Active)
            .map(|s| AiConversationSummary {
                id: s.id,
                title: s.title,
                agent_id: s.agent_id,
                status: s.status.as_str().to_string(),
                last_turn_status: s.last_turn_status.map(|status| status.as_str().to_string()),
                last_target_json: s.last_target_json,
                message_count: s.message_count,
                turn_count: s.turn_count,
                updated_at_ms: s.updated_at_ms,
                revision: s.revision,
            })
            .collect(),
        messages: data
            .messages
            .into_iter()
            .map(|m| {
                let mut parts: Vec<AiConversationPart> = data
                    .parts
                    .iter()
                    .filter(|p| p.message_id == m.id)
                    .map(|p| AiConversationPart {
                        id: p.id.clone(),
                        kind: p.kind.as_str().into(),
                        status: p.status.as_str().into(),
                        payload_json: if let Some(call_id) = p.tool_call_id.as_ref().filter(|_| {
                            matches!(p.kind, fluxdb_storage::ai_platform::sessions::PartKind::ToolCallRef | fluxdb_storage::ai_platform::sessions::PartKind::ToolResultRef)
                        }) {
                            let mut payload = serde_json::from_str::<serde_json::Value>(&p.payload_json)
                                .unwrap_or_default();
                            if let Some(object) = payload.as_object_mut() {
                                object.insert("tool_call_id".into(), call_id.clone().into());
                            }
                            payload.to_string()
                        } else {
                            p.payload_json.clone()
                        },
                    })
                    .collect();
                // 旧会话没有 context_ref 片段时，从该消息所属 Turn 的固定快照恢复公开标签。
                if m.role == "user" && !parts.iter().any(|part| part.kind == "context_ref") {
                    if let Some(snapshot) = data.turns.iter().find(|turn| Some(&turn.id) == m.turn_id.as_ref())
                        .and_then(|turn| serde_json::from_str::<serde_json::Value>(&turn.runtime_snapshot_json).ok()) {
                        if let Some(references) = snapshot.get("context_references").and_then(|value| value.as_array()) {
                            parts.extend(references.iter().enumerate().filter_map(|(index, reference)| {
                                Some(AiConversationPart {
                                    id: format!("{}-context-legacy-{index}", m.id), kind: "context_ref".into(), status: "complete".into(),
                                    payload_json: serde_json::json!({"id":reference.get("id")?.as_str()?,"label":reference.get("label")?.as_str()?,"kind":reference.get("kind")?.as_str()?,"summary":reference.get("summary").and_then(|value| value.as_str()).unwrap_or_default()}).to_string(),
                                })
                            }));
                        }
                    }
                }
                AiConversationMessage { id: m.id, role: m.role, parts }
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
        selected_model_id: data.selected_model_id,
        model_selection_revision: data.model_selection_revision,
        context_references: data.context_references.iter().filter_map(|raw|serde_json::from_str(raw).ok()).filter_map(|v|AiContextReference::parse(&v)).collect(),
        usage: data.usage,
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
                        fluxdb_storage::ai_platform::model_selection::clear_draft(conn)?;
                        fluxdb_storage::ai_platform::context_references::clear_draft(conn)?;
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
                    AiConversationCommand::SelectModel { ref model_id, expected_revision } => {
                        fluxdb_storage::ai_platform::model_selection::save(conn,self.state.ai_conversation.session_id.as_deref(),model_id.as_deref(),expected_revision)?;
                    }
                    AiConversationCommand::AddReference {ref reference} => {
                        let reference=if reference.kind=="file" {
                            ensure_ai_files_enabled(conn)?;
                            let raw:serde_json::Value=serde_json::from_str(&reference.source).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件授权损坏"))?;
                            let path=raw["path"].as_str().ok_or_else(||fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件路径缺失"))?;
                            let mut validated=authorize_ai_file(std::path::Path::new(path),storage.root_dir())?;
                            validated.id=reference.id.clone();validated
                        } else { reference.clone() };
                        fluxdb_storage::ai_platform::context_references::add(conn,self.state.ai_conversation.session_id.as_deref(),&reference.id,&reference.json().to_string())?;
                    }
                    AiConversationCommand::RemoveReference {ref reference_id} => fluxdb_storage::ai_platform::context_references::remove(conn,self.state.ai_conversation.session_id.as_deref(),reference_id)?,
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
            if !model.enabled || model.kind != fluxdb_storage::ai_platform::models::ModelKind::Generation {
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
        self.prepare_ai_conversation_context_with_editor(prompt, target, context, None)
    }
    /// 发送时同时登记宿主持有的编辑器快照。登记先于任务创建：登记失败不会产生
    /// 半成品任务，任务创建失败则清理本轮登记，不让其它轮次误领残留快照。
    pub fn prepare_ai_conversation_context_with_editor(
        &mut self,
        prompt: &str,
        target: &TurnTargetPin,
        context: &str,
        editor_grant: Option<EditorGrant>,
    ) -> fluxdb_core::Result<AiConversationJob> {
        self.prepare_ai_conversation_with_grants(prompt, target, context, editor_grant, None)
    }

    pub fn prepare_ai_conversation_page(
        &mut self,
        prompt: &str,
        target: &TurnTargetPin,
        page: Option<CurrentPageContext>,
    ) -> fluxdb_core::Result<AiConversationJob> {
        let context = page.as_ref().map(|p| p.public_json().to_string()).unwrap_or_default();
        let (editor, data) = page.map(|p| (p.editor_grant, p.data_grant)).unwrap_or_default();
        self.prepare_ai_conversation_with_grants(prompt, target, &context, editor, data)
    }

    fn prepare_ai_conversation_with_grants(
        &mut self,
        prompt: &str,
        target: &TurnTargetPin,
        context: &str,
        editor_grant: Option<EditorGrant>,
        page_grant: Option<PageDataGrant>,
    ) -> fluxdb_core::Result<AiConversationJob> {
        if context.len() > 64 * 1024 {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "当前页面摘录超限",
            ));
        }
        let storage = self.ai_storage()?;
        if let Some(grant) = page_grant.as_ref() {
            let config = grant.target.connection_id.and_then(|id| self.state.connections.iter()
                .find(|connection|connection.config.id.0 as i64 == id).map(|connection|&connection.config))
                .ok_or_else(||editor_error("页面数据所属连接不可用"))?;
            self.validate_ai_database_resource(config)?;
        }
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
            // 新会话的 Agent 选择：内置默认 Agent 是代码内置入口，但如果它没有可用的
            // 生成模型（既未绑定、也没有全局默认模型），就回退到"已绑定可用模型的自定义
            // Agent"，避免用户配好自定义 Agent + 模型后仍无法对话。
            let agent_has_usable_model = |agent: &&fluxdb_storage::ai_platform::agents::AgentView| {
                agent.default_model_profile_id.is_some() || runtime.default_model_profile_id.is_some()
            };
            let agent = if let Some(session) = current.as_ref() {
                // 已有会话维持原身份。
                agents.iter().find(|a| a.id == session.agent_id)
            } else {
                let preferred = runtime
                    .default_agent_id
                    .as_deref()
                    .and_then(|id| agents.iter().find(|a| a.id == id && a.enabled));
                let builtin = agents.iter().find(|a| {
                    a.id == fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID && a.enabled
                });
                preferred
                    .filter(|agent| agent_has_usable_model(agent))
                    .or_else(|| {
                        builtin.filter(|agent| agent_has_usable_model(agent))
                    })
                    .or_else(|| {
                        // 兜底：任一已启用且绑定了模型的 Agent；都没有时再退回内置
                        // （此时会给出"请先配置默认主模型"的明确提示）。
                        agents
                            .iter()
                            .find(|a| a.enabled && a.default_model_profile_id.is_some())
                            .or(builtin)
                    })
            }
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "会话使用的 Agent 已不可用，请新建会话使用默认助手",
                )
            })?;
            if !agent.enabled {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "当前 Agent 已停用",
                ));
            }
            let (selected_model_id, _) = fluxdb_storage::ai_platform::model_selection::load(conn, current.as_ref().map(|s|s.id.as_str()))?;
            // 路由 precedence（设计 §17）：会话/草稿显式选择 → Agent 的 main_candidate 角色绑定
            // → Agent 默认模型 → 全局默认。显式选择不合格时绝不静默回退，避免固定模型被换掉。
            let (model_id, route_source) = match selected_model_id {
                Some(id) => (id, crate::RouteSource::SessionSelection),
                None => {
                    let bound = fluxdb_storage::ai_platform::model_bindings::list_role_bindings(
                        conn,
                        &agent.id,
                        fluxdb_storage::ai_platform::model_bindings::ModelRole::MainCandidate,
                    )?
                    .into_iter()
                    .next()
                    .map(|binding| binding.model_profile_id);
                    if let Some(id) = bound {
                        (id, crate::RouteSource::AgentBinding)
                    } else if let Some(id) = agent.default_model_profile_id.clone() {
                        (id, crate::RouteSource::AgentDefault)
                    } else if let Some(id) = runtime.default_model_profile_id.clone() {
                        (id, crate::RouteSource::GlobalDefault)
                    } else {
                        return Err(fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "请在输入框下方选择模型，或在大模型设置中配置默认主模型",
                        ));
                    }
                }
            };
            let explicit = route_source == crate::RouteSource::SessionSelection;
            let model = fluxdb_storage::ai_platform::models::load_model(conn, &model_id)?;
            // 出站许可只看该模型自己的标记。被拒绝时给出可见原因，绝不静默换成别的模型。
            if let Err(rejection) = candidate_check(&model, CandidateRole::Main) {
                let suffix = if explicit {
                    "，请重新选择"
                } else {
                    "，请在输入框下方选择模型或在大模型设置中修正绑定"
                };
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("{}{suffix}", rejection.message(CandidateRole::Main)),
                ));
            }
            let session_id = current
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let turn_id = uuid::Uuid::new_v4().to_string();
            let references=fluxdb_storage::ai_platform::context_references::list(conn,current.as_ref().map(|s|s.id.as_str()))?
                .iter().filter_map(|raw|serde_json::from_str(raw).ok()).filter_map(|v|AiContextReference::parse(&v)).collect::<Vec<_>>();
            let mut target_json = self.referenced_execution_target(&references)?
                .map(|target| target.to_json()).unwrap_or_else(|| target.target_json.clone());
            // 清空的是下一轮输入引用，不是会话绑定；主页追问沿用原目标，显式引用/当前页面优先。
            if ai_tool_target_handle(&target_json).is_none() && target.target.is_empty() {
                if let Some(session) = current.as_ref() {
                    if let Some(previous) = fluxdb_storage::ai_platform::sessions::list_turns(conn, &session.id)?.last()
                        .filter(|turn| ai_tool_target_handle(&turn.target_json).is_some()) {
                        let value: serde_json::Value = serde_json::from_str(&previous.target_json)
                            .map_err(|_| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "会话目标损坏"))?;
                        if !self.state.connections.iter().any(|connection| i64::try_from(connection.config.id.0).ok() == value["connection_id"].as_i64()) {
                            return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "会话目标连接已不存在，请重新选择引用或连接"));
                        }
                        target_json = previous.target_json.clone();
                    }
                }
            }
            let reference_grants = self.ai_reference_detail_grants(&references)?;
            let public_refs=references.iter().map(|r|self.ai_public_reference(r)).collect::<fluxdb_core::Result<Vec<_>>>()?;
            let database_context=self.ai_database_context(&target_json)?;
            let file_grants=references.iter().filter(|r|r.kind=="file").map(AiContextReference::json).collect::<Vec<_>>();
            let snapshot=serde_json::json!({"model_profile_id":model_id,"agent_id":agent.id,"agent_revision":agent.revision,"compression_trigger_percent":agent.compression_trigger_percent,"max_iterations":agent.max_iterations,"system_prompt":agent.system_prompt,"ui_context":context,"context_references":public_refs,"database_context":database_context,"file_grants":file_grants,"reference_grants":reference_grants});
            if let Some(grant) = editor_grant {
                register_editor_grant(&turn_id, grant)?;
            }
            if let Some(grant) = page_grant {
                let mut registry = page_data_registry().lock().map_err(|_| {
                    clear_editor_grant(&turn_id);
                    editor_error("页面快照登记失败")
                })?;
                registry.insert(turn_id.clone(), grant);
            }
            if let Err(error) = fluxdb_storage::ai_platform::conversation::send(conn,ai_chat_now(),current.is_none(),&session_id,&agent.id,&turn_id,&uuid::Uuid::new_v4().to_string(),prompt,&target_json,&snapshot.to_string()) {
                clear_editor_grant(&turn_id);
                take_page_data_grant(&turn_id);
                return Err(error.into());
            }
            // 路由决策落成运行事件：验收与 UI 都能核对"Provider 实际用哪个模型"。
            // 事件是诊断证据而非控制点，写失败不阻断本次发送，但必须留痕。
            let route_summary = serde_json::json!({
                "role": "main",
                "source": route_source.as_str(),
                "agent_id": agent.id,
                "model_profile_id": model.id,
                "provider": model.provider,
                "model": model.model,
            })
            .to_string();
            if let Err(error) = fluxdb_storage::ai_platform::run_events::append(
                conn,
                &turn_id,
                fluxdb_storage::ai_platform::run_events::KIND_MODEL_ROUTE,
                &route_summary,
                ai_chat_now(),
            ) {
                tracing::warn!(target: "fluxdb_app", turn_id = %turn_id, "模型路由事件写入失败：{}", error.message);
            }

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

include!("ai_conversation_delivery.rs");
include!("ai_conversation_execution.rs");
include!("ai_conversation_runtime.rs");
