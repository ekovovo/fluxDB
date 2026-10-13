// 历史工具事实只提供定位和状态，不回放数据值，也不把旧轮句柄当作当前授权。
fn ai_historical_tool_facts(
    conn: &rusqlite::Connection,
    job: &AiConversationJob,
) -> fluxdb_core::Result<HashMap<String, Vec<serde_json::Value>>> {
    let mut statement = conn.prepare(
        "SELECT p.message_id,c.tool_id,c.status,c.provider_call_id,c.turn_id,
                coalesce(json_extract(p.payload_json,'$.error_code'),json_extract(c.receipt_json,'$.code')),
                json_extract(r.metadata_json,'$.result_ref'),
                json_extract(c.receipt_json,'$.row_count')
         FROM ai_message_parts p
         JOIN ai_tool_calls c ON c.id=p.tool_call_id
         LEFT JOIN ai_results r ON r.id=p.result_id
         WHERE p.kind='tool_result_ref' AND c.session_id=?1 AND c.turn_id<>?2
         ORDER BY c.finished_at_ms DESC,c.id DESC LIMIT 12"
    ).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
    let rows = statement
        .query_map(rusqlite::params![job.session_id, job.turn_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
    let mut grouped: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut bytes = 0;
    let mut tokens = 0;
    for row in rows {
        let (message, tool, status, call, turn, code, reference, count) =
            row.map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        let fact = serde_json::json!({
            "tool": tool, "status": status, "call_id": call, "source_turn": turn,
            "error_code": code, "acquired_items": count,
            "result_ref": reference,
            "readable": false,
            "unavailable_reason": "turn_scope",
        });
        let encoded = fact.to_string();
        bytes += encoded.len();
        tokens += estimate_tokens(&encoded);
        if bytes > TOOL_HISTORY_FACT_BYTES || tokens > TOOL_HISTORY_FACT_TOKENS as i64 {
            break;
        }
        grouped.entry(message).or_default().insert(0, fact);
    }
    Ok(grouped)
}
