// 主循环授权/安全语义测试（设计 §20 工具风险、§25 目标授权/审批、§29）。
// 子 Agent A：只做测试，不改生产代码。
//
// 只验证生产链路：`TurnRunner` → `ToolCatalog::describe/prepare`（schema 校验）
// → 强制审计（fail closed）→ `authorize`（白名单/风险/连接权限/审计求交集，deny 优先）
// → 执行（真实副作用计数）→ `project`（有界投影）。
//
// 用 scripted Provider + RecordingTool：不发网络、不读生产配置、不碰 SQLite。
// 每条断言"真实副作用次数"（`RecordingTool::call_count()`）而不是只看事件文案。

/// 固定结局审计 Hook（模拟内核审计返回 Allow / RequireApproval / Deny）。
struct AuthzFixedAudit(crate::AuditOutcome);

impl crate::SecurityAuditHook for AuthzFixedAudit {
    fn audit(&self, _input: &crate::AuditInput) -> fluxdb_core::Result<crate::AuditOutcome> {
        Ok(self.0)
    }
}

/// 审计不可用（模拟落盘/审计服务故障）：必须 fail closed。
struct AuthzFailingAudit;

impl crate::SecurityAuditHook for AuthzFailingAudit {
    fn audit(&self, _input: &crate::AuditInput) -> fluxdb_core::Result<crate::AuditOutcome> {
        Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "审计服务不可用（测试注入）",
        ))
    }
}

/// 走完整生产链路跑一轮 Turn，允许自定义权限上下文与审计 Hook。
fn authz_run(
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &crate::ToolCatalog,
    permission: crate::TurnPermissionContext,
    audit: &dyn crate::SecurityAuditHook,
    target_json: &str,
) -> (crate::TurnOutcome, Vec<crate::LoopEvent>) {
    let runner = crate::TurnRunner {
        provider,
        tools,
        limits: crate::LoopLimits::default(),
        permission,
        audit,
    };
    let state = crate::TurnState {
        model: "m".to_string(),
        messages: vec![ai_loop_test_support::user_message("授权测试")],
        max_output_tokens: Some(64),
        target_json: target_json.to_string(),
    };
    let mut steering = Vec::new();
    let cancel = fluxdb_core::CancelFlag::new();
    let mut events = Vec::new();
    let outcome = runner.run(state, &mut steering, &cancel, &mut |e| events.push(e));
    (outcome, events)
}

/// 内核记录的授权/风险提示（`Notice`）原文集合，便于断言被拒绝的原因。
fn authz_notices(events: &[crate::LoopEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            crate::LoopEvent::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

/// 全部工具回执（真实产出或是内核合成的拒绝回执）。
fn authz_receipts(events: &[crate::LoopEvent]) -> Vec<fluxdb_core::ToolReceipt> {
    events
        .iter()
        .filter_map(|e| match e {
            crate::LoopEvent::ToolFinished { receipt } => Some(receipt.clone()),
            _ => None,
        })
        .collect()
}

/// `ApprovalRequired` 事件（plan_hash + scope）。
fn authz_approvals(events: &[crate::LoopEvent]) -> Vec<(String, crate::ApprovalScope)> {
    events
        .iter()
        .filter_map(|e| match e {
            crate::LoopEvent::ApprovalRequired { plan_hash, scope, .. } => {
                Some((plan_hash.clone(), scope.clone()))
            }
            _ => None,
        })
        .collect()
}

fn authz_default_audit_allow() -> AuthzFixedAudit {
    AuthzFixedAudit(crate::AuditOutcome::Allow)
}

/// 构造"严格 schema"工具目录：`additionalProperties:false` + `required:["sql"]`。
fn authz_strict_catalog(
    id: &str,
    tool: std::sync::Arc<crate::RecordingTool>,
) -> crate::ToolCatalog {
    let spec = crate::ToolSpec {
        id: id.to_string(),
        version: 1,
        description: format!("strict {id}"),
        input_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["sql"],
            "properties": { "sql": { "type": "string" } }
        }),
        output_schema: serde_json::json!({ "type": "object" }),
        risk: fluxdb_core::ToolRisk::Read,
        effect: crate::EffectClass::PureRead,
        timeout_ms: 30_000,
        output_limit_bytes: 128 * 1024,
    };
    let mut catalog = crate::ToolCatalog::new(vec![spec]);
    catalog
        .register(Box::new(ai_loop_test_support::SharedRecordingTool(tool)))
        .expect("register strict tool");
    catalog
}

// 1) 写工具默认被拒且不执行（KernelPolicy::default() → write_allowed=false）。
#[test]
fn ai_loop_authz_write_denied_by_default_policy_without_execution() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop(
                "db.execute",
                "call-w",
                r#"{"sql":"delete from orders"}"#,
            ),
            ai_loop_test_support::text_stop("已放弃写操作"),
        ],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.execute",
        fluxdb_core::ToolRisk::Write,
    ));
    let tools = ai_loop_test_support::catalog_with_shared_tool(tool.clone());

    let (outcome, events) = authz_run(
        &provider,
        &tools,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &authz_default_audit_allow(),
        r#"{"connection_id":1}"#,
    );

    let receipts = authz_receipts(&events);
    let denied: Vec<_> = receipts
        .iter()
        .filter(|r| r.status == fluxdb_core::ToolReceiptStatus::Denied)
        .collect();
    assert_eq!(
        denied.len(),
        1,
        "应有且仅有一个 Denied 回执，实际 {receipts:?}"
    );
    assert_eq!(denied[0].tool_id, "db.execute");
    assert!(
        !receipts
            .iter()
            .any(|r| r.status == fluxdb_core::ToolReceiptStatus::Succeeded),
        "写工具在默认策略下不得出现成功回执，实际 {receipts:?}"
    );
    assert_eq!(tool.call_count(), 0, "被拒的写工具绝不能真实执行");
    assert!(
        !matches!(outcome, crate::TurnOutcome::Failed { .. }),
        "拒绝是受控结果而非基础设施失败，实际 {outcome:?}"
    );
}

// 2) 写工具需要审批且审批前绝不执行。
#[test]
fn ai_loop_authz_write_requires_approval_without_execution() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![ai_loop_test_support::tool_call_stop(
            "db.execute",
            "call-w",
            r#"{"sql":"update orders set x=1"}"#,
        )],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.execute",
        fluxdb_core::ToolRisk::Write,
    ));
    let tools = ai_loop_test_support::catalog_with_shared_tool(tool.clone());

    let (outcome, events) = authz_run(
        &provider,
        &tools,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy {
                read_allowed: true,
                write_allowed: true,
                admin_allowed: false,
            },
            plugin_digest: None,
        },
        &authz_default_audit_allow(),
        r#"{"connection_id":1}"#,
    );

    match outcome {
        crate::TurnOutcome::Paused { .. } => {}
        other => panic!("写工具应暂停等审批，实际 {other:?}"),
    }
    let approvals = authz_approvals(&events);
    assert_eq!(
        approvals.len(),
        1,
        "应有且仅有一个 ApprovalRequired，实际 {events:?}"
    );
    let (plan_hash, scope) = &approvals[0];
    assert!(!plan_hash.is_empty(), "plan_hash 不能为空");
    assert_eq!(scope.tool_id, "db.execute", "审批范围必须指向真实工具");
    assert_eq!(tool.call_count(), 0, "审批前绝不能执行写工具");
    assert!(
        authz_receipts(&events).is_empty(),
        "审批前不得产生任何工具回执"
    );
}

// 3) Agent 白名单：不在名单 → Denied；在名单 → Read 工具真实执行一次。
#[test]
fn ai_loop_authz_agent_whitelist_deny_and_allow() {
    // 3a) 白名单不含 db.query → 拒绝且不执行。
    let provider_deny = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            ai_loop_test_support::text_stop("改走其它路径"),
        ],
    );
    let tool_deny = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools_deny = ai_loop_test_support::catalog_with_shared_tool(tool_deny.clone());
    let (_outcome, events) = authz_run(
        &provider_deny,
        &tools_deny,
        crate::TurnPermissionContext {
            agent_allowed_tools: vec!["db.other".to_string()],
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &authz_default_audit_allow(),
        r#"{"connection_id":1}"#,
    );
    let receipts = authz_receipts(&events);
    assert_eq!(receipts.len(), 1, "白名单外应有拒绝回执，实际 {receipts:?}");
    assert_eq!(receipts[0].status, fluxdb_core::ToolReceiptStatus::Denied);
    assert_eq!(tool_deny.call_count(), 0, "白名单外工具绝不能执行");

    // 3b) 白名单含 db.query → Read 工具真实执行一次。
    let provider_allow = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            ai_loop_test_support::text_stop("完成"),
        ],
    );
    let tool_allow = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools_allow = ai_loop_test_support::catalog_with_shared_tool(tool_allow.clone());
    let (_outcome, events) = authz_run(
        &provider_allow,
        &tools_allow,
        crate::TurnPermissionContext {
            agent_allowed_tools: vec!["db.query".to_string()],
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &authz_default_audit_allow(),
        r#"{"connection_id":1}"#,
    );
    let receipts = authz_receipts(&events);
    assert_eq!(receipts.len(), 1, "白名单内应有真实回执");
    assert_eq!(
        receipts[0].status,
        fluxdb_core::ToolReceiptStatus::Succeeded
    );
    assert_eq!(tool_allow.call_count(), 1, "白名单内工具应真实执行一次");
}

// 4) schema 校验在链路生效：多余字段 / 缺必填 → 不执行，且下一轮 tool 消息含 invalid_arguments。
#[test]
fn ai_loop_authz_schema_validation_blocks_execution() {
    for (label, args) in [
        ("多余字段", r#"{"sql":"select 1","extra":1}"#),
        ("缺必填字段", r#"{"nope":1}"#),
    ] {
        let provider = crate::ScriptedGenerationProvider::new(
            fluxdb_core::ModelCapabilities {
                streaming: true,
                tool_calling: true,
                reasoning_summary: false,
            },
            vec![
                ai_loop_test_support::tool_call_stop("db.query", "call-1", args),
                ai_loop_test_support::text_stop("已修正参数"),
            ],
        );
        let tool = std::sync::Arc::new(crate::RecordingTool::new(
            "db.query",
            fluxdb_core::ToolRisk::Read,
        ));
        let tools = authz_strict_catalog("db.query", tool.clone());

        let (_outcome, events) = authz_run(
            &provider,
            &tools,
            crate::TurnPermissionContext {
                agent_allowed_tools: Vec::new(),
                policy: crate::KernelPolicy::default(),
                plugin_digest: None,
            },
            &authz_default_audit_allow(),
            r#"{"connection_id":1}"#,
        );

        assert_eq!(tool.call_count(), 0, "{label}: schema 未通过不得执行工具");
        assert!(
            authz_receipts(&events).is_empty(),
            "{label}: schema 未通过不得产生工具回执"
        );
        let second = provider.request_at(1).expect("{label}: 应有第 2 次请求");
        let texts = ai_loop_test_support::tool_message_texts(&second);
        assert!(
            texts.iter().any(|t| t.contains("invalid_arguments")),
            "{label}: 下一轮 tool 消息应含受控错误码 invalid_arguments，实际 {texts:?}"
        );
    }
}

// 5) 审计失败 = fail closed：审计 Err 与审计 Deny 都必须拒绝且不执行。
#[test]
fn ai_loop_authz_audit_failure_and_deny_fail_closed() {
    // 5a) 审计 Hook 返回 Err。
    let provider_err = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            ai_loop_test_support::text_stop("审计不可用，已停止"),
        ],
    );
    let tool_err = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools_err = ai_loop_test_support::catalog_with_shared_tool(tool_err.clone());
    let (_outcome, events) = authz_run(
        &provider_err,
        &tools_err,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &AuthzFailingAudit,
        r#"{"connection_id":1}"#,
    );
    let receipts = authz_receipts(&events);
    assert_eq!(receipts.len(), 1, "审计失败应有拒绝回执，实际 {receipts:?}");
    assert_eq!(receipts[0].status, fluxdb_core::ToolReceiptStatus::Denied);
    assert_eq!(tool_err.call_count(), 0, "审计失败绝不能执行工具");

    // 5b) 审计 Hook 明确 Deny。
    let provider_deny = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            ai_loop_test_support::text_stop("审计拒绝，已停止"),
        ],
    );
    let tool_deny = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools_deny = ai_loop_test_support::catalog_with_shared_tool(tool_deny.clone());
    let (_outcome, events) = authz_run(
        &provider_deny,
        &tools_deny,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &AuthzFixedAudit(crate::AuditOutcome::Deny),
        r#"{"connection_id":1}"#,
    );
    let receipts = authz_receipts(&events);
    assert_eq!(receipts.len(), 1, "审计拒绝应有拒绝回执，实际 {receipts:?}");
    assert_eq!(receipts[0].status, fluxdb_core::ToolReceiptStatus::Denied);
    assert_eq!(tool_deny.call_count(), 0, "审计拒绝绝不能执行工具");
}

// 6) 审计 RequireApproval 对 Read 工具同样触发审批且不执行。
#[test]
fn ai_loop_authz_audit_require_approval_on_read() {
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![ai_loop_test_support::tool_call_stop(
            "db.query",
            "call-1",
            r#"{"sql":"select 1"}"#,
        )],
    );
    let tool = std::sync::Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let tools = ai_loop_test_support::catalog_with_shared_tool(tool.clone());

    let (outcome, events) = authz_run(
        &provider,
        &tools,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &AuthzFixedAudit(crate::AuditOutcome::RequireApproval),
        r#"{"connection_id":1}"#,
    );

    match outcome {
        crate::TurnOutcome::Paused { .. } => {}
        other => panic!("Read 工具被审计要求审批时应暂停，实际 {other:?}"),
    }
    let approvals = authz_approvals(&events);
    assert_eq!(approvals.len(), 1, "应有 ApprovalRequired，实际 {events:?}");
    assert_eq!(approvals[0].1.tool_id, "db.query");
    assert_eq!(tool.call_count(), 0, "审批前绝不能执行");
    assert!(authz_receipts(&events).is_empty(), "审批前不得有回执");
}

// 7) plan hash 失效语义：参数/目标/版本/插件摘要任一变化 → hash 变；审批复核必须完全一致。
#[test]
fn ai_loop_authz_plan_hash_and_approval_binding() {
    use fluxdb_storage::ai_platform::audits::ApprovalStatus;

    let base = crate::plan_hash(
        "db.query",
        1,
        r#"{"connection_id":1}"#,
        r#"{"sql":"select 1"}"#,
        None,
    );
    // 稳定：同一输入必须同 hash。
    assert_eq!(
        base,
        crate::plan_hash(
            "db.query",
            1,
            r#"{"connection_id":1}"#,
            r#"{"sql":"select 1"}"#,
            None
        )
    );
    // 参数变。
    assert_ne!(
        base,
        crate::plan_hash(
            "db.query",
            1,
            r#"{"connection_id":1}"#,
            r#"{"sql":"select 2"}"#,
            None
        ),
        "参数变化必须换 hash"
    );
    // target 变。
    assert_ne!(
        base,
        crate::plan_hash(
            "db.query",
            1,
            r#"{"connection_id":2}"#,
            r#"{"sql":"select 1"}"#,
            None
        ),
        "目标变化必须换 hash"
    );
    // 版本变。
    assert_ne!(
        base,
        crate::plan_hash(
            "db.query",
            2,
            r#"{"connection_id":1}"#,
            r#"{"sql":"select 1"}"#,
            None
        ),
        "工具版本变化必须换 hash"
    );
    // 插件摘要变。
    assert_ne!(
        base,
        crate::plan_hash(
            "db.query",
            1,
            r#"{"connection_id":1}"#,
            r#"{"sql":"select 1"}"#,
            Some("digest-a")
        ),
        "插件摘要变化必须换 hash"
    );

    // 全等 + 未过期 + 已批准 → 匹配。
    assert!(crate::approval_matches(
        &base,
        &base,
        ApprovalStatus::Approved,
        Some(2_000),
        1_000
    ));
    assert!(crate::approval_matches(
        &base,
        &base,
        ApprovalStatus::Approved,
        None,
        1_000
    ));
    // hash 不等 → 不匹配。
    assert!(!crate::approval_matches(
        "deadbeef",
        &base,
        ApprovalStatus::Approved,
        None,
        1_000
    ));
    // 状态非 Approved → 不匹配（表驱动）。
    for status in [
        ApprovalStatus::Pending,
        ApprovalStatus::Denied,
        ApprovalStatus::Expired,
        ApprovalStatus::Invalidated,
    ] {
        assert!(
            !crate::approval_matches(&base, &base, status, None, 1_000),
            "状态 {status:?} 不得通过审批复核"
        );
    }
    // 过期（expires_at <= now）→ 不匹配。
    assert!(!crate::approval_matches(
        &base,
        &base,
        ApprovalStatus::Approved,
        Some(1_000),
        1_000
    ));
    assert!(!crate::approval_matches(
        &base,
        &base,
        ApprovalStatus::Approved,
        Some(999),
        1_000
    ));
}

// 8) 投影有界且不改回执：大结果只截断回给模型，ToolFinished 回执保持完整原文。
#[test]
fn ai_loop_authz_projection_is_bounded_and_receipt_intact() {
    // 200KB 结果载荷（超过 ToolSpec 默认 128KiB 输出上限）。
    let big_result = format!(r#"{{"rows":"{}"}}"#, "x".repeat(200 * 1024));
    let provider = crate::ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            ai_loop_test_support::tool_call_stop("db.query", "call-1", r#"{"sql":"select 1"}"#),
            ai_loop_test_support::text_stop("已看到截断结果"),
        ],
    );
    let tool = std::sync::Arc::new(
        crate::RecordingTool::new("db.query", fluxdb_core::ToolRisk::Read).with_result(&big_result),
    );
    let tools = ai_loop_test_support::catalog_with_shared_tool(tool.clone());

    let (_outcome, events) = authz_run(
        &provider,
        &tools,
        crate::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: crate::KernelPolicy::default(),
            plugin_digest: None,
        },
        &authz_default_audit_allow(),
        r#"{"connection_id":1}"#,
    );

    assert_eq!(tool.call_count(), 1, "只读工具应真实执行一次");

    // 回执必须是完整原文（投影不得改写事实）。
    let receipts = authz_receipts(&events);
    assert_eq!(receipts.len(), 1, "应有一个工具回执");
    assert_eq!(
        receipts[0].result_json.len(),
        big_result.len(),
        "ToolFinished 回执必须保持完整原文"
    );

    // 模型侧 tool 消息被截断并带 truncated:true。
    let second = provider.request_at(1).expect("应有第 2 次请求");
    let texts = ai_loop_test_support::tool_message_texts(&second);
    let tool_text = texts
        .iter()
        .find(|t| t.contains("\"tool\":\"db.query\""))
        .expect("下一轮应含 db.query 的 tool 消息");
    assert!(
        tool_text.contains("\"truncated\":true"),
        "投影必须标记 truncated:true"
    );
    assert!(
        tool_text.len() < big_result.len(),
        "回给模型的投影必须小于完整结果（有界），实际 {} vs {}",
        tool_text.len(),
        big_result.len()
    );

    // 授权链给模型的拒绝/放行提示也应存在（本例为放行，不应出现授权 Notice）。
    assert!(
        !authz_notices(&events).iter().any(|n| n.contains("拒绝")),
        "放行的只读工具不应产生拒绝提示，实际 {:?}",
        authz_notices(&events)
    );
}
