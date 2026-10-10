//! Prepare an explicitly marked, isolated configuration copy for Redis GUI acceptance.
use fluxdb_app::{AiAgentSaveInput, AiConversationCommand, AppCommand, AppController, AppEvent};
use fluxdb_core::{ConnectionConfig, ConnectionId, DatabaseKind, Endpoint};
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{models, settings},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(std::env::var("FLUXDB_AI_GUI_ROOT")?);
    if !root.join("e2e-authorized-copy").is_file() {
        return Err("Requires an explicitly authorized isolated copy".into());
    }
    let port: u16 = std::env::var("FLUXDB_T025_REDIS_PORT")?.parse()?;
    let name = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "公司自建".into());
    let storage = FileStorage::new(&root);
    let model_id = storage.with_ai_connection(|conn| {
        models::list_models_page(conn, 200, 0)?
            .into_iter()
            .find(|model| {
                model.enabled
                    && model.display_name == name
                    && model.kind == models::ModelKind::Generation
            })
            .map(|model| model.id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "Requested model not found in copy",
                )
            })
    })?;
    let config = ConnectionConfig {
        id: ConnectionId(25),
        name: "T025 isolated Redis".into(),
        kind: DatabaseKind::Redis,
        endpoint: Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
            database: Some("0".into()),
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    fluxdb_connectors::connector_for(&config)?.test_connection(&config)?;
    storage.save_connections(&[config])?;
    let mut app_settings = storage.load_settings()?;
    app_settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&app_settings)?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let agent = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "T025 Redis acceptance".into(), model_profile_id: Some(model_id.clone()),
        enabled: true,
        system_prompt: "仅访问 conn:25/db:0 的 fluxdb:t025:gui:* 测试键。工具参数使用 target=conn:25/db:0。必须真实调用 scan、inspect，SET 写入须等待用户审批；拒绝后不得再申请或重试写入。只依据回执总结，不要调用 EVAL/CONFIG/ACL/FLUSH。".into(),
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => return Err(format!("Unable to configure Agent: {other:?}").into()),
    };
    storage.with_ai_connection(|conn| {
        let mut runtime = settings::load_runtime_settings(conn)?;
        runtime.default_agent_id = Some(agent.clone());
        runtime.default_model_profile_id = Some(model_id.clone());
        settings::save_runtime_settings(conn, 1_000, &runtime, runtime.revision)?;
        Ok(())
    })?;
    match controller.dispatch(AppCommand::AiConversation(AiConversationCommand::New)) {
        AppEvent::AiConversationLoaded(_) => {}
        other => return Err(format!("Unable to create isolated session: {other:?}").into()),
    }
    println!("Isolated Redis and selected model ready; only copy modified.");
    Ok(())
}
