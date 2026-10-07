//! T015 会话/Turn/消息/片段/运行事件/UI 状态持久化的隔离 fixture 与测试。
//!
//! 每个用例用独立 SQLite + 真实 `ensure_schema`，并预置一个 Agent（会话有外键依赖）。
//! 测试既断言公开 API 返回值，也直接查询表，避免只验证“成功提示”。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::ai_platform::{agents, ensure_schema};

use super::sessions::{
    self, PartKind, PartStatus, PersistenceKind, SessionUiStateEdit, TurnStatus,
};
use super::settings::{self, PanelMode, PanelPreferences};

/// 建一个带 Agent 的隔离库。
pub(super) fn setup() -> Connection {
    let conn = Connection::open_in_memory().expect("内存库");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("外键");
    ensure_schema(&conn).expect("建表");
    agents::create_agent(
        &conn,
        1_000,
        &agents::AgentEdit {
            compression_trigger_percent: 80,
            id: "agent-1".into(),
            display_name: "通用 Agent".into(),
            description: String::new(),
            enabled: true,
            default_model_profile_id: None,
            system_prompt: "测试".into(),
            budget_policy_id: None,
            allowed_tools: Vec::new(),
        },
    )
    .expect("建 Agent");
    conn
}

fn create_session(conn: &Connection, id: &str) -> sessions::SessionView {
    sessions::create_session(conn, 1_000, id, "agent-1", "测试会话").expect("建会话")
}

fn start_turn(conn: &Connection, session_id: &str, id: &str, now_ms: i64) -> sessions::TurnView {
    sessions::start_turn(
        conn,
        now_ms,
        session_id,
        id,
        r#"{"kind":"sql"}"#,
        r#"{"model":"test"}"#,
    )
    .expect("建 Turn")
}

fn append_message(
    conn: &Connection,
    id: &str,
    session_id: &str,
    turn_id: Option<&str>,
    now_ms: i64,
) -> sessions::MessageView {
    sessions::append_message(
        conn,
        now_ms,
        id,
        session_id,
        turn_id,
        "user",
        "{}",
        PersistenceKind::Final,
    )
    .expect("追加消息")
}

fn append_part(
    conn: &Connection,
    id: &str,
    message_id: &str,
    kind: PartKind,
    ordinal_seed: i64,
) -> sessions::MessagePartView {
    sessions::append_part(
        conn,
        ordinal_seed,
        id,
        message_id,
        kind,
        PartStatus::Complete,
        r#"{"text":"ok"}"#,
        None,
        None,
        None,
    )
    .expect("追加片段")
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect("计数")
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-sessions-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("临时目录");
    dir
}

#[test]
fn sessions_message_seq_is_atomic_unique_and_session_scoped() {
    let conn = setup();
    create_session(&conn, "session-a");
    create_session(&conn, "session-b");
    let turn = start_turn(&conn, "session-a", "turn-a", 1_100);

    let m1 = append_message(&conn, "message-a1", "session-a", Some(&turn.id), 1_101);
    let m2 = append_message(&conn, "message-a2", "session-a", Some(&turn.id), 1_102);
    let m3 = append_message(&conn, "message-a3", "session-a", Some(&turn.id), 1_103);

    assert_eq!((m1.seq, m2.seq, m3.seq), (1, 2, 3));
    let next_a: i64 = conn
        .query_row(
            "SELECT next_message_seq FROM ai_sessions WHERE id='session-a'",
            [],
            |row| row.get(0),
        )
        .expect("读取下一消息序号");
    assert_eq!(next_a, 4);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_messages WHERE session_id='session-a'",
        ),
        3
    );

    let b1 = append_message(&conn, "message-b1", "session-b", None, 1_104);
    let b2 = append_message(&conn, "message-b2", "session-b", None, 1_105);
    assert_eq!((b1.seq, b2.seq), (1, 2));
    assert_eq!(
        sessions::load_session(&conn, "session-b")
            .unwrap()
            .next_message_seq,
        3
    );
}

#[test]
fn sessions_part_ordinal_is_message_scoped_and_dense() {
    let conn = setup();
    create_session(&conn, "session-a");
    let message_a = append_message(&conn, "message-a", "session-a", None, 1_100);
    let message_b = append_message(&conn, "message-b", "session-a", None, 1_101);

    let p0 = append_part(&conn, "part-a0", &message_a.id, PartKind::Text, 1_102);
    let p1 = append_part(&conn, "part-a1", &message_a.id, PartKind::Plan, 1_103);
    let p2 = append_part(&conn, "part-a2", &message_a.id, PartKind::Notice, 1_104);
    let q0 = append_part(&conn, "part-b0", &message_b.id, PartKind::Text, 1_105);

    assert_eq!((p0.ordinal, p1.ordinal, p2.ordinal), (0, 1, 2));
    assert_eq!(q0.ordinal, 0, "另一条消息必须从 ordinal=0 重新开始");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_message_parts WHERE message_id='message-a'",
        ),
        3
    );
}

#[test]
fn sessions_one_active_turn_and_turn_ordinal_are_enforced() {
    let conn = setup();
    create_session(&conn, "session-a");
    let first = start_turn(&conn, "session-a", "turn-1", 1_100);
    assert_eq!(first.ordinal, 1);

    let err = sessions::start_turn(
        &conn,
        1_101,
        "session-a",
        "turn-2",
        r#"{"kind":"sql"}"#,
        r#"{"model":"test"}"#,
    )
    .expect_err("未终态 Turn 存在时不能再启动 Turn");
    assert!(
        err.to_string().contains("已有进行中的 Turn"),
        "错误应说明单活跃 Turn 约束: {err}"
    );
    assert_eq!(sessions::list_turns(&conn, "session-a").unwrap().len(), 1);

    let revision = sessions::update_turn_status(
        &conn,
        1_200,
        "turn-1",
        TurnStatus::Completed,
        None,
        first.revision,
    )
    .expect("完成第一个 Turn");
    assert_eq!(revision, 2);

    let second = start_turn(&conn, "session-a", "turn-2", 1_201);
    assert_eq!(second.ordinal, 2);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_turns WHERE session_id='session-a'",
        ),
        2
    );
}

#[test]
fn sessions_message_turn_ownership_mismatch_creates_no_message_or_seq_gap() {
    let conn = setup();
    create_session(&conn, "session-a");
    create_session(&conn, "session-b");
    let foreign_turn = start_turn(&conn, "session-b", "turn-b", 1_100);
    let before_messages = count(&conn, "SELECT COUNT(*) FROM ai_messages");
    let before_next: i64 = conn
        .query_row(
            "SELECT next_message_seq FROM ai_sessions WHERE id='session-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let err = sessions::append_message(
        &conn,
        1_101,
        "message-wrong-turn",
        "session-a",
        Some(&foreign_turn.id),
        "user",
        "{}",
        PersistenceKind::Final,
    )
    .expect_err("跨会话 Turn 必须被拒绝");
    assert!(
        err.to_string().contains("Turn 不属于该会话"),
        "错误应说明 Turn 归属: {err}"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM ai_messages"),
        before_messages
    );
    let after_next: i64 = conn
        .query_row(
            "SELECT next_message_seq FROM ai_sessions WHERE id='session-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_next, before_next, "失败的 append 不应消耗消息序号");
}

#[test]
fn sessions_message_append_to_missing_session_creates_no_row() {
    let conn = setup();
    let before = count(&conn, "SELECT COUNT(*) FROM ai_messages");
    let err = sessions::append_message(
        &conn,
        1_100,
        "message-missing-session",
        "missing-session",
        None,
        "user",
        "{}",
        PersistenceKind::Final,
    )
    .expect_err("不存在的会话必须被拒绝");
    assert!(
        err.to_string().contains("会话不存在"),
        "错误应说明会话不存在: {err}"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM ai_messages"), before);
}

#[test]
fn sessions_turn_status_cas_and_failed_finish_metadata_are_persisted() {
    let conn = setup();
    create_session(&conn, "session-a");
    let turn = start_turn(&conn, "session-a", "turn-1", 1_100);

    let stale = sessions::update_turn_status(
        &conn,
        1_200,
        "turn-1",
        TurnStatus::Failed,
        Some("stale"),
        turn.revision + 1,
    )
    .expect_err("过期 revision 必须被拒绝");
    assert!(
        stale.to_string().contains("重新加载"),
        "CAS 错误应提示重新加载: {stale}"
    );
    let unchanged = sessions::load_turn(&conn, "turn-1").unwrap();
    assert_eq!(unchanged.status, TurnStatus::Queued);
    assert_eq!(unchanged.revision, 1);
    assert_eq!(unchanged.error_code, None);
    assert_eq!(unchanged.finished_at_ms, None);

    let revision = sessions::update_turn_status(
        &conn,
        2_000,
        "turn-1",
        TurnStatus::Failed,
        Some("E_TEST"),
        turn.revision,
    )
    .expect("更新为失败态");
    assert_eq!(revision, 2);
    let failed = sessions::load_turn(&conn, "turn-1").unwrap();
    assert_eq!(failed.status, TurnStatus::Failed);
    assert_eq!(failed.revision, 2);
    assert_eq!(failed.error_code.as_deref(), Some("E_TEST"));
    assert_eq!(failed.finished_at_ms, Some(2_000));

    let direct: (String, Option<String>, Option<i64>, i64) = conn
        .query_row(
            "SELECT status, error_code, finished_at_ms, revision FROM ai_turns WHERE id='turn-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        direct,
        ("failed".into(), Some("E_TEST".into()), Some(2_000), 2)
    );
}

#[test]
fn sessions_run_event_seq_and_late_event_rejection_do_not_consume_seq() {
    let conn = setup();
    create_session(&conn, "session-a");
    let turn = start_turn(&conn, "session-a", "turn-1", 1_100);

    assert_eq!(
        sessions::append_run_event(&conn, 1_101, "turn-1", "iteration_started", r#"{"n":1}"#)
            .unwrap(),
        1
    );
    assert_eq!(
        sessions::append_run_event(&conn, 1_102, "turn-1", "text_delta", r#"{"n":2}"#).unwrap(),
        2
    );
    assert_eq!(
        sessions::load_turn(&conn, "turn-1").unwrap().next_event_seq,
        3
    );
    let before_events = count(
        &conn,
        "SELECT COUNT(*) FROM ai_run_events WHERE turn_id='turn-1'",
    );
    assert_eq!(before_events, 2);

    sessions::update_turn_status(
        &conn,
        1_200,
        "turn-1",
        TurnStatus::Completed,
        None,
        turn.revision,
    )
    .unwrap();
    let err = sessions::append_run_event(&conn, 1_201, "turn-1", "late", "{}")
        .expect_err("终态 Turn 不能接受迟到事件");
    assert!(
        err.to_string().contains("迟到事件"),
        "错误应说明迟到事件: {err}"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_run_events WHERE turn_id='turn-1'"
        ),
        before_events
    );
    assert_eq!(
        sessions::load_turn(&conn, "turn-1").unwrap().next_event_seq,
        3
    );
}

#[test]
fn sessions_streaming_part_upsert_merges_without_extra_rows() {
    let conn = setup();
    create_session(&conn, "session-a");
    let message = append_message(&conn, "message-a", "session-a", None, 1_100);

    let mut last = None;
    for (index, text) in ["a", "ab", "abc", "abcd", "abcde"].iter().enumerate() {
        let payload = format!(r#"{{"text":"{text}"}}"#);
        last = Some(
            sessions::upsert_streaming_part(
                &conn,
                1_101 + index as i64,
                "part-stream",
                &message.id,
                PartKind::Text,
                &payload,
            )
            .unwrap(),
        );
    }
    let merged = last.expect("至少有一次 upsert");
    assert_eq!(merged.payload_json, r#"{"text":"abcde"}"#);
    assert_eq!(merged.revision, 5, "首次插入 revision=1，后续四次各递增 1");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_message_parts WHERE message_id='message-a'",
        ),
        1
    );

    let second_kind = sessions::upsert_streaming_part(
        &conn,
        1_110,
        "part-plan",
        &message.id,
        PartKind::Plan,
        r#"{"steps":[]}"#,
    )
    .unwrap();
    assert_eq!(second_kind.ordinal, 1, "换 kind 才新增片段");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_message_parts WHERE message_id='message-a'",
        ),
        2
    );
}

#[test]
fn sessions_private_reasoning_fields_are_rejected_without_rows() {
    let conn = setup();
    create_session(&conn, "session-a");
    let message = append_message(&conn, "message-a", "session-a", None, 1_100);
    let before = count(
        &conn,
        "SELECT COUNT(*) FROM ai_message_parts WHERE message_id='message-a'",
    );

    for (id, payload) in [
        ("part-private", r#"{"private_reasoning":"secret"}"#),
        ("part-hidden", r#"{"hidden_cot":"secret"}"#),
    ] {
        let err = sessions::append_part(
            &conn,
            1_101,
            id,
            &message.id,
            PartKind::ReasoningSummary,
            PartStatus::Complete,
            payload,
            None,
            None,
            None,
        )
        .expect_err("私有推理字段必须被拒绝");
        assert!(
            err.to_string().contains("私有推理字段"),
            "错误应说明拒绝原因: {err}"
        );
    }
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_message_parts WHERE message_id='message-a'",
        ),
        before
    );

    let public = sessions::append_part(
        &conn,
        1_102,
        "part-public",
        &message.id,
        PartKind::ReasoningSummary,
        PartStatus::Complete,
        r#"{"summary":"公开摘要"}"#,
        None,
        None,
        None,
    )
    .expect("公开摘要可写入");
    assert_eq!(public.kind, PartKind::ReasoningSummary);
}

#[test]
fn sessions_ui_state_roundtrip_and_cas_validation() {
    let conn = setup();
    create_session(&conn, "session-a");
    let turn = start_turn(&conn, "session-a", "turn-1", 1_100);
    let message = append_message(&conn, "message-a", "session-a", Some(&turn.id), 1_101);

    let default = sessions::load_ui_state(&conn, "session-a").unwrap();
    assert_eq!(default.revision, 0);
    assert_eq!(default.selected_turn_id, None);
    assert_eq!(default.expanded_part_ids_json, "[]");
    assert_eq!(default.ui_options_json, "{}");
    assert_eq!(default.draft_json, None);

    let edit = SessionUiStateEdit {
        session_id: "session-a".into(),
        selected_turn_id: Some(turn.id.clone()),
        expanded_part_ids_json: r#"["part-a"]"#.into(),
        scroll_anchor_message_id: Some(message.id.clone()),
        ui_options_json: r#"{"dense":true}"#.into(),
        draft_json: Some(r#"{"text":"draft"}"#.into()),
    };
    assert_eq!(sessions::save_ui_state(&conn, 2_000, &edit, 0).unwrap(), 1);
    let loaded = sessions::load_ui_state(&conn, "session-a").unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(loaded.selected_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(loaded.expanded_part_ids_json, r#"["part-a"]"#);
    assert_eq!(
        loaded.scroll_anchor_message_id.as_deref(),
        Some("message-a")
    );
    assert_eq!(loaded.ui_options_json, r#"{"dense":true}"#);
    assert_eq!(loaded.draft_json.as_deref(), Some(r#"{"text":"draft"}"#));
    assert_eq!(loaded.updated_at_ms, 2_000);

    let direct: (i64, String, Option<String>) = conn
        .query_row(
            "SELECT revision, ui_options_json, draft_json FROM ai_session_ui_state WHERE session_id='session-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        direct,
        (
            1,
            r#"{"dense":true}"#.into(),
            Some(r#"{"text":"draft"}"#.into())
        )
    );

    let mut stale = edit.clone();
    stale.draft_json = Some(r#"{"text":"stale"}"#.into());
    assert!(
        sessions::save_ui_state(&conn, 2_100, &stale, 0)
            .unwrap_err()
            .to_string()
            .contains("重新加载")
    );
    let after_stale = sessions::load_ui_state(&conn, "session-a").unwrap();
    assert_eq!(
        after_stale.draft_json.as_deref(),
        Some(r#"{"text":"draft"}"#)
    );

    let mut invalid_expanded = edit.clone();
    invalid_expanded.expanded_part_ids_json = "not-json".into();
    assert!(
        sessions::save_ui_state(&conn, 2_200, &invalid_expanded, 1)
            .unwrap_err()
            .to_string()
            .contains("展开片段列表")
    );
    let mut invalid_draft = edit.clone();
    invalid_draft.draft_json = Some("not-json".into());
    assert!(
        sessions::save_ui_state(&conn, 2_201, &invalid_draft, 1)
            .unwrap_err()
            .to_string()
            .contains("草稿")
    );
    let after_invalid = sessions::load_ui_state(&conn, "session-a").unwrap();
    assert_eq!(after_invalid.revision, 1);
    assert_eq!(
        after_invalid.draft_json.as_deref(),
        Some(r#"{"text":"draft"}"#)
    );
}

#[test]
fn sessions_panel_preferences_include_pending_draft_and_reject_invalid_or_stale() {
    let conn = setup();
    create_session(&conn, "session-a");

    let default = settings::load_panel_preferences(&conn).unwrap();
    assert_eq!(default.revision, 0);
    assert_eq!(default.pending_draft_json, None);

    let mut prefs = default.clone();
    prefs.preferred_mode = PanelMode::Embedded;
    prefs.drawer_width_dp = 510.0;
    prefs.embedded_width_dp = 390.0;
    prefs.last_session_id = Some("session-a".into());
    prefs.pending_draft_json = Some(r#"{"text":"尚未发送"}"#.into());
    let saved = settings::save_panel_preferences(&conn, 2_000, &prefs, 0).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(
        saved.pending_draft_json.as_deref(),
        Some(r#"{"text":"尚未发送"}"#)
    );

    let loaded = settings::load_panel_preferences(&conn).unwrap();
    assert_eq!(loaded, prefs_for_revision(&prefs));
    let direct = conn
        .query_row(
            "SELECT preferred_mode, drawer_width_dp, embedded_width_dp, last_session_id, pending_draft_json, revision
               FROM ai_panel_preferences WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        direct,
        (
            "embedded".into(),
            510.0,
            390.0,
            Some("session-a".into()),
            Some(r#"{"text":"尚未发送"}"#.into()),
            1,
        )
    );

    assert!(
        settings::save_panel_preferences(
            &conn,
            2_100,
            &PanelPreferences {
                pending_draft_json: Some("not-json".into()),
                ..loaded.clone()
            },
            1,
        )
        .unwrap_err()
        .to_string()
        .contains("草稿")
    );
    assert_eq!(settings::load_panel_preferences(&conn).unwrap().revision, 1);
    assert_eq!(
        settings::load_panel_preferences(&conn)
            .unwrap()
            .pending_draft_json
            .as_deref(),
        Some(r#"{"text":"尚未发送"}"#)
    );

    // 合法 JSON 但 CAS 过期仍必须被拒绝，且不能覆盖已经保存的草稿。
    let mut stale = loaded.clone();
    stale.pending_draft_json = Some(r#"{"text":"旧窗口草稿"}"#.into());
    assert!(
        settings::save_panel_preferences(&conn, 2_200, &stale, 0)
            .unwrap_err()
            .to_string()
            .contains("重试")
    );
    assert_eq!(
        settings::load_panel_preferences(&conn)
            .unwrap()
            .pending_draft_json
            .as_deref(),
        Some(r#"{"text":"尚未发送"}"#)
    );
}

fn prefs_for_revision(prefs: &PanelPreferences) -> PanelPreferences {
    let mut value = prefs.clone();
    value.revision = 1;
    value
}

#[test]
fn sessions_file_db_reopen_recovers_public_snapshot_and_ui_state() {
    let dir = unique_temp_dir("recovery");
    let db_path = dir.join("fluxdb.sqlite");

    {
        let conn = Connection::open(&db_path).expect("文件库");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("外键");
        ensure_schema(&conn).expect("建表");
        agents::create_agent(
            &conn,
            1_000,
            &agents::AgentEdit {
                compression_trigger_percent: 80,
                id: "agent-recovery".into(),
                display_name: "恢复 Agent".into(),
                description: String::new(),
                enabled: true,
                default_model_profile_id: None,
                system_prompt: "恢复测试".into(),
                budget_policy_id: None,
                allowed_tools: Vec::new(),
            },
        )
        .expect("建 Agent");
        sessions::create_session(
            &conn,
            1_000,
            "session-recovery",
            "agent-recovery",
            "恢复会话",
        )
        .expect("建会话");
        let turn = sessions::start_turn(
            &conn,
            1_001,
            "session-recovery",
            "turn-recovery",
            r#"{"kind":"sql"}"#,
            r#"{"model":"test"}"#,
        )
        .expect("建 Turn");
        let message = sessions::append_message(
            &conn,
            1_002,
            "message-recovery",
            "session-recovery",
            Some(&turn.id),
            "assistant",
            "{}",
            PersistenceKind::Checkpoint,
        )
        .expect("追加消息");
        sessions::append_part(
            &conn,
            1_003,
            "part-text",
            &message.id,
            PartKind::Text,
            PartStatus::Complete,
            r#"{"text":"恢复文本"}"#,
            None,
            None,
            None,
        )
        .expect("追加文本片段");
        sessions::append_part(
            &conn,
            1_004,
            "part-summary",
            &message.id,
            PartKind::ReasoningSummary,
            PartStatus::Complete,
            r#"{"summary":"公开摘要"}"#,
            None,
            None,
            None,
        )
        .expect("追加公开摘要片段");
        assert_eq!(
            sessions::append_run_event(
                &conn,
                1_005,
                "turn-recovery",
                "iteration_started",
                r#"{"iteration":1}"#,
            )
            .unwrap(),
            1
        );
        let edit = SessionUiStateEdit {
            session_id: "session-recovery".into(),
            selected_turn_id: Some("turn-recovery".into()),
            expanded_part_ids_json: r#"["part-summary"]"#.into(),
            scroll_anchor_message_id: Some("message-recovery".into()),
            ui_options_json: r#"{"dense":true}"#.into(),
            draft_json: Some(r#"{"text":"恢复草稿"}"#.into()),
        };
        sessions::save_ui_state(&conn, 1_006, &edit, 0).expect("保存 UI 状态");
        sessions::update_turn_status(
            &conn,
            1_007,
            "turn-recovery",
            TurnStatus::Completed,
            None,
            1,
        )
        .expect("完成 Turn");
    }

    let reopened = Connection::open(&db_path).expect("重新打开文件库");
    reopened
        .pragma_update(None, "foreign_keys", "ON")
        .expect("重开外键");
    assert_eq!(
        reopened
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );

    let snapshot = sessions::session_snapshot(&reopened, "session-recovery").expect("恢复快照");
    assert_eq!(snapshot.session.id, "session-recovery");
    assert_eq!(snapshot.session.next_message_seq, 2);
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.turns[0].id, "turn-recovery");
    assert_eq!(snapshot.turns[0].status, TurnStatus::Completed);
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].seq, 1);
    assert_eq!(snapshot.parts.len(), 2);
    assert_eq!(snapshot.parts[0].kind, PartKind::Text);
    let summary = snapshot
        .parts
        .iter()
        .find(|part| part.kind == PartKind::ReasoningSummary)
        .expect("公开摘要片段");
    assert_eq!(summary.payload_json, r#"{"summary":"公开摘要"}"#);
    for part in &snapshot.parts {
        let value: serde_json::Value = serde_json::from_str(&part.payload_json).unwrap();
        for key in [
            "private_reasoning",
            "hidden_cot",
            "chain_of_thought",
            "internal_monologue",
        ] {
            assert!(value.get(key).is_none(), "恢复片段不应含私有推理字段 {key}");
        }
    }
    let ui_state = snapshot.ui_state.expect("恢复 UI 状态");
    assert_eq!(ui_state.selected_turn_id.as_deref(), Some("turn-recovery"));
    assert_eq!(ui_state.expanded_part_ids_json, r#"["part-summary"]"#);
    assert_eq!(
        ui_state.scroll_anchor_message_id.as_deref(),
        Some("message-recovery")
    );
    assert_eq!(ui_state.ui_options_json, r#"{"dense":true}"#);
    assert_eq!(
        ui_state.draft_json.as_deref(),
        Some(r#"{"text":"恢复草稿"}"#)
    );

    // 运行事件既按独立事件表可列，也包含在 SessionSnapshot 里（关闭重开恢复用）。
    let events = sessions::list_run_events(&reopened, "turn-recovery").expect("恢复运行事件");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].kind, "iteration_started");
    assert_eq!(events[0].summary_json, r#"{"iteration":1}"#);
    assert_eq!(snapshot.run_events.len(), 1, "快照必须包含运行事件");
    assert_eq!(snapshot.run_events[0].turn_id, "turn-recovery");
    assert_eq!(snapshot.run_events[0].summary_json, r#"{"iteration":1}"#);

    let message_count: i64 = reopened
        .query_row(
            "SELECT COUNT(*) FROM ai_messages WHERE session_id='session-recovery'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let part_count: i64 = reopened
        .query_row(
            "SELECT COUNT(*) FROM ai_message_parts
               JOIN ai_messages ON ai_messages.id=ai_message_parts.message_id
              WHERE ai_messages.session_id='session-recovery'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((message_count, part_count), (1, 2));

    drop(reopened);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn session_overview_counts_and_rename_archive_restore_keep_facts() {
    let conn = setup();
    let session = create_session(&conn, "session-overview");
    let turn = start_turn(&conn, "session-overview", "turn-overview", 2_000);
    let _ = append_message(
        &conn,
        "msg-overview",
        "session-overview",
        Some("turn-overview"),
        2_100,
    );
    let _ = append_part(
        &conn,
        "part-overview",
        "msg-overview",
        PartKind::Text,
        2_200,
    );

    // 概览行带出会话身份 + 派生统计（消息/轮次/最后状态/最后目标）。
    let overviews = sessions::list_session_overviews(&conn, 10).expect("会话概览");
    assert_eq!(overviews.len(), 1);
    let overview = &overviews[0];
    assert_eq!(overview.id, session.id);
    assert_eq!(overview.message_count, 1);
    assert_eq!(overview.turn_count, 1);
    assert_eq!(overview.last_turn_status, Some(TurnStatus::Queued));
    assert_eq!(
        overview.last_target_json.as_deref(),
        Some(r#"{"kind":"sql"}"#)
    );
    assert_eq!(overview.revision, 1);

    // 无 Turn 的新会话：状态与目标为空，而不是伪装成已完成。
    let empty = create_session(&conn, "session-empty");
    let overviews = sessions::list_session_overviews(&conn, 10).expect("会话概览");
    let empty_overview = overviews
        .iter()
        .find(|row| row.id == empty.id)
        .expect("新会话出现在概览里");
    assert_eq!(empty_overview.turn_count, 0);
    assert_eq!(empty_overview.last_turn_status, None);
    assert_eq!(empty_overview.last_target_json, None);

    // 重命名只改标题 + revision，不动消息与 Turn。
    let renamed = sessions::rename_session(&conn, 3_000, "session-overview", "新标题", 1)
        .expect("重命名会话");
    assert_eq!(renamed, 2);
    let snapshot = sessions::session_snapshot(&conn, "session-overview").expect("会话快照");
    assert_eq!(snapshot.session.title, "新标题");
    assert_eq!(snapshot.session.revision, 2);
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.turns[0].id, turn.id);

    // 归档后仍是同一份事实，只是状态变化；恢复回到活跃。
    let archived =
        sessions::archive_session(&conn, 3_100, "session-overview", renamed).expect("归档会话");
    let snapshot = sessions::session_snapshot(&conn, "session-overview").expect("归档后快照");
    assert_eq!(snapshot.session.status, sessions::SessionStatus::Archived);
    assert_eq!(snapshot.messages.len(), 1, "归档不删除消息事实");
    let restored =
        sessions::restore_session(&conn, 3_200, "session-overview", archived).expect("恢复会话");
    let snapshot = sessions::session_snapshot(&conn, "session-overview").expect("恢复后快照");
    assert_eq!(snapshot.session.status, sessions::SessionStatus::Active);
    assert_eq!(snapshot.session.revision, restored);

    // 归档 / 恢复对象的 CAS 必须拒绝过期 revision 与错误前置状态。
    assert!(sessions::archive_session(&conn, 3_300, "session-overview", 1).is_err());
    assert!(sessions::restore_session(&conn, 3_400, "session-overview", restored).is_err());
}
