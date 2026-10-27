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

/// 只填筛选条件不构成执行路径选择，允许 AI 后续在页面或 SQL 间继续决定。
fn sql_page_query_is_prefill(arguments_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments_json)
        .is_ok_and(|value| value["execute"] == false)
}

/// 一轮里页面筛选查询与直接 SQL 查询只能二选一；只填条件不算执行。
fn query_path_conflict(
    tool_id: &str,
    arguments_json: &str,
    page_queried: bool,
    sql_queried: bool,
) -> bool {
    (tool_id == "db.run" && page_queried)
        || (tool_id == "page.query_table"
            && !sql_page_query_is_prefill(arguments_json)
            && (page_queried || sql_queried))
}

#[cfg(test)]
mod ai_query_path_tests {
    use super::{query_path_conflict, sql_page_query_is_prefill};

    #[test]
    fn page_and_sql_queries_are_mutually_exclusive_per_turn() {
        let page = r#"{"execute":true,"page_ref":"p"}"#;
        let prefill = r#"{"execute":false,"page_ref":"p"}"#;
        assert!(query_path_conflict("db.run", "{}", true, false));
        assert!(query_path_conflict("page.query_table", page, false, true));
        assert!(query_path_conflict("page.query_table", page, true, false));
        assert!(!query_path_conflict(
            "page.query_table",
            prefill,
            true,
            false
        ));
        assert!(!query_path_conflict("page.query_table", page, false, false));
        assert!(!query_path_conflict("db.run", "{}", false, true));
        assert!(sql_page_query_is_prefill(prefill));
    }
}

/// 运行限制。
#[derive(Clone, Copy, Debug)]
pub struct LoopLimits {
    /// 一个 Turn 内最多几次模型迭代。
    pub max_iterations: usize,
    /// 同一 (工具, 参数) 重复到该次数即判定无进展并暂停。
    pub max_repeat_signature: usize,
}

/// 主循环的执行阶段标签；只用于脱敏耗时与错误汇总，不携带业务内容。
pub mod loop_stage {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Stage {
        /// 一次模型请求（含重试后的最终一次）。
        ModelRequest,
        /// 一次工具的 schema 校验 + 审计 + 授权 + 执行。
        ToolCall,
    }

    impl Stage {
        pub fn as_str(self) -> &'static str {
            match self {
                Stage::ModelRequest => "model_request",
                Stage::ToolCall => "tool_call",
            }
        }
    }
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
    IterationStarted {
        iteration: usize,
    },
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
        /// 强制审计的判定事实；调用方据此写 append-only 的 `ai_tool_audits` 行。
        audit: AuditFacts,
    },
    /// 工具真实回执；`audit` 是本次调用的强制审计事实（含内核拒绝与执行结果）。
    ToolFinished {
        receipt: fluxdb_core::ToolReceipt,
        audit: AuditFacts,
    },
    /// 模型发布/更新了公开计划（`plan.update` 工具校验通过后的规范化结果）。
    ///
    /// 计划是**模型公开内容**，不是执行事实；步骤状态由模型显式声明，宿主不推断。
    PlanUpdated {
        plan: crate::PlanUpdate,
    },
    /// 一个执行阶段的脱敏耗时/结果；供「生命周期诊断」汇总，不含正文或凭据。
    ///
    /// 每阶段一条、在阶段边界即时发出：进程被杀后仍留有证据，不必等轮末汇总。
    StageTiming {
        stage: loop_stage::Stage,
        /// 主循环迭代序号（从 1 开始）。
        iteration: usize,
        /// 工具阶段的目标工具 ID；模型阶段为 `None`。
        tool_id: Option<String>,
        duration_ms: u64,
        /// 模型阶段：本次逻辑请求实际发出的供应商请求次数（含重试）。
        attempts: u32,
        ok: bool,
        /// 失败时的受控错误码（如 `timeout`、`http_429`）；成功为 `None`。
        error_code: Option<String>,
        /// 失败是否值得重试；只在拿得到错误对象/回执字段时有值，不按码猜。
        retryable: Option<bool>,
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
        /// 强制审计的判定事实；等待审批同样要留下审计记录。
        audit: AuditFacts,
    },
    /// 给用户/模型的可见说明（未知工具、截断未执行、无进展暂停等）。
    Notice(String),
}

/// Turn 的终态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// 模型给出了最终回答（本轮没有工具调用）。
    Completed {
        text: String,
        /// 结束原因为长度截断：正文可能不完整，推理模型下甚至可能完全没有正文。
        truncated: bool,
    },
    /// 安全暂停：达到迭代上限或无进展，需要用户介入。
    Paused {
        reason: String,
    },
    /// 基础设施失败（Provider/工具/hook/宿主返回 Err），不是普通工具错误。
    ///
    /// 携带失败阶段与原始错误对象：受控错误码与可重试性只能在错误还活着时取，
    /// 事后无法从文案重建；turn 错误码、诊断与给模型的解释都依赖它们。
    Failed {
        stage: FailureStage,
        error: fluxdb_core::Error,
    },
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
    /// 工具执行期的实时进度接收端；`None` 表示本轮不订阅进度。
    pub progress: Option<std::sync::Arc<dyn ToolProgressSource>>,
}

/// `ToolSpec` → Provider 可见的 `ToolDescriptor`（只暴露描述与 schema）。
fn loop_tool_descriptors(catalog: &ToolCatalog) -> Vec<fluxdb_core::ToolDescriptor> {
    let read_only = catalog.security.as_ref().is_some_and(|s| {
        s.settings()
            .map(|(p, _)| p.tools_default == ExecutionPolicy::ReadOnly)
            .unwrap_or(true)
    });
    catalog
        .specs()
        .iter()
        .filter(|spec| {
            catalog.is_model_visible(&spec.id)
                && (!read_only
                    || spec.risk == fluxdb_core::ToolRisk::Read
                    // db.run 的真实风险按 SQL 分类；只读策略下仍需暴露只读查询，
                    // 写语句会在授权阶段按 effective_risk 被拒绝。
                    || spec.id == "db.run")
        })
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

/// 回执里显式声明的 `retryable`（工具实现自己给的判定）；没有就不猜。
fn receipt_retryable(receipt: &fluxdb_core::ToolReceipt) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(&receipt.result_json)
        .ok()
        .and_then(|value| value.get("retryable").and_then(serde_json::Value::as_bool))
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

/// 把错误类别映射成稳定的脱敏码；不含 message 正文，避免把上游细节写进诊断。
///
/// 同一口径同时用于阶段事件与 Turn 终态错误码：人对齐、诊断可聚合，
/// 而不是一处写码、一处写中文文案。
pub(crate) fn stage_error_code(error: &fluxdb_core::Error) -> String {
    if let Some(status) = error.http_status {
        return format!("http_{status}");
    }
    match error.kind {
        fluxdb_core::ErrorKind::Timeout => "timeout",
        fluxdb_core::ErrorKind::Cancelled => "cancelled",
        fluxdb_core::ErrorKind::Connection => "connection",
        fluxdb_core::ErrorKind::Authentication => "authentication",
        fluxdb_core::ErrorKind::Permission => "permission",
        fluxdb_core::ErrorKind::Query => "query",
        fluxdb_core::ErrorKind::Unsupported => "unsupported",
        fluxdb_core::ErrorKind::Internal => "internal",
        fluxdb_core::ErrorKind::OutcomeUnknown => "write_outcome_unknown",
    }
    .to_string()
}

impl<'a> TurnRunner<'a> {
    pub fn run(
        &self,
        state: TurnState,
        steering: &mut Vec<String>,
        cancel: &fluxdb_core::CancelFlag,
        emit: &mut (dyn FnMut(LoopEvent) + Send),
    ) -> TurnOutcome {
        self.run_with_steering(state, cancel, &mut || std::mem::take(steering), emit)
    }

    /// 补充在每次模型请求前消费；不改变目标或绕过工具授权。
    fn run_with_steering(
        &self,
        mut state: TurnState,
        cancel: &fluxdb_core::CancelFlag,
        steering: &mut dyn FnMut() -> Vec<String>,
        emit: &mut (dyn FnMut(LoopEvent) + Send),
    ) -> TurnOutcome {
        let mut signature_counts: HashMap<String, usize> = HashMap::new();
        let mut last_text = String::new();
        // 页面筛选查询与直接 SQL 查询互斥；浏览/打开表不算执行路径。
        let (mut page_queried, mut sql_queried) = (false, false);

        for iteration in 1..=self.limits.max_iterations {
            if cancel.is_cancelled() {
                return TurnOutcome::Cancelled;
            }
            // 安全边界：把用户补充吸收进消息历史（不改执行目标）。
            for text in steering() {
                state.messages.push(fluxdb_core::ModelMessage::new(
                    fluxdb_core::ModelRole::User,
                    text,
                ));
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
            let stage_start = std::time::Instant::now();
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            let mut finish = None;
            let (response, attempts) = retry_streamed_model_call(
                cancel,
                &mut |receive| {
                    crate::ai_executor::block_on_cancellable(
                        self.provider
                            .stream_events(request.clone(), cancel.clone(), receive),
                        cancel,
                    )
                    .and_then(|result| result)
                },
                &mut |event| {
                    if cancel.is_cancelled() {
                        return;
                    }
                    match event {
                        fluxdb_core::ModelEvent::TextDelta(delta) => {
                            text.push_str(delta);
                            emit(LoopEvent::TextDelta(delta.clone()));
                        }
                        fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta) => {
                            emit(LoopEvent::ReasoningSummaryDelta(delta.clone()));
                        }
                        fluxdb_core::ModelEvent::ToolCallRequested(call) => {
                            emit(LoopEvent::ToolCallRequested(call.clone()));
                            tool_calls.push(call.clone());
                        }
                        fluxdb_core::ModelEvent::Usage(_) => {}
                        fluxdb_core::ModelEvent::Finished(reason) => finish = Some(*reason),
                    }
                },
            );
            // EOF/[DONE] 不能代替模型结束事实；此校验在重试边界之外，
            // 同时让生命周期诊断把提前断流记为失败，而不是“请求成功”。
            let response = response.and_then(|()| match finish {
                None => Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Connection,
                    "模型连接提前结束，未收到完整结束事件",
                )),
                Some(fluxdb_core::FinishReason::Error) => Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "模型返回错误结束",
                )),
                _ => Ok(()),
            });
            emit(LoopEvent::StageTiming {
                stage: loop_stage::Stage::ModelRequest,
                iteration,
                tool_id: None,
                duration_ms: stage_start.elapsed().as_millis() as u64,
                attempts,
                ok: response.is_ok(),
                error_code: response.as_ref().err().map(stage_error_code),
                retryable: response
                    .as_ref()
                    .err()
                    .map(fluxdb_core::Error::is_retryable),
            });
            if cancel.is_cancelled() || finish == Some(fluxdb_core::FinishReason::Cancelled) {
                return TurnOutcome::Cancelled;
            }
            if let Err(error) = response {
                return TurnOutcome::Failed {
                    stage: FailureStage::ModelRequest,
                    error,
                };
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
                state
                    .messages
                    .push(fluxdb_core::ModelMessage::assistant_with_tool_calls(
                        text.clone(),
                        calls,
                    ));
            }
            match finish {
                Some(fluxdb_core::FinishReason::Error) => {
                    return TurnOutcome::Failed {
                        stage: FailureStage::ModelRequest,
                        error: fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "模型返回错误结束",
                        ),
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
                return TurnOutcome::Completed {
                    text: last_text,
                    truncated: finish == Some(fluxdb_core::FinishReason::Length),
                };
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
                        format!(
                            "{{\"error\":\"invalid_json\",\"tool\":\"{}\"}}",
                            call.tool_id
                        ),
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
                let original_target =
                    serde_json::from_str::<serde_json::Value>(&call.arguments_json)
                        .ok()
                        .and_then(|value| value.get("target").cloned());
                let original_call_id = call.provider_call_id.clone();
                let (call, hook_deny, hook_approval) = match self.tools.hooks.before(&call, cancel)
                {
                    Ok(value) => value,
                    Err(error) => {
                        return TurnOutcome::Failed {
                            stage: FailureStage::ToolCall,
                            error,
                        };
                    }
                };
                let final_target = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
                    .ok()
                    .and_then(|value| value.get("target").cloned());
                if original_target != final_target {
                    return TurnOutcome::Failed {
                        stage: FailureStage::ToolCall,
                        error: fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "Hook 不得改变执行目标",
                        ),
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
                let conflicts = query_path_conflict(
                    &call.tool_id,
                    &call.arguments_json,
                    page_queried,
                    sql_queried,
                );
                let prepared = match if conflicts {
                    Err(ToolError::invalid_arguments(
                        "本轮已选择另一种查询方式；页面查询与 SQL 查询不能在同一轮重复执行",
                    ))
                } else {
                    self.tools.prepare(&call.tool_id, &call.arguments_json)
                } {
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
                let mut facts = audit_or_fail_closed(
                    self.audit,
                    &AuditInput {
                        tool_id: spec.id.clone(),
                        tool_version: spec.version,
                        plan_hash: hash.clone(),
                        target_json: state.target_json.clone(),
                        normalized_arguments_json: prepared.to_json_string(),
                        ruleset_version: "kernel-audit-v1".to_string(),
                    },
                );
                // 风险等级由工具按本次入参给出（静态工具直接用 spec），不在 Hook 里推断。
                let effective_risk = self
                    .tools
                    .executable(&call.tool_id)
                    .map(|tool| tool.effective_risk(&prepared.to_json_string()))
                    .unwrap_or(spec.risk);
                facts.risk = effective_risk.to_string();
                // before-tool Hook 只能收紧：Deny 优先，其次 RequireApproval，反过来不行。
                if let Some(reason) = hook_deny {
                    emit(LoopEvent::Notice(reason));
                    if facts.outcome != AuditOutcome::Deny {
                        facts = AuditFacts {
                            outcome: AuditOutcome::Deny,
                            rule_ids: vec!["hook.before_tool".to_string()],
                            reason_codes: vec!["hook_denied".to_string()],
                            ruleset_version: facts.ruleset_version,
                            risk: facts.risk,
                        };
                    }
                } else if let Some(reason) = hook_approval {
                    if facts.outcome != AuditOutcome::Deny {
                        emit(LoopEvent::Notice(reason));
                        facts = AuditFacts {
                            outcome: AuditOutcome::RequireApproval,
                            rule_ids: vec!["hook.before_tool".to_string()],
                            reason_codes: vec!["hook_require_approval".to_string()],
                            ruleset_version: facts.ruleset_version,
                            risk: facts.risk,
                        };
                    }
                }
                let audit = facts.outcome;
                let input = AuthorizeInput {
                    tool_id: spec.id.clone(),
                    tool_version: spec.version,
                    risk: effective_risk,
                    target_json: state.target_json.clone(),
                    normalized_arguments_json: prepared.to_json_string(),
                    plugin_digest: self.permission.plugin_digest.clone(),
                    globally_disabled_tools: self.permission.disabled_tools.clone(),
                    agent_allowed_tools: self.permission.agent_allowed_tools.clone(),
                    policy: self.permission.policy,
                    audit: Some(audit),
                };
                let mut automatic_approval = false;
                let mut bounded_write = false;
                let decision = if let Some(security) = &self.tools.security {
                    let policy = match security.settings() {
                        Ok((p, _)) => p.tools_default,
                        Err(error) => {
                            return TurnOutcome::Failed {
                                stage: FailureStage::ToolCall,
                                error,
                            };
                        }
                    };
                    match security.authorize(
                        &input,
                        policy,
                        || {
                            self.tools
                                .executable(&call.tool_id)
                                .ok_or_else(|| {
                                    fluxdb_core::Error::new(
                                        fluxdb_core::ErrorKind::Unsupported,
                                        "工具不可用",
                                    )
                                })?
                                .write_risk_facts(&prepared.to_json_string())
                        },
                        cancel,
                    ) {
                        Ok(result) => {
                            automatic_approval = result.automatic_approval;
                            bounded_write =
                                result.write_commit == fluxdb_core::WriteCommit::Bounded;
                            if effective_risk != fluxdb_core::ToolRisk::Read {
                                emit(LoopEvent::Notice(result.reason.clone()));
                            }
                            // 评审事实先落盘，失败时不得发出写入；拆解与阈值一起进审计，
                            // 事后能回答"这条为什么被自动放行"。
                            let risk_json=result.risk.as_ref().map(|risk|serde_json::json!({
                                "score":risk.final_score,"threshold":risk.threshold,
                                "rule_score":risk.rule_score,"hard_floor":risk.hard_floor,
                                "model_score":risk.model_score,"model_source":risk.model_source,
                                "signals":risk.signals.iter().map(|signal|serde_json::json!({
                                    "code":signal.code,"score":signal.score,
                                    "class":match signal.class{fluxdb_core::WriteRiskClass::Hard=>"hard",fluxdb_core::WriteRiskClass::Soft=>"soft"},
                                })).collect::<Vec<_>>(),
                            }));
                            if let Err(error) = with_ai_storage(&security.storage, |conn| {
                                fluxdb_storage::ai_platform::run_events::append(conn,&state.turn_id,"execution_policy",
                                &serde_json::json!({"tool":spec.id,"plan_hash":hash,"policy":policy,
                                    "reason":result.reason,"automatic_approval":automatic_approval,
                                    "write_commit":match result.write_commit{fluxdb_core::WriteCommit::Bounded=>"bounded",fluxdb_core::WriteCommit::Ordinary=>"ordinary"},
                                    "risk":risk_json,
                                    "settings_revision":result.settings_revision}).to_string(),ai_chat_now()).map(|_|())
                            }) {
                                return TurnOutcome::Failed {
                                    stage: FailureStage::ToolCall,
                                    error,
                                };
                            }
                            result.decision
                        }
                        Err(error) => {
                            return TurnOutcome::Failed {
                                stage: FailureStage::ToolCall,
                                error,
                            };
                        }
                    }
                } else {
                    authorize(&input)
                };

                match decision {
                    // 4a) 拒绝：合成 Denied 回执回给模型，永不执行。
                    PermissionDecision::Deny { code, reason, .. } => {
                        let receipt =
                            loop_denied_receipt(&call, code, &reason, &hash, loop_now_ms());
                        emit(LoopEvent::ToolFinished {
                            receipt: receipt.clone(),
                            audit: facts.clone(),
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
                            audit: facts.clone(),
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
                    audit: facts.clone(),
                });
                let ctx = fluxdb_core::ToolExecutionContext {
                    automatic_approval,
                    bounded_write,
                    session_id: state.session_id.clone(),
                    turn_id: state.turn_id.clone(),
                    iteration: iteration as i64,
                    call_id: call.provider_call_id.clone(),
                    cancel: cancel.clone(),
                    // 进度在产生点由 sink 即时投递（见 `TurnRunner::progress`），
                    // 不再攒进 Vec 等工具结束后回放。
                    progress: self
                        .progress
                        .as_ref()
                        .map(|source| source.sink_for(&call.provider_call_id)),
                };
                let tool_stage_start = std::time::Instant::now();
                let receipt = match crate::ai_executor::block_on_cancellable(
                    tool.execute(ctx, prepared.to_json_string()),
                    cancel,
                ) {
                    Ok(Ok(receipt)) => {
                        emit(LoopEvent::StageTiming {
                            stage: loop_stage::Stage::ToolCall,
                            iteration,
                            tool_id: Some(call.tool_id.clone()),
                            duration_ms: tool_stage_start.elapsed().as_millis() as u64,
                            attempts: 1,
                            ok: receipt.status != fluxdb_core::ToolReceiptStatus::Failed,
                            error_code: receipt.error_code.clone(),
                            retryable: receipt_retryable(&receipt),
                        });
                        if receipt.is_success() {
                            if call.tool_id == "db.run" {
                                sql_queried = true;
                            } else if call.tool_id == "page.query_table"
                                && serde_json::from_str::<serde_json::Value>(&receipt.result_json)
                                    .is_ok_and(|value| value["queried"] == true)
                            {
                                page_queried = true;
                            }
                        }
                        receipt
                    }
                    Ok(Err(error)) | Err(error) => {
                        emit(LoopEvent::StageTiming {
                            stage: loop_stage::Stage::ToolCall,
                            iteration,
                            tool_id: Some(call.tool_id.clone()),
                            duration_ms: tool_stage_start.elapsed().as_millis() as u64,
                            attempts: 1,
                            ok: false,
                            error_code: Some(stage_error_code(&error)),
                            retryable: Some(error.is_retryable()),
                        });
                        // 基础设施失败：退出整个任务（普通工具错误走 Failed 回执分支）。
                        return TurnOutcome::Failed {
                            stage: FailureStage::ToolCall,
                            error,
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
                        audit: facts.clone(),
                    });
                    return TurnOutcome::Failed {
                        stage: FailureStage::ToolCall,
                        error: fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message),
                    };
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
                    audit: facts.clone(),
                });
                // `plan.update` 成功时，把校验过的规范化计划作为公开计划事件发出。
                // 只认工具成功回执；失败/拒绝的计划不落成卡片，避免显示未生效的计划。
                if receipt.tool_id == "plan.update"
                    && receipt.status == fluxdb_core::ToolReceiptStatus::Succeeded
                    && let Ok(plan) = crate::parse_plan_update(&call.arguments_json)
                {
                    emit(LoopEvent::PlanUpdated { plan });
                }
                if receipt.status == fluxdb_core::ToolReceiptStatus::Unknown {
                    return TurnOutcome::Paused {
                        reason: "写入结果未知，已停止后续工具调用，请人工核实；不会自动重试".into(),
                    };
                }
                if let Err(error) = self.tools.hooks.after(&receipt, cancel) {
                    return TurnOutcome::Failed {
                        stage: FailureStage::ToolCall,
                        error,
                    };
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
                    })
                    .to_string(),
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
