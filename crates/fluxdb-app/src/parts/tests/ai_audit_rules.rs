// 审计规则求值测试（设计 §25.1）：只验证规则语义与判定优先级，
// 不触达真实数据库、模型或凭据。

/// 构造审计输入；参数与目标都是普通 JSON，不含任何凭据。
fn rule_input(tool_id: &str, args: serde_json::Value) -> AuditInput {
    AuditInput {
        tool_id: tool_id.to_string(),
        tool_version: 1,
        plan_hash: "hash".into(),
        target_json: "{\"connection_id\":1}".into(),
        normalized_arguments_json: args.to_string(),
        ruleset_version: "test".into(),
    }
}

fn rule_sql(tool: &str, text: &str) -> AuditInput {
    rule_input(tool, serde_json::json!({"target": "conn:1", "sql": text}))
}

fn rules_from(json: serde_json::Value) -> AuditRules {
    let map = json.as_object().cloned().unwrap_or_default();
    AuditRules::from_config(&map)
}

#[test]
fn empty_rules_never_block_anything() {
    let rules = AuditRules::default();
    assert!(rules.is_empty());
    for tool in ["db.query", "db.execute", "redis.execute", "file.read"] {
        assert_eq!(
            rules.evaluate(&rule_sql(tool, "DELETE FROM orders")).outcome,
            AuditOutcome::Allow,
            "{tool} 在空规则集下必须放行"
        );
    }
}

#[test]
fn denied_tables_match_bare_and_qualified_names() {
    let rules = rules_from(serde_json::json!({"denied_tables": ["orders"]}));
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM orders")).outcome,
        AuditOutcome::Deny
    );
    assert_eq!(
        rules
            .evaluate(&rule_sql("db.query", "SELECT * FROM sales.orders"))
            .outcome,
        AuditOutcome::Deny
    );
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM items")).outcome,
        AuditOutcome::Allow
    );
}

#[test]
fn qualified_denied_table_requires_matching_schema() {
    let rules = rules_from(serde_json::json!({"denied_tables": ["sales.orders"]}));
    assert_eq!(
        rules
            .evaluate(&rule_sql("db.query", "SELECT * FROM sales.orders"))
            .outcome,
        AuditOutcome::Deny
    );
    assert_eq!(
        rules
            .evaluate(&rule_sql("db.query", "SELECT * FROM archive.orders"))
            .outcome,
        AuditOutcome::Allow
    );
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM orders")).outcome,
        AuditOutcome::Allow
    );
}

#[test]
fn allowlist_denies_tables_outside_it() {
    let rules = rules_from(serde_json::json!({"allowed_tables": ["orders", "items"]}));
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM orders")).outcome,
        AuditOutcome::Allow
    );
    let verdict = rules.evaluate(&rule_sql("db.query", "SELECT * FROM customers"));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_ALLOWED_TABLES));
    assert_eq!(
        rules
            .evaluate(&rule_sql(
                "db.query",
                "SELECT * FROM orders JOIN customers ON orders.id=customers.id"
            ))
            .outcome,
        AuditOutcome::Deny
    );
}

#[test]
fn allowlist_fails_closed_when_sql_cannot_be_parsed() {
    let rules = rules_from(serde_json::json!({"allowed_tables": ["orders"]}));
    let verdict = rules.evaluate(&rule_sql("db.query", "this is not sql at all"));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_ALLOWED_TABLES));
}

#[test]
fn protected_table_allows_read_but_denies_write() {
    let rules = rules_from(serde_json::json!({"protected_tables": ["customers"]}));
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM customers")).outcome,
        AuditOutcome::Allow
    );
    let verdict = rules.evaluate(&rule_sql(
        "db.execute",
        "UPDATE customers SET name='x' WHERE id=1",
    ));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_PROTECTED_TABLES));
}

#[test]
fn denied_columns_match_bare_and_qualified() {
    let rules = rules_from(serde_json::json!({"denied_columns": ["forbidden_col"]}));
    let verdict = rules.evaluate(&rule_sql(
        "db.query",
        "SELECT forbidden_col FROM app_users",
    ));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_DENIED_COLUMNS));
    // 表名限定形式：app_users.forbidden_col 命中，orders.forbidden_col 不命中。
    let qualified = rules_from(serde_json::json!({"denied_columns": ["app_users.forbidden_col"]}));
    assert_eq!(
        qualified
            .evaluate(&rule_sql(
                "db.query",
                "SELECT app_users.forbidden_col FROM app_users"
            ))
            .outcome,
        AuditOutcome::Deny
    );
    assert_eq!(
        qualified
            .evaluate(&rule_sql(
                "db.query",
                "SELECT orders.forbidden_col FROM orders"
            ))
            .outcome,
        AuditOutcome::Allow
    );
}

#[test]
fn forbid_ddl_blocks_structural_statements() {
    let rules = rules_from(serde_json::json!({"forbid_ddl": "true"}));
    for statement in [
        "CREATE TABLE t(id INTEGER)",
        "ALTER TABLE t ADD COLUMN c INTEGER",
        "DROP TABLE t",
        "TRUNCATE TABLE t",
        "GRANT SELECT ON t TO alice",
    ] {
        assert_eq!(
            rules.evaluate(&rule_sql("db.execute", statement)).outcome,
            AuditOutcome::Deny,
            "{statement} 应被 DDL 规则拒绝"
        );
    }
    assert_eq!(
        rules
            .evaluate(&rule_sql("db.execute", "DELETE FROM t WHERE id=1"))
            .outcome,
        AuditOutcome::Allow
    );
}

#[test]
fn write_requires_where_downgrades_to_approval() {
    let rules = rules_from(serde_json::json!({"write_requires_where": "true"}));
    let verdict = rules.evaluate(&rule_sql("db.execute", "DELETE FROM orders"));
    assert_eq!(verdict.outcome, AuditOutcome::RequireApproval);
    assert_eq!(verdict.rule_id, Some(RULE_WRITE_REQUIRES_WHERE));
    assert_eq!(
        rules
            .evaluate(&rule_sql("db.execute", "DELETE FROM orders WHERE id=1"))
            .outcome,
        AuditOutcome::Allow
    );
    assert_eq!(
        rules.evaluate(&rule_sql("db.query", "SELECT * FROM orders")).outcome,
        AuditOutcome::Allow
    );
}

#[test]
fn deny_outranks_require_approval_within_one_call() {
    let rules = rules_from(serde_json::json!({
        "write_requires_where": "true",
        "denied_tables": ["orders"]
    }));
    let verdict = rules.evaluate(&rule_sql("db.execute", "DELETE FROM orders"));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_DENIED_TABLES));
}

#[test]
fn redis_command_and_key_prefix_rules() {
    let rules = rules_from(serde_json::json!({
        "denied_redis_commands": ["SET"],
        "denied_redis_key_prefixes": ["prod:"]
    }));
    let verdict = rules.evaluate(&rule_input(
        "redis.execute",
        serde_json::json!({"target": "conn:1", "argv": ["set", "k", "v"]}),
    ));
    assert_eq!(verdict.outcome, AuditOutcome::Deny);
    assert_eq!(verdict.rule_id, Some(RULE_DENIED_REDIS_COMMANDS));
    let keyed = rules.evaluate(&rule_input(
        "redis.execute",
        serde_json::json!({"target": "conn:1", "argv": ["GET", "prod:abc:1"]}),
    ));
    assert_eq!(keyed.outcome, AuditOutcome::Deny);
    assert_eq!(keyed.rule_id, Some(RULE_DENIED_REDIS_KEY_PREFIXES));
    // 命中前缀时原因码只含前缀，不含完整 Key 名。
    let reason = keyed.reason_code.expect("应有原因码");
    assert!(!reason.contains("abc:1"), "原因码不应含完整 Key：{reason}");
}

#[test]
fn ruleset_version_changes_with_content() {
    let a = rules_from(serde_json::json!({"denied_tables": ["orders"]}));
    let b = rules_from(serde_json::json!({"denied_tables": ["items"]}));
    assert_ne!(a.version(), b.version(), "规则内容变化必须换版本");
    let c = rules_from(serde_json::json!({"denied_tables": ["orders"]}));
    assert_eq!(a.version(), c.version(), "同内容规则版本必须稳定");
}

#[test]
fn non_sql_tools_are_untouched_by_sql_rules() {
    let rules = rules_from(serde_json::json!({"denied_tables": ["orders"]}));
    assert_eq!(
        rules
            .evaluate(&rule_input("file.read", serde_json::json!({"file_ref": "orders"})))
            .outcome,
        AuditOutcome::Allow
    );
}
