// T029：宿主启动恢复。只扫描存储，不构造 Provider/Connector，不自动发任何外部请求。
impl AppController {
    fn recover_ai_on_startup(&mut self) -> AppEvent {
        let result = self.ai_storage().and_then(|storage| {
            let mut conn = storage.open_ai_connection()?;
            let summary = fluxdb_storage::ai_platform::recovery::recover_on_startup(
                &mut conn,
                ai_chat_now(),
            )?;
            tracing::info!(
                interrupted = summary.interrupted_turns,
                unknown = summary.unknown_turns,
                waiting_approval = summary.waiting_approval_turns,
                expired_results = summary.expired_results,
                "AI 启动恢复扫描完成（未重放工具）"
            );
            Ok(self.state.ai_conversation.clone())
        });
        match result {
            Ok(view) => {
                self.state.ai_conversation = view.clone();
                AppEvent::AiConversationLoaded(view)
            }
            Err(error) => {
                tracing::warn!("AI 启动恢复扫描失败，禁止自动继续任务");
                AppEvent::AiSettingsError(error.message)
            }
        }
    }
}

/// 重启后恢复也必须重新校验计划与内核权限，再用短事务占用执行权。
/// 无回执 running/unknown 永远不能靠再次点批准重放；事务不跨任何模型/数据库请求。
fn claim_ai_approved_call(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: &TurnPermissionContext,
    approval_id: &str,
) -> fluxdb_core::Result<(fluxdb_storage::ai_platform::sessions::ToolCallView, String)> {
    let mut conn = storage.open_ai_connection()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    let reject = |message: &str| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message);
    let approval = fluxdb_storage::ai_platform::audits::load_approval(&tx, approval_id)?;
    let call_id = approval
        .tool_call_id
        .as_deref()
        .ok_or_else(|| reject("该审批没有绑定工具调用"))?;
    let call = fluxdb_storage::ai_platform::sessions::load_tool_call(&tx, call_id)?;
    let turn = fluxdb_storage::ai_platform::sessions::load_turn(&tx, &call.turn_id)?;
    if call.session_id != job.session_id
        || call.turn_id != job.turn_id
        || turn.status != fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval
        || call.status != "waiting_approval"
        || call.receipt_json.is_some()
    {
        return Err(reject("该任务或工具调用已被处理，拒绝重复执行"));
    }
    let session = fluxdb_storage::ai_platform::sessions::load_session(&tx, &call.session_id)?;
    let agent = fluxdb_storage::ai_platform::agents::load_agent(&tx, &session.agent_id)?;
    if !agent.enabled
        || (!agent.allowed_tools.is_empty()
            && !agent.allowed_tools.iter().any(|id| id == &call.tool_id))
    {
        return Err(reject("当前 Agent 已停用或不再允许该工具"));
    }
    let prepared = tools
        .prepare(&call.tool_id, &call.arguments_redacted_json)
        .map_err(|_| reject("批准后的参数未通过校验"))?;
    let spec = tools
        .describe(&call.tool_id)
        .ok_or_else(|| reject("工具不可用"))?;
    if tools.executable(&call.tool_id).is_none() {
        return Err(reject("工具没有可执行实现"));
    }
    if spec.risk != fluxdb_core::ToolRisk::Read {
        // 同一连接存在未知副作用时，另一次批准也不能掩盖冲突；等待人工/只读核实。
        let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_tool_calls WHERE status='unknown' AND (json_extract(target_json,'$.connection_id')=json_extract(?1,'$.connection_id') OR target_json=?1))",[&turn.target_json],|r|r.get(0))
            .map_err(|e|reject(&format!("无法检查未知执行状态：{e}")))?;
        if conflict {
            return Err(reject("目标存在尚未核实的未知执行结果，拒绝冲突写入"));
        }
    }
    let hash = plan_hash(
        &spec.id,
        spec.version,
        &turn.target_json,
        &prepared.to_json_string(),
        permission.plugin_digest.as_deref(),
    );
    if call.tool_version != spec.version
        || call.target_json != turn.target_json
        || hash != call.plan_hash
    {
        return Err(reject("参数、目标或工具版本已变化，请重新核实并重新审批"));
    }
    let outcome = audit_or_fail_closed(
        audit,
        &AuditInput {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            plan_hash: hash.clone(),
            target_json: turn.target_json.clone(),
            normalized_arguments_json: prepared.to_json_string(),
            ruleset_version: "kernel-v1".into(),
        },
    );
    if matches!(
        authorize(&AuthorizeInput {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            risk: spec.risk,
            target_json: turn.target_json.clone(),
            normalized_arguments_json: prepared.to_json_string(),
            plugin_digest: permission.plugin_digest.clone(),
            agent_allowed_tools: permission.agent_allowed_tools.clone(),
            policy: permission.policy,
            audit: Some(outcome),
        }),
        PermissionDecision::Deny { .. }
    ) {
        return Err(reject("当前安全审计或权限拒绝恢复执行"));
    }
    if fluxdb_storage::ai_platform::audits::find_valid_approval(&tx, ai_chat_now(), call_id, &hash)?
        .is_none()
    {
        return Err(reject("批准与当前 plan_hash 不匹配或已过期，拒绝执行"));
    }
    let message =
        fluxdb_storage::ai_platform::sessions::find_assistant_message_for_turn(&tx, &call.turn_id)?
            .ok_or_else(|| reject("该任务还没有助手消息"))?;
    fluxdb_storage::ai_platform::sessions::finish_tool_call(
        &tx, call_id, "running", None, None, None,
    )?;
    fluxdb_storage::ai_platform::audits::invalidate_approval(
        &tx,
        ai_chat_now(),
        approval_id,
        "consumed",
    )?;
    tx.commit()
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    Ok((call, message))
}

impl AppController {
    /// 决定、调用状态和原审批卡必须同事务成功；未知/终态不允许留下部分批准。
    fn decide_ai_approval_atomic(&mut self, id: &str, approved: bool, revision: i64) -> AppEvent {
        let result=self.ai_storage().and_then(|storage| {
            let mut conn=storage.open_ai_connection()?;
            let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,e.to_string()))?;
            let reject=|msg:&str|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,msg);
            let approval=fluxdb_storage::ai_platform::audits::load_approval(&tx,id)?;
            let call_id=approval.tool_call_id.as_deref().ok_or_else(||reject("该审批没有绑定工具调用"))?;
            let call=fluxdb_storage::ai_platform::sessions::load_tool_call(&tx,call_id)?;
            let turn=fluxdb_storage::ai_platform::sessions::load_turn(&tx,&call.turn_id)?;
            if turn.status.is_terminal() || call.status!="waiting_approval" || call.receipt_json.is_some() {
                return Err(reject("该任务已结束或执行结果未知，不能再批准或重放"));
            }
            if approval.expires_at_ms.is_some_and(|t|t<=ai_chat_now()) {return Err(reject("审批已过期，请重新核实"));}
            let message=fluxdb_storage::ai_platform::sessions::find_assistant_message_for_turn(&tx,&call.turn_id)?
                .ok_or_else(||reject("该任务还没有助手消息"))?;
            let decided=fluxdb_storage::ai_platform::audits::decide_approval(&tx,ai_chat_now(),id,approved,None,revision)?;
            let (decision,text)=if approved {("approved","已批准；宿主会按 plan_hash 复核后执行一次。")}
                else {("denied","你已拒绝该工具调用，未执行任何操作。")};
            fluxdb_storage::ai_platform::conversation::upsert_event_part_in_transaction(&tx,ai_chat_now(),&call.session_id,&call.turn_id,
                &message,&format!("{message}-approval-{}",call.provider_call_id),
                fluxdb_storage::ai_platform::sessions::PartKind::Notice,fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                &serde_json::json!({"text":text,"approval_id":id,"decision":decision,"plan_hash":decided.plan_hash}).to_string(),Some(call_id),None)?;
            if !approved {
                fluxdb_storage::ai_platform::sessions::finish_tool_call(&tx,call_id,"denied",None,None,None)?;
                fluxdb_storage::ai_platform::sessions::update_turn_status(&tx,ai_chat_now(),&call.turn_id,
                    fluxdb_storage::ai_platform::sessions::TurnStatus::Interrupted,Some("approval_denied"),turn.revision)?;
                let notice=format!("{}-restart-notice",call.turn_id);
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_message_parts WHERE id=?1)",[&notice],|r|r.get(0)).map_err(|e|reject(&e.to_string()))?;
                if exists {
                    let revision:i64=tx.query_row("SELECT revision FROM ai_message_parts WHERE id=?1",[&notice],|r|r.get(0)).map_err(|e|reject(&e.to_string()))?;
                    fluxdb_storage::ai_platform::sessions::update_part(&tx,ai_chat_now(),&notice,
                        fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                        &serde_json::json!({"text":"重启后的调用已被你拒绝，没有执行；历史记录和草稿仍保留。","recovery_status":"interrupted"}).to_string(),revision)?;
                }

            }
            let view=fluxdb_storage::ai_platform::conversation::load(&tx,Some(&call.session_id)).map(ai_chat_project)?;
            tx.commit().map_err(|e|reject(&e.to_string()))?;
            Ok(view)
        });
        match result {
            Ok(view) => {
                self.state.ai_conversation = view.clone();
                AppEvent::AiConversationLoaded(view)
            }
            Err(e) => AppEvent::AiSettingsError(e.message),
        }
    }
}
