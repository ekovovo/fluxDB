//! T029 离线 GUI 故障窗口：只写独立空目录，退出即模拟旧进程无回执。没有 Provider/远端数据库。
use fluxdb_app::*;
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{conversation, sessions, settings},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("必须传入独立 fixture 目录")?;
    let storage = FileStorage::new(std::path::PathBuf::from(root));
    let conn = storage.open_ai_connection()?;
    let count: i64 = conn.query_row("SELECT count(*) FROM ai_sessions", [], |r| r.get(0))?;
    if count != 0 {
        return Err("fixture 非空，拒绝覆盖".into());
    }
    let mut conf = fluxdb_core::Settings::default();
    conf.log_path = storage
        .root_dir()
        .join("logs")
        .to_string_lossy()
        .into_owned();
    storage.save_settings(&conf)?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let agent = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "重启恢复 fixture".into(),
        system_prompt: "离线 fixture，不调用模型。".into(),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        event => return Err(format!("{event:?}").into()),
    };
    for (session, tool) in [
        ("recovery-read", "db.query"),
        ("recovery-unknown", "db.execute"),
    ] {
        let turn = format!("{session}-turn");
        let msg = format!("{session}-assistant");
        conversation::send(
            &conn,
            1001,
            true,
            session,
            &agent,
            &turn,
            &format!("{session}-user"),
            "查看上次中断的任务",
            "{\"connection_id\":29,\"database\":\"fixture_only\"}",
            "{}",
        )?;
        conversation::checkpoint(
            &conn,
            1002,
            session,
            &turn,
            &msg,
            "已保存的部分回复：这条内容不能因为重启而丢失。",
            "正在核对字段。",
            None,
            None,
        )?;
        let call = format!("{turn}-call");
        sessions::start_tool_call(
            &conn,
            1003,
            &call,
            session,
            &turn,
            1,
            0,
            "call-1",
            tool,
            1,
            None,
            "{}",
            "{\"connection_id\":29,\"database\":\"fixture_only\"}",
            "fixture-plan",
        )?;
        sessions::append_part(
            &conn,
            1004,
            &format!("{msg}-tool"),
            &msg,
            sessions::PartKind::ToolCallRef,
            sessions::PartStatus::Streaming,
            &serde_json::json!({"tool":tool,"text":format!("{tool}：旧进程退出前尚未收到回执")})
                .to_string(),
            Some(&call),
            None,
            None,
        )?;
        let ui = sessions::load_ui_state(&conn, session)?;
        sessions::save_ui_state(
            &conn,
            1005,
            &sessions::SessionUiStateEdit {
                session_id: session.into(),
                selected_turn_id: Some(turn),
                expanded_part_ids_json: "[]".into(),
                scroll_anchor_message_id: Some(msg),
                ui_options_json: "{}".into(),
                draft_json: Some("{\"text\":\"恢复后保留这份未发送草稿\"}".into()),
            },
            ui.revision,
        )?;
    }
    // 模拟“已批准但尚未执行就退出”的窗口；重启应要求新 revision 再次审批。
    conversation::send(
        &conn,
        1007,
        true,
        "recovery-approval",
        &agent,
        "recovery-approval-turn",
        "recovery-approval-user",
        "等待重新审批的离线任务",
        "{\"connection_id\":29}",
        "{}",
    )?;
    sessions::create_tool_call(
        &conn,
        1008,
        "recovery-approval-call",
        "recovery-approval",
        "recovery-approval-turn",
        1,
        0,
        "approval-provider",
        "db.execute",
        1,
        None,
        "{}",
        "{\"connection_id\":29}",
        "fixture-approval-plan",
        "waiting_approval",
    )?;
    let approval = fluxdb_storage::ai_platform::audits::create_approval(
        &conn,
        1009,
        "recovery-approval-id",
        "recovery-approval-call",
        "fixture-approval-plan",
        "{}",
        None,
    )?;
    fluxdb_storage::ai_platform::audits::decide_approval(
        &conn,
        1010,
        &approval.id,
        true,
        None,
        approval.revision,
    )?;
    conversation::upsert_event_part(&conn,1011,"recovery-approval","recovery-approval-turn","recovery-approval-assistant","recovery-approval-assistant-approval-approval-provider",sessions::PartKind::Notice,sessions::PartStatus::Complete,
        &serde_json::json!({"text":"上次已批准，但工具尚未执行。","approval_id":approval.id,"approval_revision":2,"decision":"approved"}).to_string(),Some("recovery-approval-call"),None)?;
    let turn = sessions::load_turn(&conn, "recovery-approval-turn")?;
    sessions::update_turn_status(
        &conn,
        1012,
        &turn.id,
        sessions::TurnStatus::WaitingApproval,
        None,
        turn.revision,
    )?;
    let mut prefs = settings::load_panel_preferences(&conn)?;
    let rev = prefs.revision;
    prefs.last_session_id = Some("recovery-unknown".into());
    settings::save_panel_preferences(&conn, 1006, &prefs, rev)?;
    println!("fixture ready: recovery-read/recovery-unknown（没有凭据或远端请求）");
    Ok(())
}
