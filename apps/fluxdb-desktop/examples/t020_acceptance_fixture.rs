//! T020 真实窗口验收的隔离 fixture:模型 + Agent + 多状态审批会话种子。
//!
//! 通过应用层 `AppCommand` 创建模型与 Agent,再通过存储层 API 预置三条审批会话:
//! - `t020-pending`  待审批(默认打开,用于批准/拒绝交互)
//! - `t020-denied`   已拒绝(验证拒绝后按钮消失且不可复活)
//! - `t020-approved` 已提交批准决定(仅验证状态回显，不代表工具执行成功)
//!
//! 全程不读生产配置库、不写凭据;模型端点指向 RFC 5737 确定不可路由。

use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_storage::ai_platform::{audits, conversation, sessions};
use fluxdb_storage::{FileStorage, Storage};

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("用法: t020_acceptance_fixture <root>");
    let root = std::path::PathBuf::from(root);
    let storage = FileStorage::new(root.clone());

    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));

    let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        id: None,
        display_name: "T020 验收模型".into(),
        provider: "openai".into(),
        model: "t020-acceptance-not-a-real-model".into(),
        endpoint: Some("http://192.0.2.1:9/v1".into()),
        endpoint_is_full_url: false,
        enabled: true,
        egress: true,
        context_window_tokens: Some(8192),
        max_output_tokens: Some(1024),
        input_price_micros: None,
        output_price_micros: None,
        cached_input_price_micros: None,
        api_key: None,
        clear_key: false,
        expected_revision: None,
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("模型保存失败: {other:?}"),
    };

    match controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(model_id.clone()),
        default_decision_model_id: None,
        expected_revision: 0,
    }) {
        AppEvent::AiDefaultModelsSaved { .. } => {}
        other => panic!("默认模型设置失败: {other:?}"),
    }

    let agent_id = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: None,
        display_name: "T020 验收 Agent".into(),
        description: "仅用于 T020 审批卡真实窗口验收".into(),
        system_prompt: "只能执行用户明确授权的写操作,每条写 SQL 都要请求审批。".into(),
        model_profile_id: Some(model_id.clone()),
        compression_trigger_percent: 80,
        allowed_tools: vec!["db.execute".into()],
        enabled: true,
        expected_revision: 0,
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("Agent 保存失败: {other:?}"),
    };

    seed_connection(&storage);

    seed_approval_session(
        &storage,
        &agent_id,
        "t020-pending",
        sessions::PartStatus::Pending,
        None,
    );
    seed_approval_session(
        &storage,
        &agent_id,
        "t020-denied",
        sessions::PartStatus::Complete,
        Some("denied"),
    );
    seed_approval_session(
        &storage,
        &agent_id,
        "t020-approved",
        sessions::PartStatus::Complete,
        Some("approved"),
    );
    // 面板打开的是 last_session_id；默认指向待审批会话，验收才能直接看到审批卡。
    storage
        .with_ai_connection(|conn| {
            let mut prefs = fluxdb_storage::ai_platform::settings::load_panel_preferences(conn)?;
            let revision = prefs.revision;
            prefs.last_session_id = Some("t020-pending".into());
            fluxdb_storage::ai_platform::settings::save_panel_preferences(
                conn,
                1_700_000_000_100,
                &prefs,
                revision,
            )?;
            Ok(())
        })
        .unwrap();

    println!(
        "FIXTURE_OK root={} model={model_id} agent={agent_id}",
        root.display()
    );
}

/// 审批卡上下文行要显示真实连接名/类型，所以 fixture 先建一条**不写凭据**的
/// 本地 SQLite 连接；审批卡只读连接名与可写标记，不会真的去拨这个连接。
fn seed_connection(storage: &FileStorage) {
    let connection = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "T020 验收库".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: storage.root_dir().join("t020-acceptance.db"),
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    storage
        .save_connections(&[connection])
        .expect("写入验收连接");
}

fn seed_approval_session(
    storage: &FileStorage,
    agent_id: &str,
    session: &str,
    part_status: sessions::PartStatus,
    decision: Option<&str>,
) {
    let now = 1_700_000_000_000_i64;
    let turn_id = format!("{session}-turn");
    let user_id = format!("{session}-user");
    let assistant_id = format!("{session}-assistant");
    // 与生产命名一致：审批决定回写按 `{assistant_message}-approval-{provider_call_id}` 定位原卡，
    // fixture 必须用同一约定，否则回写会新建一张卡而旧卡按钮不消失（假象，不是产品缺陷）。
    let provider_call_id = "provider-call-1";
    let tool_call_id = format!("{turn_id}-toolcall-{provider_call_id}");
    let part_id = format!("{assistant_id}-approval-{provider_call_id}");
    let approval_id = format!("{turn_id}-approval-{provider_call_id}");
    let plan_hash = format!("plan-hash-{session}");
    storage
        .with_ai_connection(|conn| {
            conversation::send(
                conn,
                now,
                true,
                session,
                agent_id,
                &turn_id,
                &user_id,
                "请把 id=1 的订单改成 paid",
                r#"{"connection_id":1,"database":"main","object":"orders"}"#,
                "{}",
            )?;
            sessions::create_tool_call(
                conn,
                now,
                &tool_call_id,
                session,
                &turn_id,
                1,
                0,
                provider_call_id,
                "db.execute",
                1,
                None,
                r#"{"target":"conn:1/db:main","sql":"UPDATE orders SET status='paid' WHERE id=1"}"#,
                r#"{"connection_id":1,"database":"main","object":"orders"}"#,
                &plan_hash,
                "waiting_approval",
            )?;
            let approval = audits::create_approval(
                conn,
                now,
                &approval_id,
                &tool_call_id,
                &plan_hash,
                "{}",
                None,
            )?;
            let mut payload = serde_json::json!({
                "text": "工具 db.execute 需要你先批准后才会执行。",
                "approval_id": approval_id,
                "plan_hash": plan_hash,
                "approval_revision": approval.revision,
                // 与生产 SQL 审批展示一致，卡片各区块才有真实内容可验。
                "details": serde_json::from_str::<serde_json::Value>(
                    &serde_json::to_string(&fluxdb_app::sql_approval_details(
                        "UPDATE orders SET status='paid' WHERE id=1",
                        fluxdb_app::SqlDialectKind::Sqlite,
                    )).unwrap()
                ).unwrap(),
            });
            if let Some(decision) = decision {
                payload["decision"] = serde_json::Value::from(decision);
                let decided = audits::decide_approval(
                    conn,
                    now + 1,
                    &approval_id,
                    decision == "approved",
                    None,
                    approval.revision,
                )?;
                payload["approval_revision"] = serde_json::json!(decided.revision);
            }
            conversation::upsert_event_part(
                conn,
                now,
                session,
                &turn_id,
                &assistant_id,
                &part_id,
                sessions::PartKind::Notice,
                part_status,
                &payload.to_string(),
                Some(&tool_call_id),
                None,
            )?;
            let turn = sessions::load_turn(conn, &turn_id)?;
            let target_status = if decision.is_some() {
                sessions::TurnStatus::Completed
            } else {
                sessions::TurnStatus::WaitingApproval
            };
            sessions::update_turn_status(
                conn,
                now + 2,
                &turn_id,
                target_status,
                None,
                turn.revision,
            )?;
            Ok(())
        })
        .unwrap_or_else(|err| panic!("seed {session} 失败: {err:?}"));
}
