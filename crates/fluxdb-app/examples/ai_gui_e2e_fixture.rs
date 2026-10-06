//! D（真实窗口 GUI E2E）夹具：在**隔离配置库副本**里准备 Agent，并在真实 PG 上建测试 fixture。
//!
//! `FLUXDB_AI_LIVE=1 FLUXDB_AI_GUI_ROOT=<副本> FLUXDB_AI_LIVE_MODEL='Zhipu GLM' \
//!  FLUXDB_AI_LIVE_PG_CONNECTION=1 cargo run -p fluxdb-app --example ai_gui_e2e_fixture`
use fluxdb_app::*;
use fluxdb_core::{ConnectionId, DatabaseKind, QueryExecutionOptions, QueryMode, QueryRequest};
use fluxdb_storage::Storage as _;

const SCHEMA: &str = "fluxdb_ai_smoke";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要 FLUXDB_AI_LIVE=1".into());
    }
    let root = std::env::var("FLUXDB_AI_GUI_ROOT")
        .map_err(|_| "需要 FLUXDB_AI_GUI_ROOT 指向配置库的隔离副本")?;
    let model_name = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "Zhipu GLM".into());
    let connection_id: u64 = std::env::var("FLUXDB_AI_LIVE_PG_CONNECTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let storage = fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));
    let config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id == ConnectionId(connection_id))
        .ok_or("找不到 PG 连接")?;
    if config.kind != DatabaseKind::Postgres {
        return Err("该连接不是 PostgreSQL".into());
    }

    // 真实 PG fixture（只写专用 schema）。
    let connector = fluxdb_connectors::connector_for(&config)?;
    let run = |text: &str| -> fluxdb_core::Result<()> {
        connector.execute(&QueryRequest {
            connection_id: ConnectionId(connection_id),
            database: None,
            schema: None,
            text: text.to_string(),
            mode: QueryMode::All,
            options: QueryExecutionOptions {
                continue_on_error: false,
                split_statements: true,
                page_offset: 0,
                page_size: 100,
            },
            session_id: None,
        })?;
        Ok(())
    };
    run(&format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA}"))?;
    run(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA}.orders (id integer primary key, status text not null, amount numeric(10,2) not null)"
    ))?;
    run(&format!("DELETE FROM {SCHEMA}.orders"))?;
    run(&format!(
        "INSERT INTO {SCHEMA}.orders (id, status, amount) VALUES (1,'pending',100.50),(2,'paid',250.00)"
    ))?;
    println!("PG fixture 就绪：{SCHEMA}.orders = 1|pending, 2|paid");

    // Agent（写入隔离副本）。
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let model_id = storage.with_ai_connection(|conn| {
        let models = fluxdb_storage::ai_platform::models::list_models_page(conn, 200, 0)?;
        models
            .into_iter()
            .find(|model| {
                model.enabled
                    && (model.display_name == model_name || model.model == model_name)
                    && model.kind == fluxdb_storage::ai_platform::models::ModelKind::Generation
            })
            .map(|model| model.id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("未找到可用生成模型: {model_name}"),
                )
            })
    })?;
    let existing = storage.with_ai_connection(|conn| fluxdb_storage::ai_platform::agents::list_agents(conn))?;
    if existing.iter().any(|agent| agent.enabled) {
        println!("已有启用 Agent，保持不动（{} 个）", existing.len());
    } else {
        match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "GUI E2E Agent".into(),
            description: "真实窗口验收".into(),
            system_prompt: format!(
                "你是数据库 Agent。测试 schema 是 {SCHEMA}，表 {SCHEMA}.orders(id,status,amount)。\
                 请用工具完成用户要求，并依据真实回执回答；需要写数据时使用 db.execute 并直接写内联字面量。"
            ),
            model_profile_id: Some(model_id),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { id, .. } => println!("已创建 Agent: {id}"),
            other => return Err(format!("保存 Agent 失败: {other:?}").into()),
        }
    }
    Ok(())
}
