//! 为显式指定的配置副本准备真实 SQL E2E；不读取默认配置目录，不保存测试密码。
use fluxdb_app::{AiAgentSaveInput, AiConversationCommand, AppCommand, AppController, AppEvent};
use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint};
use fluxdb_storage::{FileStorage, Storage as _, ai_platform::settings};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要显式设置 FLUXDB_AI_LIVE=1".into());
    }
    let root = std::path::PathBuf::from(std::env::var("FLUXDB_AI_GUI_ROOT")?);
    if !root.join("e2e-authorized-copy").is_file() {
        return Err("只允许操作包含 e2e-authorized-copy 标记的隔离副本".into());
    }
    let pg_port = std::env::var("FLUXDB_E2E_PG_PORT")?.parse()?;
    let mysql_port = std::env::var("FLUXDB_E2E_MYSQL_PORT")?.parse()?;
    let model_name = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "公司自建".into());
    let storage = FileStorage::new(root.clone());
    let mut app_settings = storage.load_settings()?;
    app_settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&app_settings)?;
    let mut connections = Vec::new();
    for (id, kind, port, user, database) in [
        (
            6101,
            DatabaseKind::Postgres,
            pg_port,
            "postgres",
            "t024_e2e",
        ),
        (6102, DatabaseKind::MySql, mysql_port, "root", "mysql"),
    ] {
        let mut options = std::collections::BTreeMap::new();
        options.insert("username".into(), user.into());
        let postgres_profile = if kind == DatabaseKind::Postgres {
            let mut profile = fluxdb_core::PostgresConnectionProfile::default();
            profile.basic.host = "127.0.0.1".into();
            profile.basic.port = port;
            profile.basic.username = user.into();
            profile.basic.maintenance_database = database.into();
            Some(profile)
        } else {
            None
        };
        connections.push(ConnectionConfig {
            id: ConnectionId(id),
            name: format!("T024 isolated {kind:?}"),
            kind,
            endpoint: Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port,
                database: Some(database.into()),
            },
            options,
            redis_profile: None,
            mysql_profile: None,
            postgres_profile,
        });
    }
    let path = root.join("policy.sqlite");
    let sqlite = rusqlite::Connection::open(&path)?;
    sqlite.pragma_update(None, "user_version", 7)?;
    connections.push(ConnectionConfig {
        id: ConnectionId(6103),
        name: "T024 isolated SQLite".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path,
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    });
    storage.save_connections(&connections)?;
    for config in &connections {
        fluxdb_connectors::connector_for(config)?.test_connection(config)?;
        println!("隔离连接可用：{}", config.id.0);
    }
    let model_id = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::models::list_models_page(conn, 200, 0)?
            .into_iter()
            .find(|model| model.enabled && model.display_name == model_name)
            .map(|model| model.id)
            .ok_or_else(|| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型不存在"))
    })?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let agent = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "T024 SQL E2E".into(),
        model_profile_id: Some(model_id.clone()),
        system_prompt: "使用真实数据库工具完成任务，只依据工具回执回答。PostgreSQL 参数绑定暂不支持，使用内联测试字面量；MySQL 参数使用 tagged parameters。写入必须等待桌面审批，不得绕过、拆分或自动重试。只访问目标测试表。".into(),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => return Err(format!("Agent 创建失败：{other:?}").into()),
    };
    storage.with_ai_connection(|conn| {
        let mut runtime = settings::load_runtime_settings(conn)?;
        runtime.default_agent_id = Some(agent.clone());
        runtime.default_model_profile_id = Some(model_id.clone());
        settings::save_runtime_settings(conn, 1_000, &runtime, runtime.revision)?;
        Ok(())
    })?;
    if !matches!(
        controller.dispatch(AppCommand::AiConversation(AiConversationCommand::New)),
        AppEvent::AiConversationLoaded(_)
    ) {
        return Err("无法清除副本中的旧会话指针".into());
    }
    println!("E2E 默认 Agent 就绪；配置副本与测试库均已隔离。");
    Ok(())
}
