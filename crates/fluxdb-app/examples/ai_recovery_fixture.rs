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
    // 夹具也要有可用的模型绑定：恢复执行时宿主必须能按快照里的原模型构造 Provider。
    let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "恢复夹具离线模型".into(),
        provider: "openai".into(),
        model: "recovery-fixture-offline".into(),
        endpoint: Some("http://127.0.0.1:1/v1".into()),
        enabled: true,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(1_024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        event => return Err(format!("{event:?}").into()),
    };
    let agent = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "重启恢复 fixture".into(),
        system_prompt: "离线 fixture，不调用模型。".into(),
        model_profile_id: Some(model_id.clone()),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        event => return Err(format!("{event:?}").into()),
    };
    // 运行快照与生产一致：恢复时按快照里的原模型/Agent 复核，不能改用当前绑定。
    let snapshot = serde_json::json!({
        "model_profile_id": model_id,
        "agent_id": agent,
        "agent_revision": 1,
        "system_prompt": "离线恢复夹具，不调用真实模型。",
    })
    .to_string();
    // 未知写用另一条连接(30)：既不干扰对连接 29 的批准写入，
    // 也让"同连接存在未知副作用就拒绝冲突写入"的守卫能被单独验证。
    for (session, tool, connection) in [
        ("recovery-read", "db.query", 29),
        ("recovery-unknown", "db.execute", 30),
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
            &serde_json::json!({"connection_id": connection, "database": "fixture_only"})
                .to_string(),
            &snapshot,
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
        // 工具事实行与本轮固定目标保持一致，否则冲突守卫会误判同连接。
        let target = serde_json::json!({"connection_id": connection, "database": "fixture_only"});
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
            &target.to_string(),
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
    // 可写的 demo 连接（内存 MockConnector）：真实窗口"批准执行"用它验证写操作确实执行且只执行一次。
    let target_json = "{\"connection_id\":29}";
    let mut demo_options = std::collections::BTreeMap::new();
    demo_options.insert("demo".to_string(), "true".to_string());
    storage.save_connections(&[fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(29),
        name: "T029 demo 可写".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: "t029-demo.db".into(),
            read_only: false,
        },
        options: demo_options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }])?;
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
        target_json,
        &snapshot,
    )?;
    let arguments =
        "{\"target\":\"conn:29/db:main\",\"sql\":\"UPDATE product SET name='x' WHERE id=1\"}";
    // plan_hash 必须与运行时一致（规范化参数 = 解析后重新序列化），否则批准会按设计失效。
    let normalized = serde_json::from_str::<serde_json::Value>(arguments)?.to_string();
    let plan = plan_hash("db.execute", 1, target_json, &normalized, None);
    sessions::create_tool_call(
        &conn,
        1008,
        "recovery-approval-turn-toolcall-approval-provider",
        "recovery-approval",
        "recovery-approval-turn",
        1,
        0,
        "approval-provider",
        "db.execute",
        1,
        None,
        arguments,
        target_json,
        &plan,
        "waiting_approval",
    )?;
    let approval = fluxdb_storage::ai_platform::audits::create_approval(
        &conn,
        1009,
        "recovery-approval-id",
        "recovery-approval-turn-toolcall-approval-provider",
        &plan,
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
        &serde_json::json!({"text":"上次已批准，但工具尚未执行。","approval_id":approval.id,"approval_revision":2,"decision":"approved"}).to_string(),Some("recovery-approval-turn-toolcall-approval-provider"),None)?;
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
