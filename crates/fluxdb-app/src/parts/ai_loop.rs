// 主循环与工具调用状态机（设计 §10–11，T018）。
//
// 一轮 Turn 的控制流：
//   prepare（吸收 steering）→ stream → 组装（文本/工具调用/结束原因）
//   → 截断则不执行工具（参数可能不完整）
//   → 逐个校验工具请求（未知工具/非法 JSON 返回结构化错误给模型）
//   → 执行（真实回执）→ 回执作为 tool 消息进入下一轮
//   → 直到没有工具调用（Completed）或命中限制（Paused）
//
// 明确不做的事：
// - 不做固定意图路由：循环只把模型给出的工具请求当请求，不按关键词预判流程；
// - 不要求 Jev：只依赖 `fluxdb_core::GenerationProvider`，没有决策模型也能完整跑完；
// - 工具**顺序**执行（审批/回执配对/取消语义优先），不做并行。

use std::collections::HashMap;

/// 运行限制。
#[derive(Clone, Copy, Debug)]
pub struct LoopLimits {
    /// 一个 Turn 内最多几次模型迭代。
    pub max_iterations: usize,
    /// 同一 (工具, 参数) 重复到该次数即判定无进展并暂停。
    pub max_repeat_signature: usize,
}

impl Default for LoopLimits {
    fn default() -> Self {
        Self {
            // 与 Agent 配置默认值同源，避免两处各写一个数字而漂移。
            max_iterations: fluxdb_storage::ai_platform::agents::DEFAULT_MAX_ITERATIONS as usize,
            max_repeat_signature: 3,
        }
    }
}

/// Turn 的输入状态（消息历史 + 目标模型）。
#[derive(Clone, Debug)]
pub struct TurnState {
    pub session_id: String,
    pub turn_id: String,
    pub model: String,
    pub messages: Vec<fluxdb_core::ModelMessage>,
    pub max_output_tokens: Option<i64>,
    /// Turn 开始即固定的执行目标（T021 的 `TargetRef::to_json()`）。
    ///
    /// 参与 plan hash：目标变化 → 旧审批/审计自然失效。空串表示"本轮无绑定目标"。
    pub target_json: String,
}

/// 一轮 Turn 的权限上下文（Agent 白名单 + 目标/内核权限）。
#[derive(Clone, Debug, Default)]
pub struct TurnPermissionContext {
    /// Agent 允许的工具（空 = 不限制）。
    pub agent_allowed_tools: Vec<String>,
    /// 全局停用的工具：与 Agent 白名单求交集前先被拒。
    ///
    /// 工具目录已按同一集合过滤，这里再挡一次——恢复执行的旧 Turn 快照可能
    /// 早于用户关闭工具，白名单里仍留着该 id 时不得因为"快照里有"就放行。
    pub disabled_tools: std::collections::BTreeSet<String>,
    /// 当前目标的连接/内核权限（写/管理默认关闭）。
    pub policy: KernelPolicy,
    /// 工具/插件摘要，参与 plan hash。
    pub plugin_digest: Option<String>,
}

/// 主循环的领域事件；执行方将事实持久化，再投影成会话更新。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopEvent {
    IterationStarted { iteration: usize },
    /// 文本增量（UI 合并展示）。
    TextDelta(String),
    /// 公开推理摘要增量。
    ReasoningSummaryDelta(String),
    /// 模型请求调用工具（尚未校验/授权；UI 用它展示"打算调用什么"）。
    ToolCallRequested(fluxdb_core::ToolCallRequest),
    /// 工具开始执行（此时 schema 校验、审计与授权都已完成）。
    ToolStarted {
        call: fluxdb_core::ToolCallRequest,
        plan_hash: String,
        tool_version: u32,
    },
    /// 工具真实回执。
    ToolFinished { receipt: fluxdb_core::ToolReceipt },
    /// 工具执行中的有界进度摘要。
    ToolProgress {
        call_id: String,
        progress: fluxdb_core::ToolProgressEvent,
    },
    /// 需要用户审批：**不执行**、不产生工具回执，等审批决定后继续（设计 §25.3）。
    ApprovalRequired {
        call: fluxdb_core::ToolCallRequest,
        tool_version: u32,
        plan_hash: String,
        scope: ApprovalScope,
        /// 审批卡要展示给用户的计划事实（SQL 原文、护栏命中、影响预估依据）。
        /// 只是内核已经算出的结论的投影，不参与授权判定，也不能改写放行结果。
        details: ApprovalDetails,
    },
    /// 给用户/模型的可见说明（未知工具、截断未执行、无进展暂停等）。
    Notice(String),
}

/// Turn 的终态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// 模型给出了最终回答（本轮没有工具调用）。
    Completed { text: String },
    /// 安全暂停：达到迭代上限或无进展，需要用户介入。
    Paused { reason: String },
    /// 基础设施失败（Provider/工具返回 Err），不是普通工具错误。
    Failed { message: String },
    Cancelled,
}

/// 主循环执行器。
///
/// 工具不再"直接调用"：每次调用都必须经过 `ToolCatalog::prepare`（schema 校验）→
/// 强制安全审计 → 内核授权（风险/白名单/连接权限求交集）→ 执行 → `project`（有界投影）。
pub struct TurnRunner<'a> {
    pub provider: &'a dyn fluxdb_core::GenerationProvider,
    pub tools: &'a ToolCatalog,
    pub limits: LoopLimits,
    pub permission: TurnPermissionContext,
    /// 强制安全审计 Hook（不可禁用；审计失败 fail closed）。
    pub audit: &'a dyn SecurityAuditHook,
}

/// `ToolSpec` → Provider 可见的 `ToolDescriptor`（只暴露描述与 schema）。
fn loop_tool_descriptors(catalog: &ToolCatalog) -> Vec<fluxdb_core::ToolDescriptor> {
    let read_only=catalog.security.as_ref().is_some_and(|s|
        s.settings().map(|(p,_)|p.tools_default==ExecutionPolicy::ReadOnly).unwrap_or(true));
    catalog
        .specs()
        .iter()
        .filter(|spec| !read_only || spec.risk==fluxdb_core::ToolRisk::Read)
        .map(|spec| fluxdb_core::ToolDescriptor {
            id: spec.id.clone(),
            version: spec.version,
            description: spec.description.clone(),
            risk: spec.risk,
            input_schema_json: spec.input_schema.to_string(),
        })
        .collect()
}

fn loop_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 内核合成的拒绝回执（不是工具产出，但同样是不可改写的事实）。
fn loop_denied_receipt(
    call: &fluxdb_core::ToolCallRequest,
    code: &str,
    reason: &str,
    plan_hash: &str,
    started_at_ms: i64,
) -> fluxdb_core::ToolReceipt {
    fluxdb_core::ToolReceipt {
        call_id: call.provider_call_id.clone(),
        tool_id: call.tool_id.clone(),
        status: fluxdb_core::ToolReceiptStatus::Denied,
        result_json: serde_json::json!({
            "ok": false,
            "code": code,
            "message": reason,
            "retryable": false,
            // 内核拒绝也要留下 plan_hash：宿主据此落审计事实行（拒绝不是"没有发生过"）。
            "plan_hash": plan_hash,
        })
        .to_string(),
        error_code: Some(code.to_string()),
        affected_rows: None,
        started_at_ms,
        finished_at_ms: loop_now_ms(),
    }
}

fn loop_tool_status_label(status: fluxdb_core::ToolReceiptStatus) -> &'static str {
    match status {
        fluxdb_core::ToolReceiptStatus::Succeeded => "succeeded",
        fluxdb_core::ToolReceiptStatus::Failed => "failed",
        fluxdb_core::ToolReceiptStatus::Denied => "denied",
        fluxdb_core::ToolReceiptStatus::Cancelled => "cancelled",
        fluxdb_core::ToolReceiptStatus::Unknown => "unknown",
    }
}

impl<'a> TurnRunner<'a> {
    pub fn run(
        &self,
        state: TurnState,
        steering: &mut Vec<String>,
        cancel: &fluxdb_core::CancelFlag,
        emit: &mut dyn FnMut(LoopEvent),
    ) -> TurnOutcome {
        self.run_with_steering(state, cancel, &mut || std::mem::take(steering), emit)
    }

    /// 补充在每次模型请求前消费；不改变目标或绕过工具授权。
    fn run_with_steering(
        &self,
        mut state: TurnState,
        cancel: &fluxdb_core::CancelFlag,
        steering: &mut dyn FnMut() -> Vec<String>,
        emit: &mut dyn FnMut(LoopEvent),
    ) -> TurnOutcome {
        let mut signature_counts: HashMap<String, usize> = HashMap::new();
        let mut last_text = String::new();

        for iteration in 1..=self.limits.max_iterations {
            if cancel.is_cancelled() {
                return TurnOutcome::Cancelled;
            }
            // 安全边界：把用户补充吸收进消息历史（不改执行目标）。
            for text in steering() {
                state.messages.push(fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::User, text));
            }
            emit(LoopEvent::IterationStarted { iteration });

            let messages = match bounded_tool_history(&state.messages) {
                Ok(messages) => messages,
                Err(reason) => {
                    emit(LoopEvent::Notice(reason.clone()));
                    return TurnOutcome::Paused { reason };
                }
            };
            let request = fluxdb_core::ModelRequest {
                model: state.model.clone(),
                messages,
                tools: loop_tool_descriptors(self.tools),
                max_output_tokens: state.max_output_tokens,
            };
            // Provider 缓冲整段事件后再返回，失败尝试没有可见副作用，可按错误类别重发。
            let events = match retry_model_call(cancel, || {
                block_on_ready(self.provider.stream(request.clone(), cancel.clone()))
                    .and_then(|result| result)
            }) {
                Ok(events) => events,
                Err(error) => {
                    return TurnOutcome::Failed {
                        message: error.message,
                    };
                }
            };

            // 逐事件消费：取消在事件之间即刻生效，流式输出不会"取消后继续刷"。
            let mut text = String::new();
            let mut tool_calls: Vec<fluxdb_core::ToolCallRequest> = Vec::new();
            let mut finish: Option<fluxdb_core::FinishReason> = None;
            for event in events {
                if cancel.is_cancelled() {
                    return TurnOutcome::Cancelled;
                }
                match event {
                    fluxdb_core::ModelEvent::TextDelta(delta) => {
                        text.push_str(&delta);
                        emit(LoopEvent::TextDelta(delta));
                    }
                    fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta) => {
                        emit(LoopEvent::ReasoningSummaryDelta(delta));
                    }
                    fluxdb_core::ModelEvent::ToolCallRequested(call) => {
                        emit(LoopEvent::ToolCallRequested(call.clone()));
                        tool_calls.push(call);
                    }
                    fluxdb_core::ModelEvent::Usage(_) => {
                        // usage 由统一计量宿主记入会话 token 账本，不丢弃也不伪造。
                    }
                    fluxdb_core::ModelEvent::Finished(reason) => finish = Some(reason),
                }
            }
            if cancel.is_cancelled() {
                return TurnOutcome::Cancelled;
            }
            if !text.is_empty() || !tool_calls.is_empty() {
                last_text = text.clone();
                // assistant 回合必须把本回合的工具调用一起记住：无状态协议（Responses/Chat）
                // 需要重放 function_call 才能与下一轮的 function_call_output 配对。
                let calls = tool_calls
                    .iter()
                    .map(|call| fluxdb_core::ModelToolCall {
                        provider_call_id: call.provider_call_id.clone(),
                        tool_id: call.tool_id.clone(),
                        arguments_json: call.arguments_json.clone(),
                    })
                    .collect::<Vec<_>>();
                state.messages.push(fluxdb_core::ModelMessage::assistant_with_tool_calls(
                    text.clone(),
                    calls,
                ));
            }
            match finish {
                Some(fluxdb_core::FinishReason::Error) => {
                    return TurnOutcome::Failed {
                        message: "模型返回错误结束".to_string(),
                    };
                }
                Some(fluxdb_core::FinishReason::Cancelled) => return TurnOutcome::Cancelled,
                _ => {}
            }
            if tool_calls.is_empty() {
                // 纯文本回合也可能被长度截断：给用户/UI 一个可见提示，
                // 不要静默当成正常结束（finish_reason 由 Provider 给出）。
                if finish == Some(fluxdb_core::FinishReason::Length) {
                    emit(LoopEvent::Notice(
                        "回答达到长度上限，内容可能被截断。".to_string(),
                    ));
                }
                return TurnOutcome::Completed { text: last_text };
            }

            // 输出被截断：工具参数可能不完整，**不执行**，把结构化说明回给模型让它修正。
            if finish == Some(fluxdb_core::FinishReason::Length) {
                for call in &tool_calls {
                    let notice = format!(
                        "工具 {} 未执行：本次输出被截断（finish_reason=length），参数可能不完整，请重新请求。",
                        call.tool_id
                    );
                    emit(LoopEvent::Notice(notice.clone()));
                    state.messages.push(fluxdb_core::ModelMessage::tool_result(
                        call.provider_call_id.clone(),
                        call.tool_id.clone(),
                        format!("{{\"error\":\"truncated\",\"tool\":\"{}\"}}", call.tool_id),
                    ));
                }
                continue;
            }

            for call in tool_calls {
                if cancel.is_cancelled() {
                    return TurnOutcome::Cancelled;
                }
                // 参数必须是合法 JSON：不做"猜参数"的修复。
                if serde_json::from_str::<serde_json::Value>(&call.arguments_json).is_err() {
                    let notice = format!(
                        "工具 {} 未执行：参数不是合法 JSON，请修正后重试。",
                        call.tool_id
                    );
                    emit(LoopEvent::Notice(notice.clone()));
                    state.messages.push(fluxdb_core::ModelMessage::tool_result(
                        call.provider_call_id.clone(),
                        call.tool_id.clone(),
                        format!("{{\"error\":\"invalid_json\",\"tool\":\"{}\"}}", call.tool_id),
                    ));
                    continue;
                }
                // 1) 先校验 schema：未知工具 / 非法参数都是受控错误，不进入执行。
                let Some(spec) = self.tools.describe(&call.tool_id).cloned() else {
                    let notice = format!("工具 {} 未注册，已拒绝执行。", call.tool_id);
                    emit(LoopEvent::Notice(notice.clone()));
                    state.messages.push(fluxdb_core::ModelMessage::tool_result(
                        call.provider_call_id.clone(),
                        call.tool_id.clone(),
                        format!(
                            "{{\"error\":\"unknown_tool\",\"tool\":\"{}\"}}",
                            call.tool_id
                        ),
                    ));
                    continue;
                };
                if let Err(error) = self.tools.prepare(&call.tool_id, &call.arguments_json) {
                        let notice = format!(
                            "工具 {} 未执行：参数未通过校验（{}）。",
                            call.tool_id, error.code
                        );
                        emit(LoopEvent::Notice(notice));
                        state.messages.push(fluxdb_core::ModelMessage::tool_result(
                            call.provider_call_id.clone(),
                            call.tool_id.clone(),
                            format!(
                                "{{\"tool\":\"{}\",\"status\":\"failed\",\"error\":\"{}\",\"message\":{}}}",
                                call.tool_id,
                                error.code,
                                serde_json::Value::String(error.message).to_string()
                            ),
                        ));
                        continue;
                }
                // 1b) before-tool Hook：只在初始 schema 校验通过后才允许补丁；
                // 补丁不能改变执行目标，且必须重新 schema/风险/审批校验（下方 shadow）。
                let original_target = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
                    .ok()
                    .and_then(|value| value.get("target").cloned());
                let original_call_id = call.provider_call_id.clone();
                let (call, hook_deny, hook_approval) = match self.tools.hooks.before(&call, cancel) {
                    Ok(value) => value,
                    Err(error) => return TurnOutcome::Failed { message: error.message },
                };
                let final_target = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
                    .ok()
                    .and_then(|value| value.get("target").cloned());
                if original_target != final_target {
                    return TurnOutcome::Failed {
                        message: "Hook 不得改变执行目标".into(),
                    };
                }
                // 无状态协议重放必须使用最终参数，否则 assistant 请求与工具事实不一致。
                for message in state.messages.iter_mut().rev().take(1) {
                    if let Some(original) = message
                        .tool_calls
                        .iter_mut()
                        .find(|c| c.provider_call_id == original_call_id)
                    {
                        original.arguments_json = call.arguments_json.clone();
                    }
                }
                let prepared = match self.tools.prepare(&call.tool_id, &call.arguments_json) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let notice = format!(
                            "工具 {} 未执行：Hook 补丁未通过校验（{}）。",
                            call.tool_id, error.code
                        );
                        emit(LoopEvent::Notice(notice));
                        state.messages.push(fluxdb_core::ModelMessage::tool_result(
                            call.provider_call_id.clone(),
                            call.tool_id.clone(),
                            format!(
                                "{{\"tool\":\"{}\",\"status\":\"failed\",\"error\":\"{}\",\"message\":{}}}",
                                call.tool_id,
                                error.code,
                                serde_json::Value::String(error.message).to_string()
                            ),
                        ));
                        continue;
                    }
                };

                // 2) 无进展检测：相同工具 + 相同参数反复出现。
                let signature = format!("{}|{}", call.tool_id, call.arguments_json);
                let count = signature_counts.entry(signature).or_insert(0);
                *count += 1;
                if *count >= self.limits.max_repeat_signature {
                    let reason = format!(
                        "检测到相同工具与参数重复 {} 次且无进展，已安全暂停；可调整目标或参数后继续。",
                        count
                    );
                    emit(LoopEvent::Notice(reason.clone()));
                    return TurnOutcome::Paused { reason };
                }

                // 3) 强制安全审计（fail closed）→ 内核授权（deny 优先）。
                let hash = plan_hash(
                    &spec.id,
                    spec.version,
                    &state.target_json,
                    &prepared.to_json_string(),
                    self.permission.plugin_digest.as_deref(),
                );
                let mut audit = audit_or_fail_closed(
                    self.audit,
                    &AuditInput {
                        tool_id: spec.id.clone(),
                        tool_version: spec.version,
                        plan_hash: hash.clone(),
                        target_json: state.target_json.clone(),
                        normalized_arguments_json: prepared.to_json_string(),
                        ruleset_version: "kernel-v1".to_string(),
                    },
                );
                if let Some(reason)=hook_deny {emit(LoopEvent::Notice(reason));audit=AuditOutcome::Deny;}
                else if let Some(reason)=hook_approval {
                    if audit!=AuditOutcome::Deny {emit(LoopEvent::Notice(reason));audit=AuditOutcome::RequireApproval;}
                }
                let input = AuthorizeInput {
                    tool_id: spec.id.clone(),
                    tool_version: spec.version,
                    risk: spec.risk,
                    target_json: state.target_json.clone(),
                    normalized_arguments_json: prepared.to_json_string(),
                    plugin_digest: self.permission.plugin_digest.clone(),
                    globally_disabled_tools: self.permission.disabled_tools.clone(),
                    agent_allowed_tools: self.permission.agent_allowed_tools.clone(),
                    policy: self.permission.policy,
                    audit: Some(audit),
                };
                let mut automatic_approval=false;
                let decision=if let Some(security)=&self.tools.security {
                    let policy=match security.settings() {
                        Ok((p,_))=>p.tools_default,
                        Err(error)=>return TurnOutcome::Failed{message:error.message},
                    };
                    match security.authorize(&input,policy,||{
                        self.tools.executable(&call.tool_id).ok_or_else(||
                            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Unsupported,"工具不可用")
                        )?.automatic_approval_facts(&prepared.to_json_string())
                    },cancel) {
                        Ok(result)=>{
                            automatic_approval=result.automatic_approval;
                            if spec.risk!=fluxdb_core::ToolRisk::Read {
                                emit(LoopEvent::Notice(result.reason.clone()));
                            }
                            // 审批事实先落盘，失败时不得发出写入。
                            if let Err(error)=with_ai_storage(&security.storage,|conn|
                                fluxdb_storage::ai_platform::run_events::append(conn,&state.turn_id,"execution_policy",
                                &serde_json::json!({"tool":spec.id,"plan_hash":hash,"policy":policy,
                                    "reason":result.reason,"automatic_approval":automatic_approval,
                                    "settings_revision":result.settings_revision}).to_string(),ai_chat_now()).map(|_|()))
                            {return TurnOutcome::Failed{message:error.message};}
                            result.decision
                        },
                        Err(error)=>return TurnOutcome::Failed{message:error.message},
                    }
                } else {authorize(&input)};

                match decision {
                    // 4a) 拒绝：合成 Denied 回执回给模型，永不执行。
                    PermissionDecision::Deny { code, reason, .. } => {
                        let receipt = loop_denied_receipt(&call, code, &reason, &hash, loop_now_ms());
                        emit(LoopEvent::ToolFinished {
                            receipt: receipt.clone(),
                        });
                        state.messages.push(fluxdb_core::ModelMessage::tool_result(
                            receipt.call_id.clone(),
                            receipt.tool_id.clone(),
                            format!(
                                "{{\"tool\":\"{}\",\"status\":\"{}\",\"result\":{}}}",
                                receipt.tool_id,
                                loop_tool_status_label(receipt.status),
                                receipt.result_json
                            ),
                        ));
                        continue;
                    }
                    // 4b) 需要审批：不执行、不回执，暂停等用户决定（审批绑定 plan_hash）。
                    PermissionDecision::RequireApproval { plan_hash, scope } => {
                        let reason = format!("工具 {} 需要审批后才能执行。", spec.id);
                        // 审批展示事实由工具自己按入参给出（各自的分类器），默认按高风险兜底。
                        let details = self
                            .tools
                            .executable(&call.tool_id)
                            .map(|tool| tool.approval_details(&prepared.to_json_string()))
                            .unwrap_or_else(|| fluxdb_core::ApprovalDetails {
                                risk: fluxdb_core::ApprovalRisk::High,
                                guardrails: vec!["计划风险尚未核实".to_string()],
                                impact_basis: "未核实".to_string(),
                                ..Default::default()
                            });
                        emit(LoopEvent::ApprovalRequired {
                            call: call.clone(),
                            tool_version: spec.version,
                            plan_hash,
                            scope,
                            details,
                        });
                        return TurnOutcome::Paused { reason };
                    }
                    // 4c) 放行：必须有可执行实现（只有 spec 没有实现 = 受控错误）。
                    PermissionDecision::Allow { .. } => {}
                }
                let Some(tool) = self.tools.executable(&call.tool_id) else {
                    let notice = format!("工具 {} 没有可执行实现，已拒绝执行。", call.tool_id);
                    emit(LoopEvent::Notice(notice));
                    state.messages.push(fluxdb_core::ModelMessage::tool_result(
                        call.provider_call_id.clone(),
                        call.tool_id.clone(),
                        format!(
                            "{{\"error\":\"tool_not_executable\",\"tool\":\"{}\"}}",
                            call.tool_id
                        ),
                    ));
                    continue;
                };

                emit(LoopEvent::ToolStarted {
                    call: call.clone(),
                    plan_hash: hash.clone(),
                    tool_version: spec.version,
                });
                let progress_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let ctx = fluxdb_core::ToolExecutionContext {
                    automatic_approval,
                    session_id: state.session_id.clone(),
                    turn_id: state.turn_id.clone(),
                    iteration: iteration as i64,
                    call_id: call.provider_call_id.clone(),
                    cancel: cancel.clone(),
                    progress: Some(progress_events.clone()),
                };
                let receipt = match block_on_ready(tool.execute(ctx, prepared.to_json_string())) {
                    Ok(Ok(receipt)) => receipt,
                    Ok(Err(error)) | Err(error) => {
                        // 基础设施失败：退出整个任务（普通工具错误走 Failed 回执分支）。
                        return TurnOutcome::Failed {
                            message: error.message,
                        };
                    }
                };
                if receipt.call_id != call.provider_call_id || receipt.tool_id != call.tool_id {
                    let message = "工具回执归属与已授权调用不符；执行结果不可确认，禁止自动重放";
                    tracing::warn!(tool_id=%call.tool_id, "拒绝登记归属不符的工具回执");
                    emit(LoopEvent::ToolFinished {
                        receipt: fluxdb_core::ToolReceipt {
                            call_id: call.provider_call_id.clone(),
                            tool_id: call.tool_id.clone(),
                            status: fluxdb_core::ToolReceiptStatus::Unknown,
                            result_json: serde_json::json!({"ok":false,"code":"receipt_identity_mismatch","message":message,"retryable":false}).to_string(),
                            error_code: Some("receipt_identity_mismatch".into()),
                            affected_rows: None,
                            started_at_ms: loop_now_ms(),
                            finished_at_ms: loop_now_ms(),
                        },
                    });
                    return TurnOutcome::Failed { message: message.into() };
                }
                let progress = progress_events
                    .lock()
                    .map(|events| events.clone())
                    .unwrap_or_default();
                for event in progress {
                    emit(LoopEvent::ToolProgress {
                        call_id: call.provider_call_id.clone(),
                        progress: event,
                    });
                }
                // 5) 回给模型的是**该工具有界投影**，原始回执保持不变。
                let projection = match self.tools.project(&receipt.tool_id, &receipt, None) {
                    Ok(projection) => projection,
                    Err(error) => ModelProjection {
                        text: format!(
                            "{{\"error\":\"{}\",\"message\":{}}}",
                            error.code,
                            serde_json::Value::String(error.message).to_string()
                        ),
                        truncated: true,
                        result_ref: None,
                    },
                };
                // 快照先生成，落库才能登记非 SQL 输出的真实引用与 spill 事实。
                emit(LoopEvent::ToolFinished {
                    receipt: receipt.clone(),
                });
                if receipt.status==fluxdb_core::ToolReceiptStatus::Unknown {
                    return TurnOutcome::Paused {reason:"写入结果未知，已停止后续工具调用，请人工核实；不会自动重试".into()};
                }
                if let Err(error)=self.tools.hooks.after(&receipt,cancel) {
                    return TurnOutcome::Failed{message:error.message};
                }
                state.messages.push(fluxdb_core::ModelMessage::tool_result(
                    receipt.call_id.clone(),
                    receipt.tool_id.clone(),
                    serde_json::json!({
                        "tool":receipt.tool_id,
                        "status":loop_tool_status_label(receipt.status),
                        "truncated":projection.truncated,
                        "result":serde_json::from_str::<serde_json::Value>(&projection.text)
                            .unwrap_or(serde_json::Value::Null),
                    }).to_string(),
                ));
            }
        }

        let reason = format!(
            "达到最大迭代次数 {}，已安全暂停。",
            self.limits.max_iterations
        );
        emit(LoopEvent::Notice(reason.clone()));
        TurnOutcome::Paused { reason }
    }
}
