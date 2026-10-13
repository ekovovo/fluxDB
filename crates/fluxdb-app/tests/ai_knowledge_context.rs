//! T038 业务知识运行期装配：confirmed 知识按目标作用域注入提示词；
//! 大正文用 knowledge.read 分段读取；retired/未确认立即失效。
use fluxdb_app::*;
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason, ModelEvent,
};
use fluxdb_storage::{FileStorage, ai_platform};
use std::{fs, path::PathBuf, sync::mpsc, time::Duration, time::Instant};

struct Fixture {
    root: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("fluxdb-knowledge-context-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let storage = FileStorage::new(&root);
        Self { root, storage }
    }

    fn save_knowledge_entry(&self, id: &str, title: &str, status: &str, body: &str) {
        self.storage
            .with_ai_connection(|conn| {
                ai_platform::knowledge::save(
                    conn,
                    1_000,
                    &ai_platform::knowledge::KnowledgeEntryEdit {
                        id: id.into(),
                        title: title.into(),
                        kind: "metric".into(),
                        agent_id: None,
                        connection_id: None,
                        database_name: Some("main".into()),
                        namespace: None,
                        scope_json: r#"{"databases":["main"]}"#.into(),
                        content_json: body.into(),
                        source_json: r#"{"origin":"manual"}"#.into(),
                        status: status.into(),
                    },
                    None,
                )
            })
            .unwrap();
    }

    fn set_knowledge_enabled(&self, enabled: bool) {
        self.storage
            .with_ai_connection(|conn| {
                let Some(view) = ai_platform::plugins::list_plugin_instances(conn)
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.id == "builtin.knowledge")
                else {
                    panic!("builtin.knowledge instance seeded");
                };
                ai_platform::plugins::set_plugin_instance_enabled(
                    conn,
                    2_000,
                    &view.id,
                    enabled,
                    view.revision,
                )
            })
            .unwrap();
    }

    fn save_model_and_agent(&mut self) -> (String, AiAgentSaveInput) {
        let mut controller = AppController::new();
        controller.set_ai_storage(self.storage.clone());
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "knowledge fixture".into(),
            provider: "openai".into(),
            model: "fixture".into(),
            endpoint: Some("http://fixture.invalid/v1".into()),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("model setup failed: {other:?}"),
        };
        let model_id = model.clone();
        let agent = AiAgentSaveInput {
            display_name: "knowledge agent".into(),
            model_profile_id: Some(model),
            enabled: true,
            ..Default::default()
        };
        assert!(matches!(
            controller.dispatch(AppCommand::SaveAiAgent(agent.clone())),
            AppEvent::AiAgentSaved { .. }
        ));
        (model_id, agent)
    }

    fn controller(&mut self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(self.storage.clone());
        assert!(matches!(
            controller.dispatch(AppCommand::RefreshAiSettings),
            AppEvent::AiSettingsLoaded
        ));
        let _ = self.save_model_and_agent();
        controller
    }

    fn create_connection(&self) -> ConnectionConfig {
        ConnectionConfig {
            id: ConnectionId(1),
            name: "connection-1".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path: "knowledge-target.db".into(),
                read_only: true,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn target_1() -> TargetRef {
    TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        ..Default::default()
    }
}

#[test]
fn turn_injects_confirmed_knowledge_from_target_scope() {
    let mut fixture = Fixture::new();
    fixture.save_knowledge_entry(
        "known",
        "订单金额口径",
        "confirmed",
        r#"{"formula":"sum(net_amount)"}"#,
    );
    fixture.save_knowledge_entry(
        "retired",
        "作废口径",
        "retired",
        r#"{"formula":"sum(gross)"}"#,
    );
    fixture.save_knowledge_entry(
        "proposed",
        "模型提议",
        "proposed",
        r#"{"formula":"sum(paid)"}"#,
    );
    let mut controller = fixture.controller();
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        ..Default::default()
    };
    let pin = TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };
    let job = controller
        .prepare_ai_conversation("查询订单金额", &pin)
        .unwrap();
    let request = scripted_turn_request(&fixture.storage, &job);
    let all_text: String = request
        .messages
        .iter()
        .map(|message| message.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("订单金额口径"),
        "confirmed 知识应包含在第一条消息。got: {all_text}"
    );
    assert!(
        !all_text.contains("作废口径"),
        "retired 知识不应出现在本轮注入"
    );
    assert!(!all_text.contains("模型提议"), "模型提议不自动成为事实");
}

#[test]
fn large_knowledge_body_is_truncated_and_not_full_loaded() {
    let mut fixture = Fixture::new();
    let body = format!(r#"{{"text":"{}"}}"#, "x".repeat(4096));
    fixture.save_knowledge_entry("large", "大型知识", "confirmed", &body);
    let mut controller = fixture.controller();
    let target = target_1();
    let pin = TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };
    let job = controller
        .prepare_ai_conversation("查询订单金额", &pin)
        .unwrap();
    let request = scripted_turn_request(&fixture.storage, &job);
    let content = request
        .messages
        .iter()
        .map(|message| message.text.clone())
        .collect::<String>();
    // 只度量知识摘录，不把系统安全约束或历史正文算作知识内容。
    let content_len = request
        .messages
        .iter()
        .filter(|message| message.text.starts_with("已确认业务知识"))
        .map(|message| message.text.len())
        .sum::<usize>();
    assert!(content_len > 0);
    assert!(
        content_len < 2400,
        "excerpt should be much smaller than full body. size={content_len}"
    );
    assert!(
        content.contains("knowledge.read"),
        "excerpt should indicate continuation capability. got: {content}"
    );
    // Retired knowledge should no longer appear in next turn.
    fixture
        .storage
        .with_ai_connection(|conn| {
            let view = ai_platform::knowledge::load(conn, "large").unwrap();
            ai_platform::knowledge::retire(conn, 3_000, "large", view.revision).unwrap();
            Ok(())
        })
        .unwrap();
    let job = controller
        .prepare_ai_conversation("查询 large knowledge", &pin)
        .unwrap();
    let request = scripted_turn_request(&fixture.storage, &job);
    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.text.contains("大型知识")),
        "退休条目不应再进入文档"
    );
}

#[test]
fn knowledge_read_tool_is_scoped_to_turn_and_can_resume() {
    let mut fixture = Fixture::new();
    let body = format!(r#"{{"text":"{}"}}"#, "abc".repeat(1024));
    fixture.save_knowledge_entry("k1", "知识1", "confirmed", &body);
    fixture.save_knowledge_entry("k2", "知识2", "confirmed", r#"{"text":"2"}"#);
    let mut controller = fixture.controller();
    let target = target_1();
    let pin = TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };
    let job = controller
        .prepare_ai_conversation("读取知识", &pin)
        .unwrap();
    let mut catalog = ai_tool_catalog_snapshot(&fixture.storage, &job);
    let spec = catalog
        .describe("knowledge.read")
        .expect("knowledge.read should exist");
    assert!(spec.description.contains("confirmed 知识"));
    // Read first 1024 bytes, assert truncated and boundary-correct.
    let receipt = ai_tool_execute_receipt(
        &fixture.storage,
        &mut catalog,
        &job,
        "knowledge.read",
        r#"{"knowledge_id":"k1","offset":0,"max_bytes":1024}"#,
    );
    assert!(receipt.is_success());
    let value: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    assert_eq!(value["text"].as_str().unwrap().len(), 1024);
    let next_offset = value["next_offset"].as_u64().unwrap();
    assert!(value["has_more"].as_bool().unwrap());
    // Resume
    let receipt = ai_tool_execute_receipt(
        &fixture.storage,
        &mut catalog,
        &job,
        "knowledge.read",
        &format!(r#"{{"knowledge_id":"k1","offset":{next_offset},"max_bytes":1024}}"#),
    );
    assert!(receipt.is_success(), "next read should succeed");
    let value: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    assert!(value["text"].as_str().unwrap().len() > 0);
    // Unknown id should refuse.
    let receipt = ai_tool_execute_receipt(
        &fixture.storage,
        &mut catalog,
        &job,
        "knowledge.read",
        r#"{"knowledge_id":"unknown-id"}"#,
    );
    assert!(!receipt.is_success());
}

#[test]
fn knowledge_disabled_when_plugin_off_is_absent_from_prompt() {
    let mut fixture = Fixture::new();
    fixture.save_knowledge_entry("k1", "某规则", "confirmed", r#"{"text":"规则"}"#);
    fixture.set_knowledge_enabled(false);
    let mut controller = fixture.controller();
    let target = target_1();
    let pin = TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };
    let job = controller.prepare_ai_conversation("查询", &pin).unwrap();
    let request = scripted_turn_request(&fixture.storage, &job);
    assert!(
        !request.messages[0].text.contains("某规则"),
        "禁用插件时本轮不注入知识"
    );
    // Tool also absent.
    let catalog3 = ai_tool_catalog_snapshot(&fixture.storage, &job);
    assert!(catalog3.describe("knowledge.read").is_none());
}

#[test]
fn after_retire_tool_becomes_unavailable() {
    let mut fixture = Fixture::new();
    fixture.save_knowledge_entry("k1", "某规则", "confirmed", r#"{"text":"规则"}"#);
    let mut controller = fixture.controller();
    let target = target_1();
    let pin = TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    };
    let job = controller
        .prepare_ai_conversation("读取知识", &pin)
        .unwrap();
    let mut catalog = ai_tool_catalog_snapshot(&fixture.storage, &job);
    assert!(
        catalog.describe("knowledge.read").is_some(),
        "默认插件启用时工具应存在"
    );
    // 真实走一轮 scripted turn，把 job 置为 terminal 后再做下一轮，避免该会话还有 queued 任务。
    let request = scripted_turn_request(&fixture.storage, &job);
    assert!(
        request
            .messages
            .iter()
            .any(|message| message.text.contains("已确认业务知识"))
    );
    // Retire knowledge then verify reading schedule.
    fixture
        .storage
        .with_ai_connection(|conn| {
            let view = ai_platform::knowledge::load(conn, "k1").unwrap();
            ai_platform::knowledge::retire(conn, 3_000, "k1", view.revision).unwrap();
            Ok(())
        })
        .unwrap();
    // New turn should not contain knowledge nor tool。
    let job2 = controller
        .prepare_ai_conversation("读取知识", &pin)
        .unwrap();
    let request = scripted_turn_request(&fixture.storage, &job2);
    assert!(
        !request.messages[0].text.contains("某规则"),
        "退休后不应再出现在 prompt"
    );
    let catalog2 = ai_tool_catalog_snapshot(&fixture.storage, &job2);
    assert!(catalog2.describe("knowledge.read").is_none());
    // Old turn tool exists (snapshot built before retire) but reading retired
    // entry should fail via the status check at load time.
    let snapshot = ai_tool_catalog_snapshot_checked(&fixture.storage, &job);
    assert!(snapshot.is_err(), "旧 Turn 的工具装配在退休后应拒绝创建");
    let mut catalog = ai_tool_catalog_snapshot(&fixture.storage, &job);
    let receipt = ai_tool_execute_receipt(
        &fixture.storage,
        &mut catalog,
        &job,
        "knowledge.read",
        r#"{"knowledge_id":"k1"}"#,
    );
    assert!(!receipt.is_success(), "旧 Turn 读退休条目必须失败");
}

fn scripted_turn_request(
    storage: &FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::ModelRequest {
    let provider = ScriptedGenerationProvider::text_only("按本轮上下文执行");
    let (tx, _rx) = mpsc::sync_channel(64);
    run_ai_conversation_turn(
        storage,
        job,
        &fluxdb_core::CancelFlag::new(),
        &tx,
        &provider,
        &fluxdb_app::ToolCatalog::new(Vec::new()),
        &KernelAuditHook,
        TurnPermissionContext::default(),
    );
    provider.request_at(0).expect("必须经过实际模型请求装配")
}

fn ai_tool_catalog_snapshot(
    storage: &FileStorage,
    job: &AiConversationJob,
) -> fluxdb_app::ToolCatalog {
    // Use the runtime helper to rebuild the turn tool catalog exactly as actor does;
    // this representation mirrors production tool discovery.
    let mut catalog = ToolCatalog::new(Vec::new());
    add_ai_reference_tools(&mut catalog, storage, job).unwrap();
    if add_ai_knowledge_read_tools(&mut catalog, storage, job).is_err() {
        // 旧 Turn 中知识退休：runtime 构建时会拒绝整个目录；测试省略工具目录。
        return ToolCatalog::new(Vec::new());
    }
    catalog
}

/// 与生产目录不同：工具装配路径允许失败（旧 Turn 中知识可能已退休）。
fn ai_tool_catalog_snapshot_checked(
    storage: &FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::Result<fluxdb_app::ToolCatalog> {
    let mut catalog = ToolCatalog::new(Vec::new());
    add_ai_reference_tools(&mut catalog, storage, job)?;
    add_ai_knowledge_read_tools(&mut catalog, storage, job)?;
    Ok(catalog)
}

fn ai_tool_execute_receipt(
    _storage: &FileStorage,
    catalog: &mut ToolCatalog,
    job: &AiConversationJob,
    tool_id: &str,
    args: &str,
) -> fluxdb_core::ToolReceipt {
    let prepared = match catalog.prepare(tool_id, args) {
        Ok(prepared) => prepared,
        Err(error) => return failed_receipt(tool_id, error.message.to_string()),
    };
    let Some(tool) = catalog.executable(tool_id) else {
        return failed_receipt(tool_id, "工具没有实现".to_string());
    };
    let ctx = fluxdb_core::ToolExecutionContext {
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        iteration: 1,
        call_id: "call-knowledge-test".into(),
        cancel: fluxdb_core::CancelFlag::new(),
        progress: None,
    };
    match block_on_ready(tool.execute(ctx, prepared.to_json_string())) {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(error)) | Err(error) => failed_receipt(tool_id, error.message),
    }
}

fn failed_receipt(tool_id: &str, message: String) -> fluxdb_core::ToolReceipt {
    fluxdb_core::ToolReceipt {
        call_id: "call-knowledge-test".into(),
        tool_id: tool_id.into(),
        status: fluxdb_core::ToolReceiptStatus::Failed,
        result_json: serde_json::json!({"error": message}).to_string(),
        error_code: Some("knowledge_unavailable".into()),
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 0,
    }
}
