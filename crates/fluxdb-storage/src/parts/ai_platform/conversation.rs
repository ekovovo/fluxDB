//! T028 面板的数据入口。首次发送事务创建会话/Turn/用户消息，打开与关闭不创建空历史。
use rusqlite::{Connection, OptionalExtension, params};
use fluxdb_core::{Error, ErrorKind, Result};
use crate::sqlite::sqlite_error;
use super::{sessions, settings};

/// 面板打开状态：只取最近 100 条消息，较老历史由后续分页入口读取。
#[derive(Clone, Debug)]
pub struct ConversationData {
    pub sessions: Vec<sessions::SessionView>,
    pub preferences: settings::PanelPreferences,
    pub session: Option<sessions::SessionView>,
    pub messages: Vec<sessions::MessageView>,
    pub parts: Vec<sessions::MessagePartView>,
    pub turns: Vec<sessions::TurnView>,
    pub ui_state: Option<sessions::SessionUiStateView>,
}

pub fn load(conn: &Connection, selected: Option<&str>) -> Result<ConversationData> {
    let preferences = settings::load_panel_preferences(conn)?;
    let selected = selected.map(str::to_owned).or_else(|| preferences.last_session_id.clone());
    let mut data = ConversationData {
        sessions: sessions::list_sessions(conn, 100)?, preferences,
        session: None, messages: Vec::new(), parts: Vec::new(), turns: Vec::new(), ui_state: None,
    };
    if let Some(id) = selected {
        // 损坏/失效指针不能随机改选其它会话；保留明确失败供 UI 重试。
        let session = sessions::load_session(conn, &id)?;
        if session.status == sessions::SessionStatus::Archived { return Ok(data); }
        let mut stmt = conn.prepare("SELECT id FROM (SELECT id,seq FROM ai_messages WHERE session_id=?1 ORDER BY seq DESC LIMIT 100) ORDER BY seq") .map_err(sqlite_error)?;
        let ids = stmt.query_map([&id], |r| r.get::<_,String>(0)).map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>,_>>().map_err(sqlite_error)?;
        for msg in ids {
            let message = sessions::load_message(conn, &msg)?;
            data.parts.extend(sessions::list_parts(conn, &msg)?);
            data.messages.push(message);
        }
        data.turns = sessions::list_turns(conn, &id)?;
        data.ui_state = Some(sessions::load_ui_state(conn, &id)?);
        data.session = Some(session);
    }
    Ok(data)
}

/// 原子发送：所有身份由宿主生成。禁止同会话同时启动两个任务，失败不留空会话。
#[allow(clippy::too_many_arguments)]
pub fn send(conn: &Connection, now: i64, new_session: bool, session_id: &str, agent_id: &str,
    turn_id: &str, message_id: &str, text: &str, target_json: &str, snapshot_json: &str) -> Result<()> {
    if text.trim().is_empty() || text.len()>64*1024 { return Err(Error::new(ErrorKind::Internal,"消息不能为空或超过 64 KiB")); }
    for raw in [target_json,snapshot_json] {
        serde_json::from_str::<serde_json::Value>(raw).map_err(|_| Error::new(ErrorKind::Internal,"任务快照不是合法 JSON"))?;
    }
    let tx=conn.unchecked_transaction().map_err(sqlite_error)?;
    if new_session {
        let title: String=text.trim().chars().take(40).collect();
        sessions::create_session(&tx, now, session_id, agent_id, &title)?;
    }
    let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_turns WHERE session_id=?1 AND status IN ('queued','preparing','generating','executing_tools','waiting_approval','waiting_user','cancel_requested'))",[session_id],|r| r.get(0)).map_err(sqlite_error)?;
    if active { return Err(Error::new(ErrorKind::Internal,"该会话已有进行中的任务")); }
    sessions::start_turn(&tx, now, session_id, turn_id,target_json,snapshot_json)?;
    let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
    tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,metadata_json,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'user','{}','final',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    sessions::append_part(&tx,now,&format!("{message_id}-text"),message_id,sessions::PartKind::Text,sessions::PartStatus::Complete,&serde_json::json!({"text":text}).to_string(),None,None,None)?;
    let mut prefs=settings::load_panel_preferences(&tx)?;
    let rev=prefs.revision; prefs.last_session_id=Some(session_id.into()); prefs.pending_draft_json=None;
    settings::save_panel_preferences(&tx,now,&prefs,rev)?;
    // 已有会话的草稿清空与消息提交属于同一次事务。
    tx.execute("UPDATE ai_session_ui_state SET draft_json=NULL,revision=revision+1,updated_at_ms=?2 WHERE session_id=?1",params![session_id,now]).map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

/// 事件 checkpoint：正文单点存于 parts；回调只修改属于本 Turn 的片段。
#[allow(clippy::too_many_arguments)]
pub fn checkpoint(conn:&Connection, now:i64, session_id:&str, turn_id:&str, message_id:&str,
    text:&str, summary:&str, terminal:Option<sessions::TurnStatus>, error:Option<&str>) -> Result<()> {
    if text.len()+summary.len()>4*1024*1024 {return Err(Error::new(ErrorKind::Internal,"回复超过持久化上限"));}
    let tx=conn.unchecked_transaction().map_err(sqlite_error)?;
    let turn=sessions::load_turn(&tx,turn_id)?;
    if turn.session_id!=session_id || turn.status.is_terminal() { return Err(Error::new(ErrorKind::Internal,"拒绝迟到或归属不符的任务事件")); }
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_messages WHERE id=?1 AND session_id=?2 AND turn_id=?3)",params![message_id,session_id,turn_id],|r|r.get(0)).map_err(sqlite_error)?;
    if !exists {
        let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1,updated_at_ms=?2 WHERE id=?1 RETURNING next_message_seq-1",params![session_id,now],|r|r.get(0)).map_err(sqlite_error)?;
        tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'assistant','checkpoint',?5)",params![message_id,session_id,turn_id,seq,now]).map_err(sqlite_error)?;
    }
    let status=match terminal { None=>sessions::PartStatus::Streaming, Some(sessions::TurnStatus::Completed)=>sessions::PartStatus::Complete, Some(sessions::TurnStatus::Cancelled)=>sessions::PartStatus::Interrupted,_=>sessions::PartStatus::Error };
    for (suffix,kind,value) in [("text",sessions::PartKind::Text,text),("summary",sessions::PartKind::ReasoningSummary,summary)] {
        if value.is_empty() {continue;}
        let id=format!("{message_id}-{suffix}"); let payload=serde_json::json!({"text":value}).to_string();
        let rev:Option<i64>=tx.query_row("SELECT revision FROM ai_message_parts WHERE id=?1",[&id],|r|r.get(0)).optional().map_err(sqlite_error)?;
        match rev {Some(rev)=>{sessions::update_part(&tx,now,&id,status,&payload,rev)?;},None=>{sessions::append_part(&tx,now,&id,message_id,kind,status,&payload,None,None,None)?;} }
    }
    if terminal.is_some() {
        tx.execute("UPDATE ai_message_parts SET status=?2,revision=revision+1,updated_at_ms=?3 WHERE message_id=?1 AND status IN ('pending','streaming')",params![message_id,status.as_str(),now]).map_err(sqlite_error)?;
    }
    if let Some(status)=terminal {
        sessions::update_turn_status(&tx,now,turn_id,status,error,turn.revision)?;
        tx.execute("UPDATE ai_messages SET persistence_kind='final' WHERE id=?1",[message_id]).map_err(sqlite_error)?;
    }
    tx.commit().map_err(sqlite_error)
}
