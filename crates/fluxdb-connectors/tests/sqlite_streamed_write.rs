//! 流式（按块追加）写入大字段的真实 SQLite 往返验证。
//!
//! 用临时文件库自建数据，不依赖外部服务。重点验证三件事：
//! 1. 多块追加后落库内容与暂存内容逐字节一致（块边界不能补/漏字节）；
//! 2. 占位值（只有类型与大小的摘要）绝不能落库，否则就是静默数据损坏；
//! 3. 暂存内容提前结束时报错并整体回滚，不能写半份。

use fluxdb_connectors::connector_for;
use fluxdb_core::{
    CellContentMeta, CellContentStore, CellUpdate, CellValue, ConnectionConfig, ConnectionId,
    DataChangeSet, DatabaseKind, Endpoint, LargeValueKind, ObjectKind, ObjectPath, RowIdentity,
    RowUpdate, StagedContent, StreamedValue,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

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
        id: ConnectionId(9_911),
        name: "SQLite streamed write".into(),
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

/// 内存内容存储：只为让 `streamed_chunk_at` 按块读到内容，验证的重点在连接器。
#[derive(Default)]
struct MemoryStore {
    entries: Mutex<HashMap<String, Vec<u8>>>,
}

/// 内容存储是进程级单例：多个用例并行注册会互相覆盖，所以只注册一次。
fn store() -> Arc<MemoryStore> {
    static STORE: std::sync::OnceLock<Arc<MemoryStore>> = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| {
            let store = Arc::new(MemoryStore::default());
            fluxdb_core::register_cell_content_store(store.clone());
            store
        })
        .clone()
}

impl CellContentStore for MemoryStore {
    fn put(
        &self,
        _owner: &str,
        _kind: LargeValueKind,
        bytes: &[u8],
    ) -> fluxdb_core::Result<String> {
        let mut entries = self.entries.lock().expect("entries");
        let id = format!("content-{}", entries.len());
        entries.insert(id.clone(), bytes.to_vec());
        Ok(id)
    }

    fn stat(&self, content_id: &str) -> Option<CellContentMeta> {
        let entries = self.entries.lock().ok()?;
        let bytes = entries.get(content_id)?;
        Some(CellContentMeta {
            byte_length: bytes.len() as u64,
            kind: LargeValueKind::Binary,
        })
    }

    fn read(&self, content_id: &str, offset: u64, len: usize) -> fluxdb_core::Result<Vec<u8>> {
        // 每次只给 4 KiB：即使内容远小于连接器的分块大小，追加循环也会走多轮，
        // 从而验证「按实际读到的长度推进」而不是一次读完就算完。
        const MAX_READ: usize = 4096;
        let entries = self.entries.lock().expect("entries");
        let bytes = entries
            .get(content_id)
            .ok_or_else(|| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, "内容不存在"))?;
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(len.min(MAX_READ)).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }

    fn release(&self, content_id: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(content_id);
        }
    }

    fn release_owner(&self, _owner: &str) {}
}

fn execute(config: &ConnectionConfig, text: &str) {
    let result = connector_for(config)
        .expect("connector")
        .execute(&fluxdb_core::QueryRequest {
            connection_id: config.id,
            database: Some("main".into()),
            schema: None,
            session_id: None,
            text: text.into(),
            mode: fluxdb_core::QueryMode::All,
            options: fluxdb_core::QueryExecutionOptions::default(),
        })
        .expect("execute");
    for summary in &result.summaries {
        assert!(summary.success, "{}: {}", text, summary.message);
    }
}

/// 读出 id=1 行的 DATA 列原始字节。
///
/// 大字段会被连接器外置成内容引用（页里只有预览），这里按引用把真内容读回来，
/// 读完立即释放，避免测试自己把归属配额吃满。
fn read_data(config: &ConnectionConfig) -> Vec<u8> {
    let result = connector_for(config)
        .expect("connector")
        .execute(&fluxdb_core::QueryRequest {
            connection_id: config.id,
            database: Some("main".into()),
            schema: None,
            session_id: None,
            text: "SELECT DATA FROM blobs WHERE ID = 1".into(),
            mode: fluxdb_core::QueryMode::All,
            options: fluxdb_core::QueryExecutionOptions::default(),
        })
        .expect("select");
    let page = result.results.first().expect("结果页");
    match page.rows.first().and_then(|row| row.values.first()) {
        Some(CellValue::Bytes(bytes)) => bytes.clone(),
        Some(CellValue::Text(text)) | Some(CellValue::Json(text)) => text.as_bytes().to_vec(),
        Some(CellValue::LargeValue(large)) => {
            let store = store();
            let mut out = Vec::with_capacity(large.byte_length.unwrap_or(0) as usize);
            let mut offset = 0u64;
            loop {
                let chunk = store
                    .read(&large.content_id, offset, fluxdb_core::CELL_CHUNK_BYTES)
                    .expect("read content");
                if chunk.is_empty() {
                    break;
                }
                offset += chunk.len() as u64;
                out.extend_from_slice(&chunk);
            }
            store.release(&large.content_id);
            out
        }
        other => panic!("意外的取值：{other:?}"),
    }
}

fn streamed_update(content_id: &str, byte_length: u64) -> DataChangeSet {
    streamed_update_of(
        content_id,
        byte_length,
        LargeValueKind::Binary,
        CellValue::BinarySummary(fluxdb_core::BinaryCellSummary {
            type_name: "BLOB".into(),
            is_null: false,
            byte_length,
            preview_hex: None,
        }),
    )
}

/// 文本列的流式写入：占位值是可读摘要，`kind` 决定连接器按文本追加。
fn streamed_text_update(content_id: &str, byte_length: u64) -> DataChangeSet {
    streamed_update_of(
        content_id,
        byte_length,
        LargeValueKind::Text,
        CellValue::Text(format!(
            "TEXT [{}]",
            fluxdb_core::format_byte_length(byte_length)
        )),
    )
}

fn streamed_update_of(
    content_id: &str,
    byte_length: u64,
    kind: LargeValueKind,
    placeholder: CellValue,
) -> DataChangeSet {
    DataChangeSet {
        object: ObjectPath {
            connection_id: ConnectionId(9_911),
            database: Some("main".into()),
            schema: None,
            name: "blobs".into(),
            kind: ObjectKind::Table,
        },
        inserts: Vec::new(),
        updates: vec![RowUpdate {
            identity: RowIdentity {
                values: BTreeMap::from([("ID".to_string(), CellValue::I64(1))]),
            },
            cells: vec![CellUpdate::streamed(
                "DATA",
                StreamedValue::new(
                    Arc::new(StagedContent::new(content_id, byte_length)),
                    kind,
                    placeholder,
                ),
            )],
        }],
        deletes: Vec::new(),
        insert_intents: None,
    }
}

fn setup(tag: &str) -> (TempDb, ConnectionConfig) {
    let db = temp_db(tag);
    let config = config(&db);
    execute(
        &config,
        "CREATE TABLE blobs (ID INTEGER PRIMARY KEY, DATA BLOB)",
    );
    execute(&config, "INSERT INTO blobs (ID, DATA) VALUES (1, x'00')");
    (db, config)
}

/// 内容被存储切成多块返回：多轮追加后必须逐字节等于原文，
/// 且占位摘要（类型 + 大小）绝不能落库。
#[test]
fn streamed_blob_round_trips_across_chunk_boundary() {
    let store = store();
    let (_db, config) = setup("streamed-blob");

    let payload = (0..10_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<u8>>();
    let content_id = store
        .put("upload-staging", LargeValueKind::Binary, &payload)
        .expect("put");

    connector_for(&config)
        .expect("connector")
        .apply_changes(&streamed_update(&content_id, payload.len() as u64))
        .expect("apply streamed write");

    assert_eq!(read_data(&config), payload, "落库内容必须与原文逐字节一致");
}

/// 暂存内容短于声明的字节数（例如暂存被提前释放）：必须报错并回滚，
/// 不能把半份内容当成成功写入。
#[test]
fn short_staged_content_fails_and_rolls_back() {
    let store = store();
    let (_db, config) = setup("short-staged");
    let original = read_data(&config);

    let payload = vec![7u8; 1024];
    let content_id = store
        .put("upload-staging", LargeValueKind::Binary, &payload)
        .expect("put");
    // 声明的长度比实际内容多，读到最后一块会提前结束。
    let error = connector_for(&config)
        .expect("connector")
        .apply_changes(&streamed_update(&content_id, payload.len() as u64 + 1))
        .expect_err("短内容必须失败");
    assert!(
        error.message.contains("提前结束"),
        "错误要说明内容不完整：{}",
        error.message
    );
    assert_eq!(read_data(&config), original, "失败必须回滚，原值不变");
}

/// `TEXT` 列同样走流式追加：SQLite 的 `||` 在 BLOB 参与时会返回 TEXT，
/// 这里验证追加结果仍能按文本读回（`CAST(... AS BLOB)` 不影响文本往返）。
#[test]
fn streamed_write_replaces_existing_value_instead_of_appending_to_it() {
    let store = store();
    let (_db, config) = setup("replace-existing");
    // 先写入一份既有内容，确认流式写入是「替换」而不是在上面继续追加。
    execute(&config, "UPDATE blobs SET DATA = x'DEADBEEF' WHERE ID = 1");

    let payload = vec![0xABu8; 64];
    let content_id = store
        .put("upload-staging", LargeValueKind::Binary, &payload)
        .expect("put");

    connector_for(&config)
        .expect("connector")
        .apply_changes(&streamed_update(&content_id, payload.len() as u64))
        .expect("apply streamed write");

    assert_eq!(read_data(&config), payload);
}

/// 文本列：多字节字符跨块边界时，分块必须回退到字符边界，
/// 否则每块单独绑定都会因为半截字符而失败。
#[test]
fn streamed_text_round_trips_when_chunk_splits_multibyte_char() {
    let store = store();
    let db = temp_db("streamed-text");
    let config = config(&db);
    execute(
        &config,
        "CREATE TABLE blobs (ID INTEGER PRIMARY KEY, DATA TEXT)",
    );
    execute(&config, "INSERT INTO blobs (ID, DATA) VALUES (1, 'old')");

    // 4 KiB 的读取上限对 3 字节字符必然会切在字符中间。
    let text = "数据".repeat(3_000);
    let payload = text.as_bytes().to_vec();
    let content_id = store
        .put("upload-staging", LargeValueKind::Text, &payload)
        .expect("put");

    connector_for(&config)
        .expect("connector")
        .apply_changes(&streamed_text_update(&content_id, payload.len() as u64))
        .expect("apply streamed text write");

    assert_eq!(
        String::from_utf8(read_data(&config)).expect("落库内容必须是合法 UTF-8"),
        text,
        "文本内容必须逐字一致，不能因为分块丢字符"
    );
}

/// 非法 UTF-8 内容写文本列必须报错回滚，不能交给数据库做隐式转换。
#[test]
fn invalid_utf8_text_write_is_rejected_and_rolled_back() {
    let store = store();
    let db = temp_db("invalid-utf8-text");
    let config = config(&db);
    execute(
        &config,
        "CREATE TABLE blobs (ID INTEGER PRIMARY KEY, DATA TEXT)",
    );
    execute(&config, "INSERT INTO blobs (ID, DATA) VALUES (1, 'old')");

    let payload = vec![0xFFu8; 32];
    let content_id = store
        .put("upload-staging", LargeValueKind::Text, &payload)
        .expect("put");
    let error = connector_for(&config)
        .expect("connector")
        .apply_changes(&streamed_text_update(&content_id, payload.len() as u64))
        .expect_err("非法 UTF-8 必须拒绝");
    assert!(error.message.contains("UTF-8"), "{}", error.message);
    assert_eq!(
        String::from_utf8(read_data(&config)).expect("utf8"),
        "old",
        "失败必须回滚"
    );
}

/// 暂存引用被丢弃时自动释放内容，不需要调用方逐点记账。
#[test]
fn dropping_changeset_releases_staged_content() {
    let store = store();
    let content_id = store
        .put("upload-staging", LargeValueKind::Binary, b"abc")
        .expect("put");
    {
        let _changes = streamed_update(&content_id, 3);
        assert!(
            store.stat(&content_id).is_some(),
            "改动集持有期间内容必须存在"
        );
    }
    assert!(
        store.stat(&content_id).is_none(),
        "改动集丢弃后内容必须释放"
    );
}
