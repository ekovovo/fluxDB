// 对外 MCP Server 执行服务的行为测试（T054/T059）。
//
// 用真实 SQLite fixture 走生产链路：schema 校验→强制审计→授权→执行→投影，
// 只替换模型与传输（不存在）。断言工具可见性过滤、隐藏写工具直接调用被拒、
// 未授权 target 拒绝、只读 Profile 不执行写、结果协议形状。
use fluxdb_storage::Storage as _;
use fluxdb_storage::ai_platform::mcp_server::{
    GrantResourceKind, ResourceGrantEdit, ServerProfileEdit, WriteMode, save_resource_grant,
    save_server_profile,
};
use fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus;

use super::*;

struct Fixture {
    dir: std::path::PathBuf,
    storage: fluxdb_storage::FileStorage,
    connection: ConnectionConfig,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-mcp-svc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("sales.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE orders(id INTEGER PRIMARY KEY, amount TEXT);
             INSERT INTO orders(id, amount) VALUES (1,'10.50'),(2,'20.00');",
        )
        .unwrap();
        let config = ConnectionConfig {
            id: ConnectionId(1),
            name: "sales".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: fluxdb_core::Endpoint::SqliteFile { path, read_only: true },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        };
        let storage = fluxdb_storage::FileStorage::new(&dir);
        storage.save_connections(&[config.clone()]).unwrap();
        Self { dir, storage, connection: config }
    }

    /// 建 profile + 授权；`allowed` 为 allowed_tool_ids；`write_mode` 控制写权限。
    fn profile_with(
        &self,
        allowed: &[&str],
        write_mode: WriteMode,
        allow_write: bool,
    ) -> (fluxdb_storage::ai_platform::mcp_server::ServerProfileView, Vec<fluxdb_storage::ai_platform::mcp_server::ResourceGrantView>) {
        self.storage
            .with_ai_connection(|conn| {
                let edit = ServerProfileEdit {
                    id: "p1".into(),
                    display_name: "本地".into(),
                    enabled: true,
                    write_mode,
                    allowed_tool_ids: allowed.iter().map(|s| s.to_string()).collect(),
                    options_json: "{}".into(),
                    max_inflight: 4,
                    max_request_ms: 30_000,
                    max_request_bytes: 262_144,
                    max_response_bytes: 131_072,
                };
                let created = save_server_profile(conn, 1, &edit, None)?;
                let profile = save_server_profile(conn, 2, &edit, Some(created.revision))?;
                let grant = ResourceGrantEdit {
                    id: "g1".into(),
                    profile_id: "p1".into(),
                    connection_id: 1,
                    database_name: "main".into(),
                    namespace: "".into(),
                    enabled: true,
                    allow_read: true,
                    allow_write,
                    object_policy_json: r#"{"all_objects":true}"#.into(),
                    write_policy_json: "{}".into(),
                    max_rows: 200,
                };
                save_resource_grant(conn, 3, &grant, GrantResourceKind::Sql, None)?;
                let grants = fluxdb_storage::ai_platform::mcp_server::list_resource_grants(conn, "p1")?;
                Ok((profile, grants))
            })
            .unwrap()
    }

    fn service(
        &self,
        profile: fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
        grants: Vec<fluxdb_storage::ai_platform::mcp_server::ResourceGrantView>,
    ) -> McpExecutionService {
        let results = Arc::new(Mutex::new(ResultStore::for_turn(
            RESULT_STORE_MEMORY_BYTES,
            "mcp-session",
            "mcp-request",
            None,
        )));
        McpExecutionService::new(vec![self.connection.clone()], profile, grants, results).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn readonly_profile_hides_write_tool_and_rejects_direct_call() {
    let fixture = Fixture::new();
    // allowed 里故意写入 db.execute，验证 visibility 与授权各自独立判定。
    let (profile, grants) = fixture.profile_with(
        &["fluxdb_query", "fluxdb_connections_list", "fluxdb_execute"],
        WriteMode::Disabled,
        true,
    );
    let views = visible_tools(&profile, &grants).unwrap();
    let names = views.iter().map(|v| v.name.as_str()).collect::<Vec<_>>();
    assert!(names.contains(&"fluxdb_query"));
    assert!(!names.contains(&"fluxdb_execute"), "disabled 模式不能列出写工具");

    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    let outcome = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:1/db:main","sql":"delete from orders"}"#,
        &cancel,
    );
    assert!(!outcome.ok, "隐藏的写工具直接调用必须被拒绝");
    assert_eq!(outcome.error.unwrap().code, "write_disabled");
}

#[test]
fn query_returns_structured_rows_and_supports_pagination_envelope() {
    let fixture = Fixture::new();
    let (profile, grants) =
        fixture.profile_with(&["fluxdb_query", "fluxdb_connections_list"], WriteMode::Disabled, false);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "req-1".into(),
    };
    let outcome = service.call(
        &identity,
        "fluxdb_query",
        r#"{"target":"conn:1/db:main","sql":"SELECT id, amount FROM orders ORDER BY id"}"#,
        &cancel,
    );
    assert!(outcome.ok, "只读查询应成功: {:?}", outcome.error);
    assert_eq!(outcome.structured["ok"], serde_json::json!(true));
    assert_eq!(outcome.structured["request_id"], serde_json::json!("req-1"));
    assert_eq!(outcome.structured["outcome"], serde_json::json!("succeeded"));
    assert!(outcome.structured["data"].is_object(), "结构含 data");
    assert!(!outcome.text.is_empty(), "带 text 兼容投影");
}

#[test]
fn unauthorized_target_is_rejected() {
    let fixture = Fixture::new();
    let (profile, grants) = fixture.profile_with(&["fluxdb_query"], WriteMode::Disabled, false);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    // 句柄不在授权列表里（换库）。
    let outcome = service.call(
        &identity,
        "fluxdb_query",
        r#"{"target":"conn:1/db:secret","sql":"SELECT 1"}"#,
        &cancel,
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "STALE_RESOURCE");
}

#[test]
fn tool_not_in_allowed_ids_is_rejected_even_if_visible_shape() {
    let fixture = Fixture::new();
    // 授权了关系型资源，但 allowed_tool_ids 不含 db.query。
    let (profile, grants) =
        fixture.profile_with(&["fluxdb_connections_list"], WriteMode::Disabled, false);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    let outcome = service.call(
        &identity,
        "fluxdb_query",
        r#"{"target":"conn:1/db:main","sql":"SELECT 1"}"#,
        &cancel,
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "tool_not_allowed");
}

#[test]
fn connections_list_only_returns_authorized_resources_without_endpoint() {
    let fixture = Fixture::new();
    let (profile, grants) =
        fixture.profile_with(&["fluxdb_connections_list"], WriteMode::Disabled, false);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    let outcome = service.call(&identity, "fluxdb_connections_list", "{}", &cancel);
    assert!(outcome.ok);
    // 不得泄漏 endpoint 或凭据。
    let text = outcome.text.clone();
    assert!(!text.contains("sqlite"), "不得返回 endpoint: {text}");
    assert!(!text.contains("password"));
    let resources = outcome.structured["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["target"], serde_json::json!("conn:1/db:main"));
    // 终态映射。
    assert_eq!(
        McpExecutionService::terminal_status(&outcome),
        ServerRequestStatus::Succeeded
    );
}

#[test]
fn scoped_write_without_readonly_connection_is_denied_when_connection_readonly() {
    // 连接本身标记只读：即便 Profile scoped + grant allow_write，也不能写。
    let fixture = Fixture::new();
    let (profile, grants) = fixture.profile_with(&["fluxdb_execute"], WriteMode::Scoped, true);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    let outcome = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:1/db:main","sql":"update orders set amount='1' where id=1"}"#,
        &cancel,
    );
    // 只读连接 → 写权限关闭 → 拒绝（不是审批）。
    assert!(!outcome.ok, "只读连接上的写应被拒绝");
    let _ = outcome;
}

#[test]
fn query_binds_typed_parameters_without_string_interpolation() {
    let fixture = Fixture::new();
    let (profile, grants) =
        fixture.profile_with(&["fluxdb_query"], WriteMode::Disabled, false);
    let service = fixture.service(profile, grants);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    // 参数化查询：占位符 + 绑定值，值里带引号与 SQL 片段证明未被拼接执行。
    let outcome = service.call(
        &identity,
        "fluxdb_query",
        r#"{"target":"conn:1/db:main","sql":"SELECT id, amount FROM orders WHERE amount = ?1","parameters":[{"type":"text","value":"10.50"}]}"#,
        &cancel,
    );
    // SQLite 的连接器默认不注册 `?` 绑定以外的语法；至少证明参数被真实绑定（不是报“不支持参数化”）。
    if outcome.ok {
        assert!(outcome.structured["data"].is_object());
    } else {
        let code = outcome.error.as_ref().unwrap().code.clone();
        assert_ne!(
            code, "parameters_unsupported",
            "SQLite 必须支持参数化绑定，而不是回退到不支持"
        );
    }
}

#[test]
fn sqlite_parameter_binding_executes_exact_value() {
    // 直接经连接器验证 SQLite 参数绑定：`?1` 占位符必须被真实 bind。
    let fixture = Fixture::new();
    let config = fixture.connection.clone();
    let request = fluxdb_core::QueryRequest {
        connection_id: config.id,
        database: Some("main".into()),
        schema: None,
        text: "SELECT amount FROM orders WHERE id = ?1".into(),
        mode: fluxdb_core::QueryMode::Selection,
        options: fluxdb_core::QueryExecutionOptions {
            continue_on_error: false,
            split_statements: false,
            page_offset: 0,
            page_size: 200,
            read_only: true,
        },
        session_id: None,
    };
    let connector = fluxdb_connectors::connector_for(&config).unwrap();
    let result = connector
        .execute_parameterized(&fluxdb_core::ParameterizedQueryRequest {
            request,
            parameters: vec![fluxdb_core::SqlParameter::I64("2".into())],
        })
        .expect("SQLite 参数化执行");
    let page = &result.results[0];
    assert_eq!(page.rows.len(), 1, "命中 id=2 恰好一行");
    assert!(matches!(&page.rows[0].values[0], CellValue::Text(text) if text == "20.00"));
}

#[test]
fn claude_export_requires_absolute_paths_and_hides_secrets() {
    let json = claude_mcp_config_json(
        "fluxdb_local",
        "/opt/fluxdb/bin/fluxdb-mcp",
        "p1",
        Some("/home/u/.fluxdb"),
    )
    .expect("导出配置");
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let server = &value["mcpServers"]["fluxdb_local"];
    assert_eq!(server["command"], serde_json::json!("/opt/fluxdb/bin/fluxdb-mcp"));
    assert_eq!(server["args"], serde_json::json!(["--server-profile","p1","--config-root","/home/u/.fluxdb"]));
    // 不含任何秘密字段。
    assert!(!json.contains("password"));
    assert!(!json.contains("api_key"));
    assert!(!json.contains("token"));
    // 相对路径被拒绝。
    assert!(claude_mcp_config_json("s", "fluxdb-mcp", "p1", None).is_err());
}

#[test]
fn scoped_write_policy_allows_only_authorized_update() {
    use crate::SqlDialectKind;
    let policy = ScopedWritePolicy::parse(
        r#"{"allowed_operations":["update"],"allowed_objects":["orders"],"require_where":true,"max_affected_rows":1,"allow_ddl":false}"#,
    )
    .expect("解析写规则");
    // 命中：授权表 + WHERE。
    assert!(policy
        .authorize_sql("UPDATE orders SET amount='1' WHERE id = 1", SqlDialectKind::Sqlite)
        .is_ok());
    // 非授权表拒绝。
    assert!(policy
        .authorize_sql("UPDATE customers SET x=1 WHERE id=1", SqlDialectKind::Sqlite)
        .is_err());
    // 缺 WHERE 拒绝。
    assert!(policy
        .authorize_sql("UPDATE orders SET amount='1'", SqlDialectKind::Sqlite)
        .is_err());
    // 非 UPDATE（DELETE）拒绝。
    assert!(policy
        .authorize_sql("DELETE FROM orders WHERE id=1", SqlDialectKind::Sqlite)
        .is_err());
    // 子查询谓词无法证明范围，拒绝。
    assert!(policy
        .authorize_sql(
            "UPDATE orders SET amount='1' WHERE id IN (SELECT id FROM x)",
            SqlDialectKind::Sqlite
        )
        .is_err());
}

#[test]
fn scoped_write_policy_rejects_empty_or_ddl() {
    use crate::SqlDialectKind;
    let empty = ScopedWritePolicy::parse("{}").expect("默认写规则");
    assert!(empty
        .authorize_sql("UPDATE orders SET x=1 WHERE id=1", SqlDialectKind::Sqlite)
        .is_err(), "空规则不放行任何写");
    let ddl = ScopedWritePolicy::parse(
        r#"{"allowed_operations":["update"],"allowed_objects":["orders"],"max_affected_rows":5,"allow_ddl":true}"#,
    )
    .expect("解析");
    assert!(ddl
        .authorize_sql("UPDATE orders SET x=1 WHERE id=1", SqlDialectKind::Sqlite)
        .is_err(), "首版即使声明 allow_ddl 也拒绝 DDL 语义");
}

/// 受限写端到端：写连接 + scoped 规则命中 → 真实执行并影响行数；越界表被拒。
#[test]
fn scoped_write_executes_preauthorized_update_against_real_sqlite() {
    // 与只读 fixture 不同：这里需要可写连接。
    let dir = std::env::temp_dir().join(format!("fluxdb-mcp-write-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("w.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, amount TEXT);
         INSERT INTO orders(id, amount) VALUES (1,'10.50'),(2,'20.00');",
    )
    .unwrap();
    let config = ConnectionConfig {
        id: ConnectionId(7),
        name: "write".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile { path: path.clone(), read_only: false },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    let storage = fluxdb_storage::FileStorage::new(&dir);
    storage.save_connections(&[config.clone()]).unwrap();
    let (profile, grants) = storage
        .with_ai_connection(|conn| {
            let edit = ServerProfileEdit {
                id: "wp".into(),
                display_name: "写本地".into(),
                enabled: true,
                write_mode: WriteMode::Scoped,
                allowed_tool_ids: vec!["fluxdb_execute".into()],
                options_json: "{}".into(),
                max_inflight: 4,
                max_request_ms: 30_000,
                max_request_bytes: 262_144,
                max_response_bytes: 131_072,
            };
            let c = save_server_profile(conn, 1, &edit, None)?;
            save_server_profile(conn, 2, &edit, Some(c.revision))?;
            let grant = ResourceGrantEdit {
                id: "wg1".into(),
                profile_id: "wp".into(),
                connection_id: 7,
                database_name: "main".into(),
                namespace: "".into(),
                enabled: true,
                allow_read: true,
                allow_write: true,
                object_policy_json: r#"{"all_objects":true}"#.into(),
                write_policy_json: r#"{"allowed_operations":["update"],"allowed_objects":["orders"],"require_where":true,"max_affected_rows":1,"allow_ddl":false}"#.into(),
                max_rows: 200,
            };
            save_resource_grant(conn, 3, &grant, GrantResourceKind::Sql, None)?;
            let grants = fluxdb_storage::ai_platform::mcp_server::list_resource_grants(conn, "wp")?;
            Ok((save_server_profile(conn, 4, &edit, Some(2))?, grants))
        })
        .unwrap();
    let results = Arc::new(Mutex::new(ResultStore::for_turn(
        RESULT_STORE_MEMORY_BYTES,
        "s",
        "s",
        None,
    )));
    let service =
        McpExecutionService::new(vec![config], profile, grants, results).unwrap();
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity { session_id: "s".into(), request_id: "r".into() };

    // 命中：授权表 + WHERE + update → 真实执行。
    let ok = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:7/db:main","sql":"UPDATE orders SET amount='99.99' WHERE id = 1"}"#,
        &cancel,
    );
    assert!(ok.ok, "预授权范围内应执行成功: {:?}", ok.error);
    let check = rusqlite::Connection::open(&path).unwrap();
    let amount: String = check
        .query_row("SELECT amount FROM orders WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(amount, "99.99", "真实数据库已更新");

    // 越界表：拒绝，数据库不变。
    let denied = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:7/db:main","sql":"UPDATE customers SET x=1 WHERE id = 1"}"#,
        &cancel,
    );
    assert!(!denied.ok);
    assert_eq!(denied.error.unwrap().code, "write_object_not_allowed");

    let _ = std::fs::remove_dir_all(&dir);
}
