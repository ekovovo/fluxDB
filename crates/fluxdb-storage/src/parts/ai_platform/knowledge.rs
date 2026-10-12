//! 业务知识条目（`ai_knowledge_entries`，设计 §19/§42.10 / T038）。
//!
//! 知识是用户确认后的口径/规则：模型提议（proposed）不自动成为事实；
//! confirmed 按 `@` 纳入候选并进提示词；retired 立即退出候选且终态不可改回。
//! 正文按条目单独有界读取，候选/提示词只携带有界摘要或元数据，不整库全量装载。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeEntryView {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub content_json: String,
    pub source_json: String,
    pub status: String,
    pub scope_json: String,
    pub connection_id: Option<i64>,
    pub database_name: Option<String>,
    pub namespace: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: i64,
}

/// 新建/更新输入；失败时不写库，不留部分提交。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeEntryEdit {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub connection_id: Option<i64>,
    pub database_name: Option<String>,
    pub namespace: Option<String>,
    pub scope_json: String,
    pub content_json: String,
    pub source_json: String,
    pub status: String,
}

const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024;

fn json_object(raw: &str, label: &str) -> Result<serde_json::Value> {
    if raw.len() > MAX_JSON_BYTES {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("{label}超过 64 KiB 上限"),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        Error::new(
            ErrorKind::Internal,
            format!("{label}不是合法 JSON: {error}"),
        )
    })?;
    if value.as_object().is_none() {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("{label}必须是 JSON 对象"),
        ));
    }
    Ok(value)
}

fn is_scoped(scope_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(scope_json)
        .map(|value| {
            let connections = value["connections"].as_array();
            let databases = value["databases"].as_array();
            (connections.is_some_and(|values| !values.is_empty()))
                || (databases.is_some_and(|values| !values.is_empty()))
        })
        .unwrap_or(false)
}

fn scope_matches(
    scope_json: &str,
    connection_id: Option<i64>,
    database_name: Option<&str>,
) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(scope_json) else {
        return false;
    };
    let empty: [serde_json::Value; 0] = [];
    let allowed = value["connections"]
        .as_array()
        .map(|values| values.as_slice())
        .unwrap_or(&empty);
    let databases = value["databases"]
        .as_array()
        .map(|values| values.as_slice())
        .unwrap_or(&empty);
    if let Some(id) = connection_id {
        // 空列表表示全局可用；显式列表要求包含目标连接。
        if !allowed.is_empty() && !allowed.iter().any(|item| item.as_i64() == Some(id)) {
            return false;
        }
    } else if !allowed.is_empty() {
        return false;
    }
    if let Some(database) = database_name {
        if !databases.is_empty() && !databases.iter().any(|item| item.as_str() == Some(database)) {
            return false;
        }
    } else if !databases.is_empty() {
        return false;
    }
    true
}

/// 保存/更新一条知识条目（对象级 CAS）；
/// 正文、scope 与 source 均是受应用层校验的 JSON，不在这里解释业务语义。
pub fn save(
    conn: &Connection,
    now_ms: i64,
    edit: &KnowledgeEntryEdit,
    expected_revision: Option<i64>,
) -> Result<i64> {
    if edit.title.trim().is_empty() || edit.id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目 ID 与标题不能为空",
        ));
    }
    if edit.title.len() > MAX_TEXT_BYTES {
        return Err(Error::new(ErrorKind::Internal, "知识标题超过 64 KiB 上限"));
    }
    if edit.kind.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "知识类型不能为空"));
    }
    if !matches!(edit.status.as_str(), "proposed" | "confirmed" | "retired") {
        return Err(Error::new(ErrorKind::Internal, "未知知识条目状态"));
    }
    let scope = json_object(&edit.scope_json, "知识作用域")?;
    if let Some(database) = &edit.database_name {
        let wants = scope["databases"].as_array();
        if let Some(values) = wants.filter(|values| !values.is_empty()) {
            if !values.iter().any(|value| value.as_str() == Some(database)) {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "数据库定位必须包含在作用域白名单内",
                ));
            }
        }
    }
    json_object(&edit.content_json, "知识正文")?;
    let insert_sql = "INSERT INTO ai_knowledge_entries (id,agent_id,connection_id,database_name,namespace,scope_json,content_json, source_json,title,kind,status,revision,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?12)";
    let update_sql = "UPDATE ai_knowledge_entries SET agent_id=?2, connection_id=?3, database_name=?4, namespace=?5, scope_json=?6, content_json=?7, source_json=?8, title=?9, kind=?10, revision=revision+1, updated_at_ms=?11 WHERE id=?1 AND revision=?12 AND status != 'retired'";
    let changed = match expected_revision {
        None => conn
            .execute(
                insert_sql,
                params![
                    edit.id,
                    edit.agent_id,
                    edit.connection_id,
                    edit.database_name,
                    edit.namespace,
                    edit.scope_json,
                    edit.content_json,
                    edit.source_json,
                    edit.title,
                    edit.kind,
                    edit.status,
                    now_ms
                ],
            )
            .map_err(sqlite_error)?,
        Some(revision) => conn
            .execute(
                update_sql,
                params![
                    edit.id,
                    edit.agent_id,
                    edit.connection_id,
                    edit.database_name,
                    edit.namespace,
                    edit.scope_json,
                    edit.content_json,
                    edit.source_json,
                    edit.title,
                    edit.kind,
                    now_ms,
                    revision
                ],
            )
            .map_err(sqlite_error)?,
    };
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目已变更或不存在，请重新加载",
        ));
    }
    conn.query_row(
        "SELECT revision FROM ai_knowledge_entries WHERE id=?1",
        [edit.id.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "知识条目写入后不可见"))
}

pub fn load(conn: &Connection, id: &str) -> Result<KnowledgeEntryView> {
    let row = conn
        .query_row(
            "SELECT id, title, kind, agent_id, connection_id, database_name, namespace,
                    scope_json, content_json, source_json, status, revision,
                    created_at_ms, updated_at_ms
               FROM ai_knowledge_entries WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "知识条目不存在"))?;
    Ok(KnowledgeEntryView {
        id: row.0,
        title: row.1,
        kind: row.2,
        agent_id: row.3,
        connection_id: row.4,
        database_name: row.5,
        namespace: row.6,
        scope_json: row.7,
        created_at_ms: row.12,
        updated_at_ms: row.13,
        revision: row.11,
        content_json: row.8,
        source_json: row.9,
        status: row.10,
    })
}

/// 读取 confirmed 条目正文；只回执有界切片，不把特大知识整篇装进上下文。
pub fn confirmed_excerpt(conn: &Connection, id: &str, max_bytes: usize) -> Result<String> {
    let entry = load(conn, id)?;
    if entry.status != "confirmed" {
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目不在已确认状态，不能读取",
        ));
    }
    if max_bytes == 0 {
        return Err(Error::new(ErrorKind::Internal, "读取长度必须为正"));
    }
    if max_bytes >= entry.content_json.len() {
        return Ok(entry.content_json);
    }
    let mut end = max_bytes;
    while end > 0 && !entry.content_json.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return Ok(String::new());
    }
    Ok(format!(
        "{}… [截断，可用 knowledge.read 续读，剩余 {} 字节]",
        &entry.content_json[..end],
        entry.content_json.len() - end
    ))
}

/// confirmed 列表（@ 候选 / 提示词挑选用）。
///
/// `target` 为本轮固定目标；空作用域条目全部可用，显式白名单条目必须覆盖目标。
pub fn list_confirmed_scoped(
    conn: &Connection,
    target: Option<(i64, Option<&str>)>,
    query: &str,
    limit: usize,
) -> Result<Vec<KnowledgeEntryView>> {
    let needle = format!("%{}%", query.trim().to_lowercase());
    let (has_target, connection_id, database_name) = match target {
        Some((id, database)) => (true, Some(id), database),
        None => (false, None, None),
    };
    let limit = limit.clamp(1, 100) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, kind, agent_id, connection_id, database_name, namespace,
                    scope_json, content_json, source_json, status, revision,
                    created_at_ms, updated_at_ms
               FROM ai_knowledge_entries
              WHERE status='confirmed'
                AND (title LIKE ?1 OR kind LIKE ?1 OR COALESCE(database_name,'') LIKE ?1
                     OR namespace LIKE ?1)
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
                agent_id: row.get(3)?,
                connection_id: row.get(4)?,
                database_name: row.get(5)?,
                namespace: row.get(6)?,
                created_at_ms: row.get(12)?,
                updated_at_ms: row.get(13)?,
                revision: row.get(11)?,
                content_json: row.get(8)?,
                source_json: row.get(9)?,
                status: row.get(10)?,
                scope_json: row.get(7)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let mut all = rows
        .into_iter()
        .filter(|entry| {
            !is_scoped(&entry.scope_json)
                || (has_target && scope_matches(&entry.scope_json, connection_id, database_name))
        })
        .collect::<Vec<_>>();
    all.truncate(limit as usize);
    Ok(all)
}

/// 用户确认提议（对象级 CAS）；只允许 proposed → confirmed。
pub fn confirm(conn: &Connection, now_ms: i64, id: &str, expected_revision: i64) -> Result<i64> {
    set_status(conn, now_ms, id, expected_revision, "confirmed")
}

/// 退休确认或过时知识（对象级 CAS）；retired 是终态，不能改回其他状态。
pub fn retire(conn: &Connection, now_ms: i64, id: &str, expected_revision: i64) -> Result<i64> {
    set_status(conn, now_ms, id, expected_revision, "retired")
}

/// 删除知识条目（对象级 CAS）； proposed/confirmed/retired 都不再是长期事实之外的状态，
/// 但删除是一种显式清理，不是默默掩埋错误口径。
pub fn delete(conn: &Connection, id: &str, expected_revision: i64) -> Result<()> {
    let changed = conn
        .execute(
            "DELETE FROM ai_knowledge_entries WHERE id=?1 AND revision=?2",
            params![id, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目不存在或已变更，请重新加载",
        ));
    }
    Ok(())
}

/// 状态流转（对象级 CAS）：其他状态 → confirmed / retired；retired 是终态。
fn set_status(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    expected_revision: i64,
    to: &str,
) -> Result<i64> {
    let changed = conn
        .execute(
            "UPDATE ai_knowledge_entries
            SET status=?2, revision=revision+1, updated_at_ms=?3
          WHERE id=?1 AND revision=?4 AND status != 'retired'",
            params![id, to, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_knowledge_entries WHERE id=?1)",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .unwrap_or(false);
        if !exists {
            return Err(Error::new(ErrorKind::Internal, "知识条目不存在"));
        }
        return Err(Error::new(
            ErrorKind::Internal,
            "知识条目不在可变更状态或 revision 已过期，请重新加载",
        ));
    }
    conn.query_row(
        "SELECT revision FROM ai_knowledge_entries WHERE id=?1",
        [id],
        |row| row.get(0),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "知识条目写入后不可见"))
}
