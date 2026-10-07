//! C/T025 基础验证：RedisConnector 的**单条 argv 执行**原语（真实 Redis）。
//!
//! `FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_ROOT=<配置库副本> FLUXDB_AI_LIVE_REDIS_CONNECTION=3 \
//!  cargo run -p fluxdb-app --example ai_live_redis_argv`
//! 只读写形如 `fluxdb:ai:smoke:*` 的测试键，结束即删除；凭据走宿主解密路径，不打印。
use fluxdb_app::{RedisRisk, classify_redis_command};
use fluxdb_core::{CommandReply, ConnectionId};
use fluxdb_storage::Storage as _;

fn reply_summary(reply: &CommandReply) -> String {
    match reply {
        CommandReply::Nil => "nil".into(),
        CommandReply::Status(text) => format!("status:{text}"),
        CommandReply::Integer(number) => format!("int:{number}"),
        CommandReply::Float(number) => format!("float:{number}"),
        CommandReply::Bulk(bulk) => format!("bulk:len={} text={:?}", bulk.byte_len, bulk.text),
        CommandReply::Text(text) => format!("text:{text}"),
        CommandReply::Error(message) => format!("error:{message}"),
        CommandReply::Map(entries) => format!("map[{}]", entries.len()),
        CommandReply::Array(items) => format!(
            "array[{}]:{:?}",
            items.len(),
            items.iter().map(reply_summary).take(4).collect::<Vec<_>>()
        ),
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
    let connector = fluxdb_connectors::RedisConnector::with_config(config.clone());
    let argv = |parts: &[&str]| {
        parts
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    };

    let ping = connector.execute_argv(0, &argv(&["PING"]))?;
    println!("PING -> {}", reply_summary(&ping));

    let key = "fluxdb:ai:smoke:kv";
    connector.execute_argv(0, &argv(&["SET", key, "hello"]))?;
    let got = connector.execute_argv(0, &argv(&["GET", key]))?;
    println!("GET -> {}（无回显密钥）", reply_summary(&got));
    let scan = connector.execute_argv(
        0,
        &argv(&["SCAN", "0", "MATCH", "fluxdb:ai:smoke:*", "COUNT", "10"]),
    )?;
    println!("SCAN -> {}", reply_summary(&scan));
    let ttl = connector.execute_argv(0, &argv(&["TTL", key]))?;
    println!("TTL -> {}", reply_summary(&ttl));

    // 连接器层的受控边界：长连接/阻塞命令必须被拒绝（不依赖调用方自觉）。
    for blocked in [vec!["SUBSCRIBE", "ch"], vec!["MONITOR"]] {
        match connector.execute_argv(0, &argv(&blocked)) {
            Ok(_) => return Err(format!("受控执行不应允许 {blocked:?}").into()),
            Err(error) => println!("{blocked:?} 被连接器拒绝: {:?}", error.kind),
        }
    }
    // 管理命令（CONFIG/ACL/FLUSHALL…）由**策略层**拒绝：连接器不替调用方做业务分级，
    // 所以 AI/工具路径必须先过 classify_redis_command（见 ai_redis_policy.rs）。
    for admin in [
        vec!["CONFIG", "GET", "maxmemory"],
        vec!["FLUSHALL"],
        vec!["BLPOP", "l", "1"],
    ] {
        let risk = classify_redis_command(&argv(&admin));
        println!("{admin:?} 策略分类: {risk:?}");
        if !matches!(risk, RedisRisk::Rejected { .. }) {
            return Err(format!("策略层必须拒绝 {admin:?}").into());
        }
    }

    let deleted = connector.execute_argv(0, &argv(&["DEL", key]))?;
    println!("DEL -> {}", reply_summary(&deleted));
    println!("✅ RedisConnector::execute_argv 在真实 Redis 上可用（含受控边界）");
    Ok(())
}
