//! SQLite 大字段与零行结果的收页行为。
//!
//! 用临时文件库自建数据，不依赖外部服务。覆盖两条被改成流式收页的路径：
//! 脚本执行与参数化执行；零行结果仍必须带列结构。

use fluxdb_connectors::connector_for;
use fluxdb_core::{
    CellContentStore, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint,
    QueryExecutionOptions, QueryMode, QueryRequest,
};
use std::collections::BTreeMap;

struct TempDb(std::path::PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn temp_db(tag: &str) -> TempDb {
    let path = std::env::temp_dir().join(format!(
        "fluxdb-{tag}-{}-{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    TempDb(path)
}

fn config(db: &TempDb) -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId(9_908),
        name: "SQLite large values".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: db.0.clone(),
            read_only: false,
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
        oracle_profile: None,
    }
}

fn run(config: &ConnectionConfig, text: &str) -> fluxdb_core::QueryExecutionResult {
    let result = connector_for(config)
        .expect("connector")
        .execute(&QueryRequest {
            connection_id: config.id,
            database: Some("main".into()),
            schema: None,
            session_id: None,
            text: text.into(),
            mode: QueryMode::All,
            options: QueryExecutionOptions::default(),
        })
        .expect("execute");
    for summary in &result.summaries {
        assert!(summary.success, "{}: {}", text, summary.message);
    }
    result
}

/// 内容存储桩：只验证「页里留下的是引用」，不落盘。
struct MemoryStore;

impl CellContentStore for MemoryStore {
    fn put(
        &self,
        owner: &str,
        _kind: fluxdb_core::LargeValueKind,
        bytes: &[u8],
    ) -> fluxdb_core::Result<String> {
        Ok(format!("{owner}-{}", bytes.len()))
    }

    fn stat(&self, content_id: &str) -> Option<fluxdb_core::CellContentMeta> {
        let length = content_id.rsplit('-').next()?.parse().ok()?;
        Some(fluxdb_core::CellContentMeta {
            byte_length: length,
            kind: fluxdb_core::LargeValueKind::Text,
        })
    }

    fn read(&self, _content_id: &str, _offset: u64, _len: usize) -> fluxdb_core::Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn release(&self, _content_id: &str) {}

    fn release_owner(&self, _owner: &str) {}
}

/// 3 MB 文本：超过单格外置阈值，必须变成引用；同时验证参数化路径不丢行。
#[test]
fn sqlite_large_text_is_spilled_on_both_execution_paths() {
    fluxdb_core::register_cell_content_store(std::sync::Arc::new(MemoryStore));
    let db = temp_db("large-text");
    let config = config(&db);

    run(
        &config,
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
    );
    // 用表达式生成大文本，避免把几 MB 字面量塞进 SQL。
    run(
        &config,
        "INSERT INTO notes (id, body) VALUES (1, replace(hex(zeroblob(1500000)), '0', 'a'))",
    );

    for (tag, result) in [
        ("script", run(&config, "SELECT id, body FROM notes")),
        (
            "parameterized",
            connector_for(&config)
                .expect("connector")
                .execute_parameterized(&fluxdb_core::ParameterizedQueryRequest {
                    request: QueryRequest {
                        connection_id: config.id,
                        database: Some("main".into()),
                        schema: None,
                        session_id: None,
                        text: "SELECT id, body FROM notes WHERE id = ?".into(),
                        mode: QueryMode::All,
                        options: QueryExecutionOptions::default(),
                    },
                    parameters: vec![fluxdb_core::SqlParameter::I64("1".into())],
                })
                .expect("parameterized"),
        ),
    ] {
        let page = result
            .results
            .first()
            .unwrap_or_else(|| panic!("{tag}: 无结果页"));
        assert_eq!(page.rows.len(), 1, "{tag}: 大字段不能让行消失");
        assert!(page.incomplete.is_none(), "{tag}: 预算内不该标不完整");
        let value = &page.rows[0].values[1];
        let large = value
            .large_value()
            .unwrap_or_else(|| panic!("{tag}: 3 MB 文本必须外置"));
        assert_eq!(large.byte_length, Some(3_000_000), "{tag}");
        assert!(large.preview_truncated, "{tag}: 预览必须标明只是片段");
    }
}

/// 零行结果仍要带列结构（SQLite 靠 `describe` 补），两种路径都不能退化。
#[test]
fn sqlite_empty_result_keeps_columns() {
    let db = temp_db("empty");
    let config = config(&db);
    run(
        &config,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)",
    );

    let script = run(&config, "SELECT id, label FROM items");
    let page = script.results.first().expect("结果页");
    assert!(page.rows.is_empty());
    let names = page
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["id", "label"], "脚本路径零行也要给出列结构");

    let parameterized = connector_for(&config)
        .expect("connector")
        .execute_parameterized(&fluxdb_core::ParameterizedQueryRequest {
            request: QueryRequest {
                connection_id: config.id,
                database: Some("main".into()),
                schema: None,
                session_id: None,
                text: "SELECT id, label FROM items WHERE id = ?".into(),
                mode: QueryMode::All,
                options: QueryExecutionOptions::default(),
            },
            parameters: vec![fluxdb_core::SqlParameter::I64("7".into())],
        })
        .expect("parameterized");
    let page = parameterized.results.first().expect("结果页");
    assert!(page.rows.is_empty());
    assert_eq!(
        page.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "label"],
        "参数化路径零行也要给出列结构"
    );
}
