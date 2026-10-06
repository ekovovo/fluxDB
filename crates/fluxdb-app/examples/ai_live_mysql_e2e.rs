//! A：真实模型 + 真实 MySQL 端到端（读 → 审批 → 写），并让模型**使用 parameters** 验证绑定。
//!
//! `FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_ROOT=<配置库副本> FLUXDB_AI_LIVE_MODEL='Zhipu GLM' \
//!  FLUXDB_AI_LIVE_MYSQL_CONNECTION=2 cargo run -p fluxdb-app --example ai_live_mysql_e2e`
//! 只写专用库 `fluxdb_ai_smoke`，结束 DROP DATABASE；凭据只走宿主解密路径，不打印。
use fluxdb_app::*;
use fluxdb_core::{CancelFlag, ConnectionId, DatabaseKind, QueryExecutionOptions, QueryMode, QueryRequest};
use fluxdb_storage::Storage as _;

const DB: &str = "fluxdb_ai_smoke";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要 FLUXDB_AI_LIVE=1（真实模型与真实数据库）".into());
    }
    let root = std::env::var("FLUXDB_AI_LIVE_ROOT")
        .map_err(|_| "需要 FLUXDB_AI_LIVE_ROOT 指向配置库的隔离副本")?;
    let model_name = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "Zhipu GLM".into());
    let connection_id: u64 = std::env::var("FLUXDB_AI_LIVE_MYSQL_CONNECTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let storage = fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));
    let config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id == ConnectionId(connection_id))
        .ok_or("找不到 MySQL 连接")?;
    if config.kind != DatabaseKind::MySql {
        return Err("该连接不是 MySQL".into());
    }
    let connector = fluxdb_connectors::connector_for(&config)?;
    connector.test_connection(&config)?;
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
    let rows = |text: &str| -> fluxdb_core::Result<Vec<String>> {
        let result = connector.execute(&QueryRequest {
            connection_id: ConnectionId(connection_id),
            database: Some(DB.to_string()),
            schema: None,
            text: text.to_string(),
            mode: QueryMode::All,
            options: QueryExecutionOptions {
                continue_on_error: false,
                split_statements: false,
                page_offset: 0,
                page_size: 100,
            },
            session_id: None,
        })?;
        Ok(result
            .results
            .iter()
            .flat_map(|page| page.rows.iter())
            .map(|row| {
                row.values
                    .iter()
                    .map(fluxdb_core::CellValue::display_label)
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect())
    };

    println!("① 建立隔离 fixture（MySQL 库 {DB}）");
    run(&format!("CREATE DATABASE IF NOT EXISTS {DB}"))?;
    run(&format!(
        "CREATE TABLE IF NOT EXISTS {DB}.orders (id BIGINT PRIMARY KEY, status VARCHAR(32) NOT NULL, amount DECIMAL(10,2) NOT NULL)"
    ))?;
    run(&format!("DELETE FROM {DB}.orders"))?;
    run(&format!(
        "INSERT INTO {DB}.orders (id, status, amount) VALUES (1,'pending',100.50),(2,'paid',250.00)"
    ))?;
    let before = rows("SELECT id, status FROM orders ORDER BY id")?;
    println!("   写入前: {before:?}");

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
    let provider = AiConversationProvider::from_storage(&storage, &model_id)?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "live MySQL E2E Agent".into(),
        description: "真实模型 + 真实 MySQL".into(),
        system_prompt: format!(
            "你是数据库 Agent。测试库是 {DB}，表 {DB}.orders(id,status,amount)。\
             请用工具完成用户要求并依据真实回执回答；写数据可用 db.execute，\
             MySQL 的占位符是 ?，可以放在 parameters 里使用。"
        ),
        model_profile_id: Some(model_id.clone()),
        enabled: true,
        ..Default::default()
    }));

    let catalog = standard_sql_catalog(
        std::sync::Arc::new(MapSqlResolver::new([config.clone()])),
        true,
    )?;
    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: kernel_policy_for_connection(Some(&config)),
        plugin_digest: None,
    };
    let target_ref = TargetRef {
        connection_id: Some(connection_id as i64),
        database: Some(DB.into()),
        schema: None,
        object: Some("orders".into()),
        redis_db: None,
    };
    let target = TurnTargetPin {
        target_json: target_ref.to_json(),
        target: target_ref,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };

    println!("② 真实模型读回合（db.query）");
    let job = controller.prepare_ai_conversation_context(
        &format!("请用工具查询 {DB}.orders 的两行（id 和 status），然后一句话总结。"),
        &target,
        "",
    )?;
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(&storage, &job, &cancel, &tx, &provider, &catalog, &KernelAuditHook, permission.clone());
    let view = drain(&rx);
    for call in calls(&storage, &job.turn_id) {
        println!("   call tool={} status={} receipt={}", call.tool_id, call.status,
            call.receipt_json.clone().unwrap_or_default().chars().take(120).collect::<String>());
    }
    println!("   模型回答: {:?}", assistant_text(&view));

    println!("③ 真实模型写回合（db.execute + parameters → 审批）");
    let job2 = controller.prepare_ai_conversation_context(
        &format!("请用 db.execute 把 {DB}.orders 里 id=1 的 status 改成 'paid'。\
                  请使用 parameters 做绑定（? 占位符），不要写在 SQL 字符串里。"),
        &target,
        "",
    )?;
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(&storage, &job2, &cancel, &tx, &provider, &catalog, &KernelAuditHook, permission.clone());
    let _ = drain(&rx);
    let pending = calls(&storage, &job2.turn_id);
    let write_call = pending.iter().find(|call| call.tool_id == "db.execute").ok_or("模型没有请求写工具")?.clone();
    let mid = rows("SELECT id, status FROM orders ORDER BY id")?;
    println!("   审批前数据（必须未变）: {mid:?} status={}", write_call.status);
    if mid != before {
        return Err("审批前数据被改动".into());
    }

    println!("④ 批准并恢复执行");
    let approvals = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::audits::list_approvals_for_call(conn, &write_call.id)
    })?;
    let approval = approvals.first().ok_or("没有生成审批项")?.clone();
    match controller.dispatch(AppCommand::AiConversation(AiConversationCommand::DecideApproval {
        approval_id: approval.id.clone(),
        approved: true,
        expected_revision: approval.revision,
    })) {
        AppEvent::AiConversationLoaded(_) => {}
        other => return Err(format!("批准失败: {other:?}").into()),
    }
    let resume_job = controller.prepare_ai_resume_job(&approval.id)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(&storage, &resume_job, &cancel, &tx, &provider, &catalog,
        &KernelAuditHook, permission, &approval.id);
    let view2 = drain(&rx);
    for call in calls(&storage, &job2.turn_id) {
        println!("   call tool={} status={} receipt={}", call.tool_id, call.status,
            call.receipt_json.clone().unwrap_or_default().chars().take(140).collect::<String>());
    }
    let after = rows("SELECT id, status FROM orders ORDER BY id")?;
    println!("   审批后数据: {after:?}");
    println!("   模型继续回答: {:?}", assistant_text(&view2));
    let executed = calls(&storage, &job2.turn_id)
        .into_iter()
        .find(|call| call.tool_id == "db.execute")
        .ok_or("缺少写调用事实行")?;
    if executed.status != "succeeded" || executed.receipt_json.is_none() {
        return Err(format!("写调用未成功执行: status={}", executed.status).into());
    }
    if after == before {
        return Err("写操作没有真正生效（数据未变）".into());
    }
    println!("✅ 真实模型 + 真实 MySQL：读取 → 审批 → 写入一次 全部通过");

    println!("⑤ 清理 fixture");
    run(&format!("DROP DATABASE IF EXISTS {DB}"))?;
    Ok(())
}

fn drain(rx: &std::sync::mpsc::Receiver<AiConversationUpdate>) -> AiConversationView {
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = Some(update.view);
        }
    }
    last.unwrap_or_default()
}

fn calls(storage: &fluxdb_storage::FileStorage, turn_id: &str) -> Vec<fluxdb_storage::ai_platform::sessions::ToolCallView> {
    storage
        .with_ai_connection(|conn| fluxdb_storage::ai_platform::sessions::list_tool_calls_for_turn(conn, turn_id))
        .unwrap_or_default()
}

fn assistant_text(view: &AiConversationView) -> String {
    view.messages
        .iter()
        .filter(|message| message.role == "assistant")
        .flat_map(|message| message.parts.iter())
        .filter(|part| part.kind == "text")
        .filter_map(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
        .filter_map(|value| value.get("text").and_then(|text| text.as_str()).map(str::to_owned))
        .last()
        .unwrap_or_default()
}
