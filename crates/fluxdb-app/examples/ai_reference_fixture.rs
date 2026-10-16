//! T022 离线窗口验收夹具：只接收一个不存在的新目录，不读取生产配置和凭据。
use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, SavedQuery, Settings};
use fluxdb_storage::{FileStorage, Storage as _};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args_os().nth(1).ok_or("传入新的隔离目录")?);
    if root.exists() {
        return Err("拒绝覆盖已有目录".into());
    }
    std::fs::create_dir_all(&root)?;
    let storage = FileStorage::new(root.join("config"));
    let mut settings = Settings::default();
    settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&settings)?;
    let mut connections = Vec::new();
    let mut queries = Vec::new();
    for (id, label) in [(1, "验收 A"), (2, "验收 B")] {
        let path = root.join(format!("database-{id}.sqlite"));
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch("CREATE TABLE orders(id INTEGER PRIMARY KEY, status TEXT); INSERT INTO orders VALUES(1,'paid'),(2,'pending');")?;
        connections.push(ConnectionConfig {
            id: ConnectionId(id),
            name: label.into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path,
                read_only: true,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        });
        queries.push(SavedQuery {
            id,
            connection_id: ConnectionId(id),
            database: Some("main".into()),
            schema: None,
            name: "订单口径".into(),
            text: format!("SELECT status FROM orders WHERE id={id};"),
        });
    }
    storage.save_connections(&connections)?;
    storage.save_saved_queries(&queries)?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    // 模型端点指向本地 mock（默认 127.0.0.1:39107）；它产生真实的工具调用/回执闭环，
    // 不联网、不需要凭据。可用 FLUXDB_AI_MODEL_ENDPOINT 覆盖端口。
    let endpoint = std::env::var("FLUXDB_AI_MODEL_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:39107/v1".into());
    let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "离线验收模型".into(),
        provider: "openai".into(),
        model: "offline".into(),
        endpoint: Some(endpoint),
        enabled: true,
        context_window_tokens: Some(32768),
        max_output_tokens: Some(1024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => return Err(format!("模型夹具失败：{other:?}").into()),
    };
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "引用验收".into(),
        model_profile_id: Some(model),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { .. } => {}
        other => return Err(format!("Agent 夹具失败：{other:?}").into()),
    }
    std::fs::write(root.join("attachment.sql"), "SELECT status FROM orders;\n")?;
    std::fs::write(root.join("invalid.bin"), [0_u8, 255, 1])?;
    println!("隔离配置：{}", storage.root_dir().display());
    println!("使用 FLUXDB_AI_TEST_ROOT=<配置目录> FLUXDB_AI_E2E_SCRIPTED=1 启动桌面；不联网。");
    Ok(())
}
