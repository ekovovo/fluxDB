//! 产物声明与来源的持久化（设计文档 §27、§42.7，T039）。
//!
//! 产物是**声明式**的：`ai_artifacts.spec_json` 只描述类型/标题/renderer/口径/块结构，
//! 块通过结果句柄引用已取得的结果，不携带模型自填的数字。`ai_artifact_sources` 记录
//! 宿主**已解析**的结果/工具调用行 ID，用于追溯与删除影响查询。
//!
//! 模型在 spec 里声明的是结果**句柄**（如 `result-1`），宿主把它解析成真实的
//! `ai_results`/`ai_tool_calls` 行后，通过 [`ArtifactSourceInput`] 与本层交互；
//! 句柄原样写进来源行的 `metadata_json`，保证声明可追溯。

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// 产物状态取值，与 `ai_artifacts.status` 的 CHECK 一致。
pub const ARTIFACT_STATUSES: &[&str] = &["draft", "staged", "ready", "failed", "expired"];

/// 产物公开视图（不含渲染文件内容）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactView {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub kind: String,
    pub schema_version: i64,
    pub title: String,
    pub renderer_id: String,
    pub renderer_version: String,
    pub spec_json: String,
    pub status: String,
    pub relative_path: Option<String>,
    pub byte_count: i64,
    pub content_hash: Option<String>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// 一条产物来源的公开视图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactSourceView {
    pub artifact_id: String,
    pub ordinal: i64,
    pub result_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub metadata_json: String,
}

/// 一个来源的解析结果：模型声明的句柄 + 宿主解析出的结果/调用行 ID。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactSourceInput {
    /// 模型声明并由宿主导出的结果句柄（写进 `metadata_json` 的 `result_ref`，可追溯）。
    pub declared_ref: Option<String>,
    /// 宿主结果行 ID（`ai_results.id`）；必须可用且属于同一 session/turn。
    pub result_id: Option<String>,
    /// 工具调用行 ID（`ai_tool_calls.id`）；必须成功且属于同一 session/turn，
    /// 有结果时必须是该结果的实际 `source_call_id`。
    pub tool_call_id: Option<String>,
}

/// 解析后、待写入 `ai_artifact_sources` 的一行。
struct ResolvedSource {
    ordinal: i64,
    result_id: Option<String>,
    tool_call_id: Option<String>,
    metadata_json: String,
}

/// 新建产物草稿：写 `ai_artifacts(status='draft')` 并按解析出的来源写 `ai_artifact_sources`。
///
/// `sources` 须与 `spec.sources` 同序等长：第 n 个解析结果对应第 n 个声明来源。
/// 任一步校验失败都不留下任何行（单事务）。
#[allow(clippy::too_many_arguments)]
pub fn create_artifact(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    session_id: &str,
    turn_id: &str,
    renderer_version: &str,
    spec: &fluxdb_core::ArtifactSpec,
    sources: &[ArtifactSourceInput],
) -> Result<ArtifactView> {
    if id.trim().is_empty() || session_id.trim().is_empty() || turn_id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物 ID、会话 ID 与 Turn ID 不能为空",
        ));
    }
    if renderer_version.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "renderer 版本不能为空"));
    }
    spec.validate_shape()?;
    let spec_json = spec_json(spec)?;
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    validate_active_turn(&tx, session_id, turn_id)?;
    let resolved = resolve_sources(&tx, session_id, turn_id, spec, sources)?;
    tx.execute(
        "INSERT INTO ai_artifacts (id, session_id, turn_id, kind, schema_version, title,
            renderer_id, renderer_version, spec_json, status, relative_path, byte_count,
            content_hash, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'draft', NULL, 0, NULL, 1, ?10, ?10)",
        params![
            id,
            session_id,
            turn_id,
            spec.kind.as_str(),
            spec.schema_version as i64,
            spec.title,
            spec.renderer,
            renderer_version,
            spec_json,
            now_ms,
        ],
    )
    .map_err(sqlite_error)?;
    insert_sources(&tx, id, &resolved)?;
    tx.commit().map_err(sqlite_error)?;
    load_artifact(conn, id)
}

/// 覆盖草稿 spec（含来源重写），对象级 CAS，只允许 `status='draft'`。
///
/// 删除旧来源行后按新 spec 全量重写，与 spec 更新同一事务。
pub fn replace_artifact_spec(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    spec: &fluxdb_core::ArtifactSpec,
    sources: &[ArtifactSourceInput],
    expected_revision: i64,
) -> Result<ArtifactView> {
    spec.validate_shape()?;
    let spec_json = spec_json(spec)?;
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let current = load_artifact(&tx, id)?;
    if current.status != "draft" {
        return Err(Error::new(ErrorKind::Internal, "只有草稿产物可以修改声明"));
    }
    if current.revision != expected_revision {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    validate_active_turn(&tx, &current.session_id, &current.turn_id)?;
    let resolved = resolve_sources(&tx, &current.session_id, &current.turn_id, spec, sources)?;
    let changed = tx
        .execute(
            "UPDATE ai_artifacts SET kind=?2, schema_version=?3, title=?4, renderer_id=?5,
                spec_json=?6, revision=revision+1, updated_at_ms=?7
              WHERE id=?1 AND revision=?8 AND status='draft'",
            params![
                id,
                spec.kind.as_str(),
                spec.schema_version as i64,
                spec.title,
                spec.renderer,
                spec_json,
                now_ms,
                expected_revision,
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    tx.execute("DELETE FROM ai_artifact_sources WHERE artifact_id=?1", [id])
        .map_err(sqlite_error)?;
    insert_sources(&tx, id, &resolved)?;
    tx.commit().map_err(sqlite_error)?;
    load_artifact(conn, id)
}

/// 状态机推进（对象级 CAS）。
///
/// 允许 `draft→staged`、`draft|staged→failed`、非终态`→expired`；
/// `ready`/`failed`/`expired` 是终态，不允许再改。置 `ready` 走
/// [`mark_artifact_ready`]，需要在同一写入里带上渲染文件事实。
pub fn set_artifact_status(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    status: &str,
    expected_revision: i64,
) -> Result<ArtifactView> {
    if !ARTIFACT_STATUSES.contains(&status) {
        return Err(Error::new(ErrorKind::Internal, "非法产物状态"));
    }
    let current = load_artifact(conn, id)?;
    if current.revision != expected_revision {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    let allowed = match current.status.as_str() {
        "draft" => matches!(status, "staged" | "failed" | "expired"),
        "staged" => matches!(status, "failed" | "expired"),
        _ => false,
    };
    if !allowed {
        return Err(Error::new(
            ErrorKind::Internal,
            format!(
                "产物状态不允许从 {} 变为 {}（ready/failed/expired 为终态）",
                current.status, status
            ),
        ));
    }
    let changed = conn
        .execute(
            "UPDATE ai_artifacts SET status=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, status, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    load_artifact(conn, id)
}

/// 原子写回渲染文件事实并置 `ready`（`draft|staged → ready`，对象级 CAS）。
///
/// `relative_path` 必须是受控根内的相对路径（不得绝对路径、不得含 `..`、不得为空），
/// `byte_count >= 0`，`content_hash` 非空；否则报错且不写任何列。
#[allow(clippy::too_many_arguments)]
pub fn mark_artifact_ready(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    relative_path: &str,
    byte_count: i64,
    content_hash: &str,
    expected_revision: i64,
) -> Result<ArtifactView> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let view = mark_artifact_ready_in_transaction(
        &tx,
        now_ms,
        id,
        relative_path,
        byte_count,
        content_hash,
        expected_revision,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(view)
}

/// 与消息片段提交共用同一事务；来源复核与 ready 状态写入不可被拆开。
#[allow(clippy::too_many_arguments)]
pub fn mark_artifact_ready_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    now_ms: i64,
    id: &str,
    relative_path: &str,
    byte_count: i64,
    content_hash: &str,
    expected_revision: i64,
) -> Result<ArtifactView> {
    validate_relative_path(relative_path)?;
    if byte_count < 0 {
        return Err(Error::new(ErrorKind::Internal, "产物字节数不能为负"));
    }
    if content_hash.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "产物内容哈希不能为空"));
    }
    let current = load_artifact(&tx, id)?;
    if current.status != "draft" && current.status != "staged" {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("产物状态为 {}，不能置为就绪", current.status),
        ));
    }
    if current.revision != expected_revision {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    let spec: fluxdb_core::ArtifactSpec = serde_json::from_str(&current.spec_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "产物声明损坏"))?;
    let sources = list_artifact_sources(&tx, id)?
        .into_iter()
        .map(|source| {
            let metadata: serde_json::Value = serde_json::from_str(&source.metadata_json)
                .map_err(|_| Error::new(ErrorKind::Internal, "来源元数据损坏"))?;
            Ok(ArtifactSourceInput {
                declared_ref: metadata["result_ref"].as_str().map(str::to_owned),
                result_id: source.result_id,
                tool_call_id: source.tool_call_id,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // renderer 运行期间来源可能已过期，提交 ready 时在同一事务再复核一次。
    resolve_sources(&tx, &current.session_id, &current.turn_id, &spec, &sources)?;
    let changed = tx
        .execute(
            "UPDATE ai_artifacts SET status='ready', relative_path=?2, byte_count=?3,
                content_hash=?4, revision=revision+1, updated_at_ms=?5
              WHERE id=?1 AND revision=?6 AND status IN ('draft','staged')",
            params![
                id,
                relative_path,
                byte_count,
                content_hash,
                now_ms,
                expected_revision
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物已被其他写入者修改，请重新加载",
        ));
    }
    load_artifact(tx, id)
}

/// 读取单个产物。
pub fn load_artifact(conn: &Connection, id: &str) -> Result<ArtifactView> {
    conn.query_row(
        "SELECT id, session_id, turn_id, kind, schema_version, title, renderer_id,
                renderer_version, spec_json, status, relative_path, byte_count,
                content_hash, revision, created_at_ms, updated_at_ms
           FROM ai_artifacts WHERE id=?1",
        [id],
        |row| {
            Ok(ArtifactView {
                id: row.get(0)?,
                session_id: row.get(1)?,
                turn_id: row.get(2)?,
                kind: row.get(3)?,
                schema_version: row.get(4)?,
                title: row.get(5)?,
                renderer_id: row.get(6)?,
                renderer_version: row.get(7)?,
                spec_json: row.get(8)?,
                status: row.get(9)?,
                relative_path: row.get(10)?,
                byte_count: row.get(11)?,
                content_hash: row.get(12)?,
                revision: row.get(13)?,
                created_at_ms: row.get(14)?,
                updated_at_ms: row.get(15)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "产物不存在"))
}

/// 列出某个 Turn 的产物（按创建时间与 ID）。
pub fn list_artifacts_for_turn(conn: &Connection, turn_id: &str) -> Result<Vec<ArtifactView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_artifacts WHERE turn_id=?1 ORDER BY created_at_ms, id")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([turn_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_artifact(conn, &id)).collect()
}

/// 列出某个产物的来源（按 ordinal）。
pub fn list_artifact_sources(
    conn: &Connection,
    artifact_id: &str,
) -> Result<Vec<ArtifactSourceView>> {
    let mut stmt = conn
        .prepare(
            "SELECT artifact_id, ordinal, result_id, tool_call_id, metadata_json
               FROM ai_artifact_sources WHERE artifact_id=?1 ORDER BY ordinal",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([artifact_id], |row| {
            Ok(ArtifactSourceView {
                artifact_id: row.get(0)?,
                ordinal: row.get(1)?,
                result_id: row.get(2)?,
                tool_call_id: row.get(3)?,
                metadata_json: row.get(4)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)
}

fn spec_json(spec: &fluxdb_core::ArtifactSpec) -> Result<String> {
    serde_json::to_string(spec)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("产物声明序列化失败: {e}")))
}

/// 路径使用 `/` 分隔的普通段；同时拒绝其他平台的分隔符、盘符和路径归一化歧义。
fn validate_relative_path(relative_path: &str) -> Result<()> {
    let path = std::path::Path::new(relative_path);
    if relative_path.is_empty()
        || relative_path.contains(['\\', ':', '\0'])
        || relative_path.split('/').any(|segment| {
            segment.trim().is_empty()
                || segment == "."
                || segment == ".."
                || segment.ends_with(['.', ' '])
        })
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物路径必须是受控根内的相对路径",
        ));
    }
    Ok(())
}

fn validate_active_turn(conn: &Connection, session_id: &str, turn_id: &str) -> Result<()> {
    let turn = super::sessions::load_turn(conn, turn_id)?;
    if turn.session_id != session_id || turn.status.is_terminal() {
        return Err(Error::new(
            ErrorKind::Internal,
            "拒绝迟到或归属不符的产物声明",
        ));
    }
    Ok(())
}

/// 校验并解析来源：与声明逐项对应，结果可用且由本任务的成功调用产出。
///
/// 只声明结果句柄、未直接给出工具调用时，把结果的 `source_call_id` 一并带上，
/// 并校验实际调用。只声明工具调用时只保存追溯关系，不推导或创建结果数据。
fn resolve_sources(
    conn: &Connection,
    session_id: &str,
    turn_id: &str,
    spec: &fluxdb_core::ArtifactSpec,
    sources: &[ArtifactSourceInput],
) -> Result<Vec<ResolvedSource>> {
    if sources.is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "产物必须至少解析出一个来源",
        ));
    }
    if sources.len() != spec.sources.len() {
        return Err(Error::new(
            ErrorKind::Internal,
            "来源解析条数与产物声明的来源数量不一致",
        ));
    }
    let mut resolved = Vec::with_capacity(sources.len());
    for (ordinal, input) in sources.iter().enumerate() {
        if input.declared_ref != spec.sources[ordinal].result_ref {
            return Err(Error::new(
                ErrorKind::Internal,
                format!("第 {ordinal} 项来源句柄与产物声明不一致"),
            ));
        }
        if input.declared_ref.is_some() != input.result_id.is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "结果声明必须解析到结果行；工具调用来源不能附带未声明的结果",
            ));
        }
        let mut tool_call_id = input.tool_call_id.clone();
        if let Some(result_id) = input.result_id.as_deref() {
            let (row_session, row_turn, source_call_id, status) = conn
                .query_row(
                    "SELECT session_id, turn_id, source_call_id, status FROM ai_results WHERE id=?1",
                    [result_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("产物来源结果不存在: {result_id}"),
                    )
                })?;
            if row_session.as_deref() != Some(session_id) || row_turn.as_deref() != Some(turn_id) {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("产物来源结果不属于本任务: {result_id}"),
                ));
            }
            if status != "available" {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("产物来源结果不可用: {result_id}"),
                ));
            }
            let source_call_id = source_call_id
                .ok_or_else(|| Error::new(ErrorKind::Internal, "产物来源结果缺少实际工具调用"))?;
            if tool_call_id
                .as_deref()
                .is_some_and(|id| id != source_call_id)
            {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "产物来源工具调用与结果的实际来源不一致",
                ));
            }
            tool_call_id = Some(source_call_id);
        }
        if let Some(call_id) = tool_call_id.as_deref() {
            let (row_session, row_turn, status) = conn
                .query_row(
                    "SELECT session_id, turn_id, status FROM ai_tool_calls WHERE id=?1",
                    [call_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_error)?
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("产物来源工具调用不存在: {call_id}"),
                    )
                })?;
            if row_session != session_id || row_turn != turn_id {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("产物来源工具调用不属于本任务: {call_id}"),
                ));
            }
            if status != "succeeded" {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("产物来源工具调用未成功: {call_id}"),
                ));
            }
        }
        if input.result_id.is_none() && tool_call_id.is_none() {
            return Err(Error::new(
                ErrorKind::Internal,
                "来源解析结果必须至少包含结果行或工具调用行",
            ));
        }
        // 声明句柄原样保留，供追溯模型到底引用了哪个结果；无句柄时写空对象。
        let metadata_json = match input.declared_ref.as_deref() {
            Some(declared) => serde_json::json!({ "result_ref": declared }).to_string(),
            _ => "{}".to_string(),
        };
        resolved.push(ResolvedSource {
            ordinal: ordinal as i64,
            result_id: input.result_id.clone(),
            tool_call_id,
            metadata_json,
        });
    }
    Ok(resolved)
}

fn insert_sources(conn: &Connection, artifact_id: &str, sources: &[ResolvedSource]) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "INSERT INTO ai_artifact_sources
                (artifact_id, ordinal, result_id, tool_call_id, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sqlite_error)?;
    for source in sources {
        stmt.execute(params![
            artifact_id,
            source.ordinal,
            source.result_id,
            source.tool_call_id,
            source.metadata_json,
        ])
        .map_err(sqlite_error)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
