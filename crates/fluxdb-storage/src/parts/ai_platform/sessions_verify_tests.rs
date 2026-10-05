//! T015 的**独立对抗性复测**：不复用 `sessions_tests.rs` 的用例，专挑边界与并发。
//!
//! 每个用例自建隔离 fixture（内存库或临时文件库），不读真实用户数据库。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::ai_platform::{agents, ensure_schema};

use super::sessions::{
    self, PartKind, PartStatus, PersistenceKind, SessionUiStateEdit, TurnStatus,
};

fn setup_memory() -> Connection {
    let conn = Connection::open_in_memory().expect("内存库");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("外键");
    ensure_schema(&conn).expect("建表");
    agents::create_agent(
        &conn,
        1_000,
        &agents::AgentEdit {
            id: "agent-verify".into(),
            display_name: "复测 Agent".into(),
            description: String::new(),
            enabled: true,
            default_model_profile_id: None,
            system_prompt: "verify".into(),
            budget_policy_id: None,
            allowed_tools: Vec::new(),
        },
    )
    .expect("建 Agent");
    conn
}

fn unique_file_db(label: &str) -> (PathBuf, PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-sessions-verify-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("临时目录");
    let db = dir.join("sessions.sqlite");
    (dir, db)
}

fn create_session(conn: &Connection, id: &str) -> sessions::SessionView {
    sessions::create_session(conn, 1_100, id, "agent-verify", "复测会话").expect("建会话")
}

fn start_turn(conn: &Connection, session_id: &str, id: &str, now_ms: i64) -> sessions::TurnView {
    sessions::start_turn(
        conn,
        now_ms,
        session_id,
        id,
        r#"{"kind":"verify"}"#,
        r#"{"model":"verify"}"#,
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

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect("计数")
}

#[test]
fn verify_partial_unique_index_rejects_every_non_terminal_status() {
    let conn = setup_memory();
    create_session(&conn, "session-active-states");
    let first = start_turn(&conn, "session-active-states", "turn-a", 1_200);

    let revision = sessions::update_turn_status(
        &conn,
        1_201,
        &first.id,
        TurnStatus::WaitingApproval,
        None,
        first.revision,
    )
    .expect("进入等待审批");
    let waiting_error = sessions::start_turn(
        &conn,
        1_202,
        "session-active-states",
        "turn-b-while-waiting",
        r#"{"kind":"verify"}"#,
        r#"{"model":"verify"}"#,
    )
    .expect_err("waiting_approval 仍属于活跃 Turn");
    let waiting_text = waiting_error.to_string();
    assert!(
        waiting_text.contains("已有进行中的 Turn"),
        "错误必须可读: {waiting_text}"
    );
    assert!(
        !waiting_text.contains("UNIQUE constraint failed"),
        "不能泄漏 SQLite 原始唯一约束文本: {waiting_text}"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_turns WHERE session_id='session-active-states'",
        ),
        1
    );

    let revision = sessions::update_turn_status(
        &conn,
        1_203,
        &first.id,
        TurnStatus::CancelRequested,
        None,
        revision,
    )
    .expect("进入取消请求");
    let cancel_requested_error = sessions::start_turn(
        &conn,
        1_204,
        "session-active-states",
        "turn-b-while-cancel-requested",
        r#"{"kind":"verify"}"#,
        r#"{"model":"verify"}"#,
    )
    .expect_err("cancel_requested 仍属于活跃 Turn");
    assert!(
        cancel_requested_error
            .to_string()
            .contains("已有进行中的 Turn"),
        "取消请求态也必须给出可读错误: {cancel_requested_error}"
    );

    sessions::update_turn_status(
        &conn,
        1_205,
        &first.id,
        TurnStatus::Cancelled,
        None,
        revision,
    )
    .expect("进入终态 cancelled");
    let second = start_turn(
        &conn,
        "session-active-states",
        "turn-b-after-cancelled",
        1_206,
    );
    assert_eq!(second.ordinal, 2);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_turns WHERE session_id='session-active-states'",
        ),
        2
    );
}

#[test]
fn verify_failed_append_rolls_back_seq_and_session_timestamp() {
    let conn = setup_memory();
    create_session(&conn, "session-owner");
    create_session(&conn, "session-foreign");
    let foreign_turn = start_turn(&conn, "session-foreign", "turn-foreign", 1_300);

    let before_messages = count(&conn, "SELECT COUNT(*) FROM ai_messages");
    let before: (i64, i64) = conn
        .query_row(
            "SELECT next_message_seq, updated_at_ms FROM ai_sessions WHERE id='session-owner'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("保存会话事前值");

    let error = sessions::append_message(
        &conn,
        9_999,
        "message-foreign-turn",
        "session-owner",
        Some(&foreign_turn.id),
        "user",
        "{}",
        PersistenceKind::Final,
    )
    .expect_err("跨会话 Turn 必须失败");
    assert!(
        error.to_string().contains("Turn 不属于该会话"),
        "应映射为归属错误: {error}"
    );

    let after_messages = count(&conn, "SELECT COUNT(*) FROM ai_messages");
    let after: (i64, i64) = conn
        .query_row(
            "SELECT next_message_seq, updated_at_ms FROM ai_sessions WHERE id='session-owner'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("保存会话事后值");
    assert_eq!(after_messages, before_messages, "失败 append 不得新增消息");
    assert_eq!(after, before, "失败 append 必须完整回滚序号与会话更新时间");
}

#[test]
fn verify_two_file_connections_allocate_contiguous_unique_message_seq() {
    let (dir, db_path) = unique_file_db("two-connections");
    let first_conn = Connection::open(&db_path).expect("打开第一连接");
    first_conn
        .pragma_update(None, "foreign_keys", "ON")
        .expect("第一连接外键");
    first_conn
        .busy_timeout(Duration::from_millis(2_000))
        .expect("第一连接 busy_timeout");
    ensure_schema(&first_conn).expect("建表");
    agents::create_agent(
        &first_conn,
        1_000,
        &agents::AgentEdit {
            id: "agent-verify".into(),
            display_name: "复测 Agent".into(),
            description: String::new(),
            enabled: true,
            default_model_profile_id: None,
            system_prompt: "verify".into(),
            budget_policy_id: None,
            allowed_tools: Vec::new(),
        },
    )
    .expect("建 Agent");
    create_session(&first_conn, "session-concurrent");

    let second_conn = Connection::open(&db_path).expect("打开第二连接");
    second_conn
        .pragma_update(None, "foreign_keys", "ON")
        .expect("第二连接外键");
    second_conn
        .busy_timeout(Duration::from_millis(2_000))
        .expect("第二连接 busy_timeout");

    let mut observed = Vec::new();
    for index in 0..20 {
        let conn = if index % 2 == 0 {
            &first_conn
        } else {
            &second_conn
        };
        let message = append_message(
            conn,
            &format!("message-concurrent-{index:02}"),
            "session-concurrent",
            None,
            2_000 + index as i64,
        );
        observed.push(message.seq);
    }

    let mut sorted = observed.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (1_i64..=20).collect::<Vec<_>>(),
        "seq 应连续且无重复"
    );
    assert_eq!(
        count(
            &first_conn,
            "SELECT COUNT(DISTINCT seq) FROM ai_messages WHERE session_id='session-concurrent'",
        ),
        20,
        "数据库中不得有重复 seq"
    );
    let stored: Vec<i64> = {
        let mut stmt = first_conn
            .prepare(
                "SELECT seq FROM ai_messages WHERE session_id='session-concurrent' ORDER BY seq",
            )
            .expect("准备查询");
        stmt.query_map([], |row| row.get(0))
            .expect("查询 seq")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("收集 seq")
    };
    assert_eq!(stored, (1_i64..=20).collect::<Vec<_>>());
    for (index, expected_seq) in observed.iter().enumerate() {
        let stored_seq: i64 = first_conn
            .query_row(
                "SELECT seq FROM ai_messages WHERE id=?1",
                [format!("message-concurrent-{index:02}")],
                |row| row.get(0),
            )
            .expect("按消息 ID 读取 seq");
        assert_eq!(
            stored_seq, *expected_seq,
            "每条消息返回值必须与表内 seq 一致"
        );
    }
    assert_eq!(
        sessions::load_session(&second_conn, "session-concurrent")
            .expect("第二连接读取会话")
            .next_message_seq,
        21
    );

    drop(second_conn);
    drop(first_conn);
    fs::remove_dir_all(&dir).expect("清理临时目录");
}

#[test]
fn verify_completed_part_is_not_merged_by_later_stream_upsert() {
    let conn = setup_memory();
    create_session(&conn, "session-part-complete");
    let message = append_message(
        &conn,
        "message-part-complete",
        "session-part-complete",
        None,
        1_400,
    );

    let first = sessions::upsert_streaming_part(
        &conn,
        1_401,
        "part-stream-first",
        &message.id,
        PartKind::Text,
        r#"{"text":"half"}"#,
    )
    .expect("首次流式写入");
    sessions::update_part(
        &conn,
        1_402,
        &first.id,
        PartStatus::Complete,
        r#"{"text":"done"}"#,
        first.revision,
    )
    .expect("标记完成");

    let second = sessions::upsert_streaming_part(
        &conn,
        1_403,
        "part-stream-second",
        &message.id,
        PartKind::Text,
        r#"{"text":"next"}"#,
    )
    .expect("完成后应新建片段");

    let parts = sessions::list_parts(&conn, &message.id).expect("列出片段");
    assert_eq!(parts.len(), 2, "完成片段不得被后续流式更新合并");
    assert_eq!(parts[0].id, first.id);
    assert_eq!(parts[0].status, PartStatus::Complete);
    assert_eq!(parts[0].ordinal, 0);
    assert_eq!(parts[1].id, second.id);
    assert_eq!(parts[1].status, PartStatus::Streaming);
    assert_eq!(parts[1].ordinal, 1);
}

#[test]
fn verify_ui_state_expected_revision_zero_rejects_existing_row_without_mutation() {
    let conn = setup_memory();
    create_session(&conn, "session-ui-race");
    let initial = SessionUiStateEdit {
        session_id: "session-ui-race".into(),
        selected_turn_id: None,
        expanded_part_ids_json: r#"["part-1"]"#.into(),
        scroll_anchor_message_id: None,
        ui_options_json: r#"{"dense":true}"#.into(),
        draft_json: Some(r#"{"text":"first"}"#.into()),
    };
    assert_eq!(
        sessions::save_ui_state(&conn, 1_500, &initial, 0).expect("首次保存"),
        1
    );

    let stale = SessionUiStateEdit {
        selected_turn_id: None,
        expanded_part_ids_json: r#"["part-2"]"#.into(),
        ui_options_json: r#"{"dense":false}"#.into(),
        draft_json: Some(r#"{"text":"stale"}"#.into()),
        ..initial
    };
    let error = sessions::save_ui_state(&conn, 9_999, &stale, 0)
        .expect_err("已有行时 expected_revision=0 必须失败");
    assert!(
        error.to_string().contains("重新加载"),
        "CAS 错误应可读: {error}"
    );

    let loaded = sessions::load_ui_state(&conn, "session-ui-race").expect("读取 UI 状态");
    assert_eq!(loaded.expanded_part_ids_json, r#"["part-1"]"#);
    assert_eq!(loaded.ui_options_json, r#"{"dense":true}"#);
    assert_eq!(loaded.draft_json.as_deref(), Some(r#"{"text":"first"}"#));
    assert_eq!(loaded.revision, 1);
    assert_eq!(loaded.updated_at_ms, 1_500, "失败 CAS 不得修改时间戳");
}

#[test]
fn verify_snapshot_events_are_scoped_and_ordered_by_turn_then_seq() {
    let conn = setup_memory();
    create_session(&conn, "session-events-a");
    create_session(&conn, "session-events-b");

    let turn_a1 = start_turn(&conn, "session-events-a", "turn-a1", 1_600);
    assert_eq!(
        sessions::append_run_event(&conn, 1_601, &turn_a1.id, "first", r#"{"n":1}"#).unwrap(),
        1
    );
    assert_eq!(
        sessions::append_run_event(&conn, 1_602, &turn_a1.id, "second", r#"{"n":2}"#).unwrap(),
        2
    );
    sessions::update_turn_status(
        &conn,
        1_603,
        &turn_a1.id,
        TurnStatus::Completed,
        None,
        turn_a1.revision,
    )
    .expect("完成 turn-a1");

    let turn_a2 = start_turn(&conn, "session-events-a", "turn-a2", 1_604);
    assert_eq!(
        sessions::append_run_event(&conn, 1_605, &turn_a2.id, "third", r#"{"n":3}"#).unwrap(),
        1
    );

    let turn_b = start_turn(&conn, "session-events-b", "turn-b", 1_606);
    sessions::append_run_event(&conn, 1_607, &turn_b.id, "foreign", r#"{"n":4}"#).unwrap();

    let snapshot = sessions::session_snapshot(&conn, "session-events-a").expect("会话快照");
    let observed: Vec<(&str, i64, &str)> = snapshot
        .run_events
        .iter()
        .map(|event| (event.turn_id.as_str(), event.seq, event.kind.as_str()))
        .collect();
    assert_eq!(
        observed,
        vec![
            ("turn-a1", 1, "first"),
            ("turn-a1", 2, "second"),
            ("turn-a2", 1, "third"),
        ],
        "快照事件必须只含本会话，并按 turn/seq 排序"
    );
    assert!(
        snapshot
            .run_events
            .iter()
            .all(|event| event.turn_id != "turn-b"),
        "另一会话的事件不得混入"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_run_events WHERE turn_id='turn-b'"
        ),
        1
    );
}

#[test]
fn verify_outcome_unknown_is_terminal_for_late_events_and_sets_finished_at() {
    let conn = setup_memory();
    create_session(&conn, "session-outcome-unknown");
    let turn = start_turn(&conn, "session-outcome-unknown", "turn-unknown", 1_700);

    let revision = sessions::update_turn_status(
        &conn,
        1_701,
        &turn.id,
        TurnStatus::OutcomeUnknown,
        Some("E_OUTCOME_UNKNOWN"),
        turn.revision,
    )
    .expect("进入 outcome_unknown");
    let stored = sessions::load_turn(&conn, &turn.id).expect("读取 Turn");
    assert_eq!(stored.status, TurnStatus::OutcomeUnknown);
    assert_eq!(stored.finished_at_ms, Some(1_701));
    assert_eq!(stored.revision, revision);

    let error = sessions::append_run_event(&conn, 1_702, &turn.id, "late", "{}")
        .expect_err("outcome_unknown 后不得写迟到事件");
    assert!(
        error.to_string().contains("迟到事件"),
        "错误应说明迟到事件: {error}"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM ai_run_events WHERE turn_id='turn-unknown'",
        ),
        0,
        "失败事件不得落库"
    );
    assert_eq!(
        sessions::load_turn(&conn, &turn.id)
            .expect("再次读取 Turn")
            .next_event_seq,
        1,
        "失败事件不得消耗 seq"
    );
}
