//! T035 摘要模型/原子 checkpoint 的真实窗口验收夹具。
//!
//! 只接收一个不存在的新目录，不读生产配置与凭据；建两个指向本地 mock 的模型档案、
//! 把内置 Agent 配成"主模型 + 独立摘要模型（compaction 角色绑定）+ 触发比例 10%"。
//! 启动后无需在设置页做任何配置，直接新建会话发消息即可验收。
//!
//! 参数：`<目录> [--no-summary]`；`--no-summary` 额外把摘要模型置为停用，
//! 用于验收"摘要模型不可用 → 可见说明 + 超预算暂停"。
use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, Settings};
use fluxdb_storage::ai_platform::model_bindings::{ModelRole, set_role_model};
use fluxdb_storage::ai_platform::{conversation, sessions, settings as ai_settings};
use fluxdb_storage::{FileStorage, Storage as _};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = std::path::PathBuf::from(args.next().ok_or("传入新的隔离目录")?);
    let disable_summary = args.any(|arg| arg == "--no-summary");
    if root.exists() {
        return Err("拒绝覆盖已有目录".into());
    }
    std::fs::create_dir_all(&root)?;
    let storage = FileStorage::new(root.join("config"));
    let mut settings = Settings::default();
    settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&settings)?;
    storage.with_ai_connection(|_| Ok(()))?; // 首次建库 + seed 内置 Agent/插件

    // 一个只读 SQLite 连接，给会话一个真实目标（避免"无可用句柄"）。
    let db_path = root.join("database-1.sqlite");
    let conn = rusqlite::Connection::open(&db_path)?;
    conn.execute_batch(
        "CREATE TABLE orders(id INTEGER PRIMARY KEY, channel TEXT, status TEXT, paid_at TEXT);
         INSERT INTO orders VALUES (1,'web','paid','2026-01-02'),(2,'app','pending',NULL);",
    )?;
    storage.save_connections(&[ConnectionConfig {
        id: ConnectionId(1),
        name: "验收库".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: db_path,
            read_only: true,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }])?;

    let endpoint = std::env::var("FLUXDB_AI_MODEL_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:39107/v1".into());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));

    // 两个档案都带（无用但非空的）Key：mock 不校验；这样后续在设置页编辑模型不会
    // 因为"无 Key 模型被改"而落到"缺少凭据"，也不会让路由拒绝出站。
    let mut save_model =
        |name: &str, model: &str| -> Result<(String, i64), Box<dyn std::error::Error>> {
            match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
                display_name: name.into(),
                provider: "openai".into(),
                model: model.into(),
                endpoint: Some(endpoint.clone()),
                enabled: true,
                context_window_tokens: Some(4096),
                max_output_tokens: Some(512),
                api_key: Some("sk-mock-not-a-real-key".into()),
                ..Default::default()
            })) {
                AppEvent::AiModelSaved { id, revision } => Ok((id, revision)),
                other => Err(format!("模型 {name} 保存失败：{other:?}").into()),
            }
        };
    let (main_model, _) = save_model("T035 主模型", "mock-main")?;
    let (summary_model, summary_revision) = save_model("T035 摘要模型", "mock-summary")?;

    // 内置 Agent 是新会话默认入口：绑主模型 + 触发 10%，其余字段保持原样。
    let builtin = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::agents::load_agent(
            conn,
            fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID,
        )
    })?;
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 10,
        id: Some(builtin.id.clone()),
        display_name: builtin.display_name.clone(),
        description: builtin.description.clone(),
        system_prompt: builtin.system_prompt.clone(),
        model_profile_id: Some(main_model.clone()),
        allowed_tools: builtin.allowed_tools.clone(),
        enabled: true,
        expected_revision: builtin.revision,
    })) {
        AppEvent::AiAgentSaved { .. } => {}
        other => return Err(format!("Agent 保存失败：{other:?}").into()),
    }
    // 摘要模型走 compaction 角色绑定（未绑定时回落主模型）。
    // 该配置目前只在存储/路由层存在，不在 Agent 编辑页暴露（2026-10-11 决定）。
    storage.with_ai_connection(|conn| {
        set_role_model(
            conn,
            now_ms(),
            &format!("binding-{summary_model}"),
            &builtin.id,
            ModelRole::Compaction,
            Some(&summary_model),
            None,
        )
        .map(|_| ())
    })?;
    if disable_summary {
        // 摘要模型已绑定但停用：用于验收"不静默替换成主模型 + 可见暂停"。
        match controller.dispatch(AppCommand::SetAiModelEnabled {
            id: summary_model.clone(),
            enabled: false,
            expected_revision: summary_revision,
        }) {
            AppEvent::AiModelEnabled { .. } => {}
            other => return Err(format!("停用摘要模型失败：{other:?}").into()),
        }
    }

    // 预置一段已完成历史（6 条消息）：让"第一次发送"就能满足合法切点，不必为了凑消息反复发送。
    // `--no-summary` 变体把历史写长一些，使第一次发送即超预算，直接验到"不可用 + 暂停"。
    let seed_body = "订单表 orders 的口径说明：净销售额按 paid_at 所在自然月归集，退款单不计入；"
        .repeat(if disable_summary { 60 } else { 12 });
    storage.with_ai_connection(|conn| {
        let now = now_ms();
        for ordinal in 1..=3i64 {
            let turn = format!("t035-seed-turn-{ordinal}");
            conversation::send(
                conn,
                now + ordinal,
                ordinal == 1,
                "t035-seed",
                &builtin.id,
                &turn,
                &format!("{turn}-user"),
                &seed_body,
                "{}",
                "{}",
            )?;
            conversation::checkpoint(
                conn,
                now + ordinal,
                "t035-seed",
                &turn,
                &format!("{turn}-assistant"),
                "已记录口径与风险，等待下一条要求。",
                "",
                Some(sessions::TurnStatus::Completed),
                None,
            )?;
        }
        // 面板启动即停在这个会话，省掉"点开历史"的一步。
        let mut prefs = ai_settings::load_panel_preferences(conn)?;
        let revision = prefs.revision;
        prefs.last_session_id = Some("t035-seed".into());
        ai_settings::save_panel_preferences(conn, now + 10, &prefs, revision)?;
        Ok(())
    })?;

    let config = storage.root_dir().display().to_string();
    println!("隔离配置目录: {config}");
    println!("主模型档案  : {main_model} (mock-main)");
    println!("摘要模型档案: {summary_model} (mock-summary)");
    println!(
        "Agent       : {}（触发 10%，摘要模型已绑定{}）",
        builtin.id,
        if disable_summary {
            "，且已停用"
        } else {
            ""
        }
    );
    println!();
    println!("启动（另一个终端先跑 mock）：");
    println!("  python3 /private/tmp/fluxdb-t035-e2e/mock_model.py 39107");
    println!(
        "  FLUXDB_AI_TEST_ROOT={config} FLUXDB_DEBUG_AI_CHAT=1 \\\n    FLUXDB_DEBUG_AI_TARGET='{{\"connection_id\":1,\"database\":\"main\"}}' \\\n    ./target/debug/fluxdb-desktop"
    );
    Ok(())
}
