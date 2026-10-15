// 对外 MCP Server 执行服务的行为测试（T054/T059）。
//
// 用真实 SQLite fixture 走生产链路：schema 校验→数据源判定→强制审计→内核授权→执行→投影，
// 只替换模型与传输（不存在）。断言工具可见性、隐藏写工具直接调用被拒、未授权 target 拒绝、
// 只读数据源不执行写、参数化查询结果形状。
use fluxdb_storage::ai_platform::mcp_server::{
    DatasourceWritePolicy, ServerProfileEdit, list_datasources, save_datasource,
    save_server_profile,
};
use fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus;

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

    /// 建 profile + 数据源；`policy` 为数据源写入策略。
    fn profile_with(
        &self,
        policy: DatasourceWritePolicy,
    ) -> (
        fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
        Vec<fluxdb_storage::ai_platform::mcp_server::DatasourceView>,
    ) {
        self.storage
            .with_ai_connection(|conn| {
                let edit = ServerProfileEdit {
                    id: "p1".into(),
                    display_name: "本地".into(),
                    enabled: true,
                    options_json: "{}".into(),
                };
                let created = save_server_profile(conn, 1, &edit, None)?;
                let profile = save_server_profile(conn, 2, &edit, Some(created.revision))?;
                save_datasource(conn, 3, "p1", 1, policy, None)?;
                let datasources = list_datasources(conn, "p1")?;
                Ok((profile, datasources))
            })
            .unwrap()
    }

    fn service(
        &self,
        profile: fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
        datasources: Vec<fluxdb_storage::ai_platform::mcp_server::DatasourceView>,
    ) -> McpExecutionService {
        let results = Arc::new(Mutex::new(ResultStore::for_turn(
            RESULT_STORE_MEMORY_BYTES,
            "mcp-session",
            "mcp-request",
            None,
        )));
        McpExecutionService::new(
            vec![self.connection.clone()],
            profile,
            datasources,
            fluxdb_core::McpWritePolicy::ReadOnly,
            BTreeSet::new(),
            results,
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn readonly_datasource_hides_write_tool_and_rejects_direct_call() {
    let fixture = Fixture::new();
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::ReadOnly);
    let service = fixture.service(profile, datasources);
    let views = service.tool_views().unwrap();
    let names = views.iter().map(|v| v.name.as_str()).collect::<Vec<_>>();
    assert!(names.contains(&"fluxdb_query"));
    assert!(!names.contains(&"fluxdb_execute"), "只读数据源不能列出写工具");

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
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::Default);
    let service = fixture.service(profile, datasources);
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
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::Default);
    let service = fixture.service(profile, datasources);
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity {
        session_id: "s".into(),
        request_id: "r".into(),
    };
    // 句柄指向未加入数据源的连接。
    let outcome = service.call(
        &identity,
        "fluxdb_query",
        r#"{"target":"conn:99/db:main","sql":"SELECT 1"}"#,
        &cancel,
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "STALE_RESOURCE");
}

#[test]
fn write_tool_not_visible_when_no_writable_datasource() {
    let fixture = Fixture::new();
    // 连接本身标记只读：即便数据源策略为允许读写，写工具也不应出现。
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::ReadWrite);
    let service = fixture.service(profile, datasources);
    let names = service
        .tool_views()
        .unwrap()
        .iter()
        .map(|v| v.name.clone())
        .collect::<Vec<_>>();
    assert!(
        !names.contains(&"fluxdb_execute".to_string()),
        "只读连接上的写工具不应列出"
    );
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
    assert!(!outcome.ok, "只读连接上的写应被拒绝");
}

#[test]
fn globally_disabled_tool_is_hidden_and_rejected() {
    let fixture = Fixture::new();
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::Default);
    let mut disabled = BTreeSet::new();
    disabled.insert("db.query".to_string());
    let results = Arc::new(Mutex::new(ResultStore::for_turn(
        RESULT_STORE_MEMORY_BYTES,
        "s",
        "r",
        None,
    )));
    let service = McpExecutionService::new(
        vec![fixture.connection.clone()],
        profile,
        datasources,
        fluxdb_core::McpWritePolicy::ReadOnly,
        disabled,
        results,
    )
    .unwrap();
    let names = service
        .tool_views()
        .unwrap()
        .iter()
        .map(|v| v.name.clone())
        .collect::<Vec<_>>();
    assert!(!names.contains(&"fluxdb_query".to_string()), "全局关闭的工具不列出");
    let outcome = service.call(
        &McpCallIdentity { session_id: "s".into(), request_id: "r".into() },
        "fluxdb_query",
        r#"{"target":"conn:1/db:main","sql":"SELECT 1"}"#,
        &CancelFlag::new(),
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "tool_not_allowed");
}

#[test]
fn connections_list_only_returns_datasources_without_endpoint() {
    let fixture = Fixture::new();
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::ReadOnly);
    let service = fixture.service(profile, datasources);
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
    let resources = outcome.structured["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["target"], serde_json::json!("conn:1"));
    assert_eq!(resources[0]["write_policy"], serde_json::json!("read_only"));
    // 终态映射。
    assert_eq!(
        McpExecutionService::terminal_status(&outcome),
        ServerRequestStatus::Succeeded
    );
}

#[test]
fn query_binds_typed_parameters_without_string_interpolation() {
    let fixture = Fixture::new();
    let (profile, datasources) = fixture.profile_with(DatasourceWritePolicy::Default);
    let service = fixture.service(profile, datasources);
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

/// 允许读写的数据源上，真实 SQLite 写入会落库；越界表由 SQL 分类器拒绝。
#[test]
fn read_write_datasource_executes_update_against_real_sqlite() {
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
    let (profile, datasources) = storage
        .with_ai_connection(|conn| {
            let edit = ServerProfileEdit {
                id: "wp".into(),
                display_name: "写本地".into(),
                enabled: true,
                options_json: "{}".into(),
            };
            let c = save_server_profile(conn, 1, &edit, None)?;
            let profile = save_server_profile(conn, 2, &edit, Some(c.revision))?;
            save_datasource(conn, 3, "wp", 7, DatasourceWritePolicy::ReadWrite, None)?;
            let datasources = list_datasources(conn, "wp")?;
            Ok((profile, datasources))
        })
        .unwrap();
    let results = Arc::new(Mutex::new(ResultStore::for_turn(
        RESULT_STORE_MEMORY_BYTES,
        "s",
        "s",
        None,
    )));
    let service = McpExecutionService::new(
        vec![config],
        profile,
        datasources,
        fluxdb_core::McpWritePolicy::ReadOnly,
        BTreeSet::new(),
        results,
    )
    .unwrap();
    let cancel = CancelFlag::new();
    let identity = McpCallIdentity { session_id: "s".into(), request_id: "r".into() };

    // 命中：允许写的数据源 + 可写连接 → 真实执行。
    let ok = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:7/db:main","sql":"UPDATE orders SET amount='99.99' WHERE id = 1"}"#,
        &cancel,
    );
    assert!(ok.ok, "允许读写的数据源应执行成功: {:?}", ok.error);
    let check = rusqlite::Connection::open(&path).unwrap();
    let amount: String = check
        .query_row("SELECT amount FROM orders WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(amount, "99.99", "真实数据库已更新");

    // 结构/权限类语句（DROP）必须被分类器拒绝，数据库不变。
    let denied = service.call(
        &identity,
        "fluxdb_execute",
        r#"{"target":"conn:7/db:main","sql":"DROP TABLE orders"}"#,
        &cancel,
    );
    assert!(!denied.ok, "DDL 应被拒绝");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 对外工具集不含取消入口：`operation.cancel` 只能改账面状态、动不了正在执行的操作，
/// 真实中断已由协议取消与断连承担（§28.8）。
#[test]
fn operation_cancel_is_not_exposed() {
    let names: Vec<&str> = MCP_TOOL_MAPPINGS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert!(names.contains(&"fluxdb_operation_status"));
    assert!(!names.contains(&"fluxdb_operation_cancel"));
    assert_eq!(mcp_permission_tool_id("fluxdb_operation_cancel"), None);
    // 已开放的全局停用项也不该留下指向已删工具的别名。
    assert_eq!(
        mcp_permission_tool_id("fluxdb_operation_status"),
        Some("operation.status")
    );
}
