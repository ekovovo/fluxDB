use fluxdb_app::{SqlDialectKind, analyze_sql, plan_hash};

fn assert_not_automatic_read(cases: &[(&str, SqlDialectKind)]) {
    let failures: Vec<_> = cases
        .iter()
        .filter_map(|(sql, dialect)| {
            let risk = analyze_sql(sql, *dialect);
            risk.is_read_only()
                .then(|| format!("{dialect:?}: {sql} => {risk:?}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "unsafe automatic-read classifications:\n{}",
        failures.join("\n")
    );
}

#[test]
fn quoted_postgres_side_effect_functions_are_not_automatic_reads() {
    assert_not_automatic_read(&[
        (
            r#"SELECT pg_catalog."pg_notify"('channel', 'payload')"#,
            SqlDialectKind::Postgres,
        ),
        (r#"SELECT "nextval"('sequence')"#, SqlDialectKind::Postgres),
        (
            r#"SELECT pg_catalog."lo_unlink"(12345)"#,
            SqlDialectKind::Postgres,
        ),
        (
            r#"WITH c AS (SELECT pg_catalog."pg_notify"('channel', 'payload')) SELECT * FROM c"#,
            SqlDialectKind::Postgres,
        ),
        (
            r#"SELECT (SELECT pg_catalog."nextval"('sequence'))"#,
            SqlDialectKind::Postgres,
        ),
    ]);
}

#[test]
fn additional_lock_functions_are_not_automatic_reads() {
    assert_not_automatic_read(&[
        (
            "SELECT pg_advisory_lock_shared(1)",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT pg_try_advisory_lock_shared(1)",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT pg_advisory_xact_lock_shared(1)",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT pg_try_advisory_xact_lock(1)",
            SqlDialectKind::Postgres,
        ),
        (
            "SELECT pg_try_advisory_xact_lock_shared(1)",
            SqlDialectKind::Postgres,
        ),
        ("SELECT pg_advisory_unlock(1)", SqlDialectKind::Postgres),
        (
            "SELECT pg_advisory_unlock_shared(1)",
            SqlDialectKind::Postgres,
        ),
        ("SELECT pg_advisory_unlock_all()", SqlDialectKind::Postgres),
        ("SELECT RELEASE_ALL_LOCKS()", SqlDialectKind::MySql),
    ]);
}

#[test]
fn sqlite_parenthesized_pragma_assignments_are_not_automatic_reads() {
    // 在隔离库确认括号形式确实是赋值，而非仅读取参数。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for pragma in ["user_version", "application_id", "schema_version"] {
        let sql = format!("PRAGMA {pragma}(42)");
        conn.execute_batch(&sql).unwrap();
        let actual: i64 = conn
            .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(actual, 42, "the parenthesized form really writes {pragma}");
    }
    assert_not_automatic_read(&[
        ("PRAGMA user_version(42)", SqlDialectKind::Sqlite),
        ("PRAGMA application_id(42)", SqlDialectKind::Sqlite),
        ("PRAGMA schema_version(42)", SqlDialectKind::Sqlite),
        ("PRAGMA main.\"user_version\"(42)", SqlDialectKind::Sqlite),
        ("PRAGMA encoding('UTF-16')", SqlDialectKind::Sqlite),
        ("PRAGMA user_version = 42", SqlDialectKind::Sqlite),
    ]);
}

#[test]
fn mysql_executable_comment_is_not_an_automatic_read() {
    assert_not_automatic_read(&[(
        "SELECT 1 /*!50000 INTO OUTFILE '/tmp/t024-probe-not-executed' */",
        SqlDialectKind::MySql,
    )]);
}

#[test]
fn approval_hash_changes_for_each_execution_input() {
    let target = r#"{"connection_id":1,"database":"sales","schema":"public"}"#;
    let args = r#"{"target":"conn:1/db:sales/schema:public","sql":"UPDATE orders SET status=$1 WHERE id=$2","parameters":[{"type":"text","value":"paid"},{"type":"i64","value":"1"}]}"#;
    let original = plan_hash("db.execute", 1, target, args, Some("plugin-v1"));
    for changed in [
        plan_hash("db.query", 1, target, args, Some("plugin-v1")),
        plan_hash("db.execute", 2, target, args, Some("plugin-v1")),
        plan_hash(
            "db.execute",
            1,
            &target.replace("\"connection_id\":1", "\"connection_id\":2"),
            args,
            Some("plugin-v1"),
        ),
        plan_hash(
            "db.execute",
            1,
            &target.replace("public", "private"),
            args,
            Some("plugin-v1"),
        ),
        plan_hash(
            "db.execute",
            1,
            target,
            &args.replace("paid", "refunded"),
            Some("plugin-v1"),
        ),
        plan_hash(
            "db.execute",
            1,
            target,
            &args.replace("id=$2", "id>$2"),
            Some("plugin-v1"),
        ),
        plan_hash("db.execute", 1, target, args, Some("plugin-v2")),
        plan_hash("db.execute", 1, target, args, None),
    ] {
        assert_ne!(original, changed);
    }
}

#[test]
fn quoted_pure_functions_and_pragma_object_arguments_remain_reads() {
    for (sql, dialect) in [
        (
            r#"SELECT pg_catalog."count"(*) FROM orders"#,
            SqlDialectKind::Postgres,
        ),
        ("SELECT abs(-1)", SqlDialectKind::Sqlite),
        ("PRAGMA main.table_info('orders')", SqlDialectKind::Sqlite),
        ("PRAGMA table_xinfo('orders')", SqlDialectKind::Sqlite),
        ("PRAGMA index_list('orders')", SqlDialectKind::Sqlite),
        ("PRAGMA index_info('idx_orders')", SqlDialectKind::Sqlite),
        ("PRAGMA index_xinfo('idx_orders')", SqlDialectKind::Sqlite),
        ("PRAGMA foreign_key_list('orders')", SqlDialectKind::Sqlite),
        ("PRAGMA table_list('orders')", SqlDialectKind::Sqlite),
        ("PRAGMA user_version", SqlDialectKind::Sqlite),
        ("PRAGMA application_id", SqlDialectKind::Sqlite),
        ("PRAGMA schema_version", SqlDialectKind::Sqlite),
    ] {
        assert!(
            analyze_sql(sql, dialect).is_read_only(),
            "pure read must remain usable: {sql}"
        );
    }
}

struct SqliteFixture(std::path::PathBuf);

impl Drop for SqliteFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn query_tool_rejects_pragma_writes_and_preserves_real_sqlite_state() {
    use fluxdb_app::{AiSqlReadTool, MapSqlResolver};
    use fluxdb_core::{
        AiTool, CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint,
        ToolExecutionContext, ToolReceiptStatus,
    };
    use std::sync::Arc;

    let dir = SqliteFixture(
        std::env::temp_dir().join(format!("fluxdb-pragma-policy-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("pragma.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let config = ConnectionConfig {
        id: ConnectionId(1),
        name: "isolated-pragma".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path,
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    let tool = AiSqlReadTool::query(Arc::new(MapSqlResolver::new([config]))).unwrap();
    for (pragma, sql) in [
        ("user_version", "PRAGMA user_version(42)"),
        ("application_id", "PRAGMA application_id(42)"),
        ("schema_version", "PRAGMA schema_version(42)"),
        ("user_version", "PRAGMA main.\"user_version\"(42)"),
        ("user_version", "PRAGMA user_version = 42"),
    ] {
        conn.pragma_update(None, pragma, 7).unwrap();
        // 工具返回就绪 future，不能在 Connector 自带 runtime 外再嵌套 Tokio runtime。
        let mut future = tool.execute(
            ToolExecutionContext {
                automatic_approval: false,
                bounded_write: false,
                session_id: "isolated-session".into(),
                turn_id: "isolated-turn".into(),
                iteration: 1,
                call_id: format!("pragma-{pragma}"),
                cancel: CancelFlag::new(),
                progress: None,
            },
            serde_json::json!({
                "target": "conn:1",
                "sql": sql,
            })
            .to_string(),
        );
        let std::task::Poll::Ready(receipt) = future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        else {
            panic!("SQL tool must return a ready receipt");
        };
        let receipt = receipt.unwrap();
        let actual: i64 = conn
            .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            actual, 7,
            "db.query must not mutate {pragma}; receipt={receipt:?}"
        );
        assert_eq!(receipt.status, ToolReceiptStatus::Failed);
        assert_eq!(receipt.error_code.as_deref(), Some("not_read_only"));
    }
}
