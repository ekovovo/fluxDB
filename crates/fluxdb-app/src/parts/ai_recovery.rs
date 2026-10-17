// T029：宿主启动恢复。只扫描存储，不构造 Provider/Connector，不自动发任何外部请求。
impl AppController {
    fn recover_ai_on_startup(&mut self) -> AppEvent {
        let result = self.ai_storage().and_then(|storage| {
            let mut conn = storage.open_ai_connection()?;
            let summary = fluxdb_storage::ai_platform::recovery::recover_on_startup(
                &mut conn,
                ai_chat_now(),
            )?;
            // 过期保留结果的 spill 文件已无归属，按受控根内相对路径删除；
            // 删不掉只记日志：恢复扫描不能因为清理失败而整体失败。
            let spill_root = storage.root_dir().join("ai-results");
            for relative_path in &summary.retained_paths_to_purge {
                let path = spill_root.join(relative_path);
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            path=%path.display(),
                            %error,
                            "删除过期保留结果文件失败，留待下次清理"
                        );
                    }
                }
            }
            // T050：staged 产物崩溃对账——文件存在且字节/指纹吻合就提升 ready（幂等、不丢已完成文件）；
            // 文件缺失/指纹不符/length 不符则置 failed 并清空落点，避免留下悬挂引用。
            reconcile_staged_artifacts(&storage, &summary.staged_artifacts);
            tracing::info!(
                staged = summary.staged_artifacts.len(),
                interrupted = summary.interrupted_turns,
                unknown = summary.unknown_turns,
                waiting_approval = summary.waiting_approval_turns,
                expired_results = summary.expired_results,
                purged = summary.retained_paths_to_purge.len(),
                "AI 启动恢复扫描完成（未重放工具）"
            );
            // 设计 §18.1：启动时统一 reconcile 内置能力（幂等建立 Jev 的 system 绑定）。
            // 失败只记日志：Jev 是可选优化，不能让启动恢复整体失败。
            match reconcile_jev_builtins(&conn, ai_chat_now()) {
                Ok(status) => {
                    tracing::info!(
                        status = status.kind.as_str(),
                        model_profile_id = status.model_profile_id.as_deref().unwrap_or(""),
                        "内置 Jev 装配完成"
                    );
                }
                Err(error) => {
                    tracing::warn!(reason = %error.message, "内置 Jev 装配失败，继续按基础策略运行");
                }
            }
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
) -> fluxdb_core::Result<(
    fluxdb_storage::ai_platform::sessions::ToolCallView,
    String,
    AuditFacts,
)> {
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
    // 复用当前事务读取执行策略：在 IMMEDIATE 事务内另开连接会直接撞上库锁。
    let (execution_policy, _) = fluxdb_storage::ai_platform::execution_policy::load(&tx)?;
    if execution_policy.tools_default == ExecutionPolicy::ReadOnly
        && spec.risk != fluxdb_core::ToolRisk::Read
    {
        return Err(reject("执行策略已改为只读，旧审批不得放行"));
    }
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
    let mut facts = audit_or_fail_closed(
        audit,
        &AuditInput {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            plan_hash: hash.clone(),
            target_json: turn.target_json.clone(),
            normalized_arguments_json: prepared.to_json_string(),
            ruleset_version: "kernel-audit-v1".into(),
        },
    );
    facts.risk = spec.risk.to_string();
    if matches!(
        authorize(&AuthorizeInput {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            risk: spec.risk,
            target_json: turn.target_json.clone(),
            normalized_arguments_json: prepared.to_json_string(),
            plugin_digest: permission.plugin_digest.clone(),
            globally_disabled_tools: permission.disabled_tools.clone(),
            agent_allowed_tools: permission.agent_allowed_tools.clone(),
            policy: permission.policy,
            audit: Some(facts.outcome),
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
    // 审批恢复是第二次审计边界：旧审批可能对应旧规则，这里按当前规则再判一次并留痕。
    if !fluxdb_storage::ai_platform::audits::has_tool_audit(
        &tx,
        call_id,
        fluxdb_storage::ai_platform::audits::AuditStage::Revalidate,
    )? {
        let edit = fluxdb_storage::ai_platform::audits::ToolAuditEdit {
            id: format!("{call_id}-audit-revalidate"),
            tool_call_id: Some(call_id.to_string()),
            mcp_request_id: None,
            stage: fluxdb_storage::ai_platform::audits::AuditStage::Revalidate,
            hook_id: "kernel.security-audit".into(),
            hook_version: "1".into(),
            ruleset_version: facts.ruleset_version.clone(),
            plan_hash: hash.clone(),
            decision: match facts.outcome {
                AuditOutcome::Allow => fluxdb_storage::ai_platform::audits::AuditDecision::Allow,
                AuditOutcome::RequireApproval => {
                    fluxdb_storage::ai_platform::audits::AuditDecision::RequireApproval
                }
                AuditOutcome::Deny => fluxdb_storage::ai_platform::audits::AuditDecision::Deny,
            },
            risk: facts.risk.clone(),
            rule_ids: facts.rule_ids.clone(),
            reason_codes: facts.reason_codes.clone(),
        };
        fluxdb_storage::ai_platform::audits::insert_tool_audit(&tx, ai_chat_now(), &edit)?;
    }
    fluxdb_storage::ai_platform::audits::invalidate_approval(
        &tx,
        ai_chat_now(),
        approval_id,
        "consumed",
    )?;
    tx.commit()
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    Ok((call, message, facts))
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
            let part_id = format!("{message}-approval-{}",call.provider_call_id);
            let mut payload = serde_json::json!({});
            // 保留原计划与正文位置，决定后仍可核对当时批准或拒绝的内容。
            if let Some(part) = fluxdb_storage::ai_platform::sessions::list_parts(&tx,&message)?
                .into_iter().find(|part| part.id == part_id) {
                payload = serde_json::from_str::<serde_json::Value>(&part.payload_json)
                    .ok().filter(serde_json::Value::is_object).unwrap_or_else(||serde_json::json!({}));
            }
            payload["text"] = text.into();
            payload["approval_id"] = id.into();
            payload["decision"] = decision.into();
            payload["plan_hash"] = decided.plan_hash.into();
            fluxdb_storage::ai_platform::conversation::upsert_event_part_in_transaction(&tx,ai_chat_now(),&call.session_id,&call.turn_id,
                &message,&part_id,
                fluxdb_storage::ai_platform::sessions::PartKind::Notice,fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                &payload.to_string(),Some(call_id),None)?;
            if !approved {
                fluxdb_storage::ai_platform::sessions::finish_tool_call(&tx,call_id,"denied",None,None,None)?;
                fluxdb_storage::ai_platform::sessions::update_turn_status(&tx,ai_chat_now(),&call.turn_id,
                    fluxdb_storage::ai_platform::sessions::TurnStatus::Interrupted,Some("approval_denied"),turn.revision)?;
                // 旧等待提示可能已被启动恢复标为 interrupted，仍由本次决定替代。
                let paused_id = format!("{message}-notice-paused");
                if let Some(part) = fluxdb_storage::ai_platform::sessions::list_parts(&tx,&message)?
                    .into_iter().find(|part|part.id==paused_id && matches!(part.status,
                        fluxdb_storage::ai_platform::sessions::PartStatus::Pending | fluxdb_storage::ai_platform::sessions::PartStatus::Interrupted)) {
                    fluxdb_storage::ai_platform::sessions::update_part(&tx,ai_chat_now(),&paused_id,
                        fluxdb_storage::ai_platform::sessions::PartStatus::Interrupted,
                        &serde_json::json!({"text":"本次操作已拒绝，未执行；本轮已停止。","approval_id":id,"decision":"denied","superseded_by":part_id}).to_string(),
                        part.revision)?;
                }
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

/// 本轮安全上下文（全部非敏感）：固定目标连接 ID + Agent 允许工具 + 全局停用工具。
///
/// 三者都来自存储而不是"当前界面"：全局停用集合直接决定工具目录下发给
/// 模型的条目，Agent 白名单只能在其之上继续收窄。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnSecurityContext {
    pub connection_id: Option<i64>,
    pub agent_allowed_tools: Vec<String>,
    pub disabled_tools: std::collections::BTreeSet<String>,
}

/// 读取本轮安全上下文。
///
/// 连接与 Agent 白名单来自**本轮固定快照**而不是"当前界面"：否则切标签/换 Agent 会让
/// 旧任务悄悄扩大权限。工具全局停用集合同样在每轮开始时读取，用户新关闭的工具
/// 立即从后续轮次的工具目录中消失。
pub fn ai_turn_security_context(
    storage: &fluxdb_storage::FileStorage,
    turn_id: &str,
) -> fluxdb_core::Result<TurnSecurityContext> {
    with_ai_storage(storage, |conn| {
        let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, turn_id)?;
        let target: serde_json::Value = serde_json::from_str(&turn.target_json).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "任务目标不是合法 JSON")
        })?;
        let connection_id = target
            .get("connection_id")
            .and_then(serde_json::Value::as_i64);
        let session = fluxdb_storage::ai_platform::sessions::load_session(conn, &turn.session_id)?;
        let allowed_tools =
            fluxdb_storage::ai_platform::agents::load_agent(conn, &session.agent_id)
                .map(|agent| agent.allowed_tools)
                .unwrap_or_default();
        let disabled_tools =
            fluxdb_storage::ai_platform::tool_settings::disabled_tool_ids(conn)?;
        Ok(TurnSecurityContext {
            connection_id,
            agent_allowed_tools: allowed_tools,
            disabled_tools,
        })
    })
}

/// 读取某轮固定的目标连接 ID（T021 固定目标；恢复与工具 resolver 都按它构造）。
pub fn ai_turn_connection_id(
    storage: &fluxdb_storage::FileStorage,
    turn_id: &str,
) -> fluxdb_core::Result<Option<i64>> {
    with_ai_storage(storage, |conn| {
        let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, turn_id)?;
        let target: serde_json::Value = serde_json::from_str(&turn.target_json).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "任务目标不是合法 JSON")
        })?;
        Ok(target
            .get("connection_id")
            .and_then(serde_json::Value::as_i64))
    })
}

/// 把固定目标渲染成**工具句柄**（`conn:<id>[/db:<database>][/schema:<schema>]`）。
///
/// 模型必须用这个字符串作为工具的 `target`：只给它 JSON 目标时，它无从得知句柄格式，
/// 只能瞎猜（真实模型实测会直接编一个不存在的 target）。
pub fn ai_tool_target_handle(target_json: &str) -> Option<String> {
    let target: serde_json::Value = serde_json::from_str(target_json).ok()?;
    let id = target
        .get("connection_id")
        .and_then(serde_json::Value::as_i64)?;
    let mut handle = format!("conn:{id}");
    if let Some(database) = target
        .get("database")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        handle.push_str(&format!("/db:{database}"));
    }
    if let Some(schema) = target
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        handle.push_str(&format!("/schema:{schema}"));
    }
    Some(handle)
}

/// staged 产物对账：把「已写文件、尚未提交 ready」的崩溃残留收敛为 ready 或 failed。
///
/// 只读文件、核对字节与内容指纹后推进状态；任何异常都收敛为 failed（不伪造 ready），
/// 并清理落点，保证不会留下“登记了路径但读不到”的悬挂事实。
fn reconcile_staged_artifacts(
    storage: &fluxdb_storage::FileStorage,
    candidates: &[fluxdb_storage::ai_platform::recovery::StagedArtifactCandidate],
) {
    if candidates.is_empty() {
        return;
    }
    let root = storage.root_dir().join("ai-artifacts");
    for candidate in candidates {
        let path = std::path::Path::new(&candidate.relative_path);
        let path_ok = path.components().count() == 1
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| uuid::Uuid::parse_str(stem).is_ok())
            && path.extension() == Some(std::ffi::OsStr::new("json"));
        let ready = path_ok
            && std::fs::read_to_string(root.join(path))
                .ok()
                .is_some_and(|content| {
                    content.len() as i64 == candidate.byte_count
                        && candidate
                            .content_hash
                            .as_deref()
                            .is_some_and(|hash| crate::source_hash(&[content.as_str()]) == hash)
                });
        let result = if ready {
            with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::artifacts::reconcile_staged_artifact(
                    conn,
                    ai_chat_now(),
                    &candidate.id,
                    true,
                )
            })
        } else {
            // 先清落点（此时仍是 staged），再置 failed，避免 failed 后无法再清路径。
            if path_ok {
                let _ = std::fs::remove_file(root.join(path));
            }
            if let Err(error) = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::artifacts::clear_staged_artifact_path(
                    conn,
                    ai_chat_now(),
                    &candidate.id,
                )
            }) {
                tracing::warn!(artifact_id=%candidate.id, %error, "清理 staged 产物落点失败");
            }
            with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::artifacts::reconcile_staged_artifact(
                    conn,
                    ai_chat_now(),
                    &candidate.id,
                    false,
                )
            })
        };
        match result {
            Ok(Some(view)) => tracing::info!(
                artifact_id = %candidate.id,
                status = %view.status,
                "staged 产物对账完成"
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                artifact_id = %candidate.id,
                %error,
                "staged 产物对账失败，保留原状态待下次处理"
            ),
        }
    }
}
