//! T028 应用数据链路验收：公开命令/服务 → 文件 SQLite → AppState → 新 controller 重开。
//! 仅替换模型事件来源，不复制生产投影逻辑、不发网络请求、不使用应用真实配置。
use std::{cell::Cell, fs, path::PathBuf, sync::mpsc, time::Duration};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiConversationView,
    AiModelSaveInput, AppCommand, AppController, AppEvent, TargetRef, TurnTargetPin,
    persist_ai_conversation_event, run_ai_conversation_source,
};
use fluxdb_core::{CancelFlag, FinishReason, ModelEvent};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{agents, conversation, models, sessions, settings},
};
use sessions::{PartStatus, PersistenceKind, TurnStatus};
use settings::PanelMode;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t028-app-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        // 正式 FileStorage 入口负责建表，不能手工替换 schema。
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        controller
    }

    fn configured(&self) -> (AppController, String, String) {
        let mut controller = self.controller();
        let model = create_model(&mut controller, "openai", true);
        let agent = create_agent(&mut controller, Some(model.clone()), true);
        (controller, agent, model)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 每次 API 调用已关闭连接，只清理本 fixture 独占创建的临时 root。
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn create_model(controller: &mut AppController, provider: &str, enabled: bool) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "T028 离线模型".into(),
        provider: provider.into(),
        model: "t028-offline-model".into(),
        endpoint: Some("http://127.0.0.1:9/v1".into()),
        enabled,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(1_024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("正常配置模型失败: {other:?}"),
    }
}

fn create_agent(controller: &mut AppController, model: Option<String>, enabled: bool) -> String {
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "T028 数据链路 Agent".into(),
        description: "独立 fixture".into(),
        system_prompt: "T028 唯一系统提示词".into(),
        model_profile_id: model,
        enabled,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("正常配置 Agent 失败: {other:?}"),
    }
}

fn dispatch(controller: &mut AppController, command: AiConversationCommand) -> AiConversationView {
    match controller.dispatch(AppCommand::AiConversation(command)) {
        AppEvent::AiConversationLoaded(view) => {
            assert_eq!(
                controller.state().ai_conversation,
                view,
                "事件与即时 AppState 一致"
            );
            view
        }
        other => panic!("会话命令失败: {other:?}"),
    }
}

fn load(controller: &mut AppController) -> AiConversationView {
    dispatch(controller, AiConversationCommand::Load { session_id: None })
}

fn save_ui(
    controller: &mut AppController,
    draft: &str,
    expanded: Vec<String>,
) -> AiConversationView {
    dispatch(
        controller,
        AiConversationCommand::SaveUi {
            draft: draft.into(),
            expanded_parts: expanded,
            mode: PanelMode::Embedded,
            drawer_width: 570.0,
            embedded_width: 385.0,
        },
    )
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(28),
        database: Some("t028_database".into()),
        schema: Some("public".into()),
        object: Some("t028_table".into()),
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: Some(28),
        captured_tab_revision: 7,
    }
}

#[derive(Debug, PartialEq)]
struct DbState {
    preferences: settings::PanelPreferences,
    sessions: Vec<sessions::SessionView>,
    turns: Vec<sessions::TurnView>,
    messages: Vec<sessions::MessageView>,
    parts: Vec<sessions::MessagePartView>,
    ui_states: Vec<sessions::SessionUiStateView>,
    events: Vec<sessions::RunEventView>,
}

fn backend(storage: &FileStorage) -> DbState {
    storage
        .with_ai_connection(|conn| {
            let mut state = DbState {
                preferences: settings::load_panel_preferences(conn)?,
                sessions: sessions::list_sessions(conn, 1_000)?,
                turns: Vec::new(),
                messages: Vec::new(),
                parts: Vec::new(),
                ui_states: Vec::new(),
                events: Vec::new(),
            };
            for session in &state.sessions {
                let snapshot = sessions::session_snapshot(conn, &session.id)?;
                state.turns.extend(snapshot.turns);
                state.messages.extend(snapshot.messages);
                state.parts.extend(snapshot.parts);
                state.events.extend(snapshot.run_events);
                if let Some(ui) = snapshot.ui_state {
                    state.ui_states.push(ui);
                }
            }
            Ok(state)
        })
        .unwrap()
}

fn assert_text(view: &AiConversationView, message: &str, role: &str, expected: &str) {
    let message = view
        .messages
        .iter()
        .find(|m| m.id == message)
        .expect("真实消息进入生产投影");
    assert_eq!(message.role, role);
    let text = message
        .parts
        .iter()
        .find(|p| p.kind == "text")
        .expect("真实正文进入 AppState");
    let payload: serde_json::Value = serde_json::from_str(&text.payload_json).unwrap();
    assert_eq!(payload["text"].as_str(), Some(expected));
}

fn complete(storage: &FileStorage, job: &AiConversationJob) -> AiConversationView {
    persist_ai_conversation_event(
        storage,
        job,
        "T028 已保存回复",
        "T028 公开摘要",
        Some(TurnStatus::Completed),
        None,
    )
    .unwrap()
}

#[test]
fn preferences_and_new_draft_save_load_and_controller_reopen_without_empty_history() {
    let f = Fixture::new();
    let mut controller = f.controller();
    let empty = load(&mut controller);
    assert!(empty.session_id.is_none() && empty.sessions.is_empty() && empty.messages.is_empty());
    assert_eq!(empty.preferences.preferred_mode, PanelMode::Drawer);
    let saved = save_ui(&mut controller, "T028 新草稿，尚未发送", Vec::new());
    assert_eq!(saved.draft, "T028 新草稿，尚未发送");
    assert_eq!(saved.preferences.preferred_mode, PanelMode::Embedded);
    assert_eq!(saved.preferences.drawer_width_dp, 570.0);
    assert_eq!(saved.preferences.embedded_width_dp, 385.0);
    let db = backend(&f.storage);
    assert!(db.sessions.is_empty() && db.turns.is_empty() && db.messages.is_empty());
    assert_eq!(db.preferences, saved.preferences);
    assert_eq!(load(&mut controller), saved);
    drop(controller);
    let mut reopened = f.controller();
    assert_eq!(
        load(&mut reopened),
        saved,
        "真实文件加载经生产投影恢复草稿/偏好"
    );
    for _ in 0..3 {
        let new = dispatch(&mut reopened, AiConversationCommand::New);
        assert!(new.session_id.is_none() && new.messages.is_empty() && new.draft.is_empty());
        assert_eq!(new.preferences.preferred_mode, PanelMode::Embedded);
        assert_eq!(new.preferences.drawer_width_dp, 570.0);
        assert_eq!(new.preferences.embedded_width_dp, 385.0);
        assert!(backend(&f.storage).sessions.is_empty(), "New 不生成空历史");
    }
}

#[test]
fn existing_data_save_ui_is_immediate_and_restores_expansion_draft_and_messages() {
    let f = Fixture::new();
    let (mut controller, agent, _) = f.configured();
    load(&mut controller);
    let job = controller
        .prepare_ai_conversation("T028 已有真实用户消息", &target())
        .unwrap();
    let final_view = complete(&f.storage, &job);
    dispatch(
        &mut controller,
        AiConversationCommand::Apply { view: final_view },
    );
    let expanded = vec![format!("{}-summary", job.assistant_message_id)];
    let saved = save_ui(&mut controller, "T028 已有会话草稿", expanded.clone());
    assert_eq!(saved.session_id.as_deref(), Some(job.session_id.as_str()));
    assert_eq!(saved.sessions.len(), 1);
    assert_eq!(saved.sessions[0].agent_id, agent);
    assert_eq!(saved.messages.len(), 2);
    assert_eq!(saved.expanded_parts, expanded);
    assert_eq!(saved.draft, "T028 已有会话草稿");
    assert_text(
        &saved,
        &job.assistant_message_id,
        "assistant",
        "T028 已保存回复",
    );
    let db = backend(&f.storage);
    let ui = &db.ui_states[0];
    let draft: serde_json::Value = serde_json::from_str(ui.draft_json.as_deref().unwrap()).unwrap();
    assert_eq!(draft["text"], saved.draft);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&ui.expanded_part_ids_json).unwrap(),
        saved.expanded_parts
    );
    assert_eq!(load(&mut controller), saved);
    drop(controller);
    let mut reopened = f.controller();
    assert_eq!(load(&mut reopened), saved);
    assert_eq!(backend(&f.storage), db);

    let new = dispatch(&mut reopened, AiConversationCommand::New);
    assert!(new.session_id.is_none() && new.messages.is_empty() && new.draft.is_empty());
    assert_eq!(new.sessions.len(), 1, "新草稿不删除旧历史");
    assert_eq!(backend(&f.storage).messages, db.messages);
    let selected = dispatch(
        &mut reopened,
        AiConversationCommand::Load {
            session_id: Some(job.session_id.clone()),
        },
    );
    assert_eq!(selected.messages, saved.messages);
    assert_eq!(selected.draft, saved.draft);
    assert_eq!(selected.expanded_parts, saved.expanded_parts);
}

#[test]
fn prepare_existing_session_returns_real_job_and_user_text_and_rejects_active_resend() {
    let f = Fixture::new();
    let (mut controller, agent, model) = f.configured();
    let first = controller
        .prepare_ai_conversation("第一轮用户正文", &target())
        .unwrap();
    complete(&f.storage, &first);
    // 用正常 Load，不把后台服务对象手工塞进 state。
    load(&mut controller);
    save_ui(&mut controller, "第二轮待发送草稿", Vec::new());
    let job = controller
        .prepare_ai_conversation("T028 第二轮真实用户正文", &target())
        .unwrap();
    assert_eq!(job.session_id, first.session_id);
    assert_ne!(job.turn_id, first.turn_id);
    assert_eq!(job.model_profile_id, model);
    let state = controller.state().ai_conversation.clone();
    assert_eq!(state.active_turn_id.as_deref(), Some(job.turn_id.as_str()));
    assert_eq!(state.session_id.as_deref(), Some(job.session_id.as_str()));
    assert!(state.draft.is_empty());
    assert_eq!(state.messages.len(), 3);
    let db = backend(&f.storage);
    let user = db
        .messages
        .iter()
        .find(|m| m.turn_id.as_deref() == Some(job.turn_id.as_str()))
        .unwrap();
    assert_eq!(user.role, "user");
    assert_eq!(user.seq, 3);
    assert_text(&state, &user.id, "user", "T028 第二轮真实用户正文");
    let turn = db.turns.iter().find(|t| t.id == job.turn_id).unwrap();
    assert_eq!(turn.status, TurnStatus::Queued);
    assert_eq!(turn.target_json, target().target_json);
    let snapshot: serde_json::Value = serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
    assert_eq!(snapshot["agent_id"], agent);
    assert_eq!(snapshot["model_profile_id"], model);
    assert_eq!(snapshot["system_prompt"], "T028 唯一系统提示词");
    assert!(db.preferences.pending_draft_json.is_none());
    assert!(db.ui_states[0].draft_json.is_none());

    save_ui(&mut controller, "活跃任务拒绝时保留草稿", Vec::new());
    let before_state = controller.state().ai_conversation.clone();
    let before_db = backend(&f.storage);
    let error = controller
        .prepare_ai_conversation("不应入库", &target())
        .unwrap_err();
    assert!(error.message.contains("进行中"));
    assert_eq!(controller.state().ai_conversation, before_state);
    assert_eq!(backend(&f.storage), before_db);
}

#[test]
fn persisted_checkpoints_and_empty_terminal_apply_then_load_use_real_reply() {
    let f = Fixture::new();
    let (mut controller, _, _) = f.configured();
    let job = controller
        .prepare_ai_conversation("T028 提问", &target())
        .unwrap();
    let before = controller.state().ai_conversation.clone();
    let streaming =
        persist_ai_conversation_event(&f.storage, &job, "T028 部分回复", "T028 摘要", None, None)
            .unwrap();
    assert_eq!(
        controller.state().ai_conversation,
        before,
        "后台保存不直接修改前台 controller"
    );
    dispatch(
        &mut controller,
        AiConversationCommand::Apply { view: streaming },
    );
    let final_view =
        persist_ai_conversation_event(&f.storage, &job, "", "", Some(TurnStatus::Completed), None)
            .unwrap();
    assert!(final_view.active_turn_id.is_none());
    assert_text(
        &final_view,
        &job.assistant_message_id,
        "assistant",
        "T028 部分回复",
    );
    let assistant = final_view
        .messages
        .iter()
        .find(|m| m.id == job.assistant_message_id)
        .unwrap();
    assert!(assistant.parts.iter().all(|p| p.status == "complete"));
    let applied = dispatch(
        &mut controller,
        AiConversationCommand::Apply {
            view: final_view.clone(),
        },
    );
    assert_eq!(applied, final_view);
    assert_eq!(load(&mut controller), final_view);
    let db = backend(&f.storage);
    assert_eq!(db.messages.len(), 2);
    assert_eq!(db.messages[1].persistence_kind, PersistenceKind::Final);
    assert_eq!(db.turns[0].status, TurnStatus::Completed);
    drop(controller);
    assert_eq!(load(&mut f.controller()), final_view);
    assert_eq!(backend(&f.storage), db);
}

#[test]
fn insufficient_configuration_preserves_database_draft_and_state() {
    for scenario in [
        "no-agent",
        "no-model",
        "disabled-model",
        "decision-model",
        "disabled-agent",
    ] {
        let f = Fixture::new();
        let mut controller = f.controller();
        match scenario {
            "no-agent" => {}
            "no-model" => {
                create_agent(&mut controller, None, true);
            }
            "disabled-model" => {
                let id = create_model(&mut controller, "openai", false);
                create_agent(&mut controller, Some(id), true);
            }
            "decision-model" => {
                let id = create_model(&mut controller, "decisions", true);
                create_agent(&mut controller, Some(id), true);
            }
            "disabled-agent" => {
                let id = create_model(&mut controller, "openai", true);
                create_agent(&mut controller, Some(id), false);
            }
            _ => unreachable!(),
        }
        load(&mut controller);
        save_ui(&mut controller, "配置不足不能吞掉 T028 草稿", Vec::new());
        let before = controller.state().ai_conversation.clone();
        let db = backend(&f.storage);
        let error = controller
            .prepare_ai_conversation("不能创建空会话", &target())
            .unwrap_err();
        assert!(!error.message.is_empty(), "{scenario}");
        assert_eq!(controller.state().ai_conversation, before, "{scenario}");
        assert_eq!(backend(&f.storage), db, "{scenario}");
        assert!(db.sessions.is_empty() && db.messages.is_empty());
        let mut reopened = f.controller();
        assert_eq!(load(&mut reopened), before, "{scenario} 重开草稿仍在");
    }
}

#[test]
fn disabled_model_in_existing_session_preserves_history_and_unsent_draft() {
    let f = Fixture::new();
    let (mut controller, _, model) = f.configured();
    let job = controller
        .prepare_ai_conversation("已有用户消息", &target())
        .unwrap();
    complete(&f.storage, &job);
    load(&mut controller);
    save_ui(&mut controller, "模型失效不能丢失的草稿", Vec::new());
    let revision = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == model)
        .unwrap()
        .revision;
    assert!(matches!(
        controller.dispatch(AppCommand::SetAiModelEnabled {
            id: model,
            enabled: false,
            expected_revision: revision,
        }),
        AppEvent::AiModelEnabled { enabled: false, .. }
    ));
    let before = controller.state().ai_conversation.clone();
    let db = backend(&f.storage);
    assert!(
        controller
            .prepare_ai_conversation("应当失败", &target())
            .is_err()
    );
    assert_eq!(controller.state().ai_conversation, before);
    assert_eq!(backend(&f.storage), db);
    assert_eq!(load(&mut f.controller()), before);
}

#[test]
fn cancellation_source_persists_partial_reply_and_load_does_not_replay_or_accept_late_turn() {
    let f = Fixture::new();
    let (mut controller, _, _) = f.configured();
    let job = controller
        .prepare_ai_conversation("T028 停止测试", &target())
        .unwrap();
    let cancel = CancelFlag::new();
    let calls = Cell::new(0);
    let (sender, receiver) = mpsc::sync_channel(8);
    run_ai_conversation_source(&f.storage, &job, &cancel, &sender, |emit| {
        calls.set(calls.get() + 1);
        emit(&ModelEvent::TextDelta("T028 停止前的输出".into()));
        emit(&ModelEvent::ReasoningSummaryDelta("T028 停止前摘要".into()));
        cancel.request();
        Ok(Vec::new())
    });
    let final_update = loop {
        let update = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("生产 service 回桥有截止时间");
        if update.terminal {
            break update;
        }
    };
    assert_eq!(calls.get(), 1);
    assert_eq!(final_update.job.turn_id, job.turn_id);
    assert!(final_update.view.active_turn_id.is_none());
    assert_text(
        &final_update.view,
        &job.assistant_message_id,
        "assistant",
        "T028 停止前的输出",
    );
    let applied = dispatch(
        &mut controller,
        AiConversationCommand::Apply {
            view: final_update.view,
        },
    );
    let db = backend(&f.storage);
    assert_eq!(db.turns[0].status, TurnStatus::Cancelled);
    assert!(db.turns[0].finished_at_ms.is_some());
    assert!(
        db.parts
            .iter()
            .filter(|p| p.message_id == job.assistant_message_id)
            .all(|p| p.status == PartStatus::Interrupted)
    );
    drop(controller);
    let mut reopened = f.controller();
    for _ in 0..3 {
        assert_eq!(load(&mut reopened), applied);
        assert_eq!(backend(&f.storage), db);
        assert_eq!(calls.get(), 1, "Load 不重新调用 source");
    }
    let next = reopened
        .prepare_ai_conversation("取消后显式发送的新轮次", &target())
        .unwrap();
    let before = reopened.state().ai_conversation.clone();
    let before_db = backend(&f.storage);
    let error = persist_ai_conversation_event(
        &f.storage,
        &job,
        "迟到不能覆盖新轮次",
        "",
        Some(TurnStatus::Completed),
        None,
    )
    .unwrap_err();
    assert!(error.message.contains("迟到"));
    assert_eq!(reopened.state().ai_conversation, before);
    assert_eq!(backend(&f.storage), before_db);
    assert_eq!(
        load(&mut reopened).active_turn_id.as_deref(),
        Some(next.turn_id.as_str())
    );
    let missing_job = AiConversationJob {
        turn_id: "t028-missing-turn".into(),
        ..job
    };
    assert!(
        persist_ai_conversation_event(&f.storage, &missing_job, "不存在 Turn", "", None, None)
            .is_err()
    );
    assert_eq!(backend(&f.storage), before_db);
}

#[test]
fn source_finished_reply_flows_through_apply_to_normal_load_and_reopen() {
    let f = Fixture::new();
    let (mut controller, _, _) = f.configured();
    let job = controller
        .prepare_ai_conversation("T028 source 提问", &target())
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(8);
    run_ai_conversation_source(&f.storage, &job, &CancelFlag::new(), &sender, |emit| {
        for event in [
            ModelEvent::TextDelta("T028 source ".into()),
            ModelEvent::TextDelta("最终正文".into()),
            ModelEvent::ReasoningSummaryDelta("T028 source 公开摘要".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ] {
            emit(&event);
        }
        Ok(Vec::new())
    });
    let update = loop {
        let update = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        if update.terminal {
            break update;
        }
    };
    assert!(update.view.error.is_none());
    assert_text(
        &update.view,
        &job.assistant_message_id,
        "assistant",
        "T028 source 最终正文",
    );
    let view = dispatch(
        &mut controller,
        AiConversationCommand::Apply { view: update.view },
    );
    assert_eq!(load(&mut controller), view);
    drop(controller);
    assert_eq!(load(&mut f.controller()), view);
    assert_eq!(backend(&f.storage).turns[0].status, TurnStatus::Completed);
}

#[test]
fn failed_save_ui_and_missing_load_report_errors_without_losing_visible_data() {
    let f = Fixture::new();
    let (mut controller, _, _) = f.configured();
    let job = controller
        .prepare_ai_conversation("T028 原始消息", &target())
        .unwrap();
    complete(&f.storage, &job);
    load(&mut controller);
    save_ui(
        &mut controller,
        "T028 原始草稿",
        vec![format!("{}-text", job.assistant_message_id)],
    );
    let before = controller.state().ai_conversation.clone();
    let db = backend(&f.storage);
    for command in [
        AiConversationCommand::SaveUi {
            draft: "事务失败不能覆盖草稿".into(),
            expanded_parts: Vec::new(),
            mode: PanelMode::Drawer,
            drawer_width: 0.0,
            embedded_width: 420.0,
        },
        AiConversationCommand::Load {
            session_id: Some("t028-nonexistent-session".into()),
        },
    ] {
        match controller.dispatch(AppCommand::AiConversation(command)) {
            AppEvent::AiSettingsError(error) => assert!(!error.is_empty()),
            other => panic!("失败不能伪装成空成功: {other:?}"),
        }
        assert_eq!(controller.state().ai_conversation, before);
        assert_eq!(backend(&f.storage), db);
    }
    assert_eq!(load(&mut f.controller()), before);
}

#[test]
fn background_old_turn_finishing_keeps_current_session_pointer() {
    let f = Fixture::new();
    let (mut controller, _, _) = f.configured();
    let old = controller
        .prepare_ai_conversation("旧后台任务", &target())
        .unwrap();
    dispatch(&mut controller, AiConversationCommand::New);
    let current = controller
        .prepare_ai_conversation("当前会话", &target())
        .unwrap();
    let before = controller.state().ai_conversation.clone();
    let returned = complete(&f.storage, &old);
    assert_eq!(
        returned.session_id.as_deref(),
        Some(old.session_id.as_str())
    );
    assert_eq!(
        controller.state().ai_conversation,
        before,
        "宿主尚未 Apply 不改变当前显示"
    );
    let loaded = load(&mut controller);
    assert_eq!(
        loaded.session_id.as_deref(),
        Some(current.session_id.as_str())
    );
    assert_eq!(
        loaded.active_turn_id.as_deref(),
        Some(current.turn_id.as_str())
    );
    assert_eq!(
        loaded.preferences.last_session_id.as_deref(),
        Some(current.session_id.as_str())
    );
    assert_eq!(loaded.messages, before.messages);
    f.storage
        .with_ai_connection(|conn| {
            // 内置默认 Agent 由 schema 播种，fixture 自定义 Agent 之外还有它。
            assert_eq!(agents::list_agents(conn)?.len(), 2);
            assert_eq!(models::count_models(conn)?, 1);
            let old = conversation::load(conn, Some(&old.session_id))?;
            assert_eq!(old.messages.len(), 2);
            assert_eq!(old.turns[0].status, TurnStatus::Completed);
            Ok(())
        })
        .unwrap();
}
