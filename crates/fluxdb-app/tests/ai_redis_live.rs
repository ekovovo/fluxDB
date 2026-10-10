//! Dedicated Redis fixture only; run explicitly with FLUXDB_T025_REDIS_PORT.
use fluxdb_app::{MapSqlResolver, ToolCatalog, standard_redis_catalog};
use fluxdb_connectors::RedisConnector;
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ToolExecutionContext,
    ToolReceiptStatus,
};
use std::sync::Arc;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Wake;
    impl std::task::Wake for Wake {
        fn wake(self: Arc<Self>) {}
    }
    let waker = std::task::Waker::from(Arc::new(Wake));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(value) => value,
        _ => panic!("Redis tools must complete on the worker"),
    }
}

struct Fixture {
    connector: RedisConnector,
    catalog: ToolCatalog,
    prefix: String,
    keys: Vec<String>,
}
impl Fixture {
    fn new() -> Self {
        let port = std::env::var("FLUXDB_T025_REDIS_PORT")
            .expect("explicit isolated Redis port")
            .parse()
            .unwrap();
        let config = ConnectionConfig {
            id: ConnectionId(25),
            name: "isolated Redis".into(),
            kind: DatabaseKind::Redis,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port,
                database: Some("0".into()),
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        };
        Self {
            connector: RedisConnector::with_config(config.clone()),
            catalog: standard_redis_catalog(Arc::new(MapSqlResolver::new([config])), true).unwrap(),
            prefix: format!("fluxdb:t025:{}:", uuid::Uuid::new_v4()),
            keys: Vec::new(),
        }
    }
    fn key(&mut self, suffix: &str) -> String {
        let key = format!("{}{suffix}", self.prefix);
        self.keys.push(key.clone());
        key
    }
    fn argv(&self, db: u32, argv: &[&str]) -> fluxdb_core::CommandReply {
        self.connector
            .execute_argv(db, &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap()
    }
    fn tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        cancel: CancelFlag,
    ) -> fluxdb_core::ToolReceipt {
        let args = self.catalog.prepare(name, &arguments.to_string()).unwrap();
        block_on(self.catalog.executable(name).unwrap().execute(
            ToolExecutionContext {
                session_id: "isolated".into(),
                turn_id: "isolated".into(),
                iteration: 1,
                call_id: uuid::Uuid::new_v4().to_string(),
                cancel,
                progress: None,
            },
            args.to_json_string(),
        ))
        .unwrap()
    }
    fn inspect(
        &self,
        key: &str,
        cursor: &str,
        limit: usize,
        max_bytes: usize,
    ) -> serde_json::Value {
        let receipt = self.tool(
            "redis.inspect",
            serde_json::json!({
                "target":"conn:25/db:0","key":key,"view":"value",
                "cursor":cursor,"limit":limit,"max_bytes":max_bytes,
            }),
            CancelFlag::new(),
        );
        assert_eq!(
            receipt.status,
            ToolReceiptStatus::Succeeded,
            "{}",
            receipt.result_json
        );
        let value: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        value["rows"][0][1]["value"].clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for db in [0, 1] {
            for key in &self.keys {
                let _ = self
                    .connector
                    .execute_argv(db, &["DEL".into(), key.clone()]);
            }
        }
    }
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn redis_types_pages_budgets_db_scope_and_cancel_have_real_receipts() {
    let mut f = Fixture::new();
    let string = f.key("string");
    f.argv(0, &["SET", &string, "abcdefghij"]);
    assert_eq!(f.inspect(&string, "0", 2, 5)["value"]["text"], "abcde");
    assert_eq!(f.inspect(&string, "5", 2, 5)["value"]["text"], "fghij");
    let list = f.key("list");
    f.argv(0, &["RPUSH", &list, "a", "b", "c"]);
    assert_eq!(f.inspect(&list, "0", 2, 4096)["truncated"], true);
    assert_eq!(f.inspect(&list, "2", 2, 4096)["value"][0]["text"], "c");
    for (kind, command) in [("hash", "HSET"), ("set", "SADD"), ("zset", "ZADD")] {
        let key = f.key(kind);
        match kind {
            "hash" => {
                f.argv(0, &[command, &key, "a", "1", "b", "2", "c", "3"]);
            }
            "set" => {
                f.argv(0, &[command, &key, "a", "b", "c"]);
            }
            _ => {
                f.argv(0, &[command, &key, "1", "a", "2", "b", "3", "c"]);
            }
        }
        let reply = f.tool(
            "redis.inspect",
            serde_json::json!({"target":"conn:25/db:0","key":key,"view":"value","limit":1}),
            CancelFlag::new(),
        );
        // Compact encodings return the whole set even with COUNT 1.
        assert_eq!(reply.status, ToolReceiptStatus::Failed);
        assert_eq!(reply.error_code.as_deref(), Some("redis_page_too_large"));
        assert_eq!(f.inspect(&key, "0", 10, 4096)["complete"], true);
    }
    let stream = f.key("stream");
    f.argv(0, &["XADD", &stream, "1-0", "f", "a"]);
    f.argv(0, &["XADD", &stream, "2-0", "f", "b"]);
    let first = f.inspect(&stream, "0", 1, 4096);
    assert_eq!(first["next_cursor"], "1-0");
    assert_eq!(first["truncated"], true);
    assert_eq!(f.inspect(&stream, "1-0", 1, 4096)["next_cursor"], "2-0");
    let large = f.key("large");
    f.argv(0, &["SET", &large, &"x".repeat(100_000)]);
    let reply = f.tool(
        "redis.execute",
        serde_json::json!({"target":"conn:25/db:0","argv":["GET",large]}),
        CancelFlag::new(),
    );
    assert_eq!(reply.status, ToolReceiptStatus::Failed);
    assert!(reply.result_json.contains("VALUE_TOO_LARGE"));
    // After rejecting an incomplete frame, the next command must use a clean connection.
    assert_eq!(
        f.argv(0, &["PING"]),
        fluxdb_core::CommandReply::Status("PONG".into())
    );
    f.argv(1, &["SET", &string, "db-one"]);
    assert_eq!(
        f.inspect(&string, "0", 2, 20)["value"]["text"],
        "abcdefghij"
    );
    let cancelled = CancelFlag::new();
    cancelled.request();
    let key = f.key("cancelled");
    let reply = f.tool(
        "redis.execute",
        serde_json::json!({"target":"conn:25/db:0","argv":["SET",key,"must-not-write"]}),
        cancelled,
    );
    assert_eq!(reply.status, ToolReceiptStatus::Cancelled);
    assert_eq!(
        f.argv(0, &["EXISTS", &key]),
        fluxdb_core::CommandReply::Integer(0)
    );
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn scan_continues_without_losing_or_falsely_completing_keys() {
    let mut f = Fixture::new();
    for i in 0..200 {
        let key = f.key(&format!("key:{i}"));
        f.argv(0, &["SET", &key, "value"]);
    }
    let mut cursor = "0".to_string();
    let mut seen = std::collections::BTreeSet::new();
    let mut complete = false;
    for _ in 0..500 {
        let receipt = f.tool(
            "redis.scan",
            serde_json::json!({
                "target":"conn:25/db:0","pattern":format!("{}*",f.prefix),
                "cursor":cursor,"count":1,"max_keys":1000,
            }),
            CancelFlag::new(),
        );
        assert_eq!(
            receipt.status,
            ToolReceiptStatus::Succeeded,
            "{}",
            receipt.result_json
        );
        let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        for key in payload["rows"][0][1].as_array().unwrap() {
            seen.insert(key.as_str().unwrap().to_string());
        }
        cursor = payload["rows"][0][0].as_str().unwrap().to_string();
        complete = payload["rows"][0][2]["complete"].as_bool().unwrap();
        if complete {
            break;
        }
    }
    assert!(complete);
    assert_eq!(seen.len(), 200);
    let receipt = f.tool(
        "redis.scan",
        serde_json::json!({
            "target":"conn:25/db:0","pattern":format!("{}*",f.prefix),
            "count":1000,"max_keys":1,
        }),
        CancelFlag::new(),
    );
    assert_eq!(
        receipt.status,
        ToolReceiptStatus::Failed,
        "over-budget batch cannot silently discard keys"
    );
    assert_eq!(receipt.error_code.as_deref(), Some("redis_page_too_large"));
}
