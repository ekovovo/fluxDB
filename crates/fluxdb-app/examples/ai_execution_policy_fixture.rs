//! 创建一次性的桌面 / stdio 审批验收数据；拒绝复用已有目录。
use fluxdb_app::{
    AiMcpDatasourceSaveInput, AiMcpServerProfileSaveInput, AppCommand, AppController, AppEvent,
};
use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, McpWritePolicy};
use fluxdb_storage::{FileStorage, Storage};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::args_os().nth(1).ok_or("需要新的隔离目录")?);
    if root.exists() {
        return Err("拒绝覆盖已有目录".into());
    }
    std::fs::create_dir_all(&root)?;
    let storage = FileStorage::new(&root);
    let path = root.join("policy-demo.sqlite");
    rusqlite::Connection::open(&path)?.execute_batch(
        "CREATE TABLE items(id INTEGER PRIMARY KEY,value TEXT);INSERT INTO items VALUES(1,'before'),(2,'unchanged');")?;
    storage
        .save_connections(&[ConnectionConfig {
            id: ConnectionId(1),
            name: "审批验收 · 本地 SQLite".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path,
                read_only: false,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }])
        .map_err(|_| "无法写入隔离连接")?;
    storage
        .with_ai_connection(|conn| {
            let mut settings = fluxdb_core::ExecutionPolicySettings::default();
            settings.mcp_default = McpWritePolicy::ReadWrite;
            fluxdb_storage::ai_platform::execution_policy::save(conn, 1, &settings, 0)?;
            Ok(())
        })
        .map_err(|_| "无法初始化隔离策略")?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    if let AppEvent::AiSettingsError(message) = controller.dispatch(AppCommand::EnsureMcpControl) {
        return Err(message.into());
    }
    let profile = controller
        .state()
        .ai_mcp_server_profiles
        .iter()
        .find(|p| p.id == fluxdb_app::DEFAULT_MCP_PROFILE_ID)
        .unwrap();
    let input = AiMcpServerProfileSaveInput {
        id: Some(profile.id.clone()),
        display_name: "本地 MCP".into(),
        enabled: true,
        expected_revision: Some(profile.revision),
        ..Default::default()
    };
    if let AppEvent::AiSettingsError(message) =
        controller.dispatch(AppCommand::SaveAiMcpServerProfile(input))
    {
        return Err(message.into());
    }
    if let AppEvent::AiSettingsError(message) =
        controller.dispatch(AppCommand::SaveAiMcpDatasource(AiMcpDatasourceSaveInput {
            profile_id: fluxdb_app::DEFAULT_MCP_PROFILE_ID.into(),
            connection_id: 1,
            write_policy: "read_write".into(),
            ..Default::default()
        }))
    {
        return Err(message.into());
    }
    println!("隔离验收数据已创建；没有模型、凭据、外部数据库或自动客户端注册。");
    Ok(())
}
