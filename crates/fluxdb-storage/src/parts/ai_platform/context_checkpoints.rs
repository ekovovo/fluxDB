// 上下文压缩 checkpoint（设计 §16.1 / T069）。
//
// 只保存"成功且通过校验"的摘要与切点元数据；原始消息/审计永不删除。
// 每次提交都是一次原子写入，失败/取消/无进展都不覆盖旧 checkpoint。

use rusqlite::{Connection, OptionalExtension, params};

use fluxdb_core::{Error, ErrorKind, Result};

fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::new(ErrorKind::Internal, error.to_string())
}

/// checkpoint 行（对外只暴露非敏感字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointView {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub generation: i64,
    pub first_message_seq: i64,
    pub last_message_seq: i64,
    pub summary_text: String,
    pub protected_items_json: String,
    pub source_hash: String,
    pub model_profile_id: Option<String>,
    pub created_at_ms: i64,
}

/// 新 checkpoint 的提交输入。
#[derive(Debug, Clone)]
pub struct ContextCheckpointEdit {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub generation: i64,
    pub first_message_seq: i64,
    pub last_message_seq: i64,
    pub summary_text: String,
    pub protected_items_json: String,
    pub source_hash: String,
    pub model_profile_id: Option<String>,
}

/// 读取会话最新的有效 checkpoint（`generation` 最大者）。
pub fn load_latest(conn: &Connection, session_id: &str) -> Result<Option<ContextCheckpointView>> {
    conn.query_row(
        "SELECT id, session_id, turn_id, generation, first_message_seq, last_message_seq,
                summary_text, protected_items_json, source_hash, model_profile_id, created_at_ms
           FROM ai_context_checkpoints
          WHERE session_id=?1
          ORDER BY generation DESC
          LIMIT 1",
        [session_id],
        |row| {
            Ok(ContextCheckpointView {
                id: row.get(0)?,
                session_id: row.get(1)?,
                turn_id: row.get(2)?,
                generation: row.get(3)?,
                first_message_seq: row.get(4)?,
                last_message_seq: row.get(5)?,
                summary_text: row.get(6)?,
                protected_items_json: row.get(7)?,
                source_hash: row.get(8)?,
                model_profile_id: row.get(9)?,
                created_at_ms: row.get(10)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// 原子提交一条 checkpoint；`UNIQUE(turn_id, generation)` 冲突时直接报错，不覆盖旧摘要。
pub fn insert(conn: &Connection, now_ms: i64, edit: &ContextCheckpointEdit) -> Result<()> {
    if edit.summary_text.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "拒绝提交空摘要的上下文 checkpoint",
        ));
    }
    if edit.last_message_seq < edit.first_message_seq {
        return Err(Error::new(
            ErrorKind::Internal,
            "上下文 checkpoint 切点不合法",
        ));
    }
    conn.execute(
        "INSERT INTO ai_context_checkpoints
            (id, session_id, turn_id, generation, first_message_seq, last_message_seq,
             summary_text, protected_items_json, source_hash, model_profile_id, created_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            edit.id,
            edit.session_id,
            edit.turn_id,
            edit.generation,
            edit.first_message_seq,
            edit.last_message_seq,
            edit.summary_text,
            edit.protected_items_json,
            edit.source_hash,
            edit.model_profile_id,
            now_ms,
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}
