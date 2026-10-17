//! 会话级模型覆盖：显式选择优先，草稿不制造空历史，发送时原子转移到会话。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

pub fn load(conn: &Connection, session_id: Option<&str>) -> Result<(Option<String>, i64)> {
    conn.query_row(
        "SELECT model_profile_id,revision FROM ai_session_model_selections WHERE scope_id=?1",
        [session_id.unwrap_or("draft")],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(sqlite_error)
    .map(|row| row.unwrap_or((None, 0)))
}

pub fn save(
    conn: &Connection,
    session_id: Option<&str>,
    model_id: Option<&str>,
    expected: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    if let Some(id) = model_id {
        let model = super::models::load_model(&tx, id)?;
        if !model.enabled || model.kind != super::models::ModelKind::Generation {
            return Err(Error::new(ErrorKind::Internal, "请选择已启用的生成模型"));
        }
    }
    if let Some(id) = session_id {
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_turns WHERE session_id=?1 AND status IN ('queued','preparing','generating','executing_tools','waiting_approval','waiting_user','cancel_requested','outcome_unknown'))",
            [id], |r| r.get(0),
        ).map_err(sqlite_error)?;
        if active {
            return Err(Error::new(
                ErrorKind::Internal,
                "当前任务未结束，不能切换模型",
            ));
        }
    }
    let scope = session_id.unwrap_or("draft");
    let changed = if expected == 0 {
        tx.execute("INSERT OR IGNORE INTO ai_session_model_selections(scope_id,session_id,model_profile_id,revision) VALUES(?1,?2,?3,1)", params![scope,session_id,model_id])
    } else {
        tx.execute("UPDATE ai_session_model_selections SET model_profile_id=?2,revision=revision+1 WHERE scope_id=?1 AND revision=?3",params![scope,model_id,expected])
    }.map_err(sqlite_error)?;
    if changed != 1 {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型选择已改变，请重新加载会话",
        ));
    }
    tx.commit().map_err(sqlite_error)
}

pub fn transfer_draft(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE ai_session_model_selections SET scope_id=?1,session_id=?1 WHERE scope_id='draft'",
        [session_id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

pub fn clear_draft(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM ai_session_model_selections WHERE scope_id='draft'",
        [],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
