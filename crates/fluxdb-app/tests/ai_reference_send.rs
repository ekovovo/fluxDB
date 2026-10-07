use fluxdb_app::{
    AiAgentSaveInput, AiContextReference, AiConversationCommand, AiModelSaveInput, AppCommand,
    AppController, AppEvent, KernelAuditHook, MapSqlResolver, ScriptedGenerationProvider,
    TargetRef, TurnPermissionContext, TurnTargetPin, ai_tool_target_handle, ai_turn_connection_id,
    kernel_policy_for_connection, run_ai_conversation_turn, standard_sql_catalog,
};
use fluxdb_core::{
    ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{conversation, sessions},
};
use std::sync::Arc;

struct Fixture {
    root: std::path::PathBuf,
    storage: FileStorage,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fluxdb-ref-send-{}", uuid::Uuid::new_v4()));
        let storage = FileStorage::new(&root);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { root, storage }
    }
    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(self.storage.clone());
        controller.dispatch(AppCommand::ReplaceConnections(vec![config(17), config(18)]));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "offline".into(),
            provider: "openai".into(),
            model: "offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("{other:?}"),
        };
        controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "reference test".into(),
            model_profile_id: Some(model),
            enabled: true,
            ..Default::default()
        }));
        controller
    }
    fn attach(&self, controller: &mut AppController, id: u64) -> AiContextReference {
        let reference = controller
            .ai_reference_candidates("")
            .into_iter()
            .find(|r| r.id == format!("connection:{id}"))
            .unwrap();
        assert!(matches!(
            controller.dispatch(AppCommand::AiConversation(
                AiConversationCommand::AddReference {
                    reference: reference.clone()
                }
            )),
            AppEvent::AiConversationLoaded(_)
        ));
        reference
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn config(id: u64) -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId(id),
        name: format!("connection-{id}"),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: true,
        },
        options: [("demo".into(), "true".into())].into_iter().collect(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}
fn empty_pin() -> TurnTargetPin {
    let target = TargetRef::default();
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
    }
}

#[test]
fn connection_reference_pins_target_supplies_handle_and_executes_read_tool() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let reference = fixture.attach(&mut controller, 17);
    let job = controller
        .prepare_ai_conversation("查询产品", &empty_pin())
        .unwrap();
    assert_eq!(
        ai_turn_connection_id(&fixture.storage, &job.turn_id).unwrap(),
        Some(17)
    );
    assert!(
        controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    let user = controller
        .state()
        .ai_conversation
        .messages
        .iter()
        .find(|m| m.role == "user")
        .unwrap();
    let saved_ref = user.parts.iter().find(|p| p.kind == "context_ref").unwrap();
    let payload: serde_json::Value = serde_json::from_str(&saved_ref.payload_json).unwrap();
    assert_eq!(payload["id"], reference.id);
    assert!(payload.get("source").is_none());
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            vec![
                ModelEvent::ToolCallRequested(ToolCallRequest {
                    provider_call_id: "read-reference".into(),
                    tool_id: "db.query".into(),
                    arguments_json:
                        r#"{"target":"conn:17/db:main","sql":"SELECT * FROM product","max_rows":5}"#
                            .into(),
                }),
                ModelEvent::Finished(FinishReason::Stop),
            ],
            vec![
                ModelEvent::TextDelta("已按回执总结".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ],
    );
    let catalog = standard_sql_catalog(Arc::new(MapSqlResolver::new([config(17)])), false).unwrap();
    assert!(catalog.describe("db.execute").is_none());
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &fluxdb_core::CancelFlag::new(),
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        TurnPermissionContext {
            policy: kernel_policy_for_connection(Some(&config(17))),
            ..Default::default()
        },
    );
    let request = provider.request_at(0).unwrap();
    assert!(
        request
            .messages
            .iter()
            .any(|m| m.text.contains("句柄：conn:17"))
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|m| m.text.contains("（无可用句柄）"))
    );
    let terminal = rx
        .try_iter()
        .filter(|update| update.terminal)
        .last()
        .unwrap();
    assert!(
        terminal
            .view
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .any(|part| { part.kind == "tool_result_ref" && part.status == "complete" })
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            let loaded = conversation::load(conn, Some(&job.session_id))?;
            assert!(loaded.context_references.is_empty());
            assert!(
                loaded
                    .parts
                    .iter()
                    .any(|part| part.kind == sessions::PartKind::ContextRef)
            );
            Ok(())
        })
        .unwrap();
    controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Apply {
        view: terminal.view,
    }));
    let followup = controller
        .prepare_ai_conversation("继续分析同一连接", &empty_pin())
        .unwrap();
    assert_eq!(
        ai_turn_connection_id(&fixture.storage, &followup.turn_id).unwrap(),
        Some(17)
    );
    assert!(
        controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    assert!(
        controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .last()
            .unwrap()
            .parts
            .iter()
            .all(|part| part.kind != "context_ref")
    );
}

#[test]
fn table_reference_overrides_unrelated_ui_target_without_exposing_other_connections() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::ReplaceConnections(vec![config(1), config(18)]));
    // 通过生产连接命令加载 demo 的对象元数据，引用来源使用真实候选。
    controller.dispatch(AppCommand::OpenConnection(ConnectionId(1)));
    let database = controller
        .state()
        .connections
        .iter()
        .find(|c| c.config.id == ConnectionId(1))
        .unwrap()
        .objects
        .iter()
        .find(|object| object.path.kind == fluxdb_core::ObjectKind::Database)
        .unwrap()
        .path
        .clone();
    controller.dispatch(AppCommand::LoadObjectChildren(database));
    let reference = controller
        .ai_reference_candidates("")
        .into_iter()
        .find(|r| r.kind == "table")
        .unwrap();
    controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::AddReference { reference },
    ));
    let mut other = empty_pin();
    other.target.connection_id = Some(18);
    other.target_json = other.target.to_json();
    let job = controller
        .prepare_ai_conversation("解释引用的表", &other)
        .unwrap();
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            assert_eq!(
                ai_tool_target_handle(&turn.target_json),
                Some("conn:1/db:main".into())
            );
            let target: serde_json::Value = serde_json::from_str(&turn.target_json).unwrap();
            assert!(target["object"].as_str().is_some());
            Ok(())
        })
        .unwrap();
}

#[test]
fn ambiguous_or_missing_connection_keeps_pending_references_without_empty_session() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    fixture.attach(&mut controller, 17);
    fixture.attach(&mut controller, 18);
    assert!(
        controller
            .prepare_ai_conversation("比较引用", &empty_pin())
            .is_err()
    );
    assert_eq!(
        controller.state().ai_conversation.context_references.len(),
        2
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert!(sessions::list_sessions(conn, 10)?.is_empty());
            Ok(())
        })
        .unwrap();
    controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::RemoveReference {
            reference_id: "connection:18".into(),
        },
    ));
    controller.dispatch(AppCommand::ReplaceConnections(vec![config(18)]));
    assert!(
        controller
            .prepare_ai_conversation("已删除的连接", &empty_pin())
            .is_err()
    );
    assert_eq!(
        controller.state().ai_conversation.context_references.len(),
        1
    );
}

#[test]
fn send_failure_rolls_back_reference_consumption_and_message_tags() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let reference = fixture.attach(&mut controller, 17);
    fixture.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER fail_reference_send BEFORE DELETE ON ai_context_references BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        Ok(())
    }).unwrap();
    assert!(
        controller
            .prepare_ai_conversation("失败需保留引用", &empty_pin())
            .is_err()
    );
    assert_eq!(
        controller.state().ai_conversation.context_references,
        vec![reference]
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert!(sessions::list_sessions(conn, 10)?.is_empty());
            assert_eq!(
                fluxdb_storage::ai_platform::context_references::list(conn, None)?.len(),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn legacy_message_tags_are_recovered_from_its_own_turn_snapshot() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let reference = fixture.attach(&mut controller, 17);
    let job = controller
        .prepare_ai_conversation("历史引用", &empty_pin())
        .unwrap();
    fixture
        .storage
        .with_ai_connection(|conn| {
            conn.execute("DELETE FROM ai_message_parts WHERE kind='context_ref'", [])
                .unwrap();
            Ok(())
        })
        .unwrap();
    controller.dispatch(AppCommand::AiConversation(AiConversationCommand::Load {
        session_id: Some(job.session_id),
    }));
    let tags = controller
        .state()
        .ai_conversation
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| part.kind == "context_ref")
        .collect::<Vec<_>>();
    assert_eq!(tags.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&tags[0].payload_json).unwrap();
    assert_eq!(payload["id"], reference.id);
    assert_eq!(payload["label"], reference.label);
    assert!(payload.get("source").is_none());
    assert!(
        controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
}

#[test]
fn send_consumes_only_submitted_references_and_preserves_next_draft_items() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let submitted = fixture.attach(&mut controller, 17);
    fixture.attach(&mut controller, 18);
    fixture.storage.with_ai_connection(|conn| {
        let agent = fluxdb_storage::ai_platform::agents::list_agents(conn)?.into_iter()
            .find(|agent| agent.enabled).unwrap();
        let snapshot = serde_json::json!({"context_references":[{
            "id": submitted.id, "label": submitted.label, "kind":submitted.kind, "summary":submitted.summary
        }]}).to_string();
        conversation::send(conn, 100, true, "submitted", &agent.id, "submitted-turn", "submitted-user",
            "只提交捕获到的第一项", "{}", &snapshot)?;
        let remaining = fluxdb_storage::ai_platform::context_references::list(conn, Some("submitted"))?;
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].contains("connection:18"));
        assert!(fluxdb_storage::ai_platform::context_references::list(conn, None)?.is_empty());
        Ok(())
    }).unwrap();
}
