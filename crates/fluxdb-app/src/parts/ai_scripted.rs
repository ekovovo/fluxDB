// Scripted Provider 与隔离 fixture（设计 §37，T004）。
//
// 用途：让"UI 命令 → 运行时 → 工具 → 回执 → 继续对话"这条链路可复现，且
// **不读生产配置、不用真实网络、不花费用**。脚本只决定模型"说什么"，
// 工具结果仍必须由真实工具执行产生，脚本不能直接伪造数据库结果。


/// 一次 `stream()` 调用要吐出的事件序列。
pub type ScriptedStep = Vec<fluxdb_core::ModelEvent>;

/// 可编排的生成 Provider：按调用次序返回脚本，并记录收到的请求供断言。
pub struct ScriptedGenerationProvider {
    capabilities: fluxdb_core::ModelCapabilities,
    steps: Mutex<Vec<ScriptedStep>>,
    requests: Mutex<Vec<fluxdb_core::ModelRequest>>,
}

impl ScriptedGenerationProvider {
    pub fn new(capabilities: fluxdb_core::ModelCapabilities, steps: Vec<ScriptedStep>) -> Self {
        Self {
            capabilities,
            steps: Mutex::new(steps),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 默认脚本：一次纯文本回答。
    pub fn text_only(text: &str) -> Self {
        let mut caps = fluxdb_core::ModelCapabilities::default();
        caps.streaming = true;
        Self::new(
            caps,
            vec![vec![
                fluxdb_core::ModelEvent::TextDelta(text.to_string()),
                fluxdb_core::ModelEvent::Usage(fluxdb_core::Usage {
                    input_tokens: 10,
                    output_tokens: 3,
                    cached_input_tokens: 0,
                }),
                fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
            ]],
        )
    }

    /// 第 n 次调用（1 起）收到的请求。
    pub fn request_at(&self, index: usize) -> Option<fluxdb_core::ModelRequest> {
        self.requests.lock().ok()?.get(index).cloned()
    }

    pub fn call_count(&self) -> usize {
        self.requests.lock().map(|r| r.len()).unwrap_or(0)
    }
}

impl fluxdb_core::GenerationProvider for ScriptedGenerationProvider {
    fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
        self.capabilities
    }

    fn stream(
        &self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            self.requests
                .lock()
                .map_err(|_| fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "scripted provider 状态锁已损坏",
                ))?
                .push(request);
            if cancel.is_cancelled() {
                return Ok(vec![fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Cancelled)]);
            }
            let mut steps = self.steps.lock().map_err(|_| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "scripted provider 脚本锁已损坏",
                )
            })?;
            if steps.is_empty() {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "scripted provider 没有更多脚本步骤",
                ));
            }
            // 用光后重复最后一步，避免调用方因为次数算错而 panic。
            let step = if steps.len() == 1 {
                steps[0].clone()
            } else {
                steps.remove(0)
            };
            Ok(step)
        })
    }
}

/// 可编排的决策 Provider（Jev）：固定响应 + 记录请求。
pub struct ScriptedDecisionProvider {
    response: fluxdb_core::DecisionResponse,
    requests: Mutex<Vec<fluxdb_core::DecisionRequest>>,
}

impl ScriptedDecisionProvider {
    pub fn new(response: fluxdb_core::DecisionResponse) -> Self {
        Self {
            response,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn call_count(&self) -> usize {
        self.requests.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// 第 n 次调用（1 起）收到的决策请求（供审计/断言）。
    pub fn request_at(&self, index: usize) -> Option<fluxdb_core::DecisionRequest> {
        self.requests.lock().ok()?.get(index).cloned()
    }
}

impl fluxdb_core::DecisionProvider for ScriptedDecisionProvider {
    fn decide(
        &self,
        request: fluxdb_core::DecisionRequest,
        _cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::DecisionResponse> {
        Box::pin(async move {
            self.requests.lock().map_err(|_| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "决策脚本状态锁已损坏")
            })?
            .push(request);
            Ok(self.response.clone())
        })
    }
}

/// 记录型工具：真实执行"调用"，把每次调用与参数记下来，返回可核对回执。
///
/// 测试用它证明"脚本没有绕过工具执行"：回执里的 `call_id`/`arguments_json` 必须
/// 与运行时的调用一一对应，且副作用计数等于真实调用次数。
pub struct RecordingTool {
    descriptor: fluxdb_core::ToolDescriptor,
    calls: Mutex<Vec<(String, String)>>,
    /// 每次调用返回的结果载荷（默认 `{"ok":true}`）。
    result_json: String,
    status: fluxdb_core::ToolReceiptStatus,
}

impl RecordingTool {
    pub fn new(id: &str, risk: fluxdb_core::ToolRisk) -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: id.to_string(),
                version: 1,
                description: format!("scripted tool {id}"),
                risk,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            },
            calls: Mutex::new(Vec::new()),
            result_json: r#"{"ok":true}"#.to_string(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
        }
    }

    pub fn with_result(mut self, result_json: &str) -> Self {
        self.result_json = result_json.to_string();
        self
    }

    pub fn with_status(mut self, status: fluxdb_core::ToolReceiptStatus) -> Self {
        self.status = status;
        self
    }

    /// 已发生的真实调用次数（副作用计数）。
    pub fn call_count(&self) -> usize {
        self.calls.lock().map(|c| c.len()).unwrap_or(0)
    }

    pub fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl fluxdb_core::AiTool for RecordingTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            self.calls
                .lock()
                .map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "工具记录锁已损坏")
                })?
                .push((ctx.call_id.clone(), arguments_json.clone()));
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status: self.status,
                result_json: self.result_json.clone(),
                error_code: None,
                affected_rows: None,
                started_at_ms: 0,
                finished_at_ms: 1,
            })
        })
    }
}
