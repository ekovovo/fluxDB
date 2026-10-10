// Redis 工具实现（设计 §22，T025）：scan / inspect / server_info / execute。
//
// 分层：本文件只做"解析目标 + 调用连接器 + 有界投影"；
// 风险分类在 ai_redis_policy，授权/审批/审计在 TurnRunner（redis.execute 为 Write ⇒ 逐次审批）。

/// Redis 目标：连接 id + DB 序号（`conn:<id>[/db:<n>]`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiRedisTarget {
    pub connection_id: fluxdb_core::ConnectionId,
    pub database: u32,
}

impl AiRedisTarget {
    pub fn parse(handle: &str) -> Result<Self, SqlExecError> {
        let parsed = AiSqlTarget::parse(handle)?;
        let database = match parsed.database.as_deref() {
            None => 0,
            Some(raw) => raw.parse::<u32>().map_err(|_| {
                SqlExecError::new("invalid_target", format!("Redis DB 必须是 0-15 的整数: {raw}"))
            })?,
        };
        if database > 15 {
            return Err(SqlExecError::new(
                "invalid_target",
                "Redis DB 超出 0-15 的范围",
            ));
        }
        Ok(Self {
            connection_id: parsed.connection_id,
            database,
        })
    }
}

fn redis_connector(
    resolver: &dyn SqlExecutionResolver,
    target: &AiRedisTarget,
) -> Result<(fluxdb_core::ConnectionConfig, fluxdb_connectors::RedisConnector), SqlExecError> {
    let config = resolver.connection_config(target.connection_id).ok_or_else(|| {
        SqlExecError::new(
            "connection_not_found",
            format!("连接不存在: {}", target.connection_id.0),
        )
    })?;
    if config.kind != fluxdb_core::DatabaseKind::Redis {
        return Err(SqlExecError::new("invalid_target", "该目标不是 Redis 连接"));
    }
    let profile = config.redis_profile.clone().unwrap_or_else(|| fluxdb_core::RedisConnectionProfile::from_options(&config.options));
    if profile.topology.kind == fluxdb_core::RedisTopologyKind::Cluster && target.database != 0 {
        return Err(SqlExecError::new("invalid_target", "Redis Cluster 只支持 DB 0"));
    }
    let connector = fluxdb_connectors::RedisConnector::with_config(config.clone());
    Ok((config, connector))
}

/// RESP 回包 → 有界 JSON（二进制给长度与 hex 预览，不塞原始字节）。
fn redis_reply_to_json(reply: &fluxdb_core::CommandReply) -> serde_json::Value {
    use fluxdb_core::CommandReply;
    match reply {
        CommandReply::Nil => serde_json::Value::Null,
        CommandReply::Status(text) | CommandReply::Text(text) => {
            serde_json::Value::String(text.clone())
        }
        CommandReply::Integer(number) => serde_json::json!(number),
        CommandReply::Float(number) => serde_json::json!(number),
        CommandReply::Error(message) => serde_json::json!({ "error": message }),
        CommandReply::Bulk(bulk) => serde_json::json!({
            "text": bulk.text,
            "byte_len": bulk.byte_len,
            "binary": bulk.binary,
            "bytes_preview_hex": bulk.bytes_preview_hex,
        }),
        CommandReply::Array(items) => {
            serde_json::Value::Array(items.iter().map(redis_reply_to_json).collect())
        }
        CommandReply::Map(entries) => serde_json::Value::Array(
            entries
                .iter()
                .map(|(key, value)| {
                    serde_json::json!([redis_reply_to_json(key), redis_reply_to_json(value)])
                })
                .collect(),
        ),
    }
}

fn redis_argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|value| (*value).to_string()).collect()
}

// ---- redis.scan ----

pub struct AiRedisScanTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiRedisScanTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: redis_descriptor("redis.scan")?,
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiRedisScanTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let outcome = (|| -> Result<SqlExecutionOutcome, SqlExecError> {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value.get("target").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 target")
                })?;
                let pattern = value.get("pattern").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 pattern")
                })?;
                if pattern.len() > 512 {
                    return Err(SqlExecError::new("invalid_arguments", "pattern 过长"));
                }
                let cursor = value
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                let count = value.get("count").and_then(|v| v.as_u64()).unwrap_or(100).clamp(1, 1000);
                let max_keys = value
                    .get("max_keys")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100)
                    .clamp(1, 1000) as usize;
                let target = AiRedisTarget::parse(handle)?;
                cursor.parse::<u64>().map_err(|_| SqlExecError::new("invalid_arguments", "SCAN 游标必须是非负整数"))?;
                let (config, connector) = redis_connector(resolver.as_ref(), &target)?;
                let scope = resolver.redis_key_scope(target.connection_id, target.database);
                // SCAN COUNT 不保证精确返回条数；宿主按 max_keys 追加上限，
                // 游标回到 "0" 表示单节点全库扫描完成，否则 truncated 并返回 next_cursor。
                let mut current_cursor = cursor;
                let mut keys: Vec<String> = Vec::new();
                let mut complete = false;
                let mut truncated = false;
                let mut rounds = 0usize;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                const MAX_SCAN_ROUNDS: usize = 200;
                loop {
                    if ctx.cancel.is_cancelled() {
                        return Err(SqlExecError::new("cancelled", "已停止客户端扫描；不宣称撤销已发送命令"));
                    }
                    if rounds >= MAX_SCAN_ROUNDS || std::time::Instant::now() >= deadline {
                        truncated = true;
                        break;
                    }
                    rounds += 1;
                    let reply = connector
                        .execute_argv_with_budget(
                            target.database,
                            &redis_argv(&[
                                "SCAN",
                                &current_cursor,
                                "MATCH",
                                pattern,
                                "COUNT",
                                &count.to_string(),
                            ]),
                            fluxdb_connectors::RedisReadBudget::default(),
                        )
                        .map_err(|error| {
                            SqlExecError::new("redis_error", format!("SCAN 失败: {}", error.message))
                        })?;
                    let (next_cursor, batch) = redis_scan_page(reply)?;
                    for key in batch {
                        let fluxdb_core::CommandReply::Bulk(key) = key else {
                            return Err(SqlExecError::new("redis_invalid_reply", "SCAN Key 回包结构异常"));
                        };
                        let key = key.text.ok_or_else(|| SqlExecError::new("redis_binary_key_unsupported", "扫描遇到二进制 Key，不能静默跳过；需二进制引用支持"))?;
                        // pattern 是检索条件，不是授权：范围外的 Key 不能出现在结果里。
                        if !keys.contains(&key) && scope.allows(&key) {
                            if keys.len() == max_keys {
                                return Err(SqlExecError::new("redis_page_too_large", "COUNT 非硬上限，本批超过 max_keys；本批未返回续页游标，请降低 count 或增加 max_keys"));
                            }
                            keys.push(key);
                        }
                    }
                    if next_cursor == "0" {
                        complete = true;
                        current_cursor = next_cursor;
                        break;
                    }
                    current_cursor = next_cursor;
                    if keys.len() >= max_keys {
                        truncated = true;
                        break;
                    }
                }
                let profile = config.redis_profile.clone().unwrap_or_else(|| fluxdb_core::RedisConnectionProfile::from_options(&config.options));
                let cluster = profile.topology.kind == fluxdb_core::RedisTopologyKind::Cluster;
                let scope = format!("{}/conn:{}/db:{}", if cluster { "cluster-seed-node" } else { "single-node" }, target.connection_id.0, target.database);
                Ok(SqlExecutionOutcome {
                    columns: vec!["cursor".into(), "keys".into(), "scan_info".into()],
                    rows: vec![vec![
                        serde_json::Value::String(current_cursor.clone()),
                        serde_json::json!(keys),
                        serde_json::json!({
                            "complete": complete,
                            "truncated": truncated,
                            "scope": scope,
                            "count_hint": count,
                            "cluster_complete": false,
                            "consistent_snapshot": false,
                            "max_keys": max_keys,
                        }),
                    ]],
                    row_count: 1,
                    truncated,
                    elapsed_ms: 0,
                    query_session_id: resolver.query_session_id(),
                    source: fluxdb_core::CommandExecutionSource::Ai,
                    original_sql: String::new(),
                    executed_sql: String::new(),
                    progress: Vec::new(),
                    affected_rows: None,
                    statement_kind: "redis_scan",
                })
            })();
            Ok(receipt_from(&descriptor, &ctx.call_id, started_at_ms, outcome))
        })
    }
}

// ---- redis.inspect ----

pub struct AiRedisInspectTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiRedisInspectTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: redis_descriptor("redis.inspect")?,
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiRedisInspectTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let outcome = (|| -> Result<SqlExecutionOutcome, SqlExecError> {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value.get("target").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 target")
                })?;
                let key = value.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 key")
                })?;
                let view = value.get("view").and_then(|v| v.as_str()).unwrap_or("metadata");
                let cursor = value
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();
                let limit = value
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 200) as usize;
                let max_bytes = value.get("max_bytes").and_then(|value| value.as_u64()).unwrap_or(4096).clamp(1, 64 * 1024) as usize;
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                // 查看单个 Key 同样受授权范围约束：拿到 Key 名不等于有权读它。
                let scope = resolver.redis_key_scope(target.connection_id, target.database);
                check_redis_command_scope(
                    &redis_argv(&["TYPE", key]),
                    target.database,
                    &scope,
                )?;
                let call = |argv: &[&str]| -> Result<fluxdb_core::CommandReply, SqlExecError> {
                    if ctx.cancel.is_cancelled() {
                        return Err(SqlExecError::new("cancelled", "已停止后续读取；不宣称服务端撤销"));
                    }
                    connector.execute_argv_with_budget(target.database, &redis_argv(argv), fluxdb_connectors::RedisReadBudget::default()).map_err(|error| {
                        SqlExecError::new("redis_error", format!("命令失败: {}", error.message))
                    })
                };
                let kind = redis_reply_to_json(&call(&["TYPE", key])?);
                let ttl = redis_reply_to_json(&call(&["TTL", key])?);
                let mut payload = serde_json::json!({ "key": key, "type": kind, "ttl": ttl });
                if view == "value" {
                    let type_name = payload["type"].as_str().unwrap_or("").to_string();
                    payload["value"] = redis_inspect_value(&call, key, &type_name, &cursor, limit, max_bytes)?;
                    payload["consistent_snapshot"] = serde_json::json!(false);
                }
                let truncated = payload["value"]["truncated"].as_bool().unwrap_or(false);
                Ok(SqlExecutionOutcome {
                    columns: vec!["key".into(), "info".into()],
                    rows: vec![vec![serde_json::Value::String(key.to_string()), payload]],
                    row_count: 1,
                    truncated,
                    elapsed_ms: 0,
                    query_session_id: resolver.query_session_id(),
                    source: fluxdb_core::CommandExecutionSource::Ai,
                    original_sql: String::new(),
                    executed_sql: String::new(),
                    progress: Vec::new(),
                    affected_rows: None,
                    statement_kind: "redis_inspect",
                })
            })();
            Ok(receipt_from(&descriptor, &ctx.call_id, started_at_ms, outcome))
        })
    }
}

// ---- redis.server_info ----

pub struct AiRedisServerInfoTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiRedisServerInfoTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: redis_descriptor("redis.server_info")?,
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiRedisServerInfoTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let outcome = (|| -> Result<SqlExecutionOutcome, SqlExecError> {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value.get("target").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 target")
                })?;
                let sections: Vec<String> = value
                    .get("sections")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if sections.len() > 8 {
                    return Err(SqlExecError::new("invalid_arguments", "sections 最多 8 个"));
                }
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                // 默认只取只读且诊断价值最高的三段，避免暴露网络拓扑/其它 DB 信息。
                let mut request = vec!["INFO".to_string()];
                let requested = if sections.is_empty() {
                    vec!["server".to_string(), "memory".to_string(), "clients".to_string()]
                } else {
                    sections
                };
                request.extend(requested.iter().cloned());
                let argv: Vec<&str> = request.iter().map(String::as_str).collect();
                let reply = connector.execute_argv_with_budget(target.database, &redis_argv(&argv), fluxdb_connectors::RedisReadBudget::default()).map_err(
                    |error| SqlExecError::new("redis_error", format!("INFO 失败: {}", error.message)),
                )?;
                let text = match &reply {
                    fluxdb_core::CommandReply::Bulk(bulk) => bulk.text.clone().unwrap_or_default(),
                    fluxdb_core::CommandReply::Status(text) | fluxdb_core::CommandReply::Text(text) => {
                        text.clone()
                    }
                    other => serde_json::to_string(&redis_reply_to_json(other)).unwrap_or_default(),
                };
                const INFO_MAX_BYTES: usize = 16 * 1024;
                let truncated = text.len() > INFO_MAX_BYTES;
                let body = if truncated {
                    let mut end = INFO_MAX_BYTES;
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text[..end].to_string()
                } else {
                    text
                };
                Ok(SqlExecutionOutcome {
                    columns: vec!["sections".into(), "info".into()],
                    rows: vec![vec![serde_json::json!(requested), serde_json::Value::String(body)]],
                    row_count: 1,
                    truncated,
                    elapsed_ms: 0,
                    query_session_id: resolver.query_session_id(),
                    source: fluxdb_core::CommandExecutionSource::Ai,
                    original_sql: String::new(),
                    executed_sql: String::new(),
                    progress: Vec::new(),
                    affected_rows: None,
                    statement_kind: "redis_server_info",
                })
            })();
            Ok(receipt_from(&descriptor, &ctx.call_id, started_at_ms, outcome))
        })
    }
}

// ---- redis.execute ----

// ---- redis.subscribe ----

/// 有界订阅的宿主任一上限。
///
/// 工具 schema 的 maximum 只是给模型的提示；真正的边界在这里再收窄一次，避免模型
/// 给出超范围参数后放开内存/时长。缺省值与 `RedisSubscriptionLimits::default()` 对齐。
const REDIS_SUBSCRIBE_MAX_MESSAGES: usize = 500;
const REDIS_SUBSCRIBE_MAX_BYTES: usize = 256 * 1024;
const REDIS_SUBSCRIBE_MAX_DURATION_MS: u64 = 60_000;

pub struct AiRedisSubscribeTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiRedisSubscribeTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: redis_descriptor("redis.subscribe")?,
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiRedisSubscribeTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let outcome = (|| -> Result<SqlExecutionOutcome, SqlExecError> {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value.get("target").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 target")
                })?;
                let pattern = value.get("pattern").and_then(|v| v.as_bool()).unwrap_or(false);
                let channels: Vec<String> = value
                    .get("channels")
                    .and_then(|v| v.as_array())
                    .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                if channels.is_empty() || channels.len() > 16 {
                    return Err(SqlExecError::new("invalid_arguments", "channels 必须是 1-16 个通道名"));
                }
                if channels.iter().any(|channel| channel.is_empty() || channel.len() > 256) {
                    return Err(SqlExecError::new("invalid_arguments", "通道名过长或为空"));
                }
                // schema 的 maximum 只是提示，这里按宿主硬上限再收窄一次。
                let max_messages = value.get("max_messages").and_then(|v| v.as_u64()).unwrap_or(100)
                    .clamp(1, REDIS_SUBSCRIBE_MAX_MESSAGES as u64) as usize;
                let max_bytes = value.get("max_bytes").and_then(|v| v.as_u64()).unwrap_or(64 * 1024)
                    .clamp(1, REDIS_SUBSCRIBE_MAX_BYTES as u64) as usize;
                let timeout_ms = value.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(10_000)
                    .clamp(100, REDIS_SUBSCRIBE_MAX_DURATION_MS);
                if ctx.cancel.is_cancelled() {
                    return Err(SqlExecError::new("cancelled", "已取消，未发送 SUBSCRIBE"));
                }
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                let requests: Vec<fluxdb_connectors::RedisSubscriptionRequest> = channels
                    .iter()
                    .map(|channel| {
                        if pattern {
                            fluxdb_connectors::RedisSubscriptionRequest::pattern(channel.clone())
                        } else {
                            fluxdb_connectors::RedisSubscriptionRequest::new(channel.clone())
                        }
                    })
                    .collect();
                let cancel = ctx.cancel.clone();
                let collected = connector
                    .collect_subscription(
                        target.database,
                        &requests,
                        fluxdb_connectors::RedisSubscriptionLimits {
                            max_duration: std::time::Duration::from_millis(timeout_ms),
                            max_messages,
                            max_payload_bytes: max_bytes,
                            ..Default::default()
                        },
                        &move || cancel.is_cancelled(),
                    )
                    .map_err(|error| SqlExecError::new("redis_error", format!("订阅失败: {}", error.message)))?;
                // 客户端停止与已投递消息的竞态：只声明本地收工，不宣称服务端撤销。
                if collected.stop == Some(fluxdb_connectors::RedisSubscriptionStop::Canceled) {
                    return Err(SqlExecError::new("cancel_race_unknown", "客户端已停止接收；已投递消息无法撤回，请核对频道实际状态"));
                }
                let stop_reason = collected.stop.map(|stop| stop.as_str()).unwrap_or("unsubscribed");
                let messages: Vec<serde_json::Value> = collected.messages.iter().map(redis_pubsub_message_to_json).collect();
                let truncated = collected.stop.is_some();
                Ok(SqlExecutionOutcome {
                    columns: vec!["channels".into(), "messages".into(), "subscribe_info".into()],
                    rows: vec![vec![
                        serde_json::json!(collected.channels),
                        serde_json::json!(messages),
                        serde_json::json!({
                            "received": collected.messages.len(),
                            "truncated": truncated,
                            "stop_reason": stop_reason,
                            "timeout_ms": timeout_ms,
                            "max_messages": max_messages,
                            "max_bytes": max_bytes,
                            "elapsed_ms": collected.elapsed.as_millis() as u64,
                            "errors": collected.errors,
                        }),
                    ]],
                    row_count: 1,
                    truncated,
                    elapsed_ms: collected.elapsed.as_millis() as u64,
                    query_session_id: resolver.query_session_id(),
                    source: fluxdb_core::CommandExecutionSource::Ai,
                    original_sql: String::new(),
                    executed_sql: String::new(),
                    progress: Vec::new(),
                    affected_rows: None,
                    statement_kind: "redis_subscribe",
                })
            })();
            Ok(receipt_from(&descriptor, &ctx.call_id, started_at_ms, outcome))
        })
    }
}

pub struct AiRedisExecuteTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiRedisExecuteTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: redis_descriptor("redis.execute")?,
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiRedisExecuteTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn approval_details(&self, arguments_json: &str) -> fluxdb_core::ApprovalDetails {
        let argv = serde_json::from_str::<serde_json::Value>(arguments_json)
            .ok()
            .and_then(|value| value["argv"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        crate::redis_approval_details(&argv)
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let outcome = (|| -> Result<SqlExecutionOutcome, SqlExecError> {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value.get("target").and_then(|v| v.as_str()).ok_or_else(|| {
                    SqlExecError::new("invalid_arguments", "缺少 target")
                })?;
                let argv: Vec<String> = value
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() || argv.len() > 64 {
                    return Err(SqlExecError::new("invalid_arguments", "argv 必须是 1-64 个字符串"));
                }
                if argv.iter().any(|item| item.len() > 4096) {
                    return Err(SqlExecError::new("invalid_arguments", "argv 单项过长"));
                }
                // 执行前必须再过一次策略：管理/长连接命令在这里也不能被放行。
                match classify_redis_command(&argv) {
                    RedisRisk::Rejected { reasons } => {
                        return Err(SqlExecError::new(
                            "redis_command_rejected",
                            format!("命令被策略拒绝（{}）", reasons.join(",")),
                        ))
                    }
                    _ => {}
                }
                if ctx.cancel.is_cancelled() {
                    return Err(SqlExecError::new("cancelled", "已取消，未发送 Redis 命令"));
                }
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                // 范围校验必须在发送前：key 位置按命令 spec 解析，越权/无法解析都不发出。
                let scope = resolver.redis_key_scope(target.connection_id, target.database);
                let touched_keys = check_redis_command_scope(&argv, target.database, &scope)?;
                let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                let reply = connector
                    .execute_argv_with_budget(target.database, &redis_argv(&argv_refs), fluxdb_connectors::RedisReadBudget::default())
                    .map_err(|error| {
                        SqlExecError::new("redis_error", format!("命令失败: {}", error.message))
                    })?;
                if ctx.cancel.is_cancelled() {
                    return Err(SqlExecError::new("cancel_race_unknown", "客户端停止与已发送命令竞态；请核对实际 Key 状态，不能宣称撤销写入"));
                }
                Ok(SqlExecutionOutcome {
                    columns: vec!["command".into(), "reply".into(), "scope_info".into()],
                    rows: vec![vec![
                        serde_json::json!(argv),
                        redis_reply_to_json(&reply),
                        serde_json::json!({
                            "database": target.database,
                            "keys": touched_keys,
                            "key_count": touched_keys.len(),
                        }),
                    ]],
                    row_count: 1,
                    truncated: false,
                    elapsed_ms: 0,
                    query_session_id: resolver.query_session_id(),
                    source: fluxdb_core::CommandExecutionSource::Ai,
                    original_sql: String::new(),
                    executed_sql: String::new(),
                    progress: Vec::new(),
                    affected_rows: None,
                    statement_kind: "redis_execute",
                })
            })();
            Ok(receipt_from(&descriptor, &ctx.call_id, started_at_ms, outcome))
        })
    }
}

fn redis_descriptor(tool_id: &str) -> fluxdb_core::Result<fluxdb_core::ToolDescriptor> {
    standard_redis_tool_specs()?
        .into_iter()
        .find(|spec| spec.id == tool_id)
        .map(|spec| fluxdb_core::ToolDescriptor {
            id: spec.id,
            version: spec.version,
            description: spec.description,
            risk: spec.risk,
            input_schema_json: spec.input_schema.to_string(),
        })
        .ok_or_else(|| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("缺少 Redis 工具定义: {tool_id}"),
            )
        })
}

/// 用标准 Redis 工具集装配 `ToolCatalog`。
///
/// `include_write=false`（默认）：只装 scan/inspect/server_info，`redis.execute` 的 spec 也一并去掉，
/// 避免"工具可见但没接审批"。`include_write=true` 时 execute 为 Write 风险，由 `authorize` 强制逐次审批。
pub fn standard_redis_catalog<R: SqlExecutionResolver + 'static>(
    resolver: Arc<R>,
    include_write: bool,
) -> fluxdb_core::Result<ToolCatalog> {
    let mut specs = standard_redis_tool_specs()?;
    if !include_write {
        specs.retain(|spec| spec.risk != fluxdb_core::ToolRisk::Write);
    }
    let mut catalog = ToolCatalog::new(specs);
    catalog.register(Box::new(AiRedisScanTool::new(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiRedisInspectTool::new(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiRedisServerInfoTool::new(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiRedisSubscribeTool::new(Arc::clone(&resolver))?))?;
    // 不注册 result.read_page：Redis 工具结果已由各自游标（SCAN cursor / 集合游标）
    // 直接分页，不产生 ResultRef；注册没有生产者的工具会让模型看到不可用入口。
    if include_write {
        catalog.register(Box::new(AiRedisExecuteTool::new(resolver)?))?;
    }
    Ok(catalog)
}
