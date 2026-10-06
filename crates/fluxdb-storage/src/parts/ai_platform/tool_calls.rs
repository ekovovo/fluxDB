//! 工具调用事实行与结果登记；从 sessions 机械拆分，不改变行为。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// 记录一次工具调用（状态机由运行时推进；这里只写事实）。
#[allow(clippy::too_many_arguments)]
pub fn start_tool_call(
    conn: &Connection,
    now: i64,
    id: &str,
    session_id: &str,
    turn_id: &str,
    iteration: i64,
    source_ordinal: i64,
    provider_call_id: &str,
    tool_id: &str,
    tool_version: u32,
    plugin_digest: Option<&str>,
    arguments_redacted_json: &str,
    target_json: &str,
    plan_hash: &str,
) -> Result<()> {
    create_tool_call(
        conn,
        now,
        id,
        session_id,
        turn_id,
        iteration,
        source_ordinal,
        provider_call_id,
        tool_id,
        tool_version,
        plugin_digest,
        arguments_redacted_json,
        target_json,
        plan_hash,
        "running",
    )
}

/// 记录一次工具调用并指定初始状态（`waiting_approval` 用于"暂停等审批"）。
#[allow(clippy::too_many_arguments)]
pub fn create_tool_call(
    conn: &Connection,
    now: i64,
    id: &str,
    session_id: &str,
    turn_id: &str,
    iteration: i64,
    source_ordinal: i64,
    provider_call_id: &str,
    tool_id: &str,
    tool_version: u32,
    plugin_digest: Option<&str>,
    arguments_redacted_json: &str,
    target_json: &str,
    plan_hash: &str,
    status: &str,
) -> Result<()> {
    const ALLOWED: &[&str] = &[
        "prepared",
        "waiting_approval",
        "running",
        "succeeded",
        "failed",
        "denied",
        "cancelled",
        "unknown",
    ];
    if !ALLOWED.contains(&status) {
        return Err(Error::new(ErrorKind::Internal, "非法工具调用状态"));
    }
    for raw in [arguments_redacted_json, target_json] {
        serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|_| Error::new(ErrorKind::Internal, "工具调用字段不是合法 JSON"))?;
    }
    conn.execute("INSERT INTO ai_tool_calls(id,session_id,turn_id,iteration,source_ordinal,provider_call_id,tool_id,tool_version,plugin_digest,arguments_redacted_json,target_json,plan_hash,status,started_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![id,session_id,turn_id,iteration,source_ordinal,provider_call_id,tool_id,tool_version,plugin_digest,arguments_redacted_json,target_json,plan_hash,status,now]).map_err(sqlite_error)?;
    Ok(())
}

/// 读取一次工具调用（恢复执行时用它重建参数与目标）。
#[derive(Clone, Debug)]
pub struct ToolCallView {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub provider_call_id: String,
    pub tool_id: String,
    pub tool_version: u32,
    pub arguments_redacted_json: String,
    pub target_json: String,
    pub plan_hash: String,
    pub status: String,
    pub receipt_json: Option<String>,
    pub error_code: Option<String>,
}

pub fn load_tool_call(conn: &Connection, id: &str) -> Result<ToolCallView> {
    conn.query_row("SELECT id,session_id,turn_id,provider_call_id,tool_id,tool_version,arguments_redacted_json,target_json,plan_hash,status,receipt_json FROM ai_tool_calls WHERE id=?1",
        [id], |row| Ok(ToolCallView{
            id:row.get(0)?, session_id:row.get(1)?, turn_id:row.get(2)?, provider_call_id:row.get(3)?,
            tool_id:row.get(4)?, tool_version:row.get(5)?, arguments_redacted_json:row.get(6)?,
            target_json:row.get(7)?, plan_hash:row.get(8)?, status:row.get(9)?,
            receipt_json:row.get(10)?, error_code:None,
        }))
        .optional().map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal,"工具调用不存在"))
}

/// 写回工具真实回执：回执是事实来源，UI/Hook 不得改写成功与否。
pub fn finish_tool_call(
    conn: &Connection,
    id: &str,
    status: &str,
    receipt_json: Option<&str>,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
) -> Result<()> {
    const ALLOWED: &[&str] = &[
        "prepared",
        "waiting_approval",
        "running",
        "succeeded",
        "failed",
        "denied",
        "cancelled",
        "unknown",
    ];
    if !ALLOWED.contains(&status) {
        return Err(Error::new(ErrorKind::Internal, "非法工具调用状态"));
    }
    if let Some(raw) = receipt_json {
        serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|_| Error::new(ErrorKind::Internal, "回执不是合法 JSON"))?;
    }
    conn.execute("UPDATE ai_tool_calls SET status=?2,receipt_json=?3,started_at_ms=COALESCE(?4,started_at_ms),finished_at_ms=?5 WHERE id=?1",
        params![id,status,receipt_json,started_at_ms,finished_at_ms]).map_err(sqlite_error)?;
    Ok(())
}

/// 登记一个有界结果句柄（T027 结果分页/保留；内存结果也要有稳定 ID 才能被引用）。
#[allow(clippy::too_many_arguments)]
pub fn insert_result(
    conn: &Connection,
    now: i64,
    id: &str,
    session_id: &str,
    turn_id: &str,
    source_call_id: &str,
    metadata_json: &str,
    status: &str,
    byte_count: i64,
) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(metadata_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "结果元数据不是合法 JSON"))?;
    if !["available", "expired", "deleted", "failed"].contains(&status) {
        return Err(Error::new(ErrorKind::Internal, "非法结果状态"));
    }
    conn.execute("INSERT INTO ai_results(id,session_id,turn_id,source_call_id,metadata_json,retention_kind,status,byte_count,created_at_ms) VALUES(?1,?2,?3,?4,?5,'memory',?6,?7,?8)",
        params![id,session_id,turn_id,source_call_id,metadata_json,status,byte_count,now]).map_err(sqlite_error)?;
    Ok(())
}

/// 找该 Turn 的助手消息（审批决定等迟到事件要挂到同一条消息上）。
pub fn find_assistant_message_for_turn(conn: &Connection, turn_id: &str) -> Result<Option<String>> {
    conn.query_row("SELECT id FROM ai_messages WHERE turn_id=?1 AND role='assistant' ORDER BY seq DESC LIMIT 1",
        [turn_id], |row| row.get::<_,String>(0)).optional().map_err(sqlite_error)
}

/// 列出某轮的全部工具调用（按 iteration/source_ordinal 排序）。
pub fn list_tool_calls_for_turn(conn: &Connection, turn_id: &str) -> Result<Vec<ToolCallView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_tool_calls WHERE turn_id=?1 ORDER BY iteration, source_ordinal")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([turn_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter()
        .map(|id| load_tool_call(conn, &id))
        .collect()
}
