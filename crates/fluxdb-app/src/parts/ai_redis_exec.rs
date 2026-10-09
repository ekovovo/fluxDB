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
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                let reply = connector
                    .execute_argv(
                        target.database,
                        &redis_argv(&["SCAN", &cursor, "MATCH", pattern, "COUNT", &count.to_string()]),
                    )
                    .map_err(|error| {
                        SqlExecError::new("redis_error", format!("SCAN 失败: {}", error.message))
                    })?;
                let (next_cursor, keys) = match &reply {
                    fluxdb_core::CommandReply::Array(items) if items.len() == 2 => {
                        let next = match &items[0] {
                            fluxdb_core::CommandReply::Bulk(bulk) => {
                                bulk.text.clone().unwrap_or_else(|| "0".into())
                            }
                            _ => "0".into(),
                        };
                        let keys = match &items[1] {
                            fluxdb_core::CommandReply::Array(keys) => keys
                                .iter()
                                .filter_map(|key| match key {
                                    fluxdb_core::CommandReply::Bulk(bulk) => bulk.text.clone(),
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                            _ => Vec::new(),
                        };
                        (next, keys)
                    }
                    _ => ("0".to_string(), Vec::new()),
                };
                Ok(SqlExecutionOutcome {
                    columns: vec!["cursor".into(), "keys".into()],
                    rows: vec![vec![
                        serde_json::Value::String(next_cursor),
                        serde_json::json!(keys),
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
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                let call = |argv: &[&str]| -> Result<fluxdb_core::CommandReply, SqlExecError> {
                    connector.execute_argv(target.database, &redis_argv(argv)).map_err(|error| {
                        SqlExecError::new("redis_error", format!("命令失败: {}", error.message))
                    })
                };
                let kind = redis_reply_to_json(&call(&["TYPE", key])?);
                let ttl = redis_reply_to_json(&call(&["TTL", key])?);
                let mut payload = serde_json::json!({ "key": key, "type": kind, "ttl": ttl });
                if view == "value" {
                    // 只对有界类型取样：字符串直接 GET；其它类型给出类型说明，避免把大集合拉全。
                    let type_name = payload["type"].as_str().unwrap_or("").to_string();
                    if type_name == "string" {
                        payload["value"] = redis_reply_to_json(&call(&["GET", key])?);
                    } else if !type_name.is_empty() && type_name != "none" {
                        payload["value_note"] = serde_json::Value::String(format!(
                            "类型 {type_name} 的取值请用 redis.execute 按需读取（避免一次拉全）"
                        ));
                    }
                }
                Ok(SqlExecutionOutcome {
                    columns: vec!["key".into(), "info".into()],
                    rows: vec![vec![serde_json::Value::String(key.to_string()), payload]],
                    row_count: 1,
                    truncated: false,
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
                let reply = connector.execute_argv(target.database, &redis_argv(&argv)).map_err(
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
                let target = AiRedisTarget::parse(handle)?;
                let (_config, connector) = redis_connector(resolver.as_ref(), &target)?;
                let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                let reply = connector
                    .execute_argv(target.database, &redis_argv(&argv_refs))
                    .map_err(|error| {
                        SqlExecError::new("redis_error", format!("命令失败: {}", error.message))
                    })?;
                Ok(SqlExecutionOutcome {
                    columns: vec!["command".into(), "reply".into()],
                    rows: vec![vec![
                        serde_json::json!(argv),
                        redis_reply_to_json(&reply),
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
    if include_write {
        catalog.register(Box::new(AiRedisExecuteTool::new(resolver)?))?;
    }
    Ok(catalog)
}
