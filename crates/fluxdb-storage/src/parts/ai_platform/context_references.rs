//! 每个草稿/会话的显式上下文引用；不存 KV，不写凭据或文件正文。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, params};
pub fn list(conn: &Connection, session_id: Option<&str>) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT payload_json FROM ai_context_references WHERE scope_id=?1 ORDER BY rowid")
        .map_err(sqlite_error)?;
    stmt.query_map([session_id.unwrap_or("draft")], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}
pub fn add(conn: &Connection, session_id: Option<&str>, id: &str, payload: &str) -> Result<()> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_context_references WHERE scope_id=?1",
            [session_id.unwrap_or("draft")],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if count >= 16 || payload.len() > 8192 {
        return Err(Error::new(
            ErrorKind::Internal,
            "上下文引用最多 16 项，每项不超过 8 KiB",
        ));
    }
    conn.execute("INSERT OR IGNORE INTO ai_context_references(scope_id,session_id,reference_id,payload_json) VALUES(?1,?2,?3,?4)",params![session_id.unwrap_or("draft"),session_id,id,payload]).map_err(sqlite_error)?;
    Ok(())
}
pub fn remove(conn: &Connection, session_id: Option<&str>, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ai_context_references WHERE scope_id=?1 AND reference_id=?2",
        params![session_id.unwrap_or("draft"), id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
pub fn transfer(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE ai_context_references SET scope_id=?1,session_id=?1 WHERE scope_id='draft'",
        [session_id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
pub fn clear_draft(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM ai_context_references WHERE scope_id='draft'",
        [],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
