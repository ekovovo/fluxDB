//! T028 面板的数据入口。首次发送事务创建会话/Turn/用户消息，打开与关闭不创建空历史。
use super::{sessions, settings};
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// 面板打开状态：只取最近 100 条消息，较老历史由后续分页入口读取。
#[derive(Clone, Debug)]
pub struct ConversationData {
    pub sessions: Vec<sessions::SessionOverview>,
    pub preferences: settings::PanelPreferences,
    pub session: Option<sessions::SessionView>,
    pub messages: Vec<sessions::MessageView>,
    pub parts: Vec<sessions::MessagePartView>,
    pub turns: Vec<sessions::TurnView>,
    pub ui_state: Option<sessions::SessionUiStateView>,
    pub selected_model_id: Option<String>,
    pub model_selection_revision: i64,
    pub context_references: Vec<String>,
    pub usage: super::usage_ledger::UsageTotals,
}

pub fn load(conn: &Connection, selected: Option<&str>) -> Result<ConversationData> {
    let preferences = settings::load_panel_preferences(conn)?;
    let selected = selected
        .map(str::to_owned)
        .or_else(|| preferences.last_session_id.clone());
    let mut data = ConversationData {
        sessions: sessions::list_session_overviews(conn, 100)?,
        preferences,
        session: None,
        messages: Vec::new(),
        parts: Vec::new(),
        turns: Vec::new(),
        ui_state: None,
        selected_model_id: None,
        model_selection_revision: 0,
        context_references: Vec::new(),
        usage: super::usage_ledger::UsageTotals::default(),
    };
    if let Some(id) = selected {
        // 损坏/失效指针不能随机改选其它会话；保留明确失败供 UI 重试。
        let session = sessions::load_session(conn, &id)?;
        if session.status == sessions::SessionStatus::Archived {
            return Ok(data);
        }
        let mut stmt = conn.prepare("SELECT id FROM (SELECT id,seq FROM ai_messages WHERE session_id=?1 ORDER BY seq DESC LIMIT 100) ORDER BY seq") .map_err(sqlite_error)?;
        let ids = stmt
            .query_map([&id], |r| r.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        for msg in ids {
            let message = sessions::load_message(conn, &msg)?;
            data.parts.extend(sessions::list_parts(conn, &msg)?);
            data.messages.push(message);
        }
        data.turns = sessions::list_turns(conn, &id)?;
        data.ui_state = Some(sessions::load_ui_state(conn, &id)?);
        data.session = Some(session);
    }
    (data.selected_model_id, data.model_selection_revision) =
        super::model_selection::load(conn, data.session.as_ref().map(|s| s.id.as_str()))?;
    let session_id = data.session.as_ref().map(|s| s.id.as_str());
    data.context_references = super::context_references::list(conn, session_id)?;
    if let Some(id) = session_id {
        data.usage = super::usage_ledger::totals(conn, id)?;
    }
    Ok(data)
}

/// 原子发送：所有身份由宿主生成。禁止同会话同时启动两个任务，失败不留空会话。
#[allow(clippy::too_many_arguments)]
pub fn send(
    conn: &Connection,
    now: i64,
    new_session: bool,
    session_id: &str,
    agent_id: &str,
    turn_id: &str,
    message_id: &str,
    text: &str,
    target_json: &str,
    snapshot_json: &str,
) -> Result<()> {
    if text.trim().is_empty() || text.len() > 64 * 1024 {
        return Err(Error::new(ErrorKind::Internal, "消息不能为空或超过 64 KiB"));
    }
    for raw in [target_json, snapshot_json] {
        serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|_| Error::new(ErrorKind::Internal, "任务快照不是合法 JSON"))?;
    }
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    if new_session {
        let title: String = text.trim().chars().take(40).collect();
        sessions::create_session(&tx, now, session_id, agent_id, &title)?;
        super::model_selection::transfer_draft(&tx, session_id)?;
        super::context_references::transfer(&tx, session_id)?;
    }
    let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_turns WHERE session_id=?1 AND status IN ('queued','preparing','generating','executing_tools','waiting_approval','waiting_user','cancel_requested'))",[session_id],|r| r.get(0)).map_err(sqlite_error)?;
    if active {
        return Err(Error::new(ErrorKind::Internal, "该会话已有进行中的任务"));
    }
    sessions::start_turn(&tx, now, session_id, turn_id, target_json, snapshot_json)?;
    let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
    tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,metadata_json,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'user','{}','final',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    sessions::append_part(
        &tx,
        now,
        &format!("{message_id}-text"),
        message_id,
        sessions::PartKind::Text,
        sessions::PartStatus::Complete,
        &serde_json::json!({"text":text}).to_string(),
        None,
        None,
        None,
    )?;
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "任务快照不是合法 JSON"))?;
    if let Some(references) = snapshot
        .get("context_references")
        .and_then(|value| value.as_array())
    {
        if references.len() > 16 {
            return Err(Error::new(
                ErrorKind::Internal,
                "本轮上下文引用不能超过 16 项",
            ));
        }
        for (index, reference) in references.iter().enumerate() {
            let id = reference
                .get("id")
                .and_then(|value| value.as_str())
                .filter(|id| !id.is_empty())
                .ok_or_else(|| Error::new(ErrorKind::Internal, "引用缺少稳定 ID"))?;
            let label = reference
                .get("label")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::new(ErrorKind::Internal, "引用缺少名称"))?;
            let kind = reference
                .get("kind")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::new(ErrorKind::Internal, "引用缺少类型"))?;
            let summary = match reference.get("summary") {
                Some(serde_json::Value::String(summary)) => summary.as_str(),
                None | Some(serde_json::Value::Null) => "",
                _ => return Err(Error::new(ErrorKind::Internal, "引用摘要必须是文本")),
            };
            // 历史仅保存公开标签；附件路径/授权载荷仍只在固定 Turn 快照中。
            sessions::append_part(
                &tx,
                now,
                &format!("{message_id}-context-{index}"),
                message_id,
                sessions::PartKind::ContextRef,
                sessions::PartStatus::Complete,
                &serde_json::json!({"id":id,"label":label,"kind":kind,"summary":summary})
                    .to_string(),
                None,
                None,
                None,
            )?;
            // 只消费本次提交的引用，避免把发送期间新增的下一轮引用一起清掉。
            super::context_references::remove(&tx, Some(session_id), id)?;
        }
    }
    let mut prefs = settings::load_panel_preferences(&tx)?;
    let rev = prefs.revision;
    prefs.last_session_id = Some(session_id.into());
    prefs.pending_draft_json = None;
    settings::save_panel_preferences(&tx, now, &prefs, rev)?;
    // 已有会话的草稿清空与消息提交属于同一次事务。
    tx.execute("UPDATE ai_session_ui_state SET draft_json=NULL,revision=revision+1,updated_at_ms=?2 WHERE session_id=?1",params![session_id,now]).map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

/// 事件 checkpoint：正文单点存于 parts；回调只修改属于本 Turn 的片段。
#[allow(clippy::too_many_arguments)]
pub fn checkpoint(
    conn: &Connection,
    now: i64,
    session_id: &str,
    turn_id: &str,
    message_id: &str,
    text: &str,
    summary: &str,
    terminal: Option<sessions::TurnStatus>,
    error: Option<&str>,
) -> Result<()> {
    if text.len() + summary.len() > 4 * 1024 * 1024 {
        return Err(Error::new(ErrorKind::Internal, "回复超过持久化上限"));
    }
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let turn = sessions::load_turn(&tx, turn_id)?;
    if turn.session_id != session_id || turn.status.is_terminal() {
        return Err(Error::new(
            ErrorKind::Internal,
            "拒绝迟到或归属不符的任务事件",
        ));
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_messages WHERE id=?1 AND session_id=?2 AND turn_id=?3)",
            params![message_id, session_id, turn_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !exists {
        let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
        tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'assistant','checkpoint',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    }
    // 等待审批/等待用户不是失败：片段保持 pending，UI 才能区分"暂停"与"出错"。
    let status = match terminal {
        None => sessions::PartStatus::Streaming,
        Some(sessions::TurnStatus::Completed) => sessions::PartStatus::Complete,
        Some(sessions::TurnStatus::Cancelled) => sessions::PartStatus::Interrupted,
        Some(sessions::TurnStatus::WaitingApproval) | Some(sessions::TurnStatus::WaitingUser) => {
            sessions::PartStatus::Pending
        }
        _ => sessions::PartStatus::Error,
    };
    for (suffix, kind, value) in [
        ("text", sessions::PartKind::Text, text),
        ("summary", sessions::PartKind::ReasoningSummary, summary),
    ] {
        if value.is_empty() {
            continue;
        }
        let id = format!("{message_id}-{suffix}");
        let payload = serde_json::json!({"text":value}).to_string();
        let rev: Option<i64> = tx
            .query_row(
                "SELECT revision FROM ai_message_parts WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        match rev {
            Some(rev) => {
                sessions::update_part(&tx, now, &id, status, &payload, rev)?;
            }
            None => {
                sessions::append_part(
                    &tx, now, &id, message_id, kind, status, &payload, None, None, None,
                )?;
            }
        }
    }
    if terminal.is_some() {
        tx.execute("UPDATE ai_message_parts SET status=?2,revision=revision+1,updated_at_ms=?3 WHERE message_id=?1 AND status IN ('pending','streaming')",params![message_id,status.as_str(),now]).map_err(sqlite_error)?;
    }
    if let Some(status) = terminal {
        sessions::update_turn_status(&tx, now, turn_id, status, error, turn.revision)?;
        tx.execute(
            "UPDATE ai_messages SET persistence_kind='final' WHERE id=?1",
            [message_id],
        )
        .map_err(sqlite_error)?;
    }
    tx.commit().map_err(sqlite_error)
}

/// 追加/更新一条事件片段（工具卡、结果引用、通知、审批等）。
///
/// 与 `checkpoint` 同样的安全边界：Turn 归属必须匹配、终态 Turn 拒绝迟到事件；
/// 片段内容经 `validate_public_payload` 校验，不允许把凭据或加密载荷写进 UI 片段。
#[allow(clippy::too_many_arguments)]
pub fn upsert_event_part(
    conn: &Connection,
    now: i64,
    session_id: &str,
    turn_id: &str,
    message_id: &str,
    part_id: &str,
    kind: sessions::PartKind,
    status: sessions::PartStatus,
    payload_json: &str,
    tool_call_id: Option<&str>,
    result_id: Option<&str>,
) -> Result<()> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    upsert_event_part_in_transaction(
        &tx,
        now,
        session_id,
        turn_id,
        message_id,
        part_id,
        kind,
        status,
        payload_json,
        tool_call_id,
        result_id,
    )?;
    tx.commit().map_err(sqlite_error)
}

/// 与审批决定同事务更新卡片，避免决定成功但卡片更新失败的部分提交。
#[allow(clippy::too_many_arguments)]
pub fn upsert_event_part_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    session_id: &str,
    turn_id: &str,
    message_id: &str,
    part_id: &str,
    kind: sessions::PartKind,
    status: sessions::PartStatus,
    payload_json: &str,
    tool_call_id: Option<&str>,
    result_id: Option<&str>,
) -> Result<()> {
    let turn = sessions::load_turn(&tx, turn_id)?;
    if turn.session_id != session_id || turn.status.is_terminal() {
        return Err(Error::new(
            ErrorKind::Internal,
            "拒绝迟到或归属不符的任务事件",
        ));
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_messages WHERE id=?1 AND session_id=?2 AND turn_id=?3)",
            params![message_id, session_id, turn_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !exists {
        let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
        tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'assistant','checkpoint',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    }
    let rev: Option<i64> = tx
        .query_row(
            "SELECT revision FROM ai_message_parts WHERE id=?1",
            [part_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match rev {
        Some(rev) => {
            sessions::update_part(&tx, now, part_id, status, payload_json, rev)?;
        }
        None => {
            sessions::append_part(
                &tx,
                now,
                part_id,
                message_id,
                kind,
                status,
                payload_json,
                tool_call_id,
                result_id,
                None,
            )?;
        }
    }
    Ok(())
}

/// 追加/更新产物卡片段（`kind='artifact_ref'`）。
///
/// 与事件片段同样的安全边界：Turn 归属必须匹配、终态 Turn 拒绝迟到事件；
/// 产物须属于本 Turn，已有片段须匹配消息、类型及产物，不能借 ID 更新其他片段。
#[allow(clippy::too_many_arguments)]
pub fn upsert_artifact_part(
    conn: &Connection,
    now: i64,
    session_id: &str,
    turn_id: &str,
    message_id: &str,
    part_id: &str,
    status: sessions::PartStatus,
    payload_json: &str,
    artifact_id: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    upsert_artifact_part_in_transaction(
        &tx,
        now,
        session_id,
        turn_id,
        message_id,
        part_id,
        status,
        payload_json,
        artifact_id,
    )?;
    tx.commit().map_err(sqlite_error)
}

/// ready 状态与产物片段共同提交，片段写入失败不能留下假就绪产物。
#[allow(clippy::too_many_arguments)]
pub fn upsert_artifact_part_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    session_id: &str,
    turn_id: &str,
    message_id: &str,
    part_id: &str,
    status: sessions::PartStatus,
    payload_json: &str,
    artifact_id: &str,
) -> Result<()> {
    let turn = sessions::load_turn(&tx, turn_id)?;
    if turn.session_id != session_id || turn.status.is_terminal() {
        return Err(Error::new(
            ErrorKind::Internal,
            "拒绝迟到或归属不符的任务事件",
        ));
    }
    let artifact: Option<(String, String)> = tx
        .query_row(
            "SELECT session_id, turn_id FROM ai_artifacts WHERE id=?1",
            [artifact_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (artifact_session, artifact_turn) = artifact
        .ok_or_else(|| Error::new(ErrorKind::Internal, format!("产物不存在: {artifact_id}")))?;
    if artifact_session != session_id || artifact_turn != turn_id {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("产物不属于本任务: {artifact_id}"),
        ));
    }
    let existing_message: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT session_id, turn_id FROM ai_messages WHERE id=?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if existing_message
        .as_ref()
        .is_some_and(|(session, turn)| session != session_id || turn.as_deref() != Some(turn_id))
    {
        return Err(Error::new(ErrorKind::Internal, "产物消息不属于本任务"));
    }
    let existing_part: Option<(String, String, Option<String>, i64)> = tx
        .query_row(
            "SELECT message_id, kind, artifact_id, revision FROM ai_message_parts WHERE id=?1",
            [part_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if existing_part
        .as_ref()
        .is_some_and(|(message, kind, artifact, _)| {
            message != message_id
                || kind != sessions::PartKind::ArtifactRef.as_str()
                || artifact.as_deref() != Some(artifact_id)
        })
    {
        return Err(Error::new(
            ErrorKind::Internal,
            "已有片段的消息、类型或产物不匹配",
        ));
    }
    if existing_message.is_none() {
        let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
        tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'assistant','checkpoint',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    }
    match existing_part {
        Some((_, _, _, rev)) => {
            sessions::update_part(&tx, now, part_id, status, payload_json, rev)?;
        }
        None => {
            sessions::append_part(
                &tx,
                now,
                part_id,
                message_id,
                sessions::PartKind::ArtifactRef,
                status,
                payload_json,
                None,
                None,
                Some(artifact_id),
            )?;
        }
    }
    Ok(())
}
