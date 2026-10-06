// 运行时 actor 的"工具/审批/取消"业务语义测试（设计 §12 actor 事件桥、§25 审批、§20 工具协议）。
// 子 Agent A：只做测试，不改生产代码。
//
// 只通过**生产入口** `spawn_ai_runtime_with_tools` / `spawn_ai_runtime` +
// `AiRuntimeHandle::start_turn_with_spec` 驱动 actar 全链路（TurnRunner → ToolCatalog
// → 强制审计 → authorize → 真实执行 → 事件桥），禁止直接调用 TurnRunner 或往孤立组件塞数据。
//
// 用 scripted Provider + RecordingTool + 内存 sink：不发网络、不用真实凭据、不碰 SQLite。
// 每条关键断言都核对**真实副作用次数**（`RecordingTool::call_count()`），而不是只看事件文案。

mod ai_runtime_tools_support {
    use crate::{AiEvent, AiEventSink, ToolCatalog, ToolSpec};
    use fluxdb_core::{
        AiFuture, AiTool, CancelFlag, FinishReason, GenerationProvider, ModelCapabilities,
        ModelEvent, ModelRequest, ModelRole, ToolCallRequest, ToolDescriptor, ToolExecutionContext,
        ToolReceipt,
    };
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    pub fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn event_session_id(event: &AiEvent) -> &str {
        match event {
            AiEvent::TurnStarted { session_id, .. }
            | AiEvent::TextDelta { session_id, .. }
            | AiEvent::ReasoningSummaryDelta { session_id, .. }
            | AiEvent::ToolCallRequested { session_id, .. }
            | AiEvent::ToolStarted { session_id, .. }
            | AiEvent::ToolFinished { session_id, .. }
            | AiEvent::ApprovalRequired { session_id, .. }
            | AiEvent::Notice { session_id, .. }
            | AiEvent::Paused { session_id, .. }
            | AiEvent::Finished { session_id, .. }
            | AiEvent::Error { session_id, .. } => session_id,
        }
    }

    /// 声明支持 stream + tool calling 的能力集。
    pub fn tool_caps() -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        }
    }

    pub fn tool_call_stop(id: &str, call_id: &str, args: &str) -> Vec<ModelEvent> {
        vec![
            ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: call_id.to_string(),
                tool_id: id.to_string(),
                arguments_json: args.to_string(),
            }),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    }

    pub fn text_stop(text: &str) -> Vec<ModelEvent> {
        vec![
            ModelEvent::TextDelta(text.to_string()),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    }

    /// 内存事件收集器：供断言事件顺序 / seq / 会话-Turn 归属。
    #[derive(Clone)]
    pub struct RecordingSink {
        events: Arc<Mutex<Vec<AiEvent>>>,
    }

    impl RecordingSink {
        pub fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn events(&self) -> Vec<AiEvent> {
            self.events.lock().expect("事件锁").clone()
        }

        pub fn wait_for_turn_finished(&self, turn_id: &str, timeout: Duration) -> bool {
            let expected = turn_id.to_string();
            wait_until(
                || {
                    self.events.lock().expect("事件锁").iter().any(|event| {
                        event.turn_id() == expected && matches!(event, AiEvent::Finished { .. })
                    })
                },
                timeout,
            )
        }

        pub fn wait_for_paused(&self, turn_id: &str, timeout: Duration) -> bool {
            let expected = turn_id.to_string();
            wait_until(
                || {
                    self.events.lock().expect("事件锁").iter().any(|event| {
                        event.turn_id() == expected && matches!(event, AiEvent::Paused { .. })
                    })
                },
                timeout,
            )
        }
    }

    impl AiEventSink for RecordingSink {
        fn emit(&self, event: AiEvent) {
            self.events.lock().expect("事件锁").push(event);
        }
    }

    /// 让出 `AiTool` 但外部仍持有 `Arc`（对真实副作用计数）。
    pub struct SharedRecordingTool(pub Arc<crate::RecordingTool>);

    impl AiTool for SharedRecordingTool {
        fn descriptor(&self) -> &ToolDescriptor {
            self.0.descriptor()
        }

        fn execute(
            &self,
            ctx: ToolExecutionContext,
            arguments_json: String,
        ) -> AiFuture<'_, ToolReceipt> {
            self.0.execute(ctx, arguments_json)
        }
    }

    /// 把共享的 `RecordingTool` 注册进 `ToolCatalog`（spec 与实现 id 一致）。
    pub fn catalog_with(tool: Arc<crate::RecordingTool>) -> ToolCatalog {
        let spec = ToolSpec::from_descriptor(tool.descriptor()).expect("tool spec");
        let mut catalog = ToolCatalog::new(vec![spec]);
        catalog
            .register(Box::new(SharedRecordingTool(tool)))
            .expect("register tool");
        catalog
    }

    /// 让外部既能 `Box` 进 actor、又能继续读取请求记录的 Provider 适配器。
    pub struct SharedProvider<T: ?Sized>(pub Arc<T>);

    impl<T: GenerationProvider + ?Sized> GenerationProvider for SharedProvider<T> {
        fn capabilities(&self) -> ModelCapabilities {
            self.0.capabilities()
        }

        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            self.0.stream(request, cancel)
        }
    }

    /// 长回合 Provider：`stream` 内阻塞并轮询取消位，用于验证 actor 的真实取消。
    pub struct CancellableBlockingProvider {
        started: Arc<(Mutex<bool>, Condvar)>,
    }

    impl CancellableBlockingProvider {
        pub fn new() -> Self {
            Self {
                started: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        pub fn wait_started(&self, timeout: Duration) -> bool {
            let (lock, cv) = &*self.started;
            let mut started = lock.lock().expect("启动锁");
            let deadline = Instant::now() + timeout;
            while !*started {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (guard, result) = cv.wait_timeout(started, remaining).expect("启动等待");
                started = guard;
                if result.timed_out() && !*started {
                    return false;
                }
            }
            true
        }
    }

    impl GenerationProvider for CancellableBlockingProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calling: false,
                reasoning_summary: false,
            }
        }

        fn stream(
            &self,
            _request: ModelRequest,
            cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            let (lock, cv) = &*self.started;
            *lock.lock().expect("启动锁") = true;
            cv.notify_all();

            // 模拟一个"长回合"：在流式开始前阻塞，并周期性检查取消位。
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if cancel.is_cancelled() {
                    return Box::pin(async {
                        Ok(vec![ModelEvent::Finished(FinishReason::Cancelled)])
                    });
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Box::pin(async { Ok(vec![ModelEvent::Finished(FinishReason::Stop)]) })
        }
    }

    /// 让 `ModelMessage` 构造更简洁（与现有测试一致地显式给出角色）。
    pub fn user_message(text: &str) -> fluxdb_core::ModelMessage {
        fluxdb_core::ModelMessage::new(ModelRole::User, text)
    }
}

use ai_runtime_tools_support as tools_support;

/// 固定放行审计：模拟内核审计返回 Allow。
struct ToolsAllowAudit;

impl crate::SecurityAuditHook for ToolsAllowAudit {
    fn audit(&self, _input: &crate::AuditInput) -> fluxdb_core::Result<crate::AuditOutcome> {
        Ok(crate::AuditOutcome::Allow)
    }
}

/// 审计不可用（模拟审计服务/落盘故障）：必须 fail closed。
struct ToolsFailingAudit;

impl crate::SecurityAuditHook for ToolsFailingAudit {
    fn audit(&self, _input: &crate::AuditInput) -> fluxdb_core::Result<crate::AuditOutcome> {
        Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "审计服务不可用（测试注入）",
        ))
    }
}

/// 构造一轮 Turn 的完整输入（目标固定 + 权限上下文）。
fn tools_turn_spec(user_text: &str, permission: crate::TurnPermissionContext) -> crate::TurnSpec {
    crate::TurnSpec {
        model: "test-model".to_string(),
        messages: vec![tools_support::user_message(user_text)],
        max_output_tokens: Some(64),
        target_json: r#"{"connection_id":1}"#.to_string(),
        permission,
    }
}

/// 收集该次 turn 的全部工具回执（真实产出或内核合成的拒绝回执）。
fn tools_receipts(events: &[crate::AiEvent]) -> Vec<fluxdb_core::ToolReceipt> {
    events
        .iter()
        .filter_map(|event| match event {
            crate::AiEvent::ToolFinished { receipt, .. } => Some(receipt.clone()),
            _ => None,
        })
        .collect()
}

fn tools_has_started(events: &[crate::AiEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, crate::AiEvent::ToolStarted { .. }))
}

// 1) 只读工具完整链路：ToolCallRequested → ToolStarted → ToolFinished(Succeeded) → Finished(Stop)。
#[test]
fn ai_runtime_tools_read_chain_executes_once_and_finishes_stop() {
    let provider = crate::ScriptedGenerationProvider::new(
        tools_support::tool_caps(),
        vec![
            tools_support::tool_call_stop("db.query", "call-r", r#"{"sql":"select 1"}"#),
            tools_support::text_stop("查询完成"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = tools_support::catalog_with(tool.clone());
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime_with_tools(
        Box::new(provider),
        Box::new(sink.clone()),
        8,
        tools,
        Box::new(ToolsAllowAudit),
    );

    handle
        .start_turn_with_spec(
            "session-r",
            "turn-r",
            tools_turn_spec("查询订单", crate::TurnPermissionContext::default()),
        )
        .expect("启动只读链路 Turn");
    assert!(sink.wait_for_turn_finished("turn-r", std::time::Duration::from_secs(2)));

    let events = sink.events();
    assert_eq!(tool.call_count(), 1, "只读工具应真实执行一次: {events:?}");

    let idx_requested = events
        .iter()
        .position(|e| matches!(e, crate::AiEvent::ToolCallRequested { .. }))
        .expect("应有 ToolCallRequested");
    let idx_started = events
        .iter()
        .position(|e| matches!(e, crate::AiEvent::ToolStarted { .. }))
        .expect("应有 ToolStarted");
    let idx_finished = events
        .iter()
        .position(|e| matches!(e, crate::AiEvent::ToolFinished { .. }))
        .expect("应有 ToolFinished");
    let idx_terminal = events
        .iter()
        .position(|e| matches!(e, crate::AiEvent::Finished { .. }))
        .expect("应有 Finished 终态");
    assert!(
        idx_requested < idx_started && idx_started < idx_finished && idx_finished < idx_terminal,
        "事件顺序应为 Requested→Started→Finished→Finished(Stop)，实际 {events:?}"
    );

    let succeeded = events.iter().any(|e| {
        matches!(
            e,
            crate::AiEvent::ToolFinished { receipt, .. }
                if receipt.status == fluxdb_core::ToolReceiptStatus::Succeeded
        )
    });
    assert!(succeeded, "应有一个 Succeeded 回执: {events:?}");
    assert!(
        matches!(
            events.last(),
            Some(crate::AiEvent::Finished {
                reason: fluxdb_core::FinishReason::Stop,
                ..
            })
        ),
        "终态应为 Finished(Stop)，实际 {events:?}"
    );

    // seq 严格递增，且会话/Turn 归属一致。
    assert_eq!(events[0].seq(), 1, "首个事件 seq 应为 1");
    let mut last_seq = 0u64;
    for (index, event) in events.iter().enumerate() {
        assert!(
            event.seq() > last_seq,
            "seq 必须严格递增：第 {index} 个事件 {} -> {}",
            last_seq,
            event.seq()
        );
        last_seq = event.seq();
    }
    assert!(events.iter().all(|e| e.turn_id() == "turn-r"));
    assert!(events
        .iter()
        .all(|e| tools_support::event_session_id(e) == "session-r"));

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

// 2) 写工具在默认策略（write_allowed=false）下被拒，不执行，模型拿到拒绝后正常收尾。
#[test]
fn ai_runtime_tools_write_denied_by_default_policy() {
    let provider = crate::ScriptedGenerationProvider::new(
        tools_support::tool_caps(),
        vec![
            tools_support::tool_call_stop(
                "db.execute",
                "call-w",
                r#"{"sql":"delete from orders"}"#,
            ),
            tools_support::text_stop("已放弃写操作"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.execute",
        fluxdb_core::ToolRisk::Write,
    ));
    let tools = tools_support::catalog_with(tool.clone());
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime_with_tools(
        Box::new(provider),
        Box::new(sink.clone()),
        8,
        tools,
        Box::new(ToolsAllowAudit),
    );

    handle
        .start_turn_with_spec(
            "session-wd",
            "turn-wd",
            tools_turn_spec("删掉订单", crate::TurnPermissionContext::default()),
        )
        .expect("启动写拒绝 Turn");
    assert!(sink.wait_for_turn_finished("turn-wd", std::time::Duration::from_secs(2)));

    let events = sink.events();
    let receipts = tools_receipts(&events);
    let denied: Vec<_> = receipts
        .iter()
        .filter(|r| r.status == fluxdb_core::ToolReceiptStatus::Denied)
        .collect();
    assert_eq!(denied.len(), 1, "应有且仅有一个 Denied 回执: {receipts:?}");
    assert_eq!(denied[0].tool_id, "db.execute");
    assert!(
        !receipts
            .iter()
            .any(|r| r.status == fluxdb_core::ToolReceiptStatus::Succeeded),
        "默认策略下写工具不得有成功回执: {receipts:?}"
    );
    assert_eq!(tool.call_count(), 0, "被拒的写工具绝不能真实执行");
    assert!(
        matches!(
            events.last(),
            Some(crate::AiEvent::Finished {
                reason: fluxdb_core::FinishReason::Stop,
                ..
            })
        ),
        "拒绝是受控结果，模型应继续给出文本并正常收尾: {events:?}"
    );

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

// 3) 写工具在 write_allowed=true 时需审批：Paused 为终态，审批前绝不执行。
#[test]
fn ai_runtime_tools_write_requires_approval_and_pauses() {
    let provider = crate::ScriptedGenerationProvider::new(
        tools_support::tool_caps(),
        vec![tools_support::tool_call_stop(
            "db.execute",
            "call-ap",
            r#"{"sql":"update orders set status='x'"}"#,
        )],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.execute",
        fluxdb_core::ToolRisk::Write,
    ));
    let tools = tools_support::catalog_with(tool.clone());
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime_with_tools(
        Box::new(provider),
        Box::new(sink.clone()),
        8,
        tools,
        Box::new(ToolsAllowAudit),
    );

    let permission = crate::TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: crate::KernelPolicy {
            read_allowed: true,
            write_allowed: true,
            admin_allowed: false,
        },
        plugin_digest: None,
    };
    handle
        .start_turn_with_spec(
            "session-ap",
            "turn-ap",
            tools_turn_spec("更新订单", permission),
        )
        .expect("启动审批 Turn");
    assert!(sink.wait_for_paused("turn-ap", std::time::Duration::from_secs(2)));

    let events = sink.events();
    let approval = events
        .iter()
        .find_map(|event| match event {
            crate::AiEvent::ApprovalRequired {
                plan_hash, scope, ..
            } => Some((plan_hash.clone(), scope.clone())),
            _ => None,
        })
        .expect("应发出 ApprovalRequired");
    assert!(!approval.0.is_empty(), "plan_hash 必须非空");
    assert_eq!(approval.1.tool_id, "db.execute");
    assert!(
        !tools_has_started(&events),
        "审批前不得 ToolStarted: {events:?}"
    );
    assert!(
        tools_receipts(&events).is_empty(),
        "审批前不得产生任何工具回执: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(crate::AiEvent::Paused { .. })),
        "Paused 应是该 Turn 的终态事件: {events:?}"
    );
    assert_eq!(tool.call_count(), 0, "审批前绝不能真实执行写工具");

    // Paused 之后不应再出现该 turn 的迟到事件。
    let count_after_paused = events.len();
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_eq!(
        sink.events().len(),
        count_after_paused,
        "Paused 之后不得再有该 Turn 的事件"
    );

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

// 4) 审计 fail-closed 在 actor 生效：审计不可用 → 工具被拒、不执行，turn 正常收尾。
#[test]
fn ai_runtime_tools_audit_failure_fails_closed() {
    let provider = crate::ScriptedGenerationProvider::new(
        tools_support::tool_caps(),
        vec![
            tools_support::tool_call_stop("db.query", "call-a", r#"{"sql":"select 1"}"#),
            tools_support::text_stop("审计不可用，已停止"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = tools_support::catalog_with(tool.clone());
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime_with_tools(
        Box::new(provider),
        Box::new(sink.clone()),
        8,
        tools,
        Box::new(ToolsFailingAudit),
    );

    handle
        .start_turn_with_spec(
            "session-audit",
            "turn-audit",
            tools_turn_spec("查询", crate::TurnPermissionContext::default()),
        )
        .expect("启动审计失败 Turn");
    assert!(sink.wait_for_turn_finished("turn-audit", std::time::Duration::from_secs(2)));

    let events = sink.events();
    assert_eq!(
        tool.call_count(),
        0,
        "审计不可用时绝不能执行工具: {events:?}"
    );
    assert!(!tools_has_started(&events), "审计失败时不得 ToolStarted");
    let receipts = tools_receipts(&events);
    assert_eq!(receipts.len(), 1, "应有一个受控拒绝回执: {receipts:?}");
    assert_eq!(receipts[0].status, fluxdb_core::ToolReceiptStatus::Denied);
    assert_eq!(receipts[0].error_code.as_deref(), Some("audit_denied"));
    assert!(
        matches!(
            events.last(),
            Some(crate::AiEvent::Finished {
                reason: fluxdb_core::FinishReason::Stop,
                ..
            })
        ),
        "审计拒绝后模型应正常收尾: {events:?}"
    );

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

// 5) 长回合中途取消：终态 Finished(Cancelled)，且绝不成功执行写工具。
#[test]
fn ai_runtime_tools_cancel_mid_turn_finishes_cancelled() {
    let provider = std::sync::Arc::new(tools_support::CancellableBlockingProvider::new());
    let write_tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.execute",
        fluxdb_core::ToolRisk::Write,
    ));
    let tools = tools_support::catalog_with(write_tool.clone());
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime_with_tools(
        Box::new(tools_support::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        8,
        tools,
        Box::new(ToolsAllowAudit),
    );

    handle
        .start_turn_with_spec(
            "session-cancel",
            "turn-cancel",
            tools_turn_spec("长任务", crate::TurnPermissionContext::default()),
        )
        .expect("启动取消场景 Turn");
    assert!(provider.wait_started(std::time::Duration::from_secs(2)));
    handle.cancel("turn-cancel").expect("取消应通过旁路成功");
    assert!(sink.wait_for_turn_finished("turn-cancel", std::time::Duration::from_secs(3)));

    let events = sink.events();
    assert!(
        matches!(
            events.last(),
            Some(crate::AiEvent::Finished {
                reason: fluxdb_core::FinishReason::Cancelled,
                ..
            })
        ),
        "取消后的终态应为 Finished(Cancelled): {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, crate::AiEvent::Error { .. })),
        "取消不是错误，不应出现 Error: {events:?}"
    );
    assert!(!tools_has_started(&events), "取消期间不得 ToolStarted");
    assert_eq!(write_tool.call_count(), 0, "取消期间绝不能成功执行写工具");

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

// 6) 简易入口不暴露工具：模型请求 db.query 也不执行，拿到 unknown_tool 后能正常收尾。
#[test]
fn ai_runtime_tools_simple_entry_exposes_no_tools() {
    let provider = std::sync::Arc::new(crate::ScriptedGenerationProvider::new(
        tools_support::tool_caps(),
        vec![
            tools_support::tool_call_stop("db.query", "call-x", r#"{"sql":"select 1"}"#),
            tools_support::text_stop("已收到未知工具说明"),
        ],
    ));
    // 只作为"真实副作用探针"：绝不注册进 actor 空目录。
    let probe = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let sink = tools_support::RecordingSink::new();
    let mut handle = crate::spawn_ai_runtime(
        Box::new(tools_support::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        8,
    );

    handle
        .start_turn_with_spec(
            "session-simple",
            "turn-simple",
            tools_turn_spec("查询", crate::TurnPermissionContext::default()),
        )
        .expect("启动简易入口 Turn");
    assert!(sink.wait_for_turn_finished("turn-simple", std::time::Duration::from_secs(2)));

    let events = sink.events();
    assert!(!tools_has_started(&events), "简易入口不得暴露任何工具");
    assert!(
        tools_receipts(&events).is_empty(),
        "简易入口不应产生工具回执: {events:?}"
    );
    assert!(
        events.iter().any(|e| match e {
            crate::AiEvent::Notice { text, .. } => text.contains("未注册"),
            _ => false,
        }),
        "应提示工具未注册: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(crate::AiEvent::Finished {
                reason: fluxdb_core::FinishReason::Stop,
                ..
            })
        ),
        "模型拿到 unknown_tool 后应正常收尾: {events:?}"
    );

    let second = provider.request_at(1).expect("第二次请求");
    assert!(
        second
            .messages
            .iter()
            .any(|m| { m.role == fluxdb_core::ModelRole::Tool && m.text.contains("unknown_tool") }),
        "模型应收到 unknown_tool 的 tool 消息: {:?}",
        second.messages
    );
    assert_eq!(probe.call_count(), 0, "未注册的工具实现绝不能被调用");

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}
