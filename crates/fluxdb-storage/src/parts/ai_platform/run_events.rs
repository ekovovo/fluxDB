//! 会话运行事件（`ai_run_events`，设计 §42.11）。
//!
//! 事件是"这一轮真实发生了什么"的公开摘要：只存角色、阶段、ID 与脱敏原因，
//! 不存 prompt、工具参数、结果正文、endpoint 或任何凭据材料。写入按 Turn 单调递增
//! 的 `seq`，读取按 `seq` 升序，便于时间线与验收断言直接对照。

use rusqlite::{Connection, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 单条事件摘要上限：事件用于说明与断言，不是正文转储。
pub const MAX_SUMMARY_BYTES: usize = 8 * 1024;

/// 模型路由决策事件（主模型/压缩/产物/决策角色各写一条）。
pub const KIND_MODEL_ROUTE: &str = "model_route";

/// 内置 Jev 建议事件（发出/采用/放弃的脱敏诊断，不含 prompt、候选正文或凭据）。
pub const KIND_DECISION_ADVICE: &str = "decision_advice";

/// 运行事件公开视图（不含任何凭据或正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEventView {
    pub turn_id: String,
    pub seq: i64,
    pub kind: String,
    pub summary_json: String,
    pub created_at_ms: i64,
}

/// 追加一条事件，返回分配到的 `seq`。
///
/// 摘要必须是 JSON 对象且有大小上限：调用方不得把整段正文当摘要传进来。
/// `seq` 由本 Turn 现有最大值 +1 得出（单条语句内完成），重复写入同一 Turn 时
/// 主键冲突会直接报错，而不是覆盖既有事件。
pub fn append(
    conn: &Connection,
    turn_id: &str,
    kind: &str,
    summary_json: &str,
    now_ms: i64,
) -> Result<i64> {
    if turn_id.trim().is_empty() || kind.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "事件缺少 turn_id 或 kind"));
    }
    if summary_json.len() > MAX_SUMMARY_BYTES {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("事件摘要超过 {} 字节上限", MAX_SUMMARY_BYTES),
        ));
    }
    let summary: serde_json::Value = serde_json::from_str(summary_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "事件摘要不是合法 JSON"))?;
    if !summary.is_object() {
        return Err(Error::new(ErrorKind::Internal, "事件摘要必须是 JSON 对象"));
    }
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM ai_run_events WHERE turn_id=?1",
            params![turn_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    conn.execute(
        "INSERT INTO ai_run_events (turn_id, seq, kind, summary_json, created_at_ms)
         VALUES (?1,?2,?3,?4,?5)",
        params![turn_id, seq, kind, summary_json, now_ms],
    )
    .map_err(sqlite_error)?;
    Ok(seq)
}

/// 按 `seq` 升序列出某 Turn 的公开事件。
pub fn list_for_turn(conn: &Connection, turn_id: &str) -> Result<Vec<RunEventView>> {
    let mut stmt = conn
        .prepare(
            "SELECT turn_id, seq, kind, summary_json, created_at_ms
               FROM ai_run_events WHERE turn_id=?1 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![turn_id], |row| {
            Ok(RunEventView {
                turn_id: row.get(0)?,
                seq: row.get(1)?,
                kind: row.get(2)?,
                summary_json: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// 按 kind 白名单读取某 Turn 的事件（仍按 `seq` 升序）。
///
/// 白名单由调用方给出：诊断读取面只取诊断类事件，不把 `model_route`、
/// `decision_advice`、`execution_policy` 等其它数据面的载荷带进来。空白名单
/// 返回空列表，而不是退回"读全部"。
pub fn list_filtered(
    conn: &Connection,
    turn_id: &str,
    kinds: &[&str],
) -> Result<Vec<RunEventView>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (2..=kinds.len() + 1)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn
        .prepare(&format!(
            "SELECT turn_id, seq, kind, summary_json, created_at_ms
               FROM ai_run_events WHERE turn_id=?1 AND kind IN ({placeholders})
              ORDER BY seq"
        ))
        .map_err(sqlite_error)?;
    let params = std::iter::once(turn_id)
        .chain(kinds.iter().copied())
        .collect::<Vec<_>>();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok(RunEventView {
                turn_id: row.get(0)?,
                seq: row.get(1)?,
                kind: row.get(2)?,
                summary_json: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}
