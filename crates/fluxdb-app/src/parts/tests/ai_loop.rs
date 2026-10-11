// 主循环与工具状态机测试（设计 §10–11，T018）。
//
// 只用 scripted/custom Provider 与 RecordingTool：不发网络、不用真实凭据、
// 不读生产配置或 SQLite。重点是"真实副作用次数 + 真实回执内容 + 分支终态"。

mod ai_loop_test_support {
    use crate::{
        AuditInput, AuditOutcome, KernelPolicy, LoopEvent, LoopLimits, SecurityAuditHook,
        ToolCatalog, ToolSpec, TurnOutcome, TurnPermissionContext, TurnRunner, TurnState,
    };
    use fluxdb_core::{
        AiFuture, AiTool, CancelFlag, FinishReason, GenerationProvider, ModelCapabilities,
        ModelEvent, ModelMessage, ModelRequest, ModelRole, ToolCallRequest,
    };

    /// 折叠事件，便于断言 "某个 Notice 包含..." 之类。
    /// 测试用"放行"审计 Hook：真实实现由内核注册，测试里只证明链路经过审计。
    pub struct AllowAllAudit;

    impl SecurityAuditHook for AllowAllAudit {
        fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
            Ok(AuditOutcome::Allow)
        }
    }

    pub fn run(
        provider: &dyn GenerationProvider,
        tools: &ToolCatalog,
        limits: LoopLimits,
        model: &str,
        messages: Vec<ModelMessage>,
        steering: &mut Vec<String>,
        cancel: &CancelFlag,
    ) -> (TurnOutcome, Vec<LoopEvent>) {
        let audit = AllowAllAudit;
        let runner = TurnRunner {
            provider,
            tools,
            limits,
            permission: TurnPermissionContext {
                agent_allowed_tools: Vec::new(),
                disabled_tools: Default::default(),
                policy: KernelPolicy::default(),
                plugin_digest: None,
            },
            audit: &audit,
        };
        let state = TurnState {
        session_id: "loop-session".into(),
        turn_id: "loop-turn".into(),
            model: model.to_string(),
            messages,
            max_output_tokens: Some(64),
            target_json: "{\"connection_id\":1}".to_string(),
        };
        let mut events = Vec::new();
        let outcome = runner.run(state, steering, cancel, &mut |event| events.push(event));
        (outcome, events)
    }

    pub fn user_message(text: &str) -> ModelMessage {
        ModelMessage::new(ModelRole::User, text)
    }

    pub fn tool_call(id: &str, call_id: &str, args: &str) -> ModelEvent {
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.to_string(),
            tool_id: id.to_string(),
            arguments_json: args.to_string(),
        })
    }

    pub fn tool_call_stop(id: &str, call_id: &str, args: &str) -> Vec<ModelEvent> {
        vec![
            tool_call(id, call_id, args),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    }

    pub fn text_stop(text: &str) -> Vec<ModelEvent> {
        vec![
            ModelEvent::TextDelta(text.to_string()),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    }

    /// 收集一次请求里所有 Tool 角色消息的文本。
    pub fn tool_message_texts(request: &ModelRequest) -> Vec<String> {
        request
            .messages
            .iter()
            .filter(|m| m.role == ModelRole::Tool)
            .map(|m| m.text.clone())
            .collect()
    }

    /// 在第 2 轮 `stream` 内置位取消的 Provider：第 1 轮正常请求工具，第 2 轮请求取消
    /// 后返回纯文本。用于验证"取消后不会继续迭代/继续执行工具"。
    pub struct CancelOnSecondProvider {
        pub trigger: CancelFlag,
        pub calls: std::sync::Mutex<usize>,
    }

    impl CancelOnSecondProvider {
        pub fn new(trigger: CancelFlag) -> Self {
            Self {
                trigger,
                calls: std::sync::Mutex::new(0),
            }
        }

        pub fn call_count(&self) -> usize {
            *self.calls.lock().expect("calls")
        }
    }

    impl GenerationProvider for CancelOnSecondProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calling: true,
                reasoning_summary: false,
            }
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            Box::pin(async move {
                let n = {
                    let mut calls = self.calls.lock().expect("calls");
                    *calls += 1;
                    *calls
                };
                if n == 1 {
                    Ok(tool_call_stop(
                        "db.query",
                        "call-1",
                        r#"{"sql":"select 1"}"#,
                    ))
                } else {
                    // 第 2 轮：先取消，再返回文本；循环必须在读取后察觉取消并停止。
                    self.trigger.request();
                    Ok(text_stop("会话已取消"))
                }
            })
        }
    }

    /// 让出 `AiTool` 又不丢句柄：`SharedRecordingTool` 把 `Arc<RecordingTool>`
    /// 适配成 `Box<dyn AiTool>`，测试仍能用外部 `Arc` 断言真实调用次数。
    pub struct SharedRecordingTool(pub std::sync::Arc<crate::RecordingTool>);

    impl AiTool for SharedRecordingTool {
        fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
            self.0.descriptor()
        }

        fn execute(
            &self,
            ctx: fluxdb_core::ToolExecutionContext,
            arguments_json: String,
        ) -> AiFuture<'_, fluxdb_core::ToolReceipt> {
            self.0.execute(ctx, arguments_json)
        }
    }

    /// 把共享的 `RecordingTool` 注册进 `ToolCatalog`，外部仍持有 `Arc` 做副作用计数。
    pub fn catalog_with_shared_tool(tool: std::sync::Arc<crate::RecordingTool>) -> ToolCatalog {
        let spec = ToolSpec::from_descriptor(tool.descriptor()).expect("tool spec");
        let mut catalog = ToolCatalog::new(vec![spec]);
        catalog
            .register(Box::new(SharedRecordingTool(tool)))
            .expect("register tool");
        catalog
    }
}

use ai_loop_test_support::*;

#[test]
fn ai_loop_failed_tool_receipt_is_fed_back_and_completes() {
    // 第 1 轮请求工具；第 2 轮依据失败回执给出文本结论。
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            text_stop("已根据失败回执调整"),
        ],
    );
    let tool = std::sync::Arc::new(
        crate::RecordingTool::new("db.query", fluxdb_core::ToolRisk::Read)
            .with_status(fluxdb_core::ToolReceiptStatus::Failed)
            .with_result(r#"{"error":"syntax"}"#),
    );
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert_eq!(tool.call_count(), 1, "工具应真实执行一次");
    assert!(
        events.iter().any(|e| matches!(
            e,
            crate::LoopEvent::ToolFinished { receipt }
                if receipt.status == fluxdb_core::ToolReceiptStatus::Failed
        )),
        "应发出 ToolFinished(status=Failed)，实际 {events:?}"
    );
    let second = provider.request_at(1).expect("第 2 次请求");
    let texts = tool_message_texts(&second);
    assert!(
        texts.iter().any(|t| t.contains(r#""status":"failed""#)),
        "第 2 次请求应包含失败回执消息，实际 {texts:?}"
    );
    assert_eq!(
        outcome,
        crate::TurnOutcome::Completed {
            text: "已根据失败回执调整".into()
        }
    );
}

#[test]
fn ai_loop_truncated_output_does_not_execute_tool() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            vec![
                tool_call("db.query", "call-1", r#"{"sql":"select"#),
                fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Length),
            ],
            text_stop("已重试"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert_eq!(tool.call_count(), 0, "截断时不得执行工具");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, crate::LoopEvent::Notice(n) if n.contains("截断"))),
        "应提示截断，实际 {events:?}"
    );
    let second = provider.request_at(1).expect("第 2 次请求");
    let texts = tool_message_texts(&second);
    assert!(
        texts.iter().any(|t| t.contains(r#""error":"truncated""#)),
        "应把 truncated 结构化错误回给模型，实际 {texts:?}"
    );
    assert!(
        matches!(outcome, crate::TurnOutcome::Completed { .. }),
        "截断后模型重试成功应 Completed，实际 {outcome:?}"
    );
}

#[test]
fn ai_loop_repeated_signature_pauses_with_no_progress() {
    // 每轮都是完全相同的工具 + 参数；max_repeat_signature=2 → 执行 1 次后暂停。
    let one_step = tool_call_stop("db.query", "call-x", r#"{"sql":"select 1"}"#);
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![one_step],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, _events) = run(
        &provider,
        &tools,
        crate::LoopLimits {
            max_iterations: 24,
            max_repeat_signature: 2,
        },
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    match outcome {
        crate::TurnOutcome::Paused { reason } => {
            assert!(
                reason.contains("无进展"),
                "暂停原因应含无进展，实际 {reason}"
            );
        }
        other => panic!("应 Paused，实际 {other:?}"),
    }
    assert_eq!(
        tool.call_count(),
        1,
        "max_repeat_signature=2 时应只在第一次真正执行"
    );
}

#[test]
fn ai_loop_completes_without_decision_provider() {
    // 本文件不构造任何 DecisionProvider，纯文本脚本即可完成。
    let provider = crate::ScriptedGenerationProvider::text_only("你好");
    let tools = crate::ToolCatalog::new(Vec::new());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("hi")],
        &mut steering,
        &cancel,
    );

    assert_eq!(
        outcome,
        crate::TurnOutcome::Completed {
            text: "你好".into()
        }
    );
    let iterations = events
        .iter()
        .filter(|e| matches!(e, crate::LoopEvent::IterationStarted { .. }))
        .count();
    assert_eq!(iterations, 1, "纯文本应只迭代一次，实际 {events:?}");
}

#[test]
fn ai_loop_invalid_json_is_rejected_then_completes() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call_stop("db.query", "call-1", "{not json"),
            text_stop("已修正"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert_eq!(tool.call_count(), 0, "非法 JSON 不得执行工具");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, crate::LoopEvent::Notice(_))),
        "应发出 Notice，实际 {events:?}"
    );
    let second = provider.request_at(1).expect("第 2 次请求");
    let texts = tool_message_texts(&second);
    assert!(
        texts
            .iter()
            .any(|t| t.contains(r#""error":"invalid_json""#)),
        "应回 invalid_json，实际 {texts:?}"
    );
    assert!(
        matches!(outcome, crate::TurnOutcome::Completed { .. }),
        "实际 {outcome:?}"
    );
}

#[test]
fn ai_loop_unknown_tool_is_rejected_without_panic() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call_stop("nope.tool", "call-1", r#"{"x":1}"#),
            text_stop("换一条路"),
        ],
    );
    let tools = crate::ToolCatalog::new(Vec::new());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, crate::LoopEvent::Notice(n) if n.contains("未注册"))),
        "应提示未注册，实际 {events:?}"
    );
    let second = provider.request_at(1).expect("第 2 次请求");
    let texts = tool_message_texts(&second);
    assert!(
        texts
            .iter()
            .any(|t| t.contains(r#""error":"unknown_tool""#)),
        "应回 unknown_tool，实际 {texts:?}"
    );
    assert!(
        matches!(outcome, crate::TurnOutcome::Completed { .. }),
        "实际 {outcome:?}"
    );
}

#[test]
fn ai_loop_pre_cancel_stops_before_anything() {
    let provider = crate::ScriptedGenerationProvider::text_only("不应到达");
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    cancel.request();
    let mut steering = Vec::new();

    let (outcome, _events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert_eq!(outcome, crate::TurnOutcome::Cancelled);
    assert_eq!(tool.call_count(), 0, "预取消不得执行工具");
    assert_eq!(provider.call_count(), 0, "预取消不应调用模型");
}

#[test]
fn ai_loop_cancel_on_second_stream_stops_iteration() {
    // 第 1 轮模型请求工具 → 执行 1 次；第 2 轮 Provider 在 stream 内置位取消。
    let cancel = fluxdb_core::CancelFlag::new();
    let provider = CancelOnSecondProvider::new(cancel.clone());
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let mut steering = Vec::new();

    let (outcome, _events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    assert_eq!(outcome, crate::TurnOutcome::Cancelled);
    assert_eq!(
        provider.call_count(),
        2,
        "应停止在第 2 轮，不再有第 3 次 stream"
    );
    assert_eq!(tool.call_count(), 1, "第 1 轮工具执行一次后不得再执行工具");
}

#[test]
fn ai_loop_steering_is_absorbed_before_first_request() {
    let provider = crate::ScriptedGenerationProvider::text_only("ok");
    let tools = crate::ToolCatalog::new(Vec::new());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = vec!["补充要求".to_string()];

    let (outcome, _events) = run(
        &provider,
        &tools,
        crate::LoopLimits::default(),
        "m",
        vec![user_message("原始")],
        &mut steering,
        &cancel,
    );

    assert!(
        matches!(outcome, crate::TurnOutcome::Completed { .. }),
        "实际 {outcome:?}"
    );
    let first = provider.request_at(0).expect("第 1 次请求");
    assert!(
        first
            .messages
            .iter()
            .any(|m| m.role == fluxdb_core::ModelRole::User && m.text == "补充要求"),
        "第 1 次请求应含 steering user 文本，实际 {:?}",
        first.messages
    );
    assert!(steering.is_empty(), "steering 应被吸收");
}

#[test]
fn ai_loop_iteration_limit_pauses() {
    let one_step = tool_call_stop("db.query", "call-x", r#"{"sql":"select 1"}"#);
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![one_step],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = catalog_with_shared_tool(tool.clone());
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();

    let (outcome, _events) = run(
        &provider,
        &tools,
        crate::LoopLimits {
            max_iterations: 1,
            max_repeat_signature: 100,
        },
        "m",
        vec![user_message("查询")],
        &mut steering,
        &cancel,
    );

    match outcome {
        crate::TurnOutcome::Paused { reason } => {
            assert!(
                reason.contains("最大迭代次数"),
                "暂停原因应含最大迭代次数，实际 {reason}"
            );
        }
        other => panic!("应 Paused，实际 {other:?}"),
    }
    assert_eq!(tool.call_count(), 1, "单次迭代内应执行一次工具");
}
