/// 严格解析 SCAN 家族回包，畸形响应不能被当成扫描完成。
fn redis_scan_page(reply: fluxdb_core::CommandReply) -> Result<(String, Vec<fluxdb_core::CommandReply>), SqlExecError> {
    let fluxdb_core::CommandReply::Array(mut items) = reply else {
        return Err(SqlExecError::new("redis_invalid_reply", "SCAN 回包不是数组"));
    };
    if items.len() != 2 {
        return Err(SqlExecError::new("redis_invalid_reply", "SCAN 回包结构异常"));
    }
    let values = items.pop().unwrap();
    let cursor = items.pop().unwrap();
    let fluxdb_core::CommandReply::Bulk(cursor) = cursor else {
        return Err(SqlExecError::new("redis_invalid_reply", "SCAN 游标格式异常"));
    };
    let cursor = cursor.text.filter(|text| text.parse::<u64>().is_ok())
        .ok_or_else(|| SqlExecError::new("redis_invalid_reply", "SCAN 游标不是无符号整数"))?;
    let fluxdb_core::CommandReply::Array(values) = values else {
        return Err(SqlExecError::new("redis_invalid_reply", "SCAN 元素格式异常"));
    };
    Ok((cursor, values))
}

fn redis_nonnegative_integer(reply: fluxdb_core::CommandReply) -> Result<u64, SqlExecError> {
    match reply {
        fluxdb_core::CommandReply::Integer(number) if number >= 0 => Ok(number as u64),
        _ => Err(SqlExecError::new("redis_invalid_reply", "Redis 长度回包异常")),
    }
}

/// 分类型读取；错误向上传播，COUNT 只作提示。超限批次明确拒绝，不丢元素后推进游标。
fn redis_inspect_value(
    call: &dyn Fn(&[&str]) -> Result<fluxdb_core::CommandReply, SqlExecError>,
    key: &str,
    type_name: &str,
    cursor: &str,
    limit: usize,
    max_bytes: usize,
) -> Result<serde_json::Value, SqlExecError> {
    let value = match type_name {
        "string" | "list" => {
            let offset = cursor.parse::<u64>().ok().filter(|offset| *offset <= i64::MAX as u64)
                .ok_or_else(|| SqlExecError::new("invalid_arguments", "范围游标必须是非负整数"))?;
            let (length_command, read_command, size) = if type_name == "string" {
                ("STRLEN", "GETRANGE", max_bytes)
            } else {
                ("LLEN", "LRANGE", limit)
            };
            let total = redis_nonnegative_integer(call(&[length_command, key])?)?;
            // 空值或页末不能将 LRANGE 的 end 变成 -1（那会读取全集）。
            let end = offset.saturating_add(size as u64).min(total);
            let result = if offset >= total {
                serde_json::json!([])
            } else {
                redis_reply_to_json(&call(&[read_command, key, &offset.to_string(), &(end - 1).to_string()])?)
            };
            serde_json::json!({
                "kind": type_name, "value": result, "offset": offset, "total": total,
                "next_cursor": end.to_string(), "complete": end >= total, "truncated": end < total,
            })
        }
        "hash" | "set" | "zset" => {
            cursor.parse::<u64>().map_err(|_| SqlExecError::new("invalid_arguments", "扫描游标必须是非负整数"))?;
            let command = match type_name { "hash" => "HSCAN", "set" => "SSCAN", _ => "ZSCAN" };
            let (next, items) = redis_scan_page(call(&[command, key, cursor, "COUNT", &limit.to_string()])?)?;
            let width = if type_name == "set" { 1 } else { 2 };
            if items.len() % width != 0 {
                return Err(SqlExecError::new("redis_invalid_reply", "Redis 成员/值配对结构异常"));
            }
            if items.len() / width > limit {
                return Err(SqlExecError::new("redis_page_too_large", "COUNT 非硬上限，本批超过 limit；请提高 limit 至预算允许值或使用指定字段读取。本批未推进游标。"));
            }
            serde_json::json!({
                "kind": type_name, "items": redis_reply_to_json(&fluxdb_core::CommandReply::Array(items)),
                "next_cursor": next, "complete": next == "0", "truncated": next != "0",
                "consistent_snapshot": false,
            })
        }
        "stream" => {
            let start = if cursor == "0" { "-".to_string() } else {
                let (ms, seq) = cursor.split_once('-').ok_or_else(|| SqlExecError::new("invalid_arguments", "Stream 游标必须是 entry ID"))?;
                if ms.parse::<u64>().is_err() || seq.parse::<u64>().is_err() {
                    return Err(SqlExecError::new("invalid_arguments", "Stream entry ID 格式异常"));
                }
                format!("({cursor}")
            };
            let reply = call(&["XRANGE", key, &start, "+", "COUNT", &(limit + 1).to_string()])?;
            let fluxdb_core::CommandReply::Array(mut entries) = reply else {
                return Err(SqlExecError::new("redis_invalid_reply", "XRANGE 回包结构异常"));
            };
            let truncated = entries.len() > limit;
            entries.truncate(limit);
            let next = match entries.last() {
                Some(fluxdb_core::CommandReply::Array(entry)) => match entry.first() {
                    Some(fluxdb_core::CommandReply::Bulk(id)) => id.text.clone()
                        .ok_or_else(|| SqlExecError::new("redis_invalid_reply", "Stream ID 不是文本"))?,
                    _ => return Err(SqlExecError::new("redis_invalid_reply", "Stream ID 格式异常")),
                },
                None => cursor.to_string(),
                _ => return Err(SqlExecError::new("redis_invalid_reply", "Stream entry 结构异常")),
            };
            serde_json::json!({
                "kind": "stream", "entries": redis_reply_to_json(&fluxdb_core::CommandReply::Array(entries)),
                "next_cursor": next, "complete": !truncated, "truncated": truncated,
            })
        }
        "none" => serde_json::json!({"kind": "none", "exists": false, "complete": true, "truncated": false}),
        other => serde_json::json!({"kind": other, "unsupported_detail": "模块或未知类型仅返回元数据，不执行无界 JSON.GET", "complete": false}),
    };
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > max_bytes + 1024) {
        return Err(SqlExecError::new("redis_value_too_large", "值预览超过字节预算；请缩小范围"));
    }
    Ok(value)
}
/// 把一条 Pub/Sub 消息投影为有界 JSON（二进制载荷给 hex 预览而非原始字节）。
fn redis_pubsub_message_to_json(message: &fluxdb_connectors::PubSubMessage) -> serde_json::Value {
    let payload_len = message.payload.len();
    match String::from_utf8(message.payload.clone()) {
        Ok(text) => serde_json::json!({
            "pattern": message.pattern,
            "channel": message.channel,
            "payload": text,
            "payload_bytes": payload_len,
            "binary": false,
        }),
        Err(_) => {
            let preview = message.payload.iter().take(64).map(|byte| format!("{byte:02x}")).collect::<String>();
            serde_json::json!({
                "pattern": message.pattern,
                "channel": message.channel,
                "payload": serde_json::Value::Null,
                "payload_bytes": payload_len,
                "binary": true,
                "bytes_preview_hex": preview,
            })
        }
    }
}
