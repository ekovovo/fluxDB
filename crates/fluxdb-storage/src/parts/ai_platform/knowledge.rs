//! 已确认业务知识条目（`ai_knowledge_entries`）：只读列表用于 `@` 候选。
//! 只返回非敏感元数据；正文留待详情按需读取，不进候选摘要。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeEntryView {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub connection_id: Option<i64>,
    pub database_name: Option<String>,
    pub namespace: Option<String>,
    pub revision: i64,
}

/// 列出 `confirmed` 状态的知识条目；`query` 为空返回全部（有上限）。
pub fn list_confirmed(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<KnowledgeEntryView>> {
    let needle = format!("%{}%", query.trim().to_lowercase());
    let limit = limit.clamp(1, 100) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, kind, connection_id, database_name, namespace, revision
               FROM ai_knowledge_entries
              WHERE status='confirmed'
                AND (title LIKE ?1 OR kind LIKE ?1 OR COALESCE(database_name,'') LIKE ?1)
              ORDER BY updated_at_ms DESC, id
              LIMIT ?2",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![needle, limit], |row| {
            Ok(KnowledgeEntryView {
                id: row.get(0)?,
                title: row.get(1)?,
                kind: row.get(2)?,
                connection_id: row.get(3)?,
                database_name: row.get(4)?,
                namespace: row.get(5)?,
                revision: row.get(6)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// 保存/更新一条知识条目（对象级 CAS）；本切片用于测试与后续知识管理页。
#[allow(clippy::too_many_arguments)]
pub fn save(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    title: &str,
    kind: &str,
    connection_id: Option<i64>,
    database_name: Option<&str>,
    namespace: Option<&str>,
    content_json: &str,
    status: &str,
    expected_revision: Option<i64>,
) -> Result<i64> {
    if title.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "知识条目标题不能为空"));
    }
    if !matches!(status, "proposed" | "confirmed" | "retired") {
        return Err(Error::new(ErrorKind::Internal, "未知知识条目状态"));
    }
    serde_json::from_str::<serde_json::Value>(content_json)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("知识正文不是合法 JSON: {e}")))?;
    let changed = match expected_revision {
        None => conn.execute(
            "INSERT INTO ai_knowledge_entries
                (id,title,kind,connection_id,database_name,namespace,scope_json,content_json,
                 source_json,status,revision,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'{}',?7,'{}',?8,1,?9,?9)",
            params![
                id,
                title,
                kind,
                connection_id,
                database_name,
                namespace,
                content_json,
                status,
                now_ms
            ],
        ),
        Some(revision) => conn.execute(
            "UPDATE ai_knowledge_entries
                SET title=?2,kind=?3,connection_id=?4,database_name=?5,namespace=?6,
                    content_json=?7,status=?8,revision=revision+1,updated_at_ms=?9
              WHERE id=?1 AND revision=?10",
            params![
                id,
                title,
                kind,
                connection_id,
                database_name,
                namespace,
                content_json,
                status,
                now_ms,
                revision
            ],
        ),
    }
    .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目已变更，请重新加载",
        ));
    }
    conn.query_row(
        "SELECT revision FROM ai_knowledge_entries WHERE id=?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "知识条目写入后不可见"))
}
