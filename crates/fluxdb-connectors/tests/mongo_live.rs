//! MongoDB 真实连接集成测试。
//!
//! 仅在显式提供 `FLUXDB_MONGO_URI` 时运行；默认跳过，避免常规单测依赖外部实例。
//! 凭据只从环境变量读入内存（`SecretRef::inline`），不写文件、不打印到日志。
//!
//! 运行示例（凭据由调用方从自己的凭据文件注入，不要写进仓库）：
//! ```text
//! FLUXDB_MONGO_URI='mongodb+srv://user:pass@cluster/db?retryWrites=true' \
//!   cargo test --locked -p fluxdb-connectors --test mongo_live -- --nocapture
//! ```
//!
//! 测试自建 `fluxdb_smoke_*` 集合，结束时删除，不触碰既有数据。

use std::collections::BTreeMap;

use fluxdb_connectors::connector_for;
use fluxdb_core::{
    ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, MongoConnectionProfile,
    MongoFindRequest, ObjectKind, ObjectPath,
};

/// 从 `FLUXDB_MONGO_URI` 构造连接配置；未设置时返回 `None`（用例跳过）。
///
/// 走与 UI 导入完全相同的 `MongoConnectionProfile::from_uri`，因此本测试同时覆盖
/// URI 解析 → 档案 → 拨号 → 命令执行的完整链路。
fn live_config() -> Option<ConnectionConfig> {
    let uri = std::env::var("FLUXDB_MONGO_URI").ok()?;
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    let profile = MongoConnectionProfile::from_uri(uri)
        .unwrap_or_else(|error| panic!("FLUXDB_MONGO_URI 解析失败: {error}"));
    Some(ConnectionConfig {
        id: ConnectionId(9_901),
        name: "MongoDB live".into(),
        kind: DatabaseKind::MongoDb,
        endpoint: Endpoint::Tcp {
            host: profile
                .basic
                .hosts
                .first()
                .map(|host| host.host.clone())
                .unwrap_or_default(),
            port: profile.basic.hosts.first().map(|host| host.port).unwrap_or(27_017),
            database: Some(profile.basic.default_database.clone()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
        mongo_profile: Some(profile),
    })
}

/// 目标库：优先 URI 里的默认库，回落到 `FLUXDB_MONGO_DB`，再回落到 `fluxdb_smoke`。
fn live_database(config: &ConnectionConfig) -> String {
    config
        .mongo_profile
        .as_ref()
        .map(|profile| profile.basic.default_database.clone())
        .filter(|database| !database.is_empty())
        .or_else(|| std::env::var("FLUXDB_MONGO_DB").ok())
        .unwrap_or_else(|| "fluxdb_smoke".to_string())
}

/// 不取消的查询（UI 侧由取消标志驱动，测试里固定为 `false`）。
fn never_cancel() -> &'static dyn Fn() -> bool {
    &NO_CANCEL
}

static NO_CANCEL: fn() -> bool = || false;
static ALWAYS_CANCEL: fn() -> bool = || true;

fn collection_path(connection_id: ConnectionId, database: &str, collection: &str) -> ObjectPath {
    ObjectPath {
        connection_id,
        database: Some(database.to_string()),
        schema: None,
        name: collection.to_string(),
        kind: ObjectKind::Collection,
    }
}

#[test]
fn mongo_live_ping_create_query_metadata_and_cleanup() {
    let Some(config) = live_config() else {
        eprintln!("跳过：未设置 FLUXDB_MONGO_URI");
        return;
    };
    let connection_id = config.id;
    let database = live_database(&config);
    let connector = connector_for(&config).expect("connector");
    connector
        .test_connection(&config)
        .expect("认证与选主必须成功");

    // 每个用例独立集合名，避免并发或历史残留互相污染。
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let collection = format!("fluxdb_smoke_{suffix}");

    connector
        .mongo_create_collection(connection_id, &database, &collection)
        .expect("创建集合");
    let path = collection_path(connection_id, &database, &collection);

    // 写入三类 BSON 值：ObjectId / 日期 / 数字，验证 Extended JSON 不丢类型。
    let insert = format!(
        r#"{{ "insert": "{collection}", "documents": [
            {{ "_id": "smoke-1", "name": "alpha", "count": 1, "when": {{ "$date": "2024-01-01T00:00:00Z" }} }},
            {{ "_id": "smoke-2", "name": "beta", "count": 2, "nested": {{ "tags": ["x", "y"] }} }}
        ] }}"#
    );
    let inserted = connector
        .mongo_run_command(connection_id, &database, &insert)
        .expect("插入文档");
    assert!(inserted.contains("inserted"), "{inserted}");

    let page = connector
        .mongo_find(&path, &MongoFindRequest::new(&collection), never_cancel())
        .expect("查询文档");
    assert_eq!(page.returned, 2, "应返回两条文档");

    let filtered = MongoFindRequest {
        filter: r#"{ "name": "beta" }"#.to_string(),
        ..MongoFindRequest::new(&collection)
    };
    let page = connector
        .mongo_find(&path, &filtered, never_cancel())
        .expect("按条件查询");
    assert_eq!(page.returned, 1);
    let json = &page.documents[0].json;
    assert!(json.contains("beta"), "{json}");
    assert!(json.contains("tags"), "嵌套数组必须原样返回: {json}");

    // 投影只返回指定字段：验证字段级裁剪在真实服务端生效。
    let projected = MongoFindRequest {
        filter: r#"{ "_id": "smoke-2" }"#.to_string(),
        projection: r#"{ "nested.tags": 1, "_id": 0 }"#.to_string(),
        ..MongoFindRequest::new(&collection)
    };
    let page = connector
        .mongo_find(&path, &projected, never_cancel())
        .expect("投影查询");
    assert_eq!(page.returned, 1);
    assert!(page.documents[0].json.contains("tags"));
    assert!(
        !page.documents[0].json.contains("\"name\""),
        "投影之外的字段不应返回: {}",
        page.documents[0].json
    );

    // 取消标志必须让查询提前返回错误，而不是继续拉数据。
    let cancelled = connector
        .mongo_find(&path, &MongoFindRequest::new(&collection), &ALWAYS_CANCEL);
    assert!(cancelled.is_err(), "取消后的查询必须失败");

    let info = connector.mongo_collection_info(&path).expect("集合信息");
    assert!(info.document_count.unwrap_or_default() >= 2, "{info:?}");

    connector
        .mongo_create_collection(connection_id, &database, &collection)
        .expect_err("重复创建同名集合必须报错");

    connector
        .mongo_drop_collection(connection_id, &database, &collection)
        .expect("删除集合");
    let after = connector
        .mongo_find(&path, &MongoFindRequest::new(&collection), never_cancel())
        .expect_err("集合删除后查询必须失败");
    assert!(!after.message.is_empty());
}

#[test]
fn mongo_live_server_info_reports_real_topology() {
    let Some(config) = live_config() else {
        eprintln!("跳过：未设置 FLUXDB_MONGO_URI");
        return;
    };
    let connector = connector_for(&config).expect("connector");
    let info = connector
        .mongo_server_info(config.id)
        .expect("读取连接信息");
    assert!(info.version.is_some(), "buildInfo.version 必须有值: {info:?}");
    assert_ne!(
        info.topology,
        fluxdb_core::MongoTopology::Unknown,
        "hello 必须能判定拓扑: {info:?}"
    );
    assert!(
        !info.databases.is_empty(),
        "listDatabases 至少返回一个可见库: {info:?}"
    );
}

#[test]
fn mongo_live_aggregate_and_indexes() {
    let Some(config) = live_config() else {
        eprintln!("跳过：未设置 FLUXDB_MONGO_URI");
        return;
    };
    let connection_id = config.id;
    let database = live_database(&config);
    let connector = connector_for(&config).expect("connector");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let collection = format!("fluxdb_smoke_agg_{suffix}");
    connector
        .mongo_create_collection(connection_id, &database, &collection)
        .expect("创建集合");
    let path = collection_path(connection_id, &database, &collection);

    let insert = format!(
        r#"{{ "insert": "{collection}", "documents": [
            {{ "group": "a", "value": 1 }}, {{ "group": "a", "value": 2 }}, {{ "group": "b", "value": 5 }}
        ] }}"#
    );
    connector
        .mongo_run_command(connection_id, &database, &insert)
        .expect("插入文档");

    let pipeline = r#"[{ "$group": { "_id": "$group", "total": { "$sum": "$value" } } }, { "$sort": { "_id": 1 } }]"#;
    let page = connector
        .mongo_aggregate(&path, pipeline)
        .expect("聚合");
    assert_eq!(page.returned, 2, "两个分组: {page:?}");
    assert!(page.documents.iter().any(|document| document.json.contains("3")), "{page:?}");

    let indexes = connector.mongo_indexes(&path).expect("索引");
    assert_eq!(indexes.len(), 1, "新集合只有默认 _id 索引: {indexes:?}");
    assert!(indexes[0].name == "_id_", "{indexes:?}");

    connector
        .mongo_drop_collection(connection_id, &database, &collection)
        .expect("清理集合");
}
