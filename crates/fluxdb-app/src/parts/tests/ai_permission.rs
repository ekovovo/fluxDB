// 审批 / 安全审计 / 授权策略测试（设计 §25、§29、§42.11，T020）。
//
// 两条硬规则：
// 1. `ai_tool_audits` append-only：同一调用可追加多条事实，任何失败都不得落行。
// 2. `ai_approvals` 绑定 plan_hash 且走行级 CAS：重复点击不生效、撤销即时生效、
//    参数/目标/插件摘要变化即失效。
//
// 存储用例使用内存 SQLite + 真实 `ensure_schema`，并预置 Agent→会话→Turn→工具调用
// 以通过真实外键；策略用例只用纯函数 / 假 hook，不发网络、不用真实凭据。

use fluxdb_storage::ai_platform::audits;
use fluxdb_storage::ai_platform::{agents, ensure_schema, sessions};

/// 建一个带 Agent→会话→Turn 的隔离库（工具调用按需插入）。
fn ap_setup() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("内存库");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("外键");
    ensure_schema(&conn).expect("建表");
    agents::create_agent(
        &conn,
        1_000,
        &agents::AgentEdit {
            id: "agent-1".into(),
            display_name: "审批测试 Agent".into(),
            description: String::new(),
            enabled: true,
            default_model_profile_id: None,
            system_prompt: "测试".into(),
            budget_policy_id: None,
            allowed_tools: Vec::new(),
        },
    )
    .expect("建 Agent");
    sessions::create_session(&conn, 1_000, "sess-1", "agent-1", "会话").expect("建会话");
    sessions::start_turn(&conn, 1_000, "sess-1", "turn-1", "{}", "{}").expect("建 Turn");
    conn
}

/// 插入一条最小工具调用（满足 ai_tool_calls 的真实外键链）。
fn ap_tool_call(conn: &rusqlite::Connection, id: &str, ordinal: i64) {
    conn.execute(
        "INSERT INTO ai_tool_calls (id, session_id, turn_id, iteration, source_ordinal,
            provider_call_id, tool_id, tool_version, arguments_redacted_json, target_json,
            plan_hash, status)
         VALUES (?1,'sess-1','turn-1',1,?2,?1,'db.query',1,'{}','{}','init','prepared')",
        rusqlite::params![id, ordinal],
    )
    .expect("建工具调用");
}

/// 建一条最小审计编辑（默认绑定 tool_call_id）。
fn ap_audit_edit(
    id: &str,
    tool_call_id: Option<&str>,
    mcp_request_id: Option<&str>,
    stage: audits::AuditStage,
    plan_hash: &str,
    decision: audits::AuditDecision,
) -> audits::ToolAuditEdit {
    audits::ToolAuditEdit {
        id: id.to_string(),
        tool_call_id: tool_call_id.map(str::to_string),
        mcp_request_id: mcp_request_id.map(str::to_string),
        stage,
        hook_id: "hook.security".into(),
        hook_version: "1.0.0".into(),
        ruleset_version: "v1".into(),
        plan_hash: plan_hash.to_string(),
        decision,
        risk: "read".into(),
        rule_ids: vec![],
        reason_codes: vec![],
    }
}

// 1. 审计 append-only：同一 tool_call_id 追加两条事实，顺序按时间。
#[test]
fn ai_permission_audit_is_append_only_and_ordered() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-1", 0);
    audits::insert_tool_audit(
        &conn,
        1_000,
        &ap_audit_edit(
            "audit-1",
            Some("call-1"),
            None,
            audits::AuditStage::PreExecute,
            "h1",
            audits::AuditDecision::Allow,
        ),
    )
    .expect("第一条审计");
    audits::insert_tool_audit(
        &conn,
        2_000,
        &ap_audit_edit(
            "audit-2",
            Some("call-1"),
            None,
            audits::AuditStage::Revalidate,
            "h1",
            audits::AuditDecision::RequireApproval,
        ),
    )
    .expect("第二条审计(同 plan_hash 仍追加)");

    let rows = audits::list_tool_audits(&conn, "call-1").expect("查询审计");
    assert_eq!(rows.len(), 2, "同一调用的两条审计都必须保留");
    assert_eq!(rows[0].id, "audit-1");
    assert_eq!(rows[0].stage, audits::AuditStage::PreExecute);
    assert_eq!(rows[0].decision, audits::AuditDecision::Allow);
    assert_eq!(rows[1].id, "audit-2");
    assert_eq!(rows[1].stage, audits::AuditStage::Revalidate);
    assert_eq!(rows[1].decision, audits::AuditDecision::RequireApproval);
}

// 2. 审计归属校验：二选一 + plan_hash 非空，失败不落行。
#[test]
fn ai_permission_audit_ownership_validation_leaves_no_rows() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-1", 0);

    // 同时给两个归属 → 拒绝。
    let both = ap_audit_edit(
        "audit-both",
        Some("call-1"),
        Some("mcp-req-1"),
        audits::AuditStage::PreExecute,
        "h1",
        audits::AuditDecision::Allow,
    );
    assert!(
        audits::insert_tool_audit(&conn, 1_000, &both).is_err(),
        "两个归属都必须拒绝"
    );

    // 都不给 → 拒绝。
    let neither = ap_audit_edit(
        "audit-neither",
        None,
        None,
        audits::AuditStage::PreExecute,
        "h1",
        audits::AuditDecision::Allow,
    );
    assert!(
        audits::insert_tool_audit(&conn, 1_000, &neither).is_err(),
        "无归属必须拒绝"
    );

    // plan_hash 为空 → 拒绝。
    let empty_hash = ap_audit_edit(
        "audit-empty",
        Some("call-1"),
        None,
        audits::AuditStage::PreExecute,
        "   ",
        audits::AuditDecision::Allow,
    );
    assert!(
        audits::insert_tool_audit(&conn, 1_000, &empty_hash).is_err(),
        "空 plan_hash 必须拒绝"
    );

    let rows = audits::list_tool_audits(&conn, "call-1").expect("查询审计");
    assert_eq!(rows.len(), 0, "全部失败后不得有任何审计行");
}

// 3. 审批重复点击（行级 CAS）不二次生效。
#[test]
fn ai_permission_approval_decide_uses_row_cas() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-1", 0);
    let created =
        audits::create_approval(&conn, 1_000, "ap-1", "call-1", "h1", "{}", None).expect("建审批");
    assert_eq!(created.revision, 1);
    assert_eq!(created.status, audits::ApprovalStatus::Pending);

    let decided =
        audits::decide_approval(&conn, 2_000, "ap-1", true, Some("user_ok"), 1).expect("首次批准");
    assert_eq!(decided.status, audits::ApprovalStatus::Approved);
    assert_eq!(decided.revision, 2);

    let again = audits::decide_approval(&conn, 3_000, "ap-1", false, None, 1);
    assert!(again.is_err(), "过期 revision 的重复点击必须失败");
    assert!(
        again.unwrap_err().to_string().contains("审批状态已变化"),
        "错误应提示状态已变化"
    );
    let reloaded = audits::load_approval(&conn, "ap-1").expect("回读");
    assert_eq!(reloaded.status, audits::ApprovalStatus::Approved);
    assert_eq!(reloaded.revision, 2);
}

// 4. 撤销即时生效：已批准项被 invalidate 后立刻不可用。
#[test]
fn ai_permission_invalidate_takes_effect_immediately() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-1", 0);
    audits::create_approval(&conn, 1_000, "ap-1", "call-1", "h1", "{}", None).expect("建审批");
    audits::decide_approval(&conn, 2_000, "ap-1", true, None, 1).expect("批准");

    assert!(
        audits::find_valid_approval(&conn, 3_000, "call-1", "h1")
            .expect("查有效批准")
            .is_some(),
        "批准后应能查到有效审批"
    );

    audits::invalidate_approval(&conn, 4_000, "ap-1", "config_changed").expect("撤销");
    assert!(
        audits::find_valid_approval(&conn, 5_000, "call-1", "h1")
            .expect("查有效批准")
            .is_none(),
        "撤销后必须立刻失效"
    );
    let reloaded = audits::load_approval(&conn, "ap-1").expect("回读");
    assert_eq!(reloaded.status, audits::ApprovalStatus::Invalidated);
}

// 5. 过期 + plan 绑定：过期不可用；换了 hash 查不到旧批准。
#[test]
fn ai_permission_expiry_and_plan_binding_invalidate_approval() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-exp", 0);
    ap_tool_call(&conn, "call-plan", 1);

    audits::create_approval(&conn, 1_000, "ap-exp", "call-exp", "h1", "{}", Some(500))
        .expect("建审批(过期)");
    audits::decide_approval(&conn, 1_000, "ap-exp", true, None, 1).expect("批准");
    assert!(
        audits::find_valid_approval(&conn, 1_000, "call-exp", "h1")
            .expect("查有效批准")
            .is_none(),
        "即便状态是 approved，过期后也必须查不到"
    );

    audits::create_approval(&conn, 1_000, "ap-plan", "call-plan", "hash-A", "{}", None)
        .expect("建审批(plan A)");
    audits::decide_approval(&conn, 1_000, "ap-plan", true, None, 1).expect("批准");
    assert!(
        audits::find_valid_approval(&conn, 1_000, "call-plan", "hash-A")
            .expect("查有效批准")
            .is_some(),
        "同 plan_hash 应能查到"
    );
    assert!(
        audits::find_valid_approval(&conn, 1_000, "call-plan", "hash-B")
            .expect("查有效批准")
            .is_none(),
        "plan_hash 变化后旧批准必须失效"
    );
}

// 6. scope 必须是合法 JSON，非法时不落行。
#[test]
fn ai_permission_approval_scope_must_be_json() {
    let conn = ap_setup();
    ap_tool_call(&conn, "call-1", 0);
    let result = audits::create_approval(&conn, 1_000, "ap-1", "call-1", "h1", "not json", None);
    assert!(result.is_err(), "非法 scope_json 必须拒绝");
    let rows = audits::list_approvals_for_call(&conn, "call-1").expect("查审批");
    assert_eq!(rows.len(), 0, "非法 scope 不得落行");
}

/// 构造授权输入（默认读、策略全允许、白名单不限、已审计 Allow）。
fn ap_authorize_input() -> crate::AuthorizeInput {
    crate::AuthorizeInput {
        tool_id: "db.query".into(),
        tool_version: 1,
        risk: fluxdb_core::ToolRisk::Read,
        target_json: r#"{"connection":"c1","db":"app"}"#.into(),
        normalized_arguments_json: r#"{"sql":"select 1"}"#.into(),
        plugin_digest: None,
        agent_allowed_tools: vec![],
        policy: crate::KernelPolicy {
            read_allowed: true,
            write_allowed: false,
            admin_allowed: false,
        },
        audit: Some(crate::AuditOutcome::Allow),
    }
}

// 7. deny 优先：审计 Deny 不能被“全允许 + 白名单包含”覆盖。
#[test]
fn ai_permission_audit_deny_wins_over_everything() {
    use crate::PermissionDecision;
    let input = crate::AuthorizeInput {
        policy: crate::KernelPolicy {
            read_allowed: true,
            write_allowed: true,
            admin_allowed: true,
        },
        agent_allowed_tools: vec!["db.query".into()],
        audit: Some(crate::AuditOutcome::Deny),
        ..ap_authorize_input()
    };
    match crate::authorize(&input) {
        PermissionDecision::Deny {
            code, retryable, ..
        } => {
            assert_eq!(code, "audit_denied");
            assert!(!retryable, "权限类拒绝必须不可重试");
        }
        other => panic!("审计 Deny 必须直接拒绝，实际 {other:?}"),
    }
}

// 8. 写操作一律需要审批；只读连接则拒绝。
#[test]
fn ai_permission_write_requires_approval_or_denied_on_readonly() {
    use crate::PermissionDecision;
    let mut input = ap_authorize_input();
    input.risk = fluxdb_core::ToolRisk::Write;
    input.policy.write_allowed = true;
    match crate::authorize(&input) {
        PermissionDecision::RequireApproval { scope, .. } => {
            assert_eq!(scope.tool_id, "db.query");
            assert_eq!(scope.target_json, input.target_json);
        }
        other => panic!("写操作应要求审批，实际 {other:?}"),
    }

    input.policy.write_allowed = false;
    match crate::authorize(&input) {
        PermissionDecision::Deny {
            code, retryable, ..
        } => {
            assert_eq!(code, "write_not_allowed");
            assert!(!retryable);
        }
        other => panic!("只读连接写操作应拒绝，实际 {other:?}"),
    }
}

// 9. 管理类默认拒绝。
#[test]
fn ai_permission_admin_denied_by_default() {
    use crate::PermissionDecision;
    let mut input = ap_authorize_input();
    input.risk = fluxdb_core::ToolRisk::Admin;
    match crate::authorize(&input) {
        PermissionDecision::Deny { code, .. } => assert_eq!(code, "admin_not_allowed"),
        other => panic!("管理类默认应拒绝，实际 {other:?}"),
    }
}

// 10. 白名单交集：不在名单内拒绝；空名单不限制。
#[test]
fn ai_permission_agent_allowlist_is_intersection() {
    use crate::PermissionDecision;
    let mut input = ap_authorize_input();
    input.tool_id = "redis.scan".into();
    input.agent_allowed_tools = vec!["db.query".into()];
    match crate::authorize(&input) {
        PermissionDecision::Deny { code, .. } => assert_eq!(code, "tool_not_allowed"),
        other => panic!("白名单外工具应拒绝，实际 {other:?}"),
    }

    // 空名单 = 不限制；策略与审计均允许时应放行。
    input.agent_allowed_tools = vec![];
    assert!(
        crate::authorize(&input).is_allow(),
        "空白名单不应因白名单被拒"
    );
}

// 11. plan_hash 稳定性与敏感性：输入不变则相同，任一项变化则不同。
#[test]
fn ai_permission_plan_hash_stable_and_sensitive() {
    let base_target = r#"{"connection":"c1","db":"app"}"#;
    let base_args = r#"{"sql":"select 1"}"#;
    let h1 = crate::plan_hash("db.query", 1, base_target, base_args, Some("digest"));
    let h2 = crate::plan_hash("db.query", 1, base_target, base_args, Some("digest"));
    assert_eq!(h1, h2, "同输入必须同 hash");

    let args_changed = crate::plan_hash(
        "db.query",
        1,
        base_target,
        r#"{"sql":"select 2"}"#,
        Some("digest"),
    );
    let target_changed = crate::plan_hash(
        "db.query",
        1,
        r#"{"connection":"c2","db":"app"}"#,
        base_args,
        Some("digest"),
    );
    let digest_changed = crate::plan_hash("db.query", 1, base_target, base_args, Some("digest2"));
    let version_changed = crate::plan_hash("db.query", 2, base_target, base_args, Some("digest"));

    for (label, other) in [
        ("arguments", args_changed),
        ("target", target_changed),
        ("plugin_digest", digest_changed),
        ("tool_version", version_changed),
    ] {
        assert_ne!(h1, other, "{label} 变化必须换 hash");
    }
}

/// 执行时总是报错的审计 hook，用于验证 fail closed。
struct ApFailingHook;

impl crate::SecurityAuditHook for ApFailingHook {
    fn audit(&self, _input: &crate::AuditInput) -> fluxdb_core::Result<crate::AuditOutcome> {
        Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "审计服务不可用",
        ))
    }
}

// 12. fail closed：审计报错 → Deny；未审计（audit=None）不得放行。
#[test]
fn ai_permission_fail_closed_without_audit() {
    use crate::{AuditOutcome, PermissionDecision};
    let audit_input = crate::AuditInput {
        tool_id: "db.query".into(),
        tool_version: 1,
        plan_hash: "h1".into(),
        target_json: "{}".into(),
        normalized_arguments_json: "{}".into(),
        ruleset_version: "v1".into(),
    };
    assert_eq!(
        crate::audit_or_fail_closed(&ApFailingHook, &audit_input),
        AuditOutcome::Deny,
        "审计 hook 报错必须 fail closed"
    );

    let mut input = ap_authorize_input();
    input.audit = None;
    match crate::authorize(&input) {
        PermissionDecision::Allow { .. } => panic!("audit=None 绝不允许直接放行"),
        PermissionDecision::RequireApproval { .. } => {}
        PermissionDecision::Deny { .. } => {}
    }
}

// 13. approval_matches 语义：hash/状态/过期三者必须同时满足。
#[test]
fn ai_permission_approval_matches_semantics() {
    use fluxdb_storage::ai_platform::audits::ApprovalStatus;
    assert!(crate::approval_matches(
        "h1",
        "h1",
        ApprovalStatus::Approved,
        Some(10_000),
        5_000
    ));
    assert!(!crate::approval_matches(
        "h1",
        "h2",
        ApprovalStatus::Approved,
        Some(10_000),
        5_000
    ));
    assert!(!crate::approval_matches(
        "h1",
        "h1",
        ApprovalStatus::Denied,
        None,
        5_000
    ));
    assert!(!crate::approval_matches(
        "h1",
        "h1",
        ApprovalStatus::Invalidated,
        None,
        5_000
    ));
    assert!(!crate::approval_matches(
        "h1",
        "h1",
        ApprovalStatus::Approved,
        Some(5_000),
        5_000
    ));
}
