//! Dedicated Redis fixture only; run explicitly with FLUXDB_T025_REDIS_PORT.
use fluxdb_app::{MapSqlResolver, RedisKeyScope, ToolCatalog, standard_redis_catalog};
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
        Self::with_scope(None)
    }
    /// 带 prefix 授权范围构造夹具：授权范围外的 key 必须无法被 AI 工具触达。
    fn with_scope(prefixes: Option<Vec<String>>) -> Self {
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
        let resolver = Arc::new(MapSqlResolver::new([config.clone()]));
        let resolver = match prefixes {
            Some(prefixes) => Arc::new(MapSqlResolver::new([config.clone()]).with_redis_key_scope(
                config.id,
                RedisKeyScope::Prefixes {
                    database: 0,
                    prefixes,
                },
            )),
            None => resolver,
        };
        Self {
            connector: RedisConnector::with_config(config.clone()),
            catalog: standard_redis_catalog(resolver, true).unwrap(),
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

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn bounded_subscription_stops_on_message_limit() {
    let mut f = Fixture::new();
    let channel = format!("{}events", f.prefix);
    // 发布线程在订阅建立后持续灌消息，验证条数上限会提前收工而不是等时限。
    let publisher = std::thread::spawn({
        let port: u16 = std::env::var("FLUXDB_T025_REDIS_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let channel = channel.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let config = ConnectionConfig {
                id: ConnectionId(26),
                name: "publisher".into(),
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
            let publisher = RedisConnector::with_config(config);
            for i in 0..500 {
                let _ = publisher
                    .execute_argv(0, &["PUBLISH".into(), channel.clone(), format!("m{i}")]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    });
    let started = std::time::Instant::now();
    let receipt = f.tool(
        "redis.subscribe",
        serde_json::json!({
            "target":"conn:25/db:0","channels":[channel],
            "max_messages":5,"timeout_ms":30000,
        }),
        CancelFlag::new(),
    );
    let elapsed = started.elapsed();
    publisher.join().unwrap();
    assert_eq!(
        receipt.status,
        ToolReceiptStatus::Succeeded,
        "{}",
        receipt.result_json
    );
    let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    let info = &payload["rows"][0][2];
    assert_eq!(info["received"], 5);
    assert_eq!(info["stop_reason"], "message_limit");
    assert_eq!(info["truncated"], true);
    assert_eq!(payload["rows"][0][1].as_array().unwrap().len(), 5);
    // 上限是 30s，命中条数上限必须远早于时限收工。
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "应提前收工，实际 {elapsed:?}"
    );
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn bounded_subscription_stops_on_time_limit_without_traffic() {
    let f = Fixture::new();
    let channel = format!("{}idle", f.prefix);
    let started = std::time::Instant::now();
    let receipt = f.tool(
        "redis.subscribe",
        serde_json::json!({
            "target":"conn:25/db:0","channels":[channel],
            "timeout_ms":1000,
        }),
        CancelFlag::new(),
    );
    let elapsed = started.elapsed();
    assert_eq!(
        receipt.status,
        ToolReceiptStatus::Succeeded,
        "{}",
        receipt.result_json
    );
    let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    let info = &payload["rows"][0][2];
    assert_eq!(info["received"], 0);
    assert_eq!(info["stop_reason"], "time_limit");
    assert_eq!(info["truncated"], true);
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "应等到时限，实际 {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "不应远超时限，实际 {elapsed:?}"
    );
    // 订阅连接必须已断开，普通命令仍能正常走连接池。
    assert_eq!(
        f.argv(0, &["PING"]),
        fluxdb_core::CommandReply::Status("PONG".into())
    );
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn bounded_subscription_stops_on_byte_limit() {
    let mut f = Fixture::new();
    let channel = format!("{}bytes", f.prefix);
    let publisher = std::thread::spawn({
        let port: u16 = std::env::var("FLUXDB_T025_REDIS_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let channel = channel.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let publisher = RedisConnector::with_config(ConnectionConfig {
                id: ConnectionId(27),
                name: "publisher".into(),
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
            });
            // 每条 1KiB，字节上限 2KiB 时应在第 3 条之前收工。
            let payload = "p".repeat(1024);
            for _ in 0..200 {
                let _ = publisher
                    .execute_argv(0, &["PUBLISH".into(), channel.clone(), payload.clone()]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    });
    let receipt = f.tool(
        "redis.subscribe",
        serde_json::json!({
            "target":"conn:25/db:0","channels":[channel],
            "max_messages":500,"max_bytes":2048,"timeout_ms":30000,
        }),
        CancelFlag::new(),
    );
    publisher.join().unwrap();
    assert_eq!(
        receipt.status,
        ToolReceiptStatus::Succeeded,
        "{}",
        receipt.result_json
    );
    let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    let info = &payload["rows"][0][2];
    assert_eq!(info["stop_reason"], "byte_limit");
    assert_eq!(info["received"], 2);
    assert_eq!(payload["rows"][0][1].as_array().unwrap().len(), 2);
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn bounded_subscription_reports_cancel_without_claiming_server_rollback() {
    let f = Fixture::new();
    let channel = format!("{}cancel", f.prefix);
    let cancel = CancelFlag::new();
    let canceller = std::thread::spawn({
        let cancel = cancel.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            cancel.request();
        }
    });
    let receipt = f.tool(
        "redis.subscribe",
        serde_json::json!({
            "target":"conn:25/db:0","channels":[channel],"timeout_ms":30000,
        }),
        cancel,
    );
    canceller.join().unwrap();
    assert_eq!(receipt.status, ToolReceiptStatus::Unknown);
    assert_eq!(receipt.error_code.as_deref(), Some("cancel_race_unknown"));
    assert!(receipt.result_json.contains("已投递消息无法撤回"));
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn bounded_subscription_rejects_limits_beyond_host_caps() {
    let f = Fixture::new();
    // schema 的 maximum 是第一道边界：超范围值在 catalog.prepare 阶段就被拒绝，
    // 不会进入执行路径（执行路径另有 clamp 作为第二道防线，见单测）。
    for arguments in [
        serde_json::json!({"channels":["a"],"max_messages":999999}),
        serde_json::json!({"channels":["a"],"max_bytes":99999999}),
        serde_json::json!({"channels":["a"],"timeout_ms":999999}),
    ] {
        let mut arguments = arguments;
        arguments["target"] = serde_json::json!("conn:25/db:0");
        assert!(
            f.catalog
                .prepare("redis.subscribe", &arguments.to_string())
                .is_err(),
            "超范围订阅上限必须被拒绝: {arguments}"
        );
    }
    // 边界内的最大值必须放行并生效。
    let receipt = f.tool(
        "redis.subscribe",
        serde_json::json!({
            "target":"conn:25/db:0","channels":[format!("{}x",f.prefix)],
            "max_messages":500,"max_bytes":262144,"timeout_ms":1000,
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
    let info = &payload["rows"][0][2];
    assert_eq!(info["max_messages"], 500);
    assert_eq!(info["max_bytes"], 262144);
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn key_scope_blocks_out_of_prefix_writes_against_real_redis() {
    let prefix = format!("fluxdb:t025:scope:{}:", uuid::Uuid::new_v4());
    let mut f = Fixture::with_scope(Some(vec![prefix.clone()]));
    let allowed = format!("{prefix}allowed");
    // 授权外的 key：必须不以授权 prefix 开头。
    let denied = format!("outside:{}", uuid::Uuid::new_v4());
    f.keys.push(allowed.clone());
    f.keys.push(denied.clone());

    // 授权内的写入正常执行。
    let receipt = f.tool(
        "redis.execute",
        serde_json::json!({"target":"conn:25/db:0","argv":["SET",&allowed,"v"]}),
        CancelFlag::new(),
    );
    assert_eq!(
        receipt.status,
        ToolReceiptStatus::Succeeded,
        "{}",
        receipt.result_json
    );
    // 回执声明实际触碰的 key 与 DB，便于审计核对。
    let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    assert_eq!(payload["rows"][0][2]["keys"], serde_json::json!([allowed]));

    // 授权外的写入必须被拒绝，且服务端确实没有写入。
    let receipt = f.tool(
        "redis.execute",
        serde_json::json!({"target":"conn:25/db:0","argv":["SET",&denied,"v"]}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("scope_mismatch"));
    assert_eq!(
        f.argv(0, &["EXISTS", &denied]),
        fluxdb_core::CommandReply::Integer(0),
        "越权命令不得触达服务端"
    );
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn key_scope_checks_every_key_of_multi_key_commands() {
    let prefix = format!("fluxdb:t025:multi:{}:", uuid::Uuid::new_v4());
    let mut f = Fixture::with_scope(Some(vec![prefix.clone()]));
    let inside_a = format!("{prefix}a");
    let outside = format!("outside:{prefix}b");
    f.keys.push(inside_a.clone());
    f.argv(0, &["SET", &inside_a, "v"]);

    // MSET 的第二个 key 越权 → 整体拒绝，不能只写授权内的那个。
    let receipt = f.tool(
        "redis.execute",
        serde_json::json!({
            "target":"conn:25/db:0",
            "argv":["MSET",&inside_a,"x",&outside,"y"],
        }),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("scope_mismatch"));

    // RENAME 把授权内的 key 搬到授权外：目标端也要校验。
    let receipt = f.tool(
        "redis.execute",
        serde_json::json!({"target":"conn:25/db:0","argv":["RENAME",&inside_a,&outside]}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("scope_mismatch"));
    // 原 key 必须还在：RENAME 没有发生。
    assert_eq!(
        f.argv(0, &["EXISTS", &inside_a]),
        fluxdb_core::CommandReply::Integer(1)
    );

    // 范围外的既有 key 连读都不允许。
    f.keys.push(outside.clone());
    f.argv(0, &["SET", &outside, "secret"]);
    let receipt = f.tool(
        "redis.inspect",
        serde_json::json!({"target":"conn:25/db:0","key":outside,"view":"value"}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("scope_mismatch"));
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn scan_filters_out_of_scope_keys_instead_of_leaking_them() {
    let prefix = format!("fluxdb:t025:scan:{}:", uuid::Uuid::new_v4());
    let mut f = Fixture::with_scope(Some(vec![prefix.clone()]));
    let inside = format!("{prefix}inside");
    let outside = format!("outside:{}", uuid::Uuid::new_v4());
    f.keys.push(inside.clone());
    f.keys.push(outside.clone());
    f.argv(0, &["SET", &inside, "1"]);
    f.argv(0, &["SET", &outside, "2"]);

    let receipt = f.tool(
        "redis.scan",
        serde_json::json!({
            "target":"conn:25/db:0",
            // 故意用宽 pattern 试探：授权外的 key 不能出现在结果里。
            "pattern":"*t025*","count":1000,"max_keys":1000,
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
    let keys: Vec<String> = payload["rows"][0][1]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_string())
        .collect();
    assert!(keys.contains(&inside), "授权内 key 应可见: {keys:?}");
    assert!(!keys.contains(&outside), "授权外 key 不得泄漏: {keys:?}");
    for key in &keys {
        assert!(key.starts_with(&prefix), "结果包含越界 key: {key}");
    }
}

#[test]
#[ignore = "requires an explicitly selected isolated Redis"]
fn unresolvable_key_commands_are_rejected_against_real_redis() {
    let f = Fixture::new();
    for (command, expected) in [
        // EVAL 已在内核策略层直接拒绝，不给审批绕过。
        (
            serde_json::json!(["EVAL", "return redis.call('set',KEYS[1],'x')", "1", "k"]),
            "redis_command_rejected",
        ),
        // 未识别命令过得了风险分类，但 key 位置不可知 → 在范围校验层拒绝。
        (
            serde_json::json!(["BF.ADD", "k", "v"]),
            "redis_key_scope_unresolvable",
        ),
    ] {
        let receipt = f.tool(
            "redis.execute",
            serde_json::json!({"target":"conn:25/db:0","argv":command}),
            CancelFlag::new(),
        );
        assert_eq!(
            receipt.status,
            ToolReceiptStatus::Failed,
            "解析不出 key 的命令必须拒绝: {command}"
        );
        assert_eq!(receipt.error_code.as_deref(), Some(expected), "{command}");
    }
}
