//! T029 真实 PG 重启恢复夹具：在**隔离配置副本**里造出三种重启现场，并把
//! 「批准执行」对准真实的 PostgreSQL 连接，使写入副作用可由数据库触发器计数。
//!
//! 与 `ai_recovery_fixture` 的区别：后者用内存 Mock（`options["demo"]="true"`），
//! 只能证明 UI 与存储状态机，证明不了「副作用恰好执行一次」。
//!
//! 用法：
//! ```text
//! FLUXDB_AI_LIVE=1 FLUXDB_T029_ROOT=/private/tmp/fluxdb-t029-root \
//!   FLUXDB_T029_CONNECTION=8 \
//!   cargo run -p fluxdb-app --example ai_t029_pg_recovery_fixture
//! ```
use fluxdb_app::{AiAgentSaveInput, AppCommand, AppController, AppEvent, TargetRef};
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{conversation, sessions, settings},
};

const SCHEMA: &str = "t029";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要 FLUXDB_AI_LIVE=1（会写入隔离 PG 的 t029 schema）".into());
    }
    let root = std::path::PathBuf::from(std::env::var("FLUXDB_T029_ROOT")?);
    let connection_id: i64 = std::env::var("FLUXDB_T029_CONNECTION")?.parse()?;
    let storage = FileStorage::new(root.clone());
    let conn = storage.open_ai_connection()?;

    let config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id.0 as i64 == connection_id)
        .ok_or("找不到指定的 PG 连接")?;
    if config.kind != fluxdb_core::DatabaseKind::Postgres {
        return Err("该连接不是 PostgreSQL".into());
    }
    let target = TargetRef {
        connection_id: Some(connection_id),
        database: None,
        schema: Some(SCHEMA.into()),
        object: Some("orders".into()),
        redis_db: None,
    };
    let target_json = target.to_json();

    // 未知的写现场留在原连接上，用来验证「同连接存在未核实未知副作用就拒绝冲突写入」。
    // 重新审批的正向路径放到克隆连接上，否则它会被上面的守卫按设计拦下，
    // 那样就证明不了「批准后真实写入恰好一次」。
    let clean_id = connection_id + 1;
    let clean = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(clean_id as u64),
        name: format!("{SCHEMA} clean clone"),
        ..config.clone()
    };
    let mut all = storage.load_connections()?;
    all.retain(|item| item.id != clean.id);
    all.push(clean);
    storage.save_connections(&all)?;
    println!("干净克隆连接 {clean_id}（与 {connection_id} 同库同 schema）");

    let clean_target = TargetRef {
        connection_id: Some(clean_id),
        database: None,
        schema: Some(SCHEMA.into()),
        object: Some("orders".into()),
        redis_db: None,
    };
    let clean_target_json = clean_target.to_json();

    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());

    // 恢复执行按运行快照里的原模型构造 Provider，因此快照必须指向副本里真实模型。
    let model_id = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::models::list_models_page(conn, 200, 0)?
            .into_iter()
            .find(|model| {
                model.enabled
                    && model.kind == fluxdb_storage::ai_platform::models::ModelKind::Generation
            })
            .map(|model| model.id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "副本里没有启用的生成模型",
                )
            })
    })?;
    let agent = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "T029 PG 恢复验收".into(),
        description: "重启恢复现场".into(),
        system_prompt: format!(
            "你是数据库 Agent。测试 schema 是 {SCHEMA}，表 {SCHEMA}.orders(id,status,amount)。\
             只依据工具回执回答，不编造。"
        ),
        model_profile_id: Some(model_id.clone()),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => return Err(format!("保存 Agent 失败: {other:?}").into()),
    };
    storage.with_ai_connection(|conn| {
        let mut runtime = settings::load_runtime_settings(conn)?;
        runtime.default_agent_id = Some(agent.clone());
        runtime.default_model_profile_id = Some(model_id.clone());
        settings::save_runtime_settings(conn, 1_000, &runtime, runtime.revision)?;
        Ok(())
    })?;
    let snapshot = serde_json::json!({
        "model_profile_id": model_id,
        "agent_id": agent,
        "agent_revision": 1,
        "system_prompt": "T029 PG 恢复验收",
    })
    .to_string();

    let existing: i64 = conn.query_row("SELECT count(*) FROM ai_sessions", [], |r| r.get(0))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();

    // ① 读中断：db.query 处于 running，重启后必须记为 cancelled（读取中断，不伪造回执）。
    // 夹具可重复运行：用递增后缀避免会话 ID 冲突，但每次都追加全新事实行，
    // 这样每次桌面端启动都会产生一个全新的恢复窗口。
    let run = format!("{}", now % 100_000);
    seed_running_read(
        &conn,
        now,
        &agent,
        &target_json,
        &snapshot,
        connection_id,
        &run,
    )?;

    // ② 未知写：db.execute 处于 running，重启后必须记为 unknown 且不自动重放。
    //    与批准写入同连接，才能顺带验证「同连接存在未核实未知副作用就拒绝冲突写入」。
    seed_running_write(
        &conn,
        now,
        &agent,
        &target_json,
        &snapshot,
        connection_id,
        &run,
    )?;

    // ③ 已批准但未执行：重启后必须退回 pending 并要求重新审批。
    seed_approved_not_executed(
        &conn,
        now,
        &agent,
        &clean_target_json,
        &snapshot,
        clean_id,
        &run,
    )?;

    println!("已有会话 {existing} 个；已追加三个重启现场（不覆盖既有数据）。");
    println!("目标连接 {connection_id} / schema {SCHEMA}；批准写入会真实命中 PG。");
    Ok(())
}

/// 读中断现场：工具卡 + running 事实行，无回执。
fn seed_running_read(
    conn: &rusqlite::Connection,
    now: i64,
    agent: &str,
    target_json: &str,
    snapshot: &str,
    connection_id: i64,
    run: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = format!("t029-read-{run}");
    let turn = format!("t029-read-turn-{run}");
    let msg = format!("t029-read-assistant-{run}");
    conversation::send(
        conn,
        now,
        true,
        &session,
        agent,
        &turn,
        &format!("{session}-user"),
        "上次读取被打断，请重新查询",
        target_json,
        snapshot,
    )?;
    conversation::checkpoint(
        conn,
        now,
        &session,
        &turn,
        &msg,
        "已保存的部分回复：重启后这段内容不能丢失。",
        "正在核对字段。",
        None,
        None,
    )?;
    let call = format!("{turn}-call");
    sessions::start_tool_call(
        conn,
        now,
        &call,
        &session,
        &turn,
        1,
        0,
        "read-provider",
        "db.query",
        1,
        None,
        &format!(
            "{{\"target\":\"conn:{connection_id}/schema:{SCHEMA}\",\"sql\":\"SELECT id,status FROM {SCHEMA}.orders ORDER BY id\"}}"
        ),
        target_json,
        "read-plan",
    )?;
    sessions::append_part(
        conn,
        now,
        &format!("{msg}-tool"),
        &msg,
        sessions::PartKind::ToolCallRef,
        sessions::PartStatus::Streaming,
        &serde_json::json!({"tool":"db.query","text":"db.query：旧进程退出前尚未收到回执"})
            .to_string(),
        Some(&call),
        None,
        None,
    )?;
    save_draft(conn, now, &session, &turn, &msg)?;
    Ok(())
}

/// 未知写现场：写工具处于 running，重启后必须变 unknown。
fn seed_running_write(
    conn: &rusqlite::Connection,
    now: i64,
    agent: &str,
    target_json: &str,
    snapshot: &str,
    connection_id: i64,
    run: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = format!("t029-unknown-{run}");
    let turn = format!("t029-unknown-turn-{run}");
    let msg = format!("t029-unknown-assistant-{run}");
    conversation::send(
        conn,
        now,
        true,
        &session,
        agent,
        &turn,
        &format!("{session}-user"),
        "把 id=2 的 status 改成 paid",
        target_json,
        snapshot,
    )?;
    conversation::checkpoint(
        conn,
        now,
        &session,
        &turn,
        &msg,
        "准备执行写入。",
        "准备执行写入。",
        None,
        None,
    )?;
    let call = format!("{turn}-call");
    sessions::start_tool_call(
        conn,
        now,
        &call,
        &session,
        &turn,
        1,
        0,
        "unknown-provider",
        "db.execute",
        1,
        None,
        &format!(
            "{{\"target\":\"conn:{connection_id}/schema:{SCHEMA}\",\"sql\":\"UPDATE {SCHEMA}.orders SET status='paid' WHERE id=2\"}}"
        ),
        target_json,
        "unknown-plan",
    )?;
    sessions::append_part(
        conn,
        now,
        &format!("{msg}-tool"),
        &msg,
        sessions::PartKind::ToolCallRef,
        sessions::PartStatus::Streaming,
        &serde_json::json!({"tool":"db.execute","text":"db.execute：写入已发出但未收到回执"})
            .to_string(),
        Some(&call),
        None,
        None,
    )?;
    let ui = sessions::load_ui_state(conn, &session)?;
    sessions::save_ui_state(
        conn,
        now,
        &sessions::SessionUiStateEdit {
            session_id: session.into(),
            selected_turn_id: Some(turn.into()),
            expanded_part_ids_json: "[]".into(),
            scroll_anchor_message_id: Some(msg.into()),
            ui_options_json: "{}".into(),
            draft_json: Some(r#"{"text":"恢复后保留这份未发送草稿"}"#.into()),
        },
        ui.revision,
    )?;
    Ok(())
}

/// 已批准未执行现场：审批为 approved，重启后退回 pending。
fn seed_approved_not_executed(
    conn: &rusqlite::Connection,
    now: i64,
    agent: &str,
    target_json: &str,
    snapshot: &str,
    connection_id: i64,
    run: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use fluxdb_storage::ai_platform::audits;
    let session = format!("t029-approval-{run}");
    let turn = format!("t029-approval-turn-{run}");
    let msg = format!("t029-approval-assistant-{run}");
    let approval_id = format!("t029-approval-id-{run}");
    let call_id = format!("{turn}-toolcall-approval-provider");
    conversation::send(
        conn,
        now,
        true,
        &session,
        agent,
        &turn,
        &format!("{session}-user"),
        "把 id=1 的 status 改成 paid",
        target_json,
        snapshot,
    )?;
    conversation::checkpoint(
        conn,
        now,
        &session,
        &turn,
        &msg,
        "等待重新审批的写入。",
        "等待重新审批的写入。",
        None,
        None,
    )?;
    // plan_hash 必须与恢复执行的规范化结果一致，否则批准会按设计失效。
    let arguments = format!(
        "{{\"target\":\"conn:{connection_id}/schema:{SCHEMA}\",\"sql\":\"UPDATE {SCHEMA}.orders SET status='paid' WHERE id=1\"}}"
    );
    let normalized = serde_json::from_str::<serde_json::Value>(&arguments)?.to_string();
    let plan = fluxdb_app::plan_hash("db.execute", 1, target_json, &normalized, None);
    sessions::create_tool_call(
        conn,
        now,
        &call_id,
        &session,
        &turn,
        1,
        0,
        "approval-provider",
        "db.execute",
        1,
        None,
        &arguments,
        target_json,
        &plan,
        "waiting_approval",
    )?;
    let approval = audits::create_approval(conn, now, &approval_id, &call_id, &plan, "{}", None)?;
    audits::decide_approval(conn, now, &approval.id, true, None, approval.revision)?;
    conversation::upsert_event_part(
        conn,
        now,
        &session,
        &turn,
        &msg,
        &format!("{msg}-approval-approval-provider"),
        sessions::PartKind::Notice,
        sessions::PartStatus::Complete,
        &serde_json::json!({
            "text":"上次已批准，但工具尚未执行。",
            "approval_id":approval.id,
            "approval_revision":2,
            "decision":"approved",
        })
        .to_string(),
        Some(&call_id),
        None,
    )?;
    let turn_row = sessions::load_turn(conn, &turn)?;
    sessions::update_turn_status(
        conn,
        now,
        &turn_row.id,
        sessions::TurnStatus::WaitingApproval,
        None,
        turn_row.revision,
    )?;
    Ok(())
}

/// 保存草稿/锚点/展开状态，用于验证重启后草稿与滚动锚点仍被保留。
fn save_draft(
    conn: &rusqlite::Connection,
    now: i64,
    session: &str,
    turn: &str,
    msg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ui = sessions::load_ui_state(conn, session)?;
    sessions::save_ui_state(
        conn,
        now,
        &sessions::SessionUiStateEdit {
            session_id: session.into(),
            selected_turn_id: Some(turn.into()),
            expanded_part_ids_json: "[]".into(),
            scroll_anchor_message_id: Some(msg.into()),
            ui_options_json: "{}".into(),
            draft_json: Some(r#"{"text":"恢复后保留这份未发送草稿"}"#.into()),
        },
        ui.revision,
    )?;
    Ok(())
}
