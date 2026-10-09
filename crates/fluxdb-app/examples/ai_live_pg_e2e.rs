//! ③ 真实模型 + 真实 PostgreSQL 端到端（隔离副本存储；**不写用户原始配置库**）。
//!
//! 用法：
//! ```text
//! cp -R "$HOME/Library/Application Support/fluxdb" /private/tmp/fluxdb-live-e2e
//! FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_ROOT=/private/tmp/fluxdb-live-e2e \
//!   FLUXDB_AI_LIVE_MODEL='Zhipu GLM' FLUXDB_AI_LIVE_PG=1 \
//!   cargo run -p fluxdb-app --example ai_live_pg_e2e
//! ```
//! 凭据只在进程内解密后交给 connector，绝不打印。
use fluxdb_app::*;
use fluxdb_core::{CancelFlag, ConnectionId, ModelRole, QueryMode, QueryRequest};
use fluxdb_storage::Storage as _;

const SCHEMA: &str = "fluxdb_ai_smoke";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err(
            "需要 FLUXDB_AI_LIVE=1（真实模型与真实数据库，会计费/会写入测试 schema）".into(),
        );
    }
    let root = std::env::var("FLUXDB_AI_LIVE_ROOT")
        .map_err(|_| "需要 FLUXDB_AI_LIVE_ROOT 指向用户配置库的隔离副本")?;
    let model_name = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "Zhipu GLM".into());
    let connection_id: u64 = std::env::var("FLUXDB_AI_LIVE_PG_CONNECTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let storage = fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));

    // 1) 连接与凭据：`load_connections` 内部走宿主自己的解密注入（password 仅存在于内存 config，绝不打印）。
    let config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id == ConnectionId(connection_id))
        .ok_or("找不到指定连接")?;
    if config.kind != fluxdb_core::DatabaseKind::Postgres {
        return Err("该连接不是 PostgreSQL".into());
    }
    let connector = fluxdb_connectors::connector_for(&config)?;
    let run_sql = |text: &str| -> fluxdb_core::Result<()> {
        connector.execute(&QueryRequest {
            connection_id: ConnectionId(connection_id),
            database: None,
            schema: None,
            text: text.to_string(),
            mode: QueryMode::All,
            options: fluxdb_core::QueryExecutionOptions {
                continue_on_error: false,
                split_statements: true,
                page_offset: 0,
                page_size: 100,
            },
            session_id: None,
        })?;
        Ok(())
    };
    let query_rows = |text: &str| -> fluxdb_core::Result<Vec<String>> {
        let result = connector.execute(&QueryRequest {
            connection_id: ConnectionId(connection_id),
            database: None,
            schema: None,
            text: text.to_string(),
            mode: QueryMode::All,
            options: fluxdb_core::QueryExecutionOptions {
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

    println!("① 建立隔离 fixture（schema={SCHEMA}）");
    run_sql(&format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA}"))?;
    run_sql(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA}.orders (id integer primary key, status text not null, amount numeric(10,2) not null)"
    ))?;
    run_sql(&format!("DELETE FROM {SCHEMA}.orders"))?;
    run_sql(&format!(
        "INSERT INTO {SCHEMA}.orders (id, status, amount) VALUES (1,'pending',100.50),(2,'paid',250.00)"
    ))?;
    run_sql(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA}.write_audit(id integer)"
    ))?;
    run_sql(&format!("TRUNCATE TABLE {SCHEMA}.write_audit"))?;
    run_sql(&format!(
        "CREATE OR REPLACE FUNCTION {SCHEMA}.track_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO {SCHEMA}.write_audit VALUES(NEW.id); RETURN NEW; END $$"
    ))?;
    run_sql(&format!(
        "DROP TRIGGER IF EXISTS track_write ON {SCHEMA}.orders"
    ))?;
    run_sql(&format!(
        "CREATE TRIGGER track_write AFTER UPDATE ON {SCHEMA}.orders FOR EACH ROW EXECUTE FUNCTION {SCHEMA}.track_write()"
    ))?;
    let before = query_rows(&format!(
        "SELECT id, status FROM {SCHEMA}.orders ORDER BY id"
    ))?;
    println!("   写入前: {before:?}");

    // 2) 模型与 Agent（写入隔离副本，不动用户原库）。
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
                    format!("未找到可用的生成模型: {model_name}"),
                )
            })
    })?;
    let provider = AiConversationProvider::from_storage(&storage, &model_id)?;
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let agent_id = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        display_name: "live PG E2E Agent".into(),
        description: "真实模型 + 真实 PostgreSQL 验收".into(),
        system_prompt: format!(
            "你是数据库 Agent。当前测试 schema 是 {SCHEMA}，表 {SCHEMA}.orders(id,status,amount)。\n\
             请使用提供的工具完成用户要求，并依据真实回执回答；不要编造。"
        ),
        model_profile_id: Some(model_id.clone()),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => return Err(format!("保存 Agent 失败: {other:?}").into()),
    };

    let catalog = standard_sql_catalog(
        std::sync::Arc::new(MapSqlResolver::new([config.clone()])),
        true,
    )?;
    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: kernel_policy_for_connection(Some(&config)),
        plugin_digest: None,
    };
    let target = TurnTargetPin {
        target_json: TargetRef {
            connection_id: Some(connection_id as i64),
            database: None,
            schema: Some(SCHEMA.into()),
            object: Some("orders".into()),
            redis_db: None,
        }
        .to_json(),
        target: TargetRef {
            connection_id: Some(connection_id as i64),
            database: None,
            schema: Some(SCHEMA.into()),
            object: Some("orders".into()),
            redis_db: None,
        },
        captured_tab_id: None,
        captured_tab_revision: 0,
    };

    // 3) 真实模型读取回合。
    println!("② 真实模型读回合（db.query）");
    let job = controller.prepare_ai_conversation_context(
        &format!("请用工具查询 {SCHEMA}.orders 的所有行（id 和 status），然后一句话总结。"),
        &target,
        "",
    )?;
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
    );
    let view = drain_terminal(&rx);
    let read_calls = calls(&storage, &job.turn_id);
    for call in &read_calls {
        println!(
            "   call tool={} status={} receipt={}",
            call.tool_id,
            call.status,
            call.receipt_json
                .clone()
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect::<String>()
        );
    }
    let answer = assistant_text(&view);
    println!("   模型回答: {answer:?}");
    if !read_calls.iter().any(|call| call.status == "succeeded") {
        return Err("真实模型没有成功执行读取工具".into());
    }

    // 4) 真实模型写入回合 → 必须停在审批。
    println!("③ 真实模型写回合（db.execute → 审批）");
    let job2 = controller.prepare_ai_conversation_context(
        // 当前执行链尚未支持参数化绑定（连接器没有 bind，宁可拒绝也不做字符串拼接），
        // 因此显式要求模型使用内联字面量；这条限制本身是产品现状，不是测试作弊。
        &format!(
            "请用 db.execute 工具把 {SCHEMA}.orders 里 id=1 的 status 改成 'paid'。             不要使用 parameters 参数（当前执行链不支持参数绑定），请直接使用内联字面量。"
        ),
        &target,
        "",
    )?;
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &storage,
        &job2,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
    );
    let _ = drain_terminal(&rx);
    let pending = calls(&storage, &job2.turn_id);
    let write_call = pending
        .iter()
        .find(|call| call.tool_id == "db.execute")
        .ok_or("真实模型没有请求写工具")?
        .clone();
    if write_call.receipt_json.is_some() || write_call.status != "waiting_approval" {
        return Err(format!("写调用必须先停在审批，实际 status={}", write_call.status).into());
    }
    let mid = query_rows(&format!(
        "SELECT id, status FROM {SCHEMA}.orders ORDER BY id"
    ))?;
    println!("   审批前数据（必须未变）: {mid:?}");
    if mid != before {
        return Err("审批前数据被改动".into());
    }
    let denied_approval = storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::audits::list_approvals_for_call(conn, &write_call.id)
        })?
        .into_iter()
        .next()
        .ok_or("缺少待拒绝审批")?;
    let denied = controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: denied_approval.id,
            approved: false,
            expected_revision: denied_approval.revision,
        },
    ));
    if !matches!(denied, AppEvent::AiConversationLoaded(_))
        || query_rows(&format!(
            "SELECT id, status FROM {SCHEMA}.orders ORDER BY id"
        ))? != before
        || query_rows(&format!("SELECT COUNT(*) FROM {SCHEMA}.write_audit"))? != ["0"]
    {
        return Err("拒绝审批后数据或实际副作用次数异常".into());
    }
    println!("   拒绝审批：数据不变，数据库触发器计数=0");
    let job2 = controller.prepare_ai_conversation_context(
        &format!("重新生成写计划。用 db.execute 把 {SCHEMA}.orders 中 id=1 的 status 改为 'paid'，使用内联字面量，不使用 parameters。这是新计划，不复用已拒绝审批。"),
        &target,
        "",
    )?;
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &storage,
        &job2,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
    );
    let _ = drain_terminal(&rx);
    let write_call = calls(&storage, &job2.turn_id)
        .into_iter()
        .find(|call| call.tool_id == "db.execute")
        .ok_or("缺少重新生成的写调用")?;
    if write_call.status != "waiting_approval"
        || write_call.receipt_json.is_some()
        || query_rows(&format!("SELECT COUNT(*) FROM {SCHEMA}.write_audit"))? != ["0"]
    {
        return Err("重新生成的写没有在审批前停住".into());
    }

    // 服务层审批不等于桌面点击；真实窗口流程另行验收。
    println!("④ 批准并恢复执行");
    let approvals = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::audits::list_approvals_for_call(conn, &write_call.id)
    })?;
    let approval = approvals.first().ok_or("没有生成审批项")?.clone();
    match controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: approval.revision,
        },
    )) {
        AppEvent::AiConversationLoaded(_) => {}
        other => return Err(format!("批准失败: {other:?}").into()),
    }
    let resume_job = controller.prepare_ai_resume_job(&approval.id)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &storage,
        &resume_job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission.clone(),
        &approval.id,
    );
    let view2 = drain_terminal(&rx);
    let after_calls = calls(&storage, &job2.turn_id);
    for call in &after_calls {
        println!(
            "   call tool={} status={} receipt={}",
            call.tool_id,
            call.status,
            call.receipt_json
                .clone()
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect::<String>()
        );
    }
    let after = query_rows(&format!(
        "SELECT id, status FROM {SCHEMA}.orders ORDER BY id"
    ))?;
    println!("   审批后数据: {after:?}");
    println!("   模型继续回答: {:?}", assistant_text(&view2));
    let executed = after_calls
        .iter()
        .find(|call| call.tool_id == "db.execute")
        .ok_or("缺少写调用事实行")?;
    if executed.status != "succeeded" || executed.receipt_json.is_none() {
        return Err(format!("写调用未成功执行: status={}", executed.status).into());
    }
    let receipt: serde_json::Value = serde_json::from_str(executed.receipt_json.as_ref().unwrap())?;
    if after != ["1|paid", "2|paid"]
        || receipt["affected_rows"] != 1
        || query_rows(&format!("SELECT COUNT(*) FROM {SCHEMA}.write_audit"))? != ["1"]
    {
        return Err("写入后行值、affected_rows 或数据库副作用次数不正确".into());
    }
    if after_calls
        .iter()
        .filter(|call| call.tool_id == "db.execute")
        .count()
        != 1
    {
        return Err("写调用被重复执行".into());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_resume_approval(
        &storage,
        &resume_job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission,
        &approval.id,
    );
    let _ = drain_terminal(&rx);
    if query_rows(&format!("SELECT COUNT(*) FROM {SCHEMA}.write_audit"))? != ["1"] {
        return Err("重复恢复导致数据库重复写入".into());
    }
    println!(
        "✅ 真实模型 + PostgreSQL：读取、拒绝不写、重新批准写入、affected_rows=1、重复恢复后实际写入次数=1"
    );

    println!("⑤ 清理 fixture");
    run_sql(&format!("DROP SCHEMA {SCHEMA} CASCADE"))?;
    let _ = agent_id;
    Ok(())
}

fn drain_terminal(
    rx: &std::sync::mpsc::Receiver<fluxdb_app::AiConversationUpdate>,
) -> fluxdb_app::AiConversationView {
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = update.view;
        }
    }
    last.unwrap_or_default()
}

fn calls(
    storage: &fluxdb_storage::FileStorage,
    turn_id: &str,
) -> Vec<fluxdb_storage::ai_platform::sessions::ToolCallView> {
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::sessions::list_tool_calls_for_turn(conn, turn_id)
        })
        .unwrap_or_default()
}

fn assistant_text(view: &fluxdb_app::AiConversationView) -> String {
    view.messages
        .iter()
        .filter(|message| message.role == "assistant")
        .flat_map(|message| message.parts.iter())
        .filter(|part| part.kind == "text")
        .filter_map(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
        .filter_map(|value| {
            value
                .get("text")
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        })
        .last()
        .unwrap_or_default()
}

const _: fn() = || {
    let _ = ModelRole::User;
};
