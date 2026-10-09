//! 会话 / Turn / 消息 / 消息片段 / 运行事件 / UI 状态持久化（设计 §9、§12、T015）。
//!
//! 三条硬约束：
//! 1. 序号（消息 seq、片段 ordinal、事件 seq）由 SQL 原子分配，重复调用不会撞号。
//! 2. 只持久化公开内容：`reasoning_summary` 之类片段不得夹带私有 CoT 字段。
//! 3. 流式片段走 [`upsert_streaming_part`] 合并到同一行，不按 token 插行。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Archived,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Archived => "archived",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(SessionStatus::Active),
            "archived" => Ok(SessionStatus::Archived),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知会话状态: {other}"),
            )),
        }
    }
}

/// Turn 状态机（与 DDL CHECK 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Queued,
    Preparing,
    Generating,
    ExecutingTools,
    WaitingApproval,
    WaitingUser,
    CancelRequested,
    Cancelled,
    Completed,
    Failed,
    Interrupted,
    OutcomeUnknown,
}

impl TurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnStatus::Queued => "queued",
            TurnStatus::Preparing => "preparing",
            TurnStatus::Generating => "generating",
            TurnStatus::ExecutingTools => "executing_tools",
            TurnStatus::WaitingApproval => "waiting_approval",
            TurnStatus::WaitingUser => "waiting_user",
            TurnStatus::CancelRequested => "cancel_requested",
            TurnStatus::Cancelled => "cancelled",
            TurnStatus::Completed => "completed",
            TurnStatus::Failed => "failed",
            TurnStatus::Interrupted => "interrupted",
            TurnStatus::OutcomeUnknown => "outcome_unknown",
        }
    }

    /// 终态：不再接受迟到事件，也不再被单活跃 Turn 唯一索引算作活跃。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TurnStatus::Cancelled
                | TurnStatus::Completed
                | TurnStatus::Failed
                | TurnStatus::Interrupted
                | TurnStatus::OutcomeUnknown
        )
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(TurnStatus::Queued),
            "preparing" => Ok(TurnStatus::Preparing),
            "generating" => Ok(TurnStatus::Generating),
            "executing_tools" => Ok(TurnStatus::ExecutingTools),
            "waiting_approval" => Ok(TurnStatus::WaitingApproval),
            "waiting_user" => Ok(TurnStatus::WaitingUser),
            "cancel_requested" => Ok(TurnStatus::CancelRequested),
            "cancelled" => Ok(TurnStatus::Cancelled),
            "completed" => Ok(TurnStatus::Completed),
            "failed" => Ok(TurnStatus::Failed),
            "interrupted" => Ok(TurnStatus::Interrupted),
            "outcome_unknown" => Ok(TurnStatus::OutcomeUnknown),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知 Turn 状态: {other}"),
            )),
        }
    }
}

/// 消息片段类型（公开内容；不含私有推理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Text,
    ReasoningSummary,
    Plan,
    ContextRef,
    ToolCallRef,
    ToolResultRef,
    ArtifactRef,
    Notice,
}

impl PartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PartKind::Text => "text",
            PartKind::ReasoningSummary => "reasoning_summary",
            PartKind::Plan => "plan",
            PartKind::ContextRef => "context_ref",
            PartKind::ToolCallRef => "tool_call_ref",
            PartKind::ToolResultRef => "tool_result_ref",
            PartKind::ArtifactRef => "artifact_ref",
            PartKind::Notice => "notice",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "text" => Ok(PartKind::Text),
            "reasoning_summary" => Ok(PartKind::ReasoningSummary),
            "plan" => Ok(PartKind::Plan),
            "context_ref" => Ok(PartKind::ContextRef),
            "tool_call_ref" => Ok(PartKind::ToolCallRef),
            "tool_result_ref" => Ok(PartKind::ToolResultRef),
            "artifact_ref" => Ok(PartKind::ArtifactRef),
            "notice" => Ok(PartKind::Notice),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知消息片段类型: {other}"),
            )),
        }
    }
}

/// 片段状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartStatus {
    Pending,
    Streaming,
    Complete,
    Interrupted,
    Error,
}

impl PartStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PartStatus::Pending => "pending",
            PartStatus::Streaming => "streaming",
            PartStatus::Complete => "complete",
            PartStatus::Interrupted => "interrupted",
            PartStatus::Error => "error",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(PartStatus::Pending),
            "streaming" => Ok(PartStatus::Streaming),
            "complete" => Ok(PartStatus::Complete),
            "interrupted" => Ok(PartStatus::Interrupted),
            "error" => Ok(PartStatus::Error),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知片段状态: {other}"),
            )),
        }
    }
}

/// 消息持久化类型：最终消息或压缩/中断时的 checkpoint。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    Final,
    Checkpoint,
}

impl PersistenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PersistenceKind::Final => "final",
            PersistenceKind::Checkpoint => "checkpoint",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "final" => Ok(PersistenceKind::Final),
            "checkpoint" => Ok(PersistenceKind::Checkpoint),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知消息持久化类型: {other}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub status: SessionStatus,
    pub next_message_seq: i64,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// 会话历史列表行：会话身份 + 派生统计，供设置页「会话历史」渲染。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOverview {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub status: SessionStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// 已持久化的消息条数（用户 + 助手）。
    pub message_count: i64,
    /// Turn（任务轮次）总数。
    pub turn_count: i64,
    /// 该会话最后一个 Turn 的状态；没有 Turn 时为 `None`。
    pub last_turn_status: Option<TurnStatus>,
    /// 该会话最后一个 Turn 的执行目标 JSON；没有 Turn 时为 `None`。
    pub last_target_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnView {
    pub id: String,
    pub session_id: String,
    pub ordinal: i64,
    pub status: TurnStatus,
    pub target_json: String,
    pub runtime_snapshot_json: String,
    pub iteration_count: i64,
    pub next_event_seq: i64,
    pub error_code: Option<String>,
    pub revision: i64,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub seq: i64,
    pub role: String,
    pub metadata_json: String,
    pub persistence_kind: PersistenceKind,
    pub redaction_version: i64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePartView {
    pub id: String,
    pub message_id: String,
    pub ordinal: i64,
    pub kind: PartKind,
    pub status: PartStatus,
    pub schema_version: i64,
    pub payload_json: String,
    pub tool_call_id: Option<String>,
    pub result_id: Option<String>,
    pub artifact_id: Option<String>,
    pub revision: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEventView {
    pub turn_id: String,
    pub seq: i64,
    pub kind: String,
    pub summary_json: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUiStateView {
    pub session_id: String,
    pub selected_turn_id: Option<String>,
    pub expanded_part_ids_json: String,
    pub scroll_anchor_message_id: Option<String>,
    pub ui_options_json: String,
    pub draft_json: Option<String>,
    pub revision: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct SessionUiStateEdit {
    pub session_id: String,
    pub selected_turn_id: Option<String>,
    pub expanded_part_ids_json: String,
    pub scroll_anchor_message_id: Option<String>,
    pub ui_options_json: String,
    pub draft_json: Option<String>,
}

/// 关闭重开 / 强杀后恢复用的公开快照。
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub session: SessionView,
    pub turns: Vec<TurnView>,
    pub messages: Vec<MessageView>,
    /// 与 `messages` 一一对应的片段（按 message 分组），只含公开内容。
    pub parts: Vec<MessagePartView>,
    /// 该会话下所有 Turn 的运行事件（按 turn、seq 排序）。
    pub run_events: Vec<RunEventView>,
    pub ui_state: Option<SessionUiStateView>,
}

/// 私有推理字段黑名单：公开片段里出现这些键即拒绝写入。
const PRIVATE_REASONING_KEYS: &[&str] = &[
    "private_reasoning",
    "hidden_cot",
    "chain_of_thought",
    "internal_monologue",
];

fn validate_json(raw: &str, label: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(raw)
        .map(|_| ())
        .map_err(|e| Error::new(ErrorKind::Internal, format!("{label} 不是合法 JSON: {e}")))
}

/// 拒绝把私有推理写进公开片段（设计：不持久化私有 CoT）。
fn validate_public_payload(kind: PartKind, payload_json: &str) -> Result<()> {
    validate_json(payload_json, "片段 payload")?;
    if kind != PartKind::ReasoningSummary {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("片段 payload 解析失败: {e}")))?;
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if PRIVATE_REASONING_KEYS.contains(&key.as_str()) {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("片段包含私有推理字段 {key}，拒绝持久化"),
                ));
            }
        }
    }
    Ok(())
}

// ---- 会话 ----

/// 新建会话。
pub fn create_session(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    agent_id: &str,
    title: &str,
) -> Result<SessionView> {
    if id.trim().is_empty() || agent_id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "会话 ID 与 Agent ID 不能为空",
        ));
    }
    conn.execute(
        "INSERT INTO ai_sessions (id, agent_id, title, status, next_message_seq, revision,
            created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, 'active', 1, 1, ?4, ?4)",
        params![id, agent_id, title, now_ms],
    )
    .map_err(sqlite_error)?;
    load_session(conn, id)
}

/// 读取会话。
pub fn load_session(conn: &Connection, id: &str) -> Result<SessionView> {
    conn.query_row(
        "SELECT id, agent_id, title, status, next_message_seq, revision, created_at_ms, updated_at_ms
           FROM ai_sessions WHERE id=?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "会话不存在"))
    .and_then(|(id, agent_id, title, status, next_message_seq, revision, created, updated)| {
        Ok(SessionView {
            id,
            agent_id,
            title,
            status: SessionStatus::parse(&status)?,
            next_message_seq,
            revision,
            created_at_ms: created,
            updated_at_ms: updated,
        })
    })
}

/// 最近会话列表（更新时间倒序）。
pub fn list_sessions(conn: &Connection, limit: i64) -> Result<Vec<SessionView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM ai_sessions
              ORDER BY updated_at_ms DESC, id
              LIMIT ?1",
        )
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([limit], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_session(conn, &id)).collect()
}

/// 会话历史概览（设置页「会话历史」用）：一次查询取会话 + 派生统计。
///
/// `last_turn_status` 取该会话最后一个 Turn 的状态，用于列表状态列；
/// 没有 Turn 的会话为 `None`。排序与 [`list_sessions`] 一致（按更新时间倒序）。
pub fn list_session_overviews(conn: &Connection, limit: i64) -> Result<Vec<SessionOverview>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.agent_id, s.title, s.status, s.revision,
                    s.created_at_ms, s.updated_at_ms,
                    (SELECT COUNT(*) FROM ai_messages m WHERE m.session_id=s.id),
                    (SELECT COUNT(*) FROM ai_turns t WHERE t.session_id=s.id),
                    (SELECT t.status FROM ai_turns t WHERE t.session_id=s.id
                      ORDER BY t.ordinal DESC LIMIT 1),
                    (SELECT t.target_json FROM ai_turns t WHERE t.session_id=s.id
                      ORDER BY t.ordinal DESC LIMIT 1)
               FROM ai_sessions s
              ORDER BY s.updated_at_ms DESC, s.id
              LIMIT ?1",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    rows.into_iter()
        .map(
            |(
                id,
                agent_id,
                title,
                status,
                revision,
                created_at_ms,
                updated_at_ms,
                message_count,
                turn_count,
                last_turn_status,
                last_target_json,
            )| {
                Ok(SessionOverview {
                    id,
                    agent_id,
                    title,
                    status: SessionStatus::parse(&status)?,
                    revision,
                    created_at_ms,
                    updated_at_ms,
                    message_count,
                    turn_count,
                    last_turn_status: last_turn_status
                        .as_deref()
                        .map(TurnStatus::parse)
                        .transpose()?,
                    last_target_json,
                })
            },
        )
        .collect()
}

/// 重命名会话（对象级 CAS）。
pub fn rename_session(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    title: &str,
    expected_revision: i64,
) -> Result<i64> {
    let changed = conn
        .execute(
            "UPDATE ai_sessions SET title=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, title, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "会话已被其他窗口修改，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

/// 首轮完成后替换默认截断标题；只覆盖 revision=1（未经用户改名/归档）的会话。
/// revision 递增与手工重命名一致：后续 CAS 编辑以最新 revision 为准。
/// 返回是否实际更新；已改名/已归档返回 `Ok(false)`（标题生成是尽力而为，不阻塞回复）。
pub fn rename_session_if_default(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    title: &str,
) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE ai_sessions SET title=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=1",
            params![id, title, now_ms],
        )
        .map_err(sqlite_error)?;
    Ok(changed > 0)
}

/// 读取最早一条用户消息的文本拼接（标题生成用），不含后续追问；无文本返回 `None`。
pub fn first_user_message_text(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    let mut texts: Vec<String> = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT p.payload_json FROM ai_message_parts p
              WHERE p.message_id=(
                  SELECT id FROM ai_messages
                   WHERE session_id=?1 AND role='user' ORDER BY seq LIMIT 1)
                AND p.kind='text'
              ORDER BY p.ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?;
    while let Some(Ok(row)) = rows.next() {
        if let Ok(serde_json::Value::Object(payload)) = serde_json::from_str(&row) {
            if let Some(serde_json::Value::String(text)) = payload.get("text") {
                texts.push(text.clone());
            }
        }
    }
    Ok(if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    })
}

/// 恢复已归档会话（对象级 CAS）：回到活跃历史，消息与审计事实不变。
pub fn restore_session(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    expected_revision: i64,
) -> Result<i64> {
    let changed = conn
        .execute(
            "UPDATE ai_sessions SET status='active', revision=revision+1, updated_at_ms=?2
              WHERE id=?1 AND revision=?3 AND status='archived'",
            params![id, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "会话不存在、状态已变化或未归档，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

/// 归档会话（对象级 CAS）。
pub fn archive_session(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    expected_revision: i64,
) -> Result<i64> {
    let changed = conn
        .execute(
            "UPDATE ai_sessions SET status='archived', revision=revision+1, updated_at_ms=?2
              WHERE id=?1 AND revision=?3 AND status='active'",
            params![id, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "会话不存在、状态已变化或已归档，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

// ---- Turn ----

/// 开始一个新的 Turn：序号原子分配 + 固化目标与运行配置快照。
///
/// 同一会话同时只能有一个活跃 Turn（DDL 的部分唯一索引兜底）。
pub fn start_turn(
    conn: &Connection,
    now_ms: i64,
    session_id: &str,
    id: &str,
    target_json: &str,
    runtime_snapshot_json: &str,
) -> Result<TurnView> {
    validate_json(target_json, "Turn 目标")?;
    validate_json(runtime_snapshot_json, "运行配置快照")?;
    let ordinal: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM ai_turns WHERE session_id=?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    conn.execute(
        "INSERT INTO ai_turns (id, session_id, ordinal, status, target_json,
            runtime_snapshot_json, iteration_count, next_event_seq, revision,
            started_at_ms)
         VALUES (?1, ?2, ?3, 'queued', ?4, ?5, 0, 1, 1, ?6)",
        params![
            id,
            session_id,
            ordinal,
            target_json,
            runtime_snapshot_json,
            now_ms
        ],
    )
    .map_err(|e| {
        // 部分唯一索引冲突 → 给出可读原因，而不是原始 SQLite 文本。
        // SQLite 对 `ai_one_active_turn_per_session` 报的是列级文本
        // `UNIQUE constraint failed: ai_turns.session_id`，因此两种都要匹配。
        let message = e.to_string();
        if message.contains("ai_one_active_turn_per_session")
            || (message.contains("UNIQUE constraint failed")
                && message.contains("ai_turns.session_id"))
        {
            Error::new(ErrorKind::Internal, "该会话已有进行中的 Turn")
        } else {
            sqlite_error(e)
        }
    })?;
    load_turn(conn, id)
}

/// 读取 Turn。
pub fn load_turn(conn: &Connection, id: &str) -> Result<TurnView> {
    conn.query_row(
        "SELECT id, session_id, ordinal, status, target_json, runtime_snapshot_json,
                iteration_count, next_event_seq, error_code, revision, started_at_ms, finished_at_ms
           FROM ai_turns WHERE id=?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "Turn 不存在"))
    .and_then(
        |(
            id,
            session_id,
            ordinal,
            status,
            target_json,
            runtime_snapshot_json,
            iteration_count,
            next_event_seq,
            error_code,
            revision,
            started_at_ms,
            finished_at_ms,
        )| {
            Ok(TurnView {
                id,
                session_id,
                ordinal,
                status: TurnStatus::parse(&status)?,
                target_json,
                runtime_snapshot_json,
                iteration_count,
                next_event_seq,
                error_code,
                revision,
                started_at_ms,
                finished_at_ms,
            })
        },
    )
}

/// 某会话的 Turn 列表（按 ordinal）。
pub fn list_turns(conn: &Connection, session_id: &str) -> Result<Vec<TurnView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_turns WHERE session_id=?1 ORDER BY ordinal")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_turn(conn, &id)).collect()
}

/// 更新 Turn 状态（对象级 CAS）；进入终态时写 `finished_at_ms`。
pub fn update_turn_status(
    conn: &Connection,
    now_ms: i64,
    turn_id: &str,
    status: TurnStatus,
    error_code: Option<&str>,
    expected_revision: i64,
) -> Result<i64> {
    let finished_at = status.is_terminal().then_some(now_ms);
    let changed = conn
        .execute(
            "UPDATE ai_turns
                SET status=?2, error_code=?3, finished_at_ms=COALESCE(?4, finished_at_ms),
                    revision=revision+1
              WHERE id=?1 AND revision=?5",
            params![
                turn_id,
                status.as_str(),
                error_code,
                finished_at,
                expected_revision
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "Turn 已被其他执行者修改，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

/// 递增 iteration 计数（一次主模型推理 +1）。
pub fn bump_iteration(conn: &Connection, turn_id: &str, expected_revision: i64) -> Result<i64> {
    let changed = conn
        .execute(
            "UPDATE ai_turns SET iteration_count=iteration_count+1, revision=revision+1
              WHERE id=?1 AND revision=?2",
            params![turn_id, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "Turn 已被其他执行者修改，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

// ---- 消息与片段 ----

/// 追加消息；`seq` 由会话的 `next_message_seq` 原子分配。
pub fn append_message(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    session_id: &str,
    turn_id: Option<&str>,
    role: &str,
    metadata_json: &str,
    persistence_kind: PersistenceKind,
) -> Result<MessageView> {
    validate_json(metadata_json, "消息 metadata")?;
    // 序号分配与 INSERT 必须同一事务：失败时回滚，不能留下 seq 空洞或改动会话时间。
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let seq: i64 = tx
        .query_row(
            "UPDATE ai_sessions SET next_message_seq = next_message_seq + 1, updated_at_ms=?2
              WHERE id=?1
             RETURNING next_message_seq - 1",
            params![session_id, now_ms],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "会话不存在"))?;
    tx.execute(
        "INSERT INTO ai_messages (id, session_id, turn_id, seq, role, metadata_json,
            persistence_kind, redaction_version, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
        params![
            id,
            session_id,
            turn_id,
            seq,
            role,
            metadata_json,
            persistence_kind.as_str(),
            now_ms
        ],
    )
    .map_err(|e| {
        // 弱归属检查：turn_id 必须属于同一 session（DDL 复合外键兜底）。
        let message = e.to_string();
        if message.contains("FOREIGN KEY") {
            Error::new(ErrorKind::Internal, "Turn 不属于该会话")
        } else {
            sqlite_error(e)
        }
    })?;
    tx.commit().map_err(sqlite_error)?;
    load_message(conn, id)
}

/// 读取消息。
pub fn load_message(conn: &Connection, id: &str) -> Result<MessageView> {
    conn.query_row(
        "SELECT id, session_id, turn_id, seq, role, metadata_json, persistence_kind,
                redaction_version, created_at_ms
           FROM ai_messages WHERE id=?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "消息不存在"))
    .and_then(
        |(id, session_id, turn_id, seq, role, metadata_json, kind, redaction, created)| {
            Ok(MessageView {
                id,
                session_id,
                turn_id,
                seq,
                role,
                metadata_json,
                persistence_kind: PersistenceKind::parse(&kind)?,
                redaction_version: redaction,
                created_at_ms: created,
            })
        },
    )
}

/// 会话消息列表（按 seq）。
pub fn list_messages(conn: &Connection, session_id: &str) -> Result<Vec<MessageView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_messages WHERE session_id=?1 ORDER BY seq")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_message(conn, &id)).collect()
}

/// 追加片段；`ordinal` 由同消息内原子分配。
pub fn append_part(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    message_id: &str,
    kind: PartKind,
    status: PartStatus,
    payload_json: &str,
    tool_call_id: Option<&str>,
    result_id: Option<&str>,
    artifact_id: Option<&str>,
) -> Result<MessagePartView> {
    validate_public_payload(kind, payload_json)?;
    let ordinal: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM ai_message_parts WHERE message_id=?1",
            [message_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    conn.execute(
        "INSERT INTO ai_message_parts (id, message_id, ordinal, kind, status,
            schema_version, payload_json, tool_call_id, result_id, artifact_id,
            revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, 1, ?10, ?10)",
        params![
            id,
            message_id,
            ordinal,
            kind.as_str(),
            status.as_str(),
            payload_json,
            tool_call_id,
            result_id,
            artifact_id,
            now_ms
        ],
    )
    .map_err(sqlite_error)?;
    load_part(conn, id)
}

/// 读取片段。
pub fn load_part(conn: &Connection, id: &str) -> Result<MessagePartView> {
    conn.query_row(
        "SELECT id, message_id, ordinal, kind, status, schema_version, payload_json,
                tool_call_id, result_id, artifact_id, revision, updated_at_ms
           FROM ai_message_parts WHERE id=?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "消息片段不存在"))
    .and_then(
        |(
            id,
            message_id,
            ordinal,
            kind,
            status,
            schema_version,
            payload_json,
            tool_call_id,
            result_id,
            artifact_id,
            revision,
            updated_at_ms,
        )| {
            Ok(MessagePartView {
                id,
                message_id,
                ordinal,
                kind: PartKind::parse(&kind)?,
                status: PartStatus::parse(&status)?,
                schema_version,
                payload_json,
                tool_call_id,
                result_id,
                artifact_id,
                revision,
                updated_at_ms,
            })
        },
    )
}

/// 消息片段列表（按 ordinal）。
pub fn list_parts(conn: &Connection, message_id: &str) -> Result<Vec<MessagePartView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_message_parts WHERE message_id=?1 ORDER BY ordinal")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([message_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_part(conn, &id)).collect()
}

/// 更新片段的 payload/status（对象级 CAS）。流式合并走这里，不新增行。
pub fn update_part(
    conn: &Connection,
    now_ms: i64,
    part_id: &str,
    status: PartStatus,
    payload_json: &str,
    expected_revision: i64,
) -> Result<i64> {
    validate_json(payload_json, "片段 payload")?;
    let changed = conn
        .execute(
            "UPDATE ai_message_parts SET status=?2, payload_json=?3, revision=revision+1,
                updated_at_ms=?4
              WHERE id=?1 AND revision=?5",
            params![
                part_id,
                status.as_str(),
                payload_json,
                now_ms,
                expected_revision
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "消息片段已被其他写入者修改，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

/// 流式合并：同一条消息里同类型的未完成片段只更新一行，不按 token 插行。
///
/// 没有可合并的片段时新建一条 `streaming` 片段。调用方按 250–500ms 节流后调用即可。
pub fn upsert_streaming_part(
    conn: &Connection,
    now_ms: i64,
    id_if_new: &str,
    message_id: &str,
    kind: PartKind,
    payload_json: &str,
) -> Result<MessagePartView> {
    validate_public_payload(kind, payload_json)?;
    let existing: Option<(String, i64)> = conn
        .query_row(
            "SELECT id, revision FROM ai_message_parts
              WHERE message_id=?1 AND kind=?2 AND status IN ('pending','streaming')
              ORDER BY ordinal DESC LIMIT 1",
            params![message_id, kind.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match existing {
        Some((part_id, revision)) => {
            update_part(
                conn,
                now_ms,
                &part_id,
                PartStatus::Streaming,
                payload_json,
                revision,
            )?;
            load_part(conn, &part_id)
        }
        None => append_part(
            conn,
            now_ms,
            id_if_new,
            message_id,
            kind,
            PartStatus::Streaming,
            payload_json,
            None,
            None,
            None,
        ),
    }
}

// ---- 运行事件 ----

/// 追加运行事件（`seq` 由 Turn 原子分配）。
///
/// 迟到事件防护：Turn 已进入终态时拒绝写入，避免旧事件污染新的 Turn。
pub fn append_run_event(
    conn: &Connection,
    now_ms: i64,
    turn_id: &str,
    kind: &str,
    summary_json: &str,
) -> Result<i64> {
    validate_json(summary_json, "事件摘要")?;
    let turn = load_turn(conn, turn_id)?;
    if turn.status.is_terminal() {
        return Err(Error::new(
            ErrorKind::Internal,
            "Turn 已结束，拒绝写入迟到事件",
        ));
    }
    let seq: i64 = conn
        .query_row(
            "UPDATE ai_turns SET next_event_seq = next_event_seq + 1
              WHERE id=?1
             RETURNING next_event_seq - 1",
            [turn_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    conn.execute(
        "INSERT INTO ai_run_events (turn_id, seq, kind, summary_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![turn_id, seq, kind, summary_json, now_ms],
    )
    .map_err(sqlite_error)?;
    Ok(seq)
}

/// Turn 的事件列表（按 seq）。
pub fn list_run_events(conn: &Connection, turn_id: &str) -> Result<Vec<RunEventView>> {
    let mut stmt = conn
        .prepare(
            "SELECT turn_id, seq, kind, summary_json, created_at_ms
               FROM ai_run_events WHERE turn_id=?1 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([turn_id], |row| {
            Ok(RunEventView {
                turn_id: row.get(0)?,
                seq: row.get(1)?,
                kind: row.get(2)?,
                summary_json: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)
}

// ---- 会话 UI 状态 ----

/// 读取会话 UI 状态；没有记录时返回默认值（revision=0）。
pub fn load_ui_state(conn: &Connection, session_id: &str) -> Result<SessionUiStateView> {
    let row = conn
        .query_row(
            "SELECT session_id, selected_turn_id, expanded_part_ids_json,
                    scroll_anchor_message_id, ui_options_json, draft_json, revision, updated_at_ms
               FROM ai_session_ui_state WHERE session_id=?1",
            [session_id],
            |row| {
                Ok(SessionUiStateView {
                    session_id: row.get(0)?,
                    selected_turn_id: row.get(1)?,
                    expanded_part_ids_json: row.get(2)?,
                    scroll_anchor_message_id: row.get(3)?,
                    ui_options_json: row.get(4)?,
                    draft_json: row.get(5)?,
                    revision: row.get(6)?,
                    updated_at_ms: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(row.unwrap_or(SessionUiStateView {
        session_id: session_id.to_string(),
        selected_turn_id: None,
        expanded_part_ids_json: "[]".into(),
        scroll_anchor_message_id: None,
        ui_options_json: "{}".into(),
        draft_json: None,
        revision: 0,
        updated_at_ms: 0,
    }))
}

/// 保存会话 UI 状态（单例/行级 CAS；`expected_revision=0` 表示首次创建）。
pub fn save_ui_state(
    conn: &Connection,
    now_ms: i64,
    edit: &SessionUiStateEdit,
    expected_revision: i64,
) -> Result<i64> {
    validate_json(&edit.expanded_part_ids_json, "展开片段列表")?;
    validate_json(&edit.ui_options_json, "UI 选项")?;
    if let Some(draft) = edit.draft_json.as_deref() {
        validate_json(draft, "草稿")?;
    }
    let changed = conn
        .execute(
            "INSERT INTO ai_session_ui_state (session_id, selected_turn_id,
                expanded_part_ids_json, scroll_anchor_message_id, ui_options_json, draft_json,
                revision, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)
             ON CONFLICT(session_id) DO UPDATE SET
                selected_turn_id=excluded.selected_turn_id,
                expanded_part_ids_json=excluded.expanded_part_ids_json,
                scroll_anchor_message_id=excluded.scroll_anchor_message_id,
                ui_options_json=excluded.ui_options_json,
                draft_json=excluded.draft_json,
                revision=ai_session_ui_state.revision+1,
                updated_at_ms=excluded.updated_at_ms
              WHERE ai_session_ui_state.revision=?8",
            params![
                edit.session_id,
                edit.selected_turn_id,
                edit.expanded_part_ids_json,
                edit.scroll_anchor_message_id,
                edit.ui_options_json,
                edit.draft_json,
                now_ms,
                expected_revision,
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "会话 UI 状态已被其他窗口修改，请重新加载",
        ));
    }
    if expected_revision == 0 {
        Ok(1)
    } else {
        Ok(expected_revision + 1)
    }
}

/// 关闭重开 / 强杀后恢复的公开快照（不含私有推理）。
pub fn session_snapshot(conn: &Connection, session_id: &str) -> Result<SessionSnapshot> {
    let session = load_session(conn, session_id)?;
    let turns = list_turns(conn, session_id)?;
    let messages = list_messages(conn, session_id)?;
    let mut parts = Vec::new();
    for message in &messages {
        parts.extend(list_parts(conn, &message.id)?);
    }
    let ui_state = conn
        .query_row(
            "SELECT session_id FROM ai_session_ui_state WHERE session_id=?1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let ui_state = match ui_state {
        Some(_) => Some(load_ui_state(conn, session_id)?),
        None => None,
    };
    let mut run_events = Vec::new();
    for turn in &turns {
        run_events.extend(list_run_events(conn, &turn.id)?);
    }
    Ok(SessionSnapshot {
        session,
        turns,
        messages,
        parts,
        run_events,
        ui_state,
    })
}

// 兼容原有入口，工具事实行实现已拆至独立职责模块。
pub use super::tool_calls::*;
