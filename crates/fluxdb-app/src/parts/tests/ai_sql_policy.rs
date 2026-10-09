use super::{
    analyze_sql, route_for_sql, SqlDialectKind, SqlRisk, DB_EXECUTE_INPUT_SCHEMA,
    DB_EXPLAIN_INPUT_SCHEMA, DB_QUERY_INPUT_SCHEMA,
};

fn ai_sql_policy_assert_requires(sql: &str, dialect: SqlDialectKind) -> Vec<String> {
    match analyze_sql(sql, dialect) {
        SqlRisk::RequiresAuthorization { reasons } => reasons,
        other => panic!("expected RequiresAuthorization for {sql:?}, got {other:?}"),
    }
}

fn ai_sql_policy_assert_rejected(sql: &str, dialect: SqlDialectKind) -> Vec<String> {
    match analyze_sql(sql, dialect) {
        SqlRisk::Rejected { reasons } => reasons,
        other => panic!("expected Rejected for {sql:?}, got {other:?}"),
    }
}

fn ai_sql_policy_assert_reason_contains(reasons: &[String], expected: &str) {
    assert!(
        reasons.iter().any(|reason| reason == expected),
        "expected reason {expected:?}, got {reasons:?}"
    );
}

fn ai_sql_policy_assert_reason_prefix(reasons: &[String], prefix: &str) {
    assert!(
        reasons.iter().any(|reason| reason.starts_with(prefix)),
        "expected reason prefix {prefix:?}, got {reasons:?}"
    );
}

#[test]
fn ai_sql_policy_read_only_queries_are_classified_per_dialect() {
    let dialects = [
        SqlDialectKind::MySql,
        SqlDialectKind::Postgres,
        SqlDialectKind::Sqlite,
    ];

    for dialect in dialects {
        assert_eq!(
            analyze_sql("SELECT * FROM orders", dialect),
            SqlRisk::ReadOnly,
            "dialect {dialect:?}"
        );
        assert_eq!(
            analyze_sql(
                "SELECT o.id, COUNT(*) AS c \
                 FROM orders o JOIN customers c ON c.id = o.customer_id \
                 WHERE o.total > 0 GROUP BY o.id ORDER BY c DESC LIMIT 10",
                dialect,
            ),
            SqlRisk::ReadOnly,
            "dialect {dialect:?}"
        );
        assert_eq!(
            analyze_sql("WITH t AS (SELECT 1) SELECT * FROM t", dialect),
            SqlRisk::ReadOnly,
            "dialect {dialect:?}"
        );
    }

    assert_eq!(
        analyze_sql("EXPLAIN SELECT * FROM orders", SqlDialectKind::Postgres),
        SqlRisk::ReadOnly
    );
}

#[test]
fn ai_sql_policy_modifying_ctes_require_authorization() {
    let cases = [
        (
            "WITH d AS (DELETE FROM orders RETURNING id) SELECT * FROM d",
            "modifying_cte_delete",
        ),
        (
            "WITH i AS (INSERT INTO orders(id) VALUES(1) RETURNING id) SELECT * FROM i",
            "modifying_cte_insert",
        ),
        (
            "WITH u AS (UPDATE orders SET total = total + 1 RETURNING id) SELECT * FROM u",
            "modifying_cte_update",
        ),
        (
            "WITH m AS (MERGE INTO orders o USING new_orders n ON o.id = n.id \
             WHEN MATCHED THEN UPDATE SET total = n.total RETURNING o.id) SELECT * FROM m",
            "modifying_cte_merge",
        ),
    ];

    for (sql, expected) in cases {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_contains(&reasons, expected);
    }
}

#[test]
fn ai_sql_policy_select_into_requires_authorization() {
    let reasons = ai_sql_policy_assert_requires(
        "SELECT * INTO new_table FROM orders",
        SqlDialectKind::Postgres,
    );
    ai_sql_policy_assert_reason_contains(&reasons, "select_into");
}

#[test]
fn ai_sql_policy_side_effect_functions_require_authorization() {
    let mysql_cases = [
        "SELECT SLEEP(1)",
        "SELECT BENCHMARK(1000,MD5('x'))",
        "SELECT GET_LOCK('a',1)",
        "SELECT LOAD_FILE('/etc/passwd')",
    ];
    let postgres_cases = [
        "SELECT pg_sleep(1)",
        "SELECT pg_read_file('/etc/passwd')",
        "SELECT nextval('s')",
    ];

    for sql in mysql_cases {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::MySql);
        ai_sql_policy_assert_reason_prefix(&reasons, "side_effect_marker:");
    }
    for sql in postgres_cases {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "side_effect_marker:");
    }
}

#[test]
fn ai_sql_policy_locking_reads_require_authorization() {
    let cases = [
        ("SELECT * FROM orders FOR UPDATE", SqlDialectKind::Postgres),
        ("SELECT * FROM orders FOR SHARE", SqlDialectKind::Postgres),
        (
            "SELECT * FROM orders LOCK IN SHARE MODE",
            SqlDialectKind::MySql,
        ),
    ];

    for (sql, dialect) in cases {
        let reasons = ai_sql_policy_assert_requires(sql, dialect);
        assert!(
            !reasons.is_empty(),
            "locking read must explain why it is gated"
        );
    }
}

#[test]
fn ai_sql_policy_explain_analyze_requires_authorization() {
    let reasons = ai_sql_policy_assert_requires(
        "EXPLAIN ANALYZE SELECT * FROM orders",
        SqlDialectKind::Postgres,
    );
    ai_sql_policy_assert_reason_contains(&reasons, "explain_analyze");

    // sqlparser 当前对 PostgreSQL 的括号形式支持有限；解析失败必须降级为
    // unknown_syntax，不能误报为只读。
    let reasons = ai_sql_policy_assert_requires(
        "EXPLAIN (ANALYZE) SELECT * FROM orders",
        SqlDialectKind::Postgres,
    );
    assert!(
        reasons.iter().any(|reason| reason == "explain_analyze"),
        "expected explain_analyze or unknown_syntax, got {reasons:?}"
    );
}

#[test]
fn ai_sql_policy_write_and_ddl_statements_require_authorization() {
    let cases = [
        "INSERT INTO orders(id) VALUES(1)",
        "UPDATE orders SET total = 1",
        "DELETE FROM orders",
        "CREATE TABLE tmp_orders(id INTEGER)",
        "DROP TABLE orders",
        "TRUNCATE TABLE orders",
        "GRANT SELECT ON orders TO alice",
    ];

    for sql in cases {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "non_read_statement:");
    }
}

#[test]
fn ai_sql_policy_transaction_and_session_control_are_rejected() {
    let cases = [
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT s",
        "SET search_path TO x",
    ];

    for sql in cases {
        let reasons = ai_sql_policy_assert_rejected(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_contains(&reasons, "transaction_or_session_control");
    }
}

#[test]
fn ai_sql_policy_multi_statement_and_empty_input_are_rejected() {
    let reasons = ai_sql_policy_assert_rejected("SELECT 1; SELECT 2", SqlDialectKind::Postgres);
    ai_sql_policy_assert_reason_contains(&reasons, "multi_statement:2");

    assert_eq!(
        analyze_sql("SELECT 1;", SqlDialectKind::Postgres),
        SqlRisk::ReadOnly
    );

    for sql in ["", "   \n\t  "] {
        let reasons = ai_sql_policy_assert_rejected(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_contains(&reasons, "empty");
    }
}

#[test]
fn ai_sql_policy_unknown_syntax_requires_authorization_not_read_only() {
    let reasons = ai_sql_policy_assert_requires("SELEC * FROM", SqlDialectKind::Postgres);
    ai_sql_policy_assert_reason_contains(&reasons, "unknown_syntax");
}

#[test]
fn ai_sql_policy_cross_schema_reference_remains_read_only_in_classifier() {
    assert_eq!(
        analyze_sql(
            "SELECT * FROM other_schema.orders",
            SqlDialectKind::Postgres
        ),
        SqlRisk::ReadOnly
    );
}

#[test]
fn ai_sql_policy_route_matches_analysis() {
    for (sql, dialect, expected_tool) in [
        ("SELECT * FROM orders", SqlDialectKind::Postgres, "db.query"),
        (
            "UPDATE orders SET total = 1",
            SqlDialectKind::Postgres,
            "db.execute",
        ),
        ("SELEC * FROM", SqlDialectKind::Postgres, "db.execute"),
    ] {
        let (tool, risk) = route_for_sql(sql, dialect);
        assert_eq!(tool, expected_tool, "sql {sql:?}");
        assert_eq!(risk, analyze_sql(sql, dialect), "sql {sql:?}");
    }
}

#[test]
fn ai_sql_policy_tool_schemas_are_valid_and_bounded() {
    let query: serde_json::Value =
        serde_json::from_str(DB_QUERY_INPUT_SCHEMA).expect("db.query schema JSON");
    let execute: serde_json::Value =
        serde_json::from_str(DB_EXECUTE_INPUT_SCHEMA).expect("db.execute schema JSON");
    let explain: serde_json::Value =
        serde_json::from_str(DB_EXPLAIN_INPUT_SCHEMA).expect("db.explain schema JSON");

    for schema in [&query, &execute, &explain] {
        assert!(
            schema.is_object(),
            "schema must be a JSON object: {schema:?}"
        );
    }

    let object = query.as_object().expect("db.query object");
    assert_eq!(object.get("additionalProperties"), Some(&false.into()));
    let required = object
        .get("required")
        .and_then(serde_json::Value::as_array)
        .expect("db.query required");
    assert!(required.iter().any(|value| value == "target"));
    assert!(required.iter().any(|value| value == "sql"));

    let max_rows = object
        .get("properties")
        .and_then(|value| value.get("max_rows"))
        .expect("db.query max_rows");
    assert_eq!(max_rows.get("maximum"), Some(&1000.into()));
}

#[test]
fn ai_sql_policy_dialect_changes_backtick_identifier_classification() {
    let sql = "SELECT `a` FROM `t`";
    assert_eq!(analyze_sql(sql, SqlDialectKind::MySql), SqlRisk::ReadOnly);

    // PostgreSQL 方言不应把 MySQL 反引号语法误判为安全只读；当前实现按
    // unknown_syntax 降级，若未来 parser 支持则至少要由 RequiresAuthorization
    // / Rejected 明确拦住。
    let postgres = analyze_sql(sql, SqlDialectKind::Postgres);
    assert!(
        !postgres.is_read_only(),
        "PostgreSQL must not treat MySQL backtick syntax as read-only: {postgres:?}"
    );
}

#[test]
fn ai_sql_policy_nested_ctes_and_extended_side_effect_markers_require_authorization() {
    // 嵌套 CTE：只看最外层 body 会漏掉内层 DELETE。
    let reasons = ai_sql_policy_assert_requires(
        "WITH outer_c AS (WITH inner_c AS (DELETE FROM orders RETURNING id) SELECT * FROM inner_c) \
         SELECT * FROM outer_c",
        SqlDialectKind::Postgres,
    );
    ai_sql_policy_assert_reason_prefix(&reasons, "modifying_cte_");

    // 评审发现的副作用/越权函数：不在旧 marker 列表里也不能当只读。
    for sql in [
        "SELECT pg_notify('chan', 'payload')",
        "SELECT lo_unlink(12345)",
        "SELECT pg_read_binary_file('/etc/hosts')",
        "SELECT dblink_exec('conn', 'UPDATE t SET v=1')",
        "SELECT load_extension('evil')",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "side_effect_marker:");
    }
}

#[test]
fn ai_sql_policy_object_metadata_shows_are_read_only() {
    let mysql_cases = [
        "SHOW CREATE TABLE orders",
        "SHOW CREATE VIEW v_orders",
        "SHOW COLUMNS FROM orders",
        "SHOW FULL COLUMNS FROM orders",
        "SHOW INDEX FROM orders",
        "SHOW INDEXES FROM orders",
        "SHOW KEYS FROM orders",
        "DESC orders",
        "DESCRIBE orders",
        "EXPLAIN orders",
        "SHOW FULL TABLES FROM app",
        "SHOW TABLES",
    ];
    let postgres_cases = [
        "SHOW search_path",
        "SHOW server_version",
        "SHOW TIME ZONE",
        "SHOW transaction isolation level",
        "SHOW default_transaction_read_only",
    ];
    let sqlite_cases = [
        "PRAGMA table_info('orders')",
        "PRAGMA table_list",
        "PRAGMA index_list('orders')",
        "PRAGMA foreign_key_list('orders')",
        "PRAGMA database_list",
    ];

    for sql in mysql_cases {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::MySql),
            SqlRisk::ReadOnly,
            "MySQL 元数据读取应放行：{sql}"
        );
    }
    for sql in postgres_cases {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Postgres),
            SqlRisk::ReadOnly,
            "PostgreSQL 元数据读取应放行：{sql}"
        );
    }
    for sql in sqlite_cases {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Sqlite),
            SqlRisk::ReadOnly,
            "SQLite 元数据读取应放行：{sql}"
        );
    }
}

#[test]
fn ai_sql_policy_metadata_disclosure_requires_authorization() {
    let cases = [
        ("SHOW DATABASES", SqlDialectKind::MySql, "metadata_disclosure:show_databases"),
        ("SHOW SCHEMAS", SqlDialectKind::MySql, "metadata_disclosure:show_schemas"),
        ("SHOW VARIABLES LIKE 'datadir'", SqlDialectKind::MySql, "metadata_disclosure:show_variables"),
        ("SHOW STATUS", SqlDialectKind::MySql, "metadata_disclosure:show_status"),
        ("SHOW PROCESSLIST", SqlDialectKind::MySql, "metadata_disclosure:show_processlist"),
        ("SHOW GRANTS FOR 'u'@'h'", SqlDialectKind::MySql, "metadata_disclosure:show_grants"),
        ("SHOW CREATE FUNCTION f", SqlDialectKind::MySql, "metadata_disclosure:show_create_object"),
        ("SHOW ALL", SqlDialectKind::Postgres, "metadata_disclosure:show_all"),
        ("PRAGMA journal_mode", SqlDialectKind::Sqlite, "metadata_disclosure:pragma_journal_mode"),
    ];

    for (sql, dialect, expected) in cases {
        let reasons = ai_sql_policy_assert_requires(sql, dialect);
        ai_sql_policy_assert_reason_contains(&reasons, expected);
    }
}

#[test]
fn ai_sql_policy_metadata_never_accepts_session_or_table_writes() {
    // 元数据白名单不能顺带放行锁读、事务控制、带值的 PRAGMA 等写入路径。
    let write_cases = [
        ("SELECT * FROM orders FOR UPDATE", SqlDialectKind::MySql),
        ("SHOW TABLES FOR UPDATE", SqlDialectKind::MySql),
        ("PRAGMA journal_mode = 'wal'", SqlDialectKind::Sqlite),
        ("LOCK TABLES orders READ", SqlDialectKind::MySql),
    ];

    for (sql, dialect) in write_cases {
        assert!(
            !analyze_sql(sql, dialect).is_read_only(),
            "白名单不得放宽写入语句：{sql}"
        );
    }
    let reasons = ai_sql_policy_assert_requires(
        "PRAGMA journal_mode = 'wal'",
        SqlDialectKind::Sqlite,
    );
    ai_sql_policy_assert_reason_prefix(&reasons, "non_read_statement:pragma");
}
