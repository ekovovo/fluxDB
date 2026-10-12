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
fn ai_sql_policy_date_and_time_functions_are_read_only() {
    // 真实缺陷复现：模型用 `SELECT date('now')` 确定“今天/当月”边界时，
    // 因 `date` 不在纯函数白名单被判 `unknown_function`，只读入口直接拒绝，
    // 模型只能放弃时间边界或用错误口径继续。日期/时间函数不产生副作用，
    // 应按只读放行，且不能因此放宽真正的写语句判定。
    for sql in [
        "SELECT date('now') AS today",
        "SELECT DATE('now') AS today",
        "SELECT datetime('now') AS now_text",
        "SELECT strftime('%Y-%m', created_at) AS month FROM orders",
        "SELECT julianday('now') FROM orders",
        "SELECT unixepoch('now') FROM orders",
    ] {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Sqlite),
            SqlRisk::ReadOnly,
            "sqlite: {sql}"
        );
    }
    for (sql, dialect) in [
        (
            "SELECT DATE_FORMAT(created_at, '%Y-%m') AS month FROM orders",
            SqlDialectKind::MySql,
        ),
        (
            "SELECT UNIX_TIMESTAMP(created_at) FROM orders",
            SqlDialectKind::MySql,
        ),
        (
            "SELECT to_char(created_at, 'YYYY-MM') AS month FROM orders",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT date_trunc('month', created_at) FROM orders",
            SqlDialectKind::Postgres,
        ),
    ] {
        assert_eq!(
            analyze_sql(sql, dialect),
            SqlRisk::ReadOnly,
            "{dialect:?}: {sql}"
        );
    }
    // 白名单变宽不能顺带放过写语句：同一条被拒的 DML 仍必须需授权。
    assert!(!analyze_sql("UPDATE orders SET status='x'", SqlDialectKind::Sqlite).is_read_only());
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
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }
    for sql in postgres_cases {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }
}

#[test]
fn ai_sql_policy_locking_reads_require_authorization() {
    // 锁定读（`FOR UPDATE`/`FOR SHARE`）改由 AST `query.locks` 识别，含嵌套子查询形态。
    for (sql, dialect) in [
        ("SELECT * FROM orders FOR UPDATE", SqlDialectKind::Postgres),
        ("SELECT * FROM orders FOR SHARE", SqlDialectKind::Postgres),
        (
            "SELECT * FROM orders FOR UPDATE SKIP LOCKED",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT * FROM (SELECT * FROM orders FOR UPDATE) s",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT (SELECT id FROM orders FOR UPDATE) AS x",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT * FROM orders WHERE id IN (SELECT id FROM t FOR UPDATE)",
            SqlDialectKind::Postgres,
        ),
        (
            "WITH c AS (SELECT * FROM orders FOR UPDATE) SELECT * FROM c",
            SqlDialectKind::Postgres,
        ),
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, dialect);
        ai_sql_policy_assert_reason_contains(&reasons, "locking_read");
    }

    // 方言不支持锁定读语法时按 unknown_syntax 兜底，不能误判只读。
    let reasons = ai_sql_policy_assert_requires(
        "SELECT * FROM orders LOCK IN SHARE MODE",
        SqlDialectKind::MySql,
    );
    assert!(
        !reasons.is_empty(),
        "locking read must explain why it is gated"
    );
}

#[test]
fn ai_sql_policy_lock_keywords_in_literals_are_read_only() {
    // 锁定读标记不再做裸文本匹配：字面量里的这些词不能误伤普通只读查询。
    for (sql, dialect) in [
        ("SELECT 'for update' AS note FROM orders", SqlDialectKind::Postgres),
        ("SELECT 'for share' AS note FROM orders", SqlDialectKind::Postgres),
        ("SELECT 'skip locked' AS note FROM orders", SqlDialectKind::Postgres),
        ("SELECT 'nowait' AS note FROM orders", SqlDialectKind::Postgres),
        ("SELECT 'into outfile' AS note FROM orders", SqlDialectKind::MySql),
        ("SELECT 'load data' AS note FROM orders", SqlDialectKind::MySql),
    ] {
        assert_eq!(
            analyze_sql(sql, dialect),
            SqlRisk::ReadOnly,
            "lock keyword in a literal must not block a read-only query: {sql}"
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
fn ai_sql_policy_unknown_functions_default_to_read_only() {
    // denylist 方向（借鉴 Superset）：用户自定义/扩展函数不逐个枚举，
    // 未命中已知危险函数时按只读放行，最终由数据库权限/只读配置约束。
    for sql in [
        "SELECT process_data()",
        "SELECT custom_calc(total) FROM orders",
        "SELECT * FROM orders WHERE unknown_guard(id)",
    ] {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Postgres),
            SqlRisk::ReadOnly,
            "unknown function should not force approval: {sql}"
        );
    }

    // 常见纯函数仍可用（回归：`date('now')` 之类不再误伤只读查询）。
    assert_eq!(
        analyze_sql(
            "SELECT count(*), upper(name) FROM orders WHERE coalesce(note, '') <> ''",
            SqlDialectKind::Postgres
        ),
        SqlRisk::ReadOnly
    );

    // 已知危险函数仍必须需授权。
    for sql in [
        "SELECT pg_read_file('/etc/passwd')",
        "SELECT nextval('s')",
        "SELECT pg_sleep(1)",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }
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
fn ai_sql_policy_nested_ctes_and_denied_functions_require_authorization() {
    // 嵌套 CTE：只看最外层 body 会漏掉内层 DELETE。
    let reasons = ai_sql_policy_assert_requires(
        "WITH outer_c AS (WITH inner_c AS (DELETE FROM orders RETURNING id) SELECT * FROM inner_c) \
         SELECT * FROM outer_c",
        SqlDialectKind::Postgres,
    );
    ai_sql_policy_assert_reason_prefix(&reasons, "modifying_cte_");

    // 评审发现的副作用/越权函数：denylist 命中后不得只读（denylist 按方言分表）。
    for sql in [
        "SELECT pg_notify('chan', 'payload')",
        "SELECT lo_unlink(12345)",
        "SELECT pg_read_binary_file('/etc/hosts')",
        "SELECT dblink_exec('conn', 'UPDATE t SET v=1')",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }
    let reasons =
        ai_sql_policy_assert_requires("SELECT load_extension('evil')", SqlDialectKind::Sqlite);
    ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
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

#[test]
fn ai_sql_policy_from_table_functions_are_not_automatic_reads() {
    // 回归：`FROM f(...)` 表函数在 sqlparser 中是 TableFactor 而不是 Expr::Function，
    // 只扫表达式会漏掉；denylist 命中的表函数（如 pg_stat_reset 会重置统计）必须需授权。
    for sql in [
        "SELECT * FROM pg_stat_reset()",
        "SELECT * FROM pg_read_file('/etc/passwd')",
        "SELECT * FROM pg_ls_dir('/etc')",
        "SELECT * FROM dblink('c','s') AS t(a text)",
        "SELECT * FROM TABLE(pg_read_file('/x'))",
        "SELECT * FROM t JOIN pg_ls_waldir() ON TRUE",
        "WITH c AS (SELECT * FROM pg_stat_reset()) SELECT * FROM c",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }

    // 无副作用/未命中 denylist 的表函数仍按只读放行（denylist 方向，不是白名单）。
    for sql in [
        "SELECT * FROM generate_series(1, 10)",
        "SELECT * FROM unnest(ARRAY[1,2,3])",
        "SELECT * FROM my_custom_srf(1)",
        "SELECT * FROM json_each('{\"a\":1}')",
    ] {
        let dialect = if sql.contains("json_each") {
            SqlDialectKind::Sqlite
        } else {
            SqlDialectKind::Postgres
        };
        assert_eq!(
            analyze_sql(sql, dialect),
            SqlRisk::ReadOnly,
            "benign table function should stay read-only: {sql}"
        );
    }
}

#[test]
fn ai_sql_policy_denied_system_tables_require_authorization() {
    // 借鉴 Superset DISALLOWED_SQL_TABLES：会话/角色/整库元数据系统表需授权。
    for sql in [
        "SELECT * FROM pg_stat_activity",
        "SELECT usename FROM pg_shadow",
        "SELECT * FROM pg_catalog.pg_roles",
        "SELECT * FROM information_schema.tables",
        "SELECT * FROM pg_stat_activity WHERE state = 'active'",
        "WITH a AS (SELECT * FROM pg_stat_activity) SELECT * FROM a",
        "SELECT count(*) FROM (SELECT * FROM information_schema.columns) c",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_table:");
    }

    // 普通业务表（即使 schema/表名与系统视图同名但不同 schema）不受影响。
    assert!(analyze_sql("SELECT * FROM public.orders", SqlDialectKind::Postgres).is_read_only());
    assert!(
        analyze_sql("SELECT * FROM app.information_schema_tables", SqlDialectKind::Postgres)
            .is_read_only()
    );

    let mysql_reasons =
        ai_sql_policy_assert_requires("SELECT * FROM mysql.user", SqlDialectKind::MySql);
    ai_sql_policy_assert_reason_prefix(&mysql_reasons, "denied_table:");
    let sqlite_reasons =
        ai_sql_policy_assert_requires("SELECT * FROM sqlite_master", SqlDialectKind::Sqlite);
    ai_sql_policy_assert_reason_prefix(&sqlite_reasons, "denied_table:");
}

#[test]
fn ai_sql_policy_denied_functions_match_last_identifier_case_insensitively() {
    // 按最后一段标识符、大小写不敏感匹配：schema 限定 / 带引号写法都不能绕过 denylist。
    for sql in [
        "SELECT pg_catalog.nextval('s')",
        "SELECT NEXTVAL('s')",
        "SELECT pg_catalog.\"nextval\"('s')",
        "SELECT pg_catalog.\"pg_notify\"('c', 'p')",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }
}

#[test]
fn ai_sql_policy_common_functions_remain_read_only() {
    // denylist 方向的核心收益：遗漏枚举的常用函数不再把普通只读报表推入审批。
    for sql in [
        "SELECT row_number() OVER (ORDER BY id) FROM orders",
        "SELECT lag(x) OVER (ORDER BY id) FROM orders",
        "SELECT string_agg(name, ',') FROM orders",
        "SELECT array_agg(id) FROM orders",
        "SELECT jsonb_build_object('a', 1)",
        "SELECT regexp_replace(name, 'x', 'y') FROM orders",
        "SELECT to_char(created_at, 'YYYY') FROM orders",
    ] {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Postgres),
            SqlRisk::ReadOnly,
            "common read-only function must not require approval: {sql}"
        );
    }
    for sql in [
        "SELECT group_concat(name) FROM orders",
        "SELECT ifnull(total, 0) FROM orders",
        "SELECT json_extract(payload, '$.a') FROM orders",
    ] {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::MySql),
            SqlRisk::ReadOnly,
            "common read-only function must not require approval: {sql}"
        );
    }
    for sql in [
        "SELECT typeof(x) FROM t",
        "SELECT printf('%d', x) FROM t",
        "SELECT json_extract(payload, '$.a') FROM t",
    ] {
        assert_eq!(
            analyze_sql(sql, SqlDialectKind::Sqlite),
            SqlRisk::ReadOnly,
            "common read-only function must not require approval: {sql}"
        );
    }
}

#[test]
fn ai_sql_policy_postgres_admin_and_replication_functions_require_authorization() {
    // 这些函数名不带写关键字、也不改表数据，但会推进 WAL、创建/删除复制槽、控制恢复
    // 或做备份/时间线管理——Superset 的 is_mutating() 会放行它们，fluxDB 用 denylist 单独点名。
    for sql in [
        "SELECT pg_logical_emit_message(true, 'a', 'b')",
        "SELECT pg_export_snapshot()",
        "SELECT pg_switch_wal()",
        "SELECT pg_rotate_logfile()",
        "SELECT pg_promote()",
        "SELECT pg_wal_replay_pause()",
        "SELECT pg_wal_replay_resume()",
        "SELECT pg_start_backup('x')",
        "SELECT pg_stop_backup()",
        "SELECT pg_backup_start('x')",
        "SELECT pg_backup_stop()",
        "SELECT pg_create_restore_point('x')",
        "SELECT pg_drop_replication_slot('s')",
        "SELECT pg_create_logical_replication_slot('s', 'pgoutput')",
        "SELECT pg_create_physical_replication_slot('s')",
        "SELECT pg_replication_slot_advance('s', '0/0')",
        "SELECT pg_replication_origin_create('x')",
        "SELECT pg_replication_origin_drop('x')",
        "SELECT pg_import_system_collations('public')",
        "SELECT pg_logdir_ls()",
        // 事务号分配：当前事务未分配 XID 时会分配并推进全局计数。
        "SELECT txid_current()",
        "SELECT pg_current_xact_id()",
    ] {
        let reasons = ai_sql_policy_assert_requires(sql, SqlDialectKind::Postgres);
        ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
    }

    // `pg_stat_reset()` 常以表函数形式出现（FROM 位置），同样要拦住。
    let reasons =
        ai_sql_policy_assert_requires("SELECT * FROM pg_stat_reset()", SqlDialectKind::Postgres);
    ai_sql_policy_assert_reason_prefix(&reasons, "denied_function:");
}
