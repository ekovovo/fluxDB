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

/// 工具注册表：宿主稳定 ID → 工具实现。
pub struct ToolRegistry {
    tools: Vec<Box<dyn fluxdb_core::AiTool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn fluxdb_core::AiTool>>) -> Self {
        Self { tools }
    }

    pub fn descriptors(&self) -> Vec<fluxdb_core::ToolDescriptor> {
        self.tools.iter().map(|t| t.descriptor().clone()).collect()
    }

    pub fn get(&self, host_id: &str) -> Option<&dyn fluxdb_core::AiTool> {
        self.tools
            .iter()
            .find(|t| t.descriptor().id == host_id)
            .map(|t| t.as_ref())
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

impl Default for LoopLimits {
    fn default() -> Self {
        Self {
            max_iterations: 24,
            max_repeat_signature: 3,
        }
    }
}

/// Turn 的输入状态（消息历史 + 目标模型）。
#[derive(Clone, Debug)]
pub struct TurnState {
    pub model: String,
    pub messages: Vec<fluxdb_core::ModelMessage>,
    pub max_output_tokens: Option<i64>,
}

/// 循环对外发出的事件（actor 转成 `AiEvent` / 持久化片段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopEvent {
    IterationStarted { iteration: usize },
    /// 文本增量（UI 合并展示）。
    TextDelta(String),
    /// 公开推理摘要增量。
    ReasoningSummaryDelta(String),
    /// 工具开始执行。
    ToolStarted { call: fluxdb_core::ToolCallRequest },
    /// 工具真实回执。
    ToolFinished { receipt: fluxdb_core::ToolReceipt },
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
pub struct TurnRunner<'a> {
    pub provider: &'a dyn fluxdb_core::GenerationProvider,
    pub tools: &'a ToolRegistry,
    pub limits: LoopLimits,
}

impl<'a> TurnRunner<'a> {
    pub fn run(
        &self,
        mut state: TurnState,
        steering: &mut Vec<String>,
        cancel: &fluxdb_core::CancelFlag,
        emit: &mut dyn FnMut(LoopEvent),
    ) -> TurnOutcome {
        let mut signature_counts: HashMap<String, usize> = HashMap::new();
        let mut last_text = String::new();

        for iteration in 1..=self.limits.max_iterations {
            if cancel.is_cancelled() {
                return TurnOutcome::Cancelled;
            }
            // 安全边界：把用户补充吸收进消息历史（不改执行目标）。
            for text in steering.drain(..) {
                state.messages.push(fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::User, text));
            }
            emit(LoopEvent::IterationStarted { iteration });

            let request = fluxdb_core::ModelRequest {
                model: state.model.clone(),
                messages: state.messages.clone(),
                tools: self.tools.descriptors(),
                max_output_tokens: state.max_output_tokens,
            };
            let events = match block_on_ready(self.provider.stream(request, cancel.clone())) {
                Ok(Ok(events)) => events,
                Ok(Err(error)) => {
                    return TurnOutcome::Failed {
                        message: error.message,
                    };
                }
                Err(error) => {
                    return TurnOutcome::Failed {
                        message: error.message,
                    };
                }
            };

            let (text, tool_calls, finish, cancelled) = assemble(events, emit);
            if cancelled || cancel.is_cancelled() {
                return TurnOutcome::Cancelled;
            }
            if !text.is_empty() {
                last_text = text.clone();
                state
                    .messages
                    .push(fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::Assistant, text.clone()));
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
                    state.messages.push(fluxdb_core::ModelMessage::new(
                        fluxdb_core::ModelRole::Tool,
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
                    state.messages.push(fluxdb_core::ModelMessage::new(
                        fluxdb_core::ModelRole::Tool,
                        format!("{{\"error\":\"invalid_json\",\"tool\":\"{}\"}}", call.tool_id),
                    ));
                    continue;
                }
                let Some(tool) = self.tools.get(&call.tool_id) else {
                    let notice = format!("工具 {} 未注册，已拒绝执行。", call.tool_id);
                    emit(LoopEvent::Notice(notice.clone()));
                    state.messages.push(fluxdb_core::ModelMessage::new(
                        fluxdb_core::ModelRole::Tool,
                        format!("{{\"error\":\"unknown_tool\",\"tool\":\"{}\"}}", call.tool_id),
                    ));
                    continue;
                };

                // 无进展检测：相同工具 + 相同参数反复出现。
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

                emit(LoopEvent::ToolStarted {
                    call: call.clone(),
                });
                let ctx = fluxdb_core::ToolExecutionContext {
                    session_id: String::new(),
                    turn_id: String::new(),
                    iteration: iteration as i64,
                    call_id: call.provider_call_id.clone(),
                };
                let receipt = match block_on_ready(tool.execute(ctx, call.arguments_json.clone())) {
                    Ok(Ok(receipt)) => receipt,
                    Ok(Err(error)) | Err(error) => {
                        // 基础设施失败：退出整个任务（普通工具错误走下面的 Failed 回执分支）。
                        return TurnOutcome::Failed {
                            message: error.message,
                        };
                    }
                };
                emit(LoopEvent::ToolFinished {
                    receipt: receipt.clone(),
                });
                // 真实回执（含失败）都回到模型，让它观察并决定下一步。
                state.messages.push(fluxdb_core::ModelMessage::new(
                    fluxdb_core::ModelRole::Tool,
                    format!(
                        "{{\"tool\":\"{}\",\"status\":\"{}\",\"result\":{}}}",
                        receipt.tool_id,
                        match receipt.status {
                            fluxdb_core::ToolReceiptStatus::Succeeded => "succeeded",
                            fluxdb_core::ToolReceiptStatus::Failed => "failed",
                            fluxdb_core::ToolReceiptStatus::Denied => "denied",
                            fluxdb_core::ToolReceiptStatus::Cancelled => "cancelled",
                            fluxdb_core::ToolReceiptStatus::Unknown => "unknown",
                        },
                        if receipt.result_json.trim().is_empty() {
                            "null"
                        } else {
                            receipt.result_json.as_str()
                        }
                    ),
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

/// 把一次 stream 的事件折叠成（最终文本、工具调用、结束原因、是否取消）。
fn assemble(
    events: Vec<fluxdb_core::ModelEvent>,
    emit: &mut dyn FnMut(LoopEvent),
) -> (String, Vec<fluxdb_core::ToolCallRequest>, Option<fluxdb_core::FinishReason>, bool) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut finish = None;
    let mut cancelled = false;
    for event in events {
        match event {
            fluxdb_core::ModelEvent::TextDelta(delta) => {
                text.push_str(&delta);
                emit(LoopEvent::TextDelta(delta));
            }
            fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta) => {
                emit(LoopEvent::ReasoningSummaryDelta(delta));
            }
            fluxdb_core::ModelEvent::ToolCallRequested(call) => tool_calls.push(call),
            fluxdb_core::ModelEvent::Usage(_) => {
                // usage 记账由预算任务（T030）接管；本切片不丢弃也不伪造。
            }
            fluxdb_core::ModelEvent::Finished(reason) => {
                if reason == fluxdb_core::FinishReason::Cancelled {
                    cancelled = true;
                }
                finish = Some(reason);
            }
        }
    }
    (text, tool_calls, finish, cancelled)
}
