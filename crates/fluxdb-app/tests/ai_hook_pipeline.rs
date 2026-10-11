//! Hook 顺序/冲突/禁用/安全边界测试；不调用模型或用户配置。
use fluxdb_app::{AgentHook, AgentHookDecision, AgentHookInput, HookBinding, HookPipeline};
use fluxdb_core::{CancelFlag, ToolCallRequest};
use std::sync::{Arc, Mutex};
struct Hook {
    id: String,
    trace: Arc<Mutex<Vec<String>>>,
    patch: Option<String>,
    deny: bool,
}
impl AgentHook for Hook {
    fn invoke(&self, _: &AgentHookInput, _: &CancelFlag) -> fluxdb_core::Result<AgentHookDecision> {
        self.trace.lock().unwrap().push(self.id.clone());
        Ok(if self.deny {
            AgentHookDecision::Deny("拒绝".into())
        } else if let Some(raw) = &self.patch {
            AgentHookDecision::Arguments(raw.clone())
        } else {
            AgentHookDecision::Observe
        })
    }
}
fn binding(
    id: &str,
    priority: i64,
    trace: Arc<Mutex<Vec<String>>>,
    patch: Option<&str>,
) -> HookBinding {
    HookBinding {
        id: id.into(),
        priority,
        enabled: true,
        best_effort: false,
        hook: Box::new(Hook {
            id: id.into(),
            trace,
            patch: patch.map(str::to_owned),
            deny: false,
        }),
    }
}
fn call() -> ToolCallRequest {
    ToolCallRequest {
        provider_call_id: "call".into(),
        tool_id: "db.query".into(),
        arguments_json: r#"{"target":"conn:1","sql":"select 1"}"#.into(),
    }
}
#[test]
fn stable_priority_and_id_order_and_disabled_zero_calls() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut disabled = binding("disabled", 0, trace.clone(), None);
    disabled.enabled = false;
    let hooks = HookPipeline::new(vec![
        binding("z", 2, trace.clone(), None),
        binding("b", 1, trace.clone(), None),
        binding("a", 1, trace.clone(), None),
        disabled,
    ]);
    hooks.before(&call(), &CancelFlag::new()).unwrap();
    assert_eq!(*trace.lock().unwrap(), vec!["a", "b", "z"]);
}
#[test]
fn conflicting_patches_fail_closed() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let hooks = HookPipeline::new(vec![
        binding("a", 0, trace.clone(), Some("{}")),
        binding("b", 1, trace, Some(r#"{"sql":"select 2"}"#)),
    ]);
    assert!(hooks.before(&call(), &CancelFlag::new()).is_err());
}
#[test]
fn deny_kept_despite_patch_and_after_cannot_mutate_receipt() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut deny = binding("deny", 0, trace.clone(), None);
    deny.hook = Box::new(Hook {
        id: "deny".into(),
        trace: trace.clone(),
        patch: None,
        deny: true,
    });
    let hooks = HookPipeline::new(vec![deny, binding("patch", 1, trace, Some("{}"))]);
    assert!(
        hooks
            .before(&call(), &CancelFlag::new())
            .unwrap()
            .1
            .is_some()
    );
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "call".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Failed,
        result_json: "{}".into(),
        error_code: Some("failed".into()),
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 0,
    };
    assert!(hooks.after(&receipt, &CancelFlag::new()).is_err());
    assert!(!receipt.is_success());
}
#[test]
fn cancel_rejects_without_invoking_binding() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let hooks = HookPipeline::new(vec![binding("a", 0, trace.clone(), None)]);
    let cancel = CancelFlag::new();
    cancel.request();
    assert!(hooks.before(&call(), &cancel).is_err());
    assert!(trace.lock().unwrap().is_empty());
}

/// Hook 内部再派发 Hook（哪怕换一条 pipeline）必须被拒绝，防止递归。
struct NestedHook;

impl AgentHook for NestedHook {
    fn invoke(
        &self,
        _: &AgentHookInput,
        cancel: &CancelFlag,
    ) -> fluxdb_core::Result<AgentHookDecision> {
        let inner = HookPipeline::new(vec![binding(
            "inner",
            0,
            Arc::new(Mutex::new(Vec::new())),
            None,
        )]);
        inner.before(&call(), cancel)?;
        Ok(AgentHookDecision::Observe)
    }
}

#[test]
fn recursive_hook_dispatch_is_rejected() {
    let hooks = HookPipeline::new(vec![HookBinding {
        id: "outer".into(),
        priority: 0,
        enabled: true,
        best_effort: false,
        hook: Box::new(NestedHook),
    }]);
    let error = hooks
        .before(&call(), &CancelFlag::new())
        .expect_err("递归派发必须拒绝");
    assert!(error.message.contains("递归"), "实际: {}", error.message);
}

/// 超过单次调用时限的 Hook 必须失败，而不是让主循环继续等待。
struct SlowHook;

impl AgentHook for SlowHook {
    fn invoke(&self, _: &AgentHookInput, _: &CancelFlag) -> fluxdb_core::Result<AgentHookDecision> {
        std::thread::sleep(std::time::Duration::from_millis(150));
        Ok(AgentHookDecision::Observe)
    }
}

#[test]
fn hook_over_deadline_fails_instead_of_blocking_the_loop() {
    let hooks = HookPipeline::new(vec![HookBinding {
        id: "slow".into(),
        priority: 0,
        enabled: true,
        best_effort: false,
        hook: Box::new(SlowHook),
    }]);
    let error = hooks
        .before(&call(), &CancelFlag::new())
        .expect_err("超时 Hook 必须失败");
    assert!(
        error.message.contains("执行时限"),
        "实际: {}",
        error.message
    );
}

/// 失败后会返回错误的 Hook，用于验证 best_effort 的降级边界。
struct FailingHook;

impl AgentHook for FailingHook {
    fn invoke(&self, _: &AgentHookInput, _: &CancelFlag) -> fluxdb_core::Result<AgentHookDecision> {
        Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "hook 失败",
        ))
    }
}

#[test]
fn best_effort_failure_degrades_while_strict_failure_propagates() {
    let strict = HookPipeline::new(vec![HookBinding {
        id: "strict".into(),
        priority: 0,
        enabled: true,
        best_effort: false,
        hook: Box::new(FailingHook),
    }]);
    assert!(
        strict.before(&call(), &CancelFlag::new()).is_err(),
        "非 best_effort 的 Hook 失败必须向上传播"
    );

    let degrade = HookPipeline::new(vec![HookBinding {
        id: "optional".into(),
        priority: 0,
        enabled: true,
        best_effort: true,
        hook: Box::new(FailingHook),
    }]);
    degrade
        .before(&call(), &CancelFlag::new())
        .expect("best_effort 的 Hook 失败应降级跳过");
}
