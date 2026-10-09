//! C/T025 验收：四个 Redis 工具在真实 Redis 上按生产契约执行。
//!
//! `FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_ROOT=<配置库副本> FLUXDB_AI_LIVE_REDIS_CONNECTION=3 \
//!  cargo run -p fluxdb-app --example ai_live_redis_tools`
//! 只读写 `fluxdb:ai:smoke:*`，结束删除；凭据走宿主解密路径，不打印。
use fluxdb_app::*;
use fluxdb_core::{ConnectionId, ToolExecutionContext, ToolReceiptStatus};
use fluxdb_storage::Storage as _;

fn call(
    catalog: &ToolCatalog,
    tool_id: &str,
    arguments: serde_json::Value,
) -> (ToolReceiptStatus, String) {
    let prepared = catalog
        .prepare(tool_id, &arguments.to_string())
        .unwrap_or_else(|error| panic!("{tool_id} 参数未通过校验: {error:?}"));
    let tool = catalog.executable(tool_id).expect("工具实现");
    let receipt = block_on(tool.execute(
        ToolExecutionContext {
            session_id: "e2e".into(),
            turn_id: "e2e-turn".into(),
            iteration: 1,
            call_id: format!("call-{tool_id}"),
            progress: None,
            cancel: fluxdb_core::CancelFlag::new(),
        },
        prepared.to_json_string(),
    ))
    .expect("工具调用");
    (receipt.status, receipt.result_json)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};
    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: std::sync::Arc<Self>) {}
    }
    let waker = Waker::from(std::sync::Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => continue,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要 FLUXDB_AI_LIVE=1（会真实读写测试键）".into());
    }
    let root = std::env::var("FLUXDB_AI_LIVE_ROOT")
        .map_err(|_| "需要 FLUXDB_AI_LIVE_ROOT 指向配置库的隔离副本")?;
    let connection_id: u64 = std::env::var("FLUXDB_AI_LIVE_REDIS_CONNECTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let storage = fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));
    let config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id == ConnectionId(connection_id))
        .ok_or("找不到 Redis 连接")?;
    if config.kind != fluxdb_core::DatabaseKind::Redis {
        return Err("该连接不是 Redis".into());
    }
    let handle = format!("conn:{connection_id}/db:0");
    let catalog = standard_redis_catalog(
        std::sync::Arc::new(MapSqlResolver::new([config.clone()])),
        true,
    )?;
    println!(
        "工具目录: {:?}",
        catalog
            .specs()
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>()
    );

    let (status, body) = call(
        &catalog,
        "redis.execute",
        serde_json::json!({"target":handle,"argv":["SET","fluxdb:ai:smoke:kv","hello"],"intent":"写入测试键"}),
    );
    println!("redis.execute SET -> {status:?} {body}");
    assert_eq!(status, ToolReceiptStatus::Succeeded);

    let (status, body) = call(
        &catalog,
        "redis.inspect",
        serde_json::json!({"target":handle,"key":"fluxdb:ai:smoke:kv","view":"value"}),
    );
    println!("redis.inspect -> {status:?} {body}");
    assert_eq!(status, ToolReceiptStatus::Succeeded);
    assert!(body.contains("hello"), "inspect 应看到字符串值");

    let (status, body) = call(
        &catalog,
        "redis.scan",
        serde_json::json!({"target":handle,"pattern":"fluxdb:ai:smoke:*","count":10}),
    );
    println!("redis.scan -> {status:?} {body}");
    assert_eq!(status, ToolReceiptStatus::Succeeded);
    assert!(body.contains("fluxdb:ai:smoke:kv"), "scan 应命中测试键");

    let (status, body) = call(
        &catalog,
        "redis.server_info",
        serde_json::json!({"target":handle,"sections":["server"]}),
    );
    println!("redis.server_info -> {status:?} 长度={}", body.len());
    assert_eq!(status, ToolReceiptStatus::Succeeded);

    // 受控边界：管理/长连接命令在工具层也必须被拒（不依赖模型自觉）。
    for argv in [
        serde_json::json!(["CONFIG", "GET", "maxmemory"]),
        serde_json::json!(["MONITOR"]),
        serde_json::json!(["SUBSCRIBE", "ch"]),
    ] {
        let (status, body) = call(
            &catalog,
            "redis.execute",
            serde_json::json!({"target":handle,"argv":argv}),
        );
        println!("redis.execute {argv} -> {status:?} {body}");
        assert_eq!(status, ToolReceiptStatus::Failed, "受控命令必须失败");
    }

    let (status, _) = call(
        &catalog,
        "redis.execute",
        serde_json::json!({"target":handle,"argv":["DEL","fluxdb:ai:smoke:kv"]}),
    );
    assert_eq!(status, ToolReceiptStatus::Succeeded);
    println!("✅ 四个 Redis 工具在真实 Redis 上按契约执行（含受控边界），测试键已清理");
    Ok(())
}
