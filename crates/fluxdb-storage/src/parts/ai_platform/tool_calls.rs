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
///
/// `retention` 描述结果是否溢写到受控 spill 文件：`Retained` 必须带相对路径，
/// 由表 CHECK 约束兜住“保留结果却没有落点”这种无法读取的状态。
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
    retention: &ResultRetention,
) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(metadata_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "结果元数据不是合法 JSON"))?;
    if !["available", "expired", "deleted", "failed"].contains(&status) {
        return Err(Error::new(ErrorKind::Internal, "非法结果状态"));
    }
    let retention_kind = match retention {
        ResultRetention::Memory => "memory",
        ResultRetention::Retained { .. } => "retained",
    };
    let relative_path = match retention {
        ResultRetention::Memory => None,
        ResultRetention::Retained { relative_path, .. } => {
            // 受控根之外的路径不得入库：绝对路径或 `..` 上跳会让后续读取变成任意文件访问。
            let path = std::path::Path::new(relative_path);
            if path.is_absolute()
                || relative_path.is_empty()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "保留结果的路径必须是受控根内的相对路径",
                ));
            }
            Some(relative_path.as_str())
        }
    };
    // retained 行描述的是受控文件：字节数必须取文件本身的大小，
    // 否则清理与配额会按回执长度算，和真实磁盘占用对不上。
    let (content_hash, byte_count) = match retention {
        ResultRetention::Memory => (None, byte_count),
        ResultRetention::Retained {
            content_hash,
            byte_count: stored_bytes,
            ..
        } => (Some(content_hash.as_str()), *stored_bytes),
    };
    conn.execute("INSERT INTO ai_results(id,session_id,turn_id,source_call_id,metadata_json,retention_kind,status,relative_path,byte_count,content_hash,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![id,session_id,turn_id,source_call_id,metadata_json,retention_kind,status,relative_path,byte_count,content_hash,now]).map_err(sqlite_error)?;
    Ok(())
}

/// 结果保留方式：内存结果重启后必然过期，溢写结果在受控根下保留到策略清理。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultRetention {
    Memory,
    Retained {
        /// 相对受控根的路径；不得是绝对路径或含 `..` 的越界路径。
        relative_path: String,
        /// spill 文件的实际字节数（不是回执长度）。
        byte_count: i64,
        content_hash: String,
    },
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

/// 按 `(turn_id, provider_call_id)` 找工具调用（语义键；ID 命名约定可能不同来源）。
pub fn find_tool_call_id_by_provider_call(
    conn: &Connection,
    turn_id: &str,
    provider_call_id: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM ai_tool_calls WHERE turn_id=?1 AND provider_call_id=?2 ORDER BY iteration DESC LIMIT 1",
        params![turn_id, provider_call_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(sqlite_error)
}

/// 该 Turn 下一个可用的调用序号（回执补行时避免撞 UNIQUE(turn_id, iteration, source_ordinal)）。
pub fn next_tool_call_ordinal(conn: &Connection, turn_id: &str) -> Result<(i64, i64)> {
    conn.query_row(
        "SELECT COALESCE(MAX(iteration),0)+1, COALESCE(MAX(source_ordinal),-1)+1 FROM ai_tool_calls WHERE turn_id=?1",
        [turn_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )
    .map_err(sqlite_error)
}
