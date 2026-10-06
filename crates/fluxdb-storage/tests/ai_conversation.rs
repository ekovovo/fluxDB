//! T028 存储验收：仅经公开 API 写入，使用独立文件 SQLite 验证事务与重开恢复。
use std::{fs, path::PathBuf};

use fluxdb_storage::ai_platform::{agents, conversation, ensure_schema, sessions, settings};
use rusqlite::Connection;
use sessions::{PartStatus, PersistenceKind, SessionUiStateEdit, TurnStatus};
use settings::{PanelMode, PanelPreferences};

const AGENT: &str = "t028-agent";
const TARGET: &str = r#"{"kind":"sql","tab_id":"t028-tab"}"#;
const SNAPSHOT: &str = r#"{"model":"offline-fixture","marker":"T028"}"#;

struct Fixture {
    dir: PathBuf,
    conn: Option<Connection>,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-t028-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&dir).expect("独立临时目录");
        let mut fixture = Self { dir, conn: None };
        fixture.reopen();
        agents::create_agent(
            fixture.db(),
            1_000,
            &agents::AgentEdit {
                id: AGENT.into(),
                display_name: "T028 存储验收 Agent".into(),
                description: String::new(),
                enabled: true,
                default_model_profile_id: None,
                system_prompt: "离线 fixture，不调用模型".into(),
                budget_policy_id: None,
                allowed_tools: Vec::new(),
            },
        )
        .expect("公开 API 创建 Agent");
        fixture
    }

    fn db(&self) -> &Connection {
        self.conn.as_ref().expect("打开的连接")
    }

    fn reopen(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.close().expect("真正关闭 SQLite 连接");
        }
        let conn = Connection::open(self.dir.join("conversation.sqlite")).expect("文件 SQLite");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("启用 FK");
        let enabled: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(enabled, "包括重开后的连接都启用 FK");
        ensure_schema(&conn).expect("生产 schema");
        self.conn = Some(conn);
    }

    fn send(
        &self,
        new: bool,
        session: &str,
        turn: &str,
        message: &str,
        text: &str,
    ) -> fluxdb_core::Result<()> {
        conversation::send(
            self.db(),
            2_000,
            new,
            session,
            AGENT,
            turn,
            message,
            text,
            TARGET,
            SNAPSHOT,
        )
    }

    fn save_ui_draft(&self, session: &str, turn: &str, message: &str) {
        sessions::save_ui_state(
            self.db(),
            2_100,
            &SessionUiStateEdit {
                session_id: session.into(),
                selected_turn_id: Some(turn.into()),
                expanded_part_ids_json: format!(r#"["{message}-text"]"#),
                scroll_anchor_message_id: Some(message.into()),
                ui_options_json: r#"{"show_summary":true}"#.into(),
                draft_json: Some(r#"{"text":"T028 未发送草稿","refs":["tab-A"]}"#.into()),
            },
            0,
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.conn.take());
        // 仅删除本 fixture 独占创建的临时目录，不触碰应用配置。
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Debug, PartialEq)]
struct StoredState {
    preferences: PanelPreferences,
    sessions: Vec<sessions::SessionView>,
    turns: Vec<sessions::TurnView>,
    messages: Vec<sessions::MessageView>,
    parts: Vec<sessions::MessagePartView>,
    ui_states: Vec<sessions::SessionUiStateView>,
    events: Vec<sessions::RunEventView>,
}

fn stored(conn: &Connection) -> StoredState {
    let all_sessions = sessions::list_sessions(conn, 1_000).unwrap();
    let mut state = StoredState {
        preferences: settings::load_panel_preferences(conn).unwrap(),
        sessions: all_sessions,
        turns: Vec::new(),
        messages: Vec::new(),
        parts: Vec::new(),
        ui_states: Vec::new(),
        events: Vec::new(),
    };
    for session in &state.sessions {
        let snapshot = sessions::session_snapshot(conn, &session.id).unwrap();
        state.turns.extend(snapshot.turns);
        state.messages.extend(snapshot.messages);
        state.parts.extend(snapshot.parts);
        state.events.extend(snapshot.run_events);
        if let Some(ui) = snapshot.ui_state {
            state.ui_states.push(ui);
        }
    }
    state
}

fn part_text(part: &sessions::MessagePartView) -> String {
    serde_json::from_str::<serde_json::Value>(&part.payload_json).unwrap()["text"]
        .as_str()
        .expect("公开 text payload")
        .to_owned()
}

#[test]
fn repeated_empty_load_does_not_create_history_or_preferences() {
    let mut f = Fixture::new();
    let before = stored(f.db());
    for _ in 0..3 {
        let loaded = conversation::load(f.db(), None).unwrap();
        assert!(loaded.sessions.is_empty());
        assert!(loaded.session.is_none());
        assert!(loaded.messages.is_empty() && loaded.parts.is_empty() && loaded.turns.is_empty());
        assert!(loaded.ui_state.is_none());
        assert_eq!(loaded.preferences, PanelPreferences::default());
    }
    f.reopen();
    assert!(conversation::load(f.db(), None).unwrap().session.is_none());
    assert_eq!(stored(f.db()), before);
}

#[test]
fn first_send_atomically_creates_session_turn_user_part_and_pointer() {
    let f = Fixture::new();
    let mut prefs = PanelPreferences::default();
    prefs.pending_draft_json = Some(r#"{"text":"T028 首次草稿"}"#.into());
    settings::save_panel_preferences(f.db(), 1_100, &prefs, 0).unwrap();
    f.send(true, "s-first", "t-first", "u-first", "T028 首次消息")
        .unwrap();

    let loaded = conversation::load(f.db(), None).unwrap();
    let session = loaded.session.unwrap();
    assert_eq!(session.id, "s-first");
    assert_eq!(session.agent_id, AGENT);
    assert_eq!(session.title, "T028 首次消息");
    assert_eq!(session.next_message_seq, 2);
    assert_eq!(loaded.sessions.len(), 1);
    assert_eq!(
        loaded.preferences.last_session_id.as_deref(),
        Some("s-first")
    );
    assert_eq!(loaded.preferences.pending_draft_json, None);
    assert_eq!(loaded.preferences.revision, 2);
    assert_eq!(loaded.turns.len(), 1);
    assert_eq!(loaded.turns[0].id, "t-first");
    assert_eq!(loaded.turns[0].status, TurnStatus::Queued);
    assert_eq!(loaded.turns[0].target_json, TARGET);
    assert_eq!(loaded.turns[0].runtime_snapshot_json, SNAPSHOT);
    assert_eq!(loaded.turns[0].ordinal, 1);
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages[0].id, "u-first");
    assert_eq!(loaded.messages[0].turn_id.as_deref(), Some("t-first"));
    assert_eq!(loaded.messages[0].role, "user");
    assert_eq!(loaded.messages[0].seq, 1);
    assert_eq!(loaded.messages[0].persistence_kind, PersistenceKind::Final);
    assert_eq!(loaded.parts.len(), 1);
    assert_eq!(loaded.parts[0].id, "u-first-text");
    assert_eq!(loaded.parts[0].status, PartStatus::Complete);
    assert_eq!(part_text(&loaded.parts[0]), "T028 首次消息");
}

#[test]
fn failed_first_send_rolls_back_after_session_turn_and_sequence_allocation() {
    let f = Fixture::new();
    f.send(true, "s-original", "t-original", "u-duplicate", "原消息")
        .unwrap();
    let mut prefs = settings::load_panel_preferences(f.db()).unwrap();
    prefs.last_session_id = None;
    prefs.pending_draft_json = Some(r#"{"text":"失败不能丢失的新草稿"}"#.into());
    settings::save_panel_preferences(f.db(), 2_100, &prefs, prefs.revision).unwrap();
    let before = stored(f.db());

    // 消息全局 ID 冲突发生在建 Session/Turn 并分配 seq 之后，必须整笔回滚。
    assert!(
        f.send(true, "s-rollback", "t-rollback", "u-duplicate", "重名消息")
            .is_err()
    );
    assert!(sessions::load_session(f.db(), "s-rollback").is_err());
    assert!(sessions::load_turn(f.db(), "t-rollback").is_err());
    assert_eq!(stored(f.db()), before);
}

#[test]
fn invalid_first_send_preserves_new_draft_and_empty_history() {
    let f = Fixture::new();
    let mut prefs = PanelPreferences::default();
    prefs.pending_draft_json = Some(r#"{"text":"保留草稿"}"#.into());
    settings::save_panel_preferences(f.db(), 1_100, &prefs, 0).unwrap();
    let before = stored(f.db());
    for text in ["   ".to_owned(), "x".repeat(64 * 1024 + 1)] {
        assert!(
            f.send(true, "s-invalid", "t-invalid", "u-invalid", &text)
                .is_err()
        );
        assert_eq!(stored(f.db()), before);
    }
    for (target, snapshot) in [("not-json", SNAPSHOT), (TARGET, "not-json")] {
        assert!(
            conversation::send(
                f.db(),
                2_000,
                true,
                "s-invalid",
                AGENT,
                "t-invalid",
                "u-invalid",
                "有效文本",
                target,
                snapshot,
            )
            .is_err()
        );
        assert_eq!(stored(f.db()), before);
    }
}

#[test]
fn every_active_turn_status_rejects_send_without_changing_rows_sequences_or_preferences() {
    for status in [
        TurnStatus::Queued,
        TurnStatus::Preparing,
        TurnStatus::Generating,
        TurnStatus::ExecutingTools,
        TurnStatus::WaitingApproval,
        TurnStatus::WaitingUser,
        TurnStatus::CancelRequested,
    ] {
        let f = Fixture::new();
        f.send(true, "s-active", "t-active", "u-active", "第一条")
            .unwrap();
        sessions::update_turn_status(f.db(), 2_050, "t-active", status, None, 1).unwrap();
        f.save_ui_draft("s-active", "t-active", "u-active");
        let mut prefs = settings::load_panel_preferences(f.db()).unwrap();
        prefs.preferred_mode = PanelMode::Embedded;
        prefs.pending_draft_json = Some(r#"{"text":"另一份草稿"}"#.into());
        settings::save_panel_preferences(f.db(), 2_200, &prefs, prefs.revision).unwrap();
        let before = stored(f.db());
        let error = f
            .send(false, "s-active", "t-rejected", "u-rejected", "再次发送")
            .expect_err("活跃 Turn 拒绝发送");
        assert!(error.to_string().contains("进行中"), "{status:?}: {error}");
        assert_eq!(stored(f.db()), before, "{status:?} 下拒绝必须无副作用");
        assert!(sessions::load_turn(f.db(), "t-rejected").is_err());
        assert!(sessions::load_message(f.db(), "u-rejected").is_err());
    }
}

#[test]
fn repeated_checkpoints_update_one_message_and_parts_not_token_rows() {
    let f = Fixture::new();
    f.send(true, "s-stream", "t-stream", "u-stream", "开始")
        .unwrap();
    for token in 1..=32 {
        conversation::checkpoint(
            f.db(),
            3_000 + token,
            "s-stream",
            "t-stream",
            "a-stream",
            &"字".repeat(token as usize),
            &format!("公开摘要 {token}"),
            None,
            None,
        )
        .unwrap();
        let loaded = conversation::load(f.db(), None).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.parts.len(), 3);
        assert_eq!(loaded.session.unwrap().next_message_seq, 3);
        let assistant = &loaded.messages[1];
        assert_eq!(assistant.id, "a-stream");
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.seq, 2);
        assert_eq!(assistant.persistence_kind, PersistenceKind::Checkpoint);
        let parts = sessions::list_parts(f.db(), "a-stream").unwrap();
        assert_eq!(parts[0].ordinal, 0);
        assert_eq!(parts[1].ordinal, 1);
        assert_eq!(parts[0].revision, token);
        assert_eq!(parts[1].revision, token);
        assert_eq!(part_text(&parts[0]), "字".repeat(token as usize));
        assert_eq!(part_text(&parts[1]), format!("公开摘要 {token}"));
        assert!(parts.iter().all(|p| p.status == PartStatus::Streaming));
    }
    assert!(
        sessions::list_run_events(f.db(), "t-stream")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn completed_cancelled_and_failed_finalize_and_reject_all_late_events() {
    for (terminal, part_status, error_code) in [
        (TurnStatus::Completed, PartStatus::Complete, None),
        (TurnStatus::Cancelled, PartStatus::Interrupted, None),
        (TurnStatus::Failed, PartStatus::Error, Some("fixture_error")),
    ] {
        let f = Fixture::new();
        f.send(true, "s-final", "t-final", "u-final", "请求")
            .unwrap();
        conversation::checkpoint(
            f.db(),
            3_000,
            "s-final",
            "t-final",
            "a-final",
            "部分输出",
            "部分摘要",
            None,
            None,
        )
        .unwrap();
        let prefs = settings::load_panel_preferences(f.db()).unwrap();
        conversation::checkpoint(
            f.db(),
            4_000,
            "s-final",
            "t-final",
            "a-final",
            "最终可恢复输出",
            "最终公开摘要",
            Some(terminal),
            error_code,
        )
        .unwrap();
        let turn = sessions::load_turn(f.db(), "t-final").unwrap();
        assert_eq!(turn.status, terminal);
        assert_eq!(turn.finished_at_ms, Some(4_000));
        assert_eq!(turn.error_code.as_deref(), error_code);
        let message = sessions::load_message(f.db(), "a-final").unwrap();
        assert_eq!(message.persistence_kind, PersistenceKind::Final);
        let parts = sessions::list_parts(f.db(), "a-final").unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.status == part_status));
        assert_eq!(part_text(&parts[0]), "最终可恢复输出");
        assert_eq!(settings::load_panel_preferences(f.db()).unwrap(), prefs);
        let before = stored(f.db());
        for late_message in ["a-final", "a-late"] {
            for late_terminal in [None, Some(TurnStatus::Completed)] {
                assert!(
                    conversation::checkpoint(
                        f.db(),
                        5_000,
                        "s-final",
                        "t-final",
                        late_message,
                        "迟到正文",
                        "迟到摘要",
                        late_terminal,
                        None
                    )
                    .is_err()
                );
                assert_eq!(stored(f.db()), before, "{terminal:?} 终态拒绝迟到事件");
            }
        }
    }
}

#[test]
fn terminal_without_new_content_finalizes_existing_parts_without_erasing_payload() {
    let mut mismatches = Vec::new();
    for (terminal, expected) in [
        (TurnStatus::Completed, PartStatus::Complete),
        (TurnStatus::Cancelled, PartStatus::Interrupted),
        (TurnStatus::Failed, PartStatus::Error),
    ] {
        let f = Fixture::new();
        f.send(true, "s-tail", "t-tail", "u-tail", "请求").unwrap();
        conversation::checkpoint(
            f.db(),
            3_000,
            "s-tail",
            "t-tail",
            "a-tail",
            "已流式输出",
            "已有公开摘要",
            None,
            None,
        )
        .unwrap();
        // 完成/停止回调可不带新内容，不能留下永远 streaming 的历史片段。
        conversation::checkpoint(
            f.db(),
            4_000,
            "s-tail",
            "t-tail",
            "a-tail",
            "",
            "",
            Some(terminal),
            None,
        )
        .unwrap();
        let parts = sessions::list_parts(f.db(), "a-tail").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(part_text(&parts[0]), "已流式输出");
        assert_eq!(part_text(&parts[1]), "已有公开摘要");
        assert_eq!(
            sessions::load_turn(f.db(), "t-tail").unwrap().status,
            terminal
        );
        if parts.iter().any(|p| p.status != expected) {
            mismatches.push((terminal, parts.iter().map(|p| p.status).collect::<Vec<_>>()));
        }
    }
    assert!(mismatches.is_empty(), "终态片段状态不匹配: {mismatches:?}");
}

#[test]
fn failed_cancelled_interrupted_and_unknown_load_without_replay() {
    for terminal in [
        TurnStatus::Failed,
        TurnStatus::Cancelled,
        TurnStatus::Interrupted,
        TurnStatus::OutcomeUnknown,
    ] {
        let mut f = Fixture::new();
        f.send(
            true,
            "s-no-replay",
            "t-no-replay",
            "u-no-replay",
            "只执行一次",
        )
        .unwrap();
        conversation::checkpoint(
            f.db(),
            3_000,
            "s-no-replay",
            "t-no-replay",
            "a-no-replay",
            "保留的部分输出",
            "保留的公开摘要",
            Some(terminal),
            Some("fixture_stop"),
        )
        .unwrap();
        let before = stored(f.db());
        f.reopen();
        for _ in 0..3 {
            let loaded = conversation::load(f.db(), None).unwrap();
            assert_eq!(loaded.turns.len(), 1);
            assert_eq!(loaded.turns[0].status, terminal);
            assert_eq!(loaded.messages.len(), 2);
            assert_eq!(
                stored(f.db()),
                before,
                "{terminal:?} 打开只读，不新建或恢复执行"
            );
        }
    }
}

#[test]
fn close_reopen_restores_mode_independent_widths_draft_expansion_and_messages() {
    let mut f = Fixture::new();
    f.send(true, "s-reopen", "t-reopen", "u-reopen", "T028 唯一消息")
        .unwrap();
    conversation::checkpoint(
        f.db(),
        3_000,
        "s-reopen",
        "t-reopen",
        "a-reopen",
        "T028 助手输出",
        "T028 公开摘要",
        Some(TurnStatus::Completed),
        None,
    )
    .unwrap();
    f.save_ui_draft("s-reopen", "t-reopen", "u-reopen");
    let mut prefs = settings::load_panel_preferences(f.db()).unwrap();
    prefs.preferred_mode = PanelMode::Embedded;
    prefs.drawer_width_dp = 580.0;
    prefs.embedded_width_dp = 390.0;
    let saved = settings::save_panel_preferences(f.db(), 4_000, &prefs, prefs.revision).unwrap();
    let before = stored(f.db());
    f.reopen();

    let loaded = conversation::load(f.db(), None).unwrap();
    assert_eq!(loaded.preferences, saved);
    assert_eq!(loaded.preferences.preferred_mode, PanelMode::Embedded);
    assert_eq!(loaded.preferences.drawer_width_dp, 580.0);
    assert_eq!(loaded.preferences.embedded_width_dp, 390.0);
    assert_eq!(loaded.session.unwrap().id, "s-reopen");
    assert_eq!(loaded.messages, before.messages);
    assert_eq!(loaded.parts, before.parts);
    assert_eq!(loaded.turns, before.turns);
    assert_eq!(loaded.ui_state, Some(before.ui_states[0].clone()));
    assert_eq!(stored(f.db()), before);
}

#[test]
fn new_draft_with_no_pointer_does_not_select_recent_history_after_reopen() {
    let mut f = Fixture::new();
    f.send(true, "s-old", "t-old", "u-old", "旧会话").unwrap();
    let mut prefs = settings::load_panel_preferences(f.db()).unwrap();
    prefs.last_session_id = None;
    prefs.pending_draft_json = Some(r#"{"text":"T028 新草稿","refs":["tab-B"]}"#.into());
    prefs.preferred_mode = PanelMode::Embedded;
    let saved = settings::save_panel_preferences(f.db(), 3_000, &prefs, prefs.revision).unwrap();
    let before = stored(f.db());
    f.reopen();
    let loaded = conversation::load(f.db(), None).unwrap();
    assert_eq!(loaded.preferences, saved);
    assert!(loaded.session.is_none() && loaded.messages.is_empty());
    assert_eq!(loaded.sessions.len(), 1, "历史仍保留但不自动选中");
    assert_eq!(stored(f.db()), before);
}

#[test]
fn archived_pointer_does_not_fallback_to_other_session() {
    let f = Fixture::new();
    f.send(true, "s-visible", "t-visible", "u-visible", "另一会话")
        .unwrap();
    f.send(true, "s-archived", "t-archived", "u-archived", "被归档会话")
        .unwrap();
    let session = sessions::load_session(f.db(), "s-archived").unwrap();
    sessions::archive_session(f.db(), 3_000, &session.id, session.revision).unwrap();
    let before = stored(f.db());
    let loaded = conversation::load(f.db(), None).unwrap();
    assert!(loaded.session.is_none());
    assert!(loaded.messages.is_empty());
    assert_eq!(
        loaded.preferences.last_session_id.as_deref(),
        Some("s-archived")
    );
    assert_eq!(stored(f.db()), before);
}

#[test]
fn existing_session_can_send_after_terminal_and_clears_draft_atomically() {
    let f = Fixture::new();
    f.send(true, "s-next", "t-one", "u-one", "第一条").unwrap();
    conversation::checkpoint(
        f.db(),
        3_000,
        "s-next",
        "t-one",
        "a-one",
        "第一轮结束",
        "",
        Some(TurnStatus::Completed),
        None,
    )
    .unwrap();
    f.save_ui_draft("s-next", "t-one", "u-one");
    let mut prefs = settings::load_panel_preferences(f.db()).unwrap();
    prefs.pending_draft_json = Some(r#"{"text":"待清草稿"}"#.into());
    settings::save_panel_preferences(f.db(), 3_100, &prefs, prefs.revision).unwrap();
    let before = stored(f.db());
    // 已有会话的后置 ID 冲突，同样不能吞草稿或消耗 seq。
    assert!(
        f.send(false, "s-next", "t-rollback", "u-one", "冲突")
            .is_err()
    );
    assert_eq!(stored(f.db()), before);
    f.send(false, "s-next", "t-two", "u-two", "第二条").unwrap();
    let loaded = conversation::load(f.db(), None).unwrap();
    assert_eq!(loaded.sessions.len(), 1);
    assert_eq!(loaded.turns.len(), 2);
    assert_eq!(loaded.turns[1].ordinal, 2);
    assert_eq!(loaded.messages.len(), 3);
    assert_eq!(loaded.messages[2].id, "u-two");
    assert_eq!(loaded.messages[2].seq, 3);
    assert_eq!(loaded.session.unwrap().next_message_seq, 4);
    assert_eq!(loaded.preferences.pending_draft_json, None);
    let ui = loaded.ui_state.unwrap();
    assert_eq!(ui.draft_json, None);
    assert_eq!(ui.revision, before.ui_states[0].revision + 1);
    assert_eq!(
        ui.expanded_part_ids_json,
        before.ui_states[0].expanded_part_ids_json
    );
}

#[test]
fn load_returns_latest_100_messages_in_seq_order_without_deleting_old_history() {
    let f = Fixture::new();
    for index in 0..55 {
        let turn = format!("t-page-{index:03}");
        let user = format!("u-page-{index:03}");
        let assistant = format!("a-page-{index:03}");
        f.send(index == 0, "s-page", &turn, &user, &format!("用户 {index}"))
            .unwrap();
        conversation::checkpoint(
            f.db(),
            3_000 + index,
            "s-page",
            &turn,
            &assistant,
            &format!("助手 {index}"),
            "",
            Some(TurnStatus::Completed),
            None,
        )
        .unwrap();
    }
    let before = stored(f.db());
    let loaded = conversation::load(f.db(), None).unwrap();
    assert_eq!(before.messages.len(), 110);
    assert_eq!(loaded.messages.len(), 100);
    assert_eq!(loaded.messages, before.messages[10..]);
    assert_eq!(
        loaded.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        (11..=110).collect::<Vec<_>>()
    );
    assert_eq!(loaded.messages[0].id, "u-page-005");
    assert_eq!(loaded.messages[99].id, "a-page-054");
    assert_eq!(loaded.parts.len(), 100);
    assert!(
        loaded
            .parts
            .iter()
            .all(|p| loaded.messages.iter().any(|m| m.id == p.message_id))
    );
    assert_eq!(sessions::load_message(f.db(), "u-page-000").unwrap().seq, 1);
    assert_eq!(
        part_text(&sessions::list_parts(f.db(), "u-page-000").unwrap()[0]),
        "用户 0"
    );
    assert_eq!(
        sessions::list_messages(f.db(), "s-page").unwrap().len(),
        110
    );
    assert_eq!(stored(f.db()), before, "面板截取不删除 DB 历史");
}

#[test]
fn missing_session_load_reports_error_instead_of_empty_or_other_session() {
    let f = Fixture::new();
    f.send(true, "s-valid", "t-valid", "u-valid", "有效历史")
        .unwrap();
    let before = stored(f.db());
    let error = conversation::load(f.db(), Some("s-missing")).expect_err("缺失会话必须明确失败");
    assert!(error.to_string().contains("会话不存在"), "{error}");
    assert_eq!(stored(f.db()), before);
}

#[test]
fn cross_session_checkpoint_is_rejected_without_mutating_either_history() {
    let f = Fixture::new();
    f.send(true, "s-a", "t-a", "u-a", "A").unwrap();
    f.send(true, "s-b", "t-b", "u-b", "B").unwrap();
    let before = stored(f.db());
    assert!(
        conversation::checkpoint(
            f.db(),
            3_000,
            "s-b",
            "t-a",
            "a-wrong",
            "错误归属",
            "",
            Some(TurnStatus::Completed),
            None
        )
        .is_err()
    );
    assert_eq!(stored(f.db()), before);
}
