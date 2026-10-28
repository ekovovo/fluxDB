//! 真实 PostgreSQL 大字段读取验证（表浏览 + 参数化查询）。
//!
//! 仅在显式提供 `FLUXDB_PG_SMOKE=host:port:user:password` 时运行；默认忽略。
//! 自建表并清理，不动其它 schema。重点验证：流式收页后大字段变成内容引用，
//! 页驻留不随字段大小增长，且引用仍可按块读回。

use fluxdb_app::CellContentFileStore;
use fluxdb_connectors::connector_for;
use fluxdb_core::{
    CellContentStore, CellValue, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint,
    ObjectKind, ObjectPath, ParameterizedQueryRequest, PostgresBasicOptions,
    PostgresConnectionProfile, QueryExecutionOptions, QueryMode, QueryRequest, SecretRef,
    SqlParameter,
};
use std::collections::BTreeMap;
use std::sync::Arc;

fn smoke_config() -> ConnectionConfig {
    let raw = std::env::var("FLUXDB_PG_SMOKE").expect("FLUXDB_PG_SMOKE");
    let parts = raw.splitn(4, ':').collect::<Vec<_>>();
    assert_eq!(parts.len(), 4, "FLUXDB_PG_SMOKE=host:port:user:password");
    let port = parts[1].parse().expect("PostgreSQL port");
    ConnectionConfig {
        id: ConnectionId(9_909),
        name: "PostgreSQL large values".into(),
        kind: DatabaseKind::Postgres,
        endpoint: Endpoint::Tcp {
            host: parts[0].into(),
            port,
            database: Some("postgres".into()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: Some(PostgresConnectionProfile {
            basic: PostgresBasicOptions {
                host: parts[0].into(),
                port,
                maintenance_database: "postgres".into(),
                username: parts[2].into(),
                password: SecretRef::inline(parts[3]),
            },
            ..PostgresConnectionProfile::default()
        }),
        dameng_profile: None,
        sqlserver_profile: None,
        oracle_profile: None,
    }
    .postgres_resolved()
}

fn execute(config: &ConnectionConfig, text: &str) {
    let result = connector_for(config)
        .expect("connector")
        .execute(&QueryRequest {
            connection_id: config.id,
            database: Some("postgres".into()),
            schema: None,
            session_id: None,
            text: text.into(),
            mode: QueryMode::All,
            options: QueryExecutionOptions::default(),
        })
        .expect("execute setup");
    for summary in &result.summaries {
        assert!(summary.success, "{text}: {}", summary.message);
    }
}

/// 引用占用的页内存只有预览；原始 `Text`/`Json` 是整份内容。
fn page_bytes(value: &CellValue) -> usize {
    match value {
        CellValue::Text(text) | CellValue::Json(text) => text.len(),
        CellValue::Bytes(bytes) => bytes.len(),
        CellValue::LargeValue(_) | CellValue::BinarySummary(_) => 256,
        _ => 16,
    }
}

#[test]
#[ignore = "需要 FLUXDB_PG_SMOKE=host:port:user:password 指向可达的 PostgreSQL"]
fn postgres_large_cells_stream_into_references() {
    let config = smoke_config();
    let dir = std::env::temp_dir().join(format!("fluxdb-pg-large-{}", std::process::id()));
    let store = Arc::new(CellContentFileStore::new(&dir));
    fluxdb_core::register_cell_content_store(store.clone());

    let table = format!("fluxdb_large_values_{}", std::process::id());
    execute(
        &config,
        &format!("CREATE TABLE {table} (id int primary key, body text, doc jsonb)"),
    );
    execute(
        &config,
        &format!(
            "INSERT INTO {table} VALUES \
             (1, repeat('a', 8000000), NULL), \
             (2, repeat('c', 300000), NULL), \
             (3, NULL, jsonb_build_object('k', repeat('b', 2000000))), \
             (4, 'small', NULL)"
        ),
    );

    let path = ObjectPath {
        connection_id: config.id,
        database: Some("postgres".into()),
        schema: Some("public".into()),
        name: table.clone(),
        kind: ObjectKind::Table,
    };
    let table_page = connector_for(&config)
        .expect("connector")
        .load_data(&path, 0, 100, &[], &[])
        .expect("load_data");
    let table_refs = table_page
        .rows
        .iter()
        .flat_map(|row| row.values.iter())
        .filter_map(|value| value.large_value().cloned())
        .collect::<Vec<_>>();
    let table_bytes = table_page
        .rows
        .iter()
        .flat_map(|row| row.values.iter())
        .map(page_bytes)
        .sum::<usize>();

    let query = connector_for(&config)
        .expect("connector")
        .execute_parameterized(&ParameterizedQueryRequest {
            request: QueryRequest {
                connection_id: config.id,
                database: Some("postgres".into()),
                schema: None,
                session_id: None,
                text: format!("SELECT id, body FROM {table} WHERE id >= $1 ORDER BY id"),
                mode: QueryMode::All,
                options: QueryExecutionOptions::default(),
            },
            parameters: vec![SqlParameter::I64("1".into())],
        })
        .expect("parameterized");
    let query_page = query.results.first().expect("结果页");
    let query_refs = query_page
        .rows
        .iter()
        .flat_map(|row| row.values.iter())
        .filter_map(|value| value.large_value().cloned())
        .collect::<Vec<_>>();
    let readable = query_refs
        .iter()
        .all(|reference| store.stat(&reference.content_id).is_some());

    execute(&config, &format!("DROP TABLE {table}"));
    store.release_owner(&fluxdb_core::table_content_owner(&path));
    store.release_owner(&fluxdb_core::result_content_owner(None, config.id));
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(table_page.rows.len(), 4, "表浏览不能丢行");
    assert_eq!(
        table_refs.len(),
        3,
        "8 MB / 300 KB 文本与 2 MB JSON 都应外置"
    );
    assert!(
        table_bytes < 1024 * 1024,
        "表浏览页驻留 {table_bytes} 字节，大字段没有被换成引用"
    );
    assert_eq!(query_page.rows.len(), 4, "参数化查询不能丢行");
    assert_eq!(query_refs.len(), 2, "只选 body：两格超阈值文本应外置");
    assert!(readable, "执行结束前内容引用必须可读");
    for reference in table_refs.iter().chain(query_refs.iter()) {
        assert!(reference.preview_truncated, "预览必须标明只是片段");
    }
}
