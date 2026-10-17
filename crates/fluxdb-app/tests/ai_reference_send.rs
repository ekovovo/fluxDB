use fluxdb_app::{
    AiAgentSaveInput, AiContextReference, AiConversationCommand, AiModelSaveInput, AppCommand,
    AppController, AppEvent, KernelAuditHook, MapSqlResolver, ScriptedGenerationProvider,
    TargetRef, TurnPermissionContext, TurnTargetPin, ai_tool_target_handle, ai_turn_connection_id,
    kernel_policy_for_connection, run_ai_conversation_turn, standard_sql_catalog,
};
use fluxdb_core::{
    ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason, ModelCapabilities,
    ModelEvent, SavedQuery, ToolCallRequest,
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
            .ai_reference_candidates("", None)
            .unwrap()
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

fn turn_snapshot(fixture: &Fixture, turn_id: &str) -> serde_json::Value {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, turn_id)?;
            Ok(serde_json::from_str(&turn.runtime_snapshot_json).unwrap())
        })
        .unwrap()
}

fn text_model_request(
    fixture: &Fixture,
    job: &fluxdb_app::AiConversationJob,
) -> fluxdb_core::ModelRequest {
    let provider = ScriptedGenerationProvider::text_only("按本轮上下文解释");
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_turn(
        &fixture.storage,
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
    let snapshot = turn_snapshot(&fixture, &job.turn_id);
    assert_eq!(snapshot["database_context"]["database_kind"], "sqlite");
    assert_eq!(snapshot["database_context"]["sql_dialect"], "sqlite");
    assert_eq!(snapshot["database_context"]["connection_read_only"], true);
    assert_eq!(
        snapshot["context_references"][0]["target"]["connection_id"],
        17
    );
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
        request.messages[0]
            .text
            .contains(r#""database_kind":"sqlite""#)
    );
    assert!(
        provider.request_at(1).unwrap().messages[0]
            .text
            .contains(r#""sql_dialect":"sqlite""#),
        "工具回合继续时仍保留数据库方言"
    );
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
            .as_ref()
            .expect("终态回包必须携带视图")
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
        view: terminal.view.expect("终态回包必须携带视图"),
    }));
    let followup = controller
        .prepare_ai_conversation("继续分析同一连接", &empty_pin())
        .unwrap();
    assert_eq!(
        ai_turn_connection_id(&fixture.storage, &followup.turn_id).unwrap(),
        Some(17)
    );
    assert_eq!(
        turn_snapshot(&fixture, &followup.turn_id)["database_context"],
        snapshot["database_context"],
        "无 @ 追问也应从继承目标重新提取数据库上下文"
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
    let mut unrelated = config(18);
    unrelated.kind = DatabaseKind::Postgres;
    unrelated.name = "unrelated-postgres".into();
    controller.dispatch(AppCommand::ReplaceConnections(vec![config(1), unrelated]));
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
        .ai_reference_candidates("", None)
        .unwrap()
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
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            assert_eq!(
                snapshot["database_context"]["database_kind"], "sqlite",
                "以引用覆盖后的目标为准，不能用当前页面的 PostgreSQL 类型"
            );
            assert_eq!(
                snapshot["context_references"][0]["target"], target,
                "引用必须携带结构化对象定位"
            );
            Ok(())
        })
        .unwrap();
    // 发送后切换连接配置不会改写该轮已经固定的类型；也不能暴露另一连接的信息。
    let mut changed = config(1);
    changed.kind = DatabaseKind::Postgres;
    controller.dispatch(AppCommand::ReplaceConnections(vec![changed, config(18)]));
    let request = text_model_request(&fixture, &job);
    assert!(
        request.messages[0]
            .text
            .contains(r#""database_kind":"sqlite""#)
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|message| message.text.contains("unrelated-postgres"))
    );
}

#[test]
fn every_database_kind_supplies_actual_dialect_without_credentials_or_endpoints() {
    for (kind, engine, dialect) in [
        (DatabaseKind::MySql, "mysql", Some("mysql")),
        (DatabaseKind::TiDb, "tidb", Some("mysql")),
        (DatabaseKind::Postgres, "postgres", Some("postgres")),
        (DatabaseKind::Sqlite, "sqlite", Some("sqlite")),
        (DatabaseKind::Redis, "redis", None),
        (DatabaseKind::MongoDb, "mongodb", None),
    ] {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        let mut config = config(17);
        config.kind = kind;
        config.endpoint = if kind == DatabaseKind::Sqlite {
            Endpoint::SqliteFile {
                path: "PRIVATE-DATABASE-PATH".into(),
                read_only: false,
            }
        } else {
            Endpoint::Tcp {
                host: "PRIVATE-HOST".into(),
                port: 1234,
                database: Some("PRIVATE-ENDPOINT-DATABASE".into()),
            }
        };
        config
            .options
            .insert("password".into(), "PRIVATE-PASSWORD".into());
        config
            .options
            .insert("credential_ref".into(), "PRIVATE-CREDENTIAL-REF".into());
        config.options.insert("read_only".into(), "true".into());
        controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
        let mut reference = controller
            .ai_reference_candidates("", None)
            .unwrap()
            .into_iter()
            .find(|r| r.id == "connection:17")
            .unwrap();
        reference.label = "do not infer the type from this label".into();
        controller.dispatch(AppCommand::AiConversation(
            AiConversationCommand::AddReference { reference },
        ));
        let job = controller
            .prepare_ai_conversation("解释当前数据库", &empty_pin())
            .unwrap();
        let snapshot = turn_snapshot(&fixture, &job.turn_id);
        let context = &snapshot["database_context"];
        assert_eq!(context["database_kind"], engine);
        assert_eq!(context["sql_dialect"].as_str(), dialect);
        assert_eq!(context["connection_read_only"], true);
        assert_eq!(context["write_requires_approval"], true);
        let request = text_model_request(&fixture, &job);
        assert!(
            request.messages[0]
                .text
                .contains(&format!(r#""database_kind":"{engine}""#))
        );
        for secret in [
            "PRIVATE-DATABASE-PATH",
            "PRIVATE-HOST",
            "PRIVATE-ENDPOINT-DATABASE",
            "PRIVATE-PASSWORD",
            "PRIVATE-CREDENTIAL-REF",
            "endpoint",
            "options",
            "credential_ref",
        ] {
            assert!(
                !snapshot.to_string().contains(secret),
                "配置/凭据不能进入快照: {secret}"
            );
            assert!(
                !request
                    .messages
                    .iter()
                    .any(|message| message.text.contains(secret)),
                "配置/凭据不能进入模型请求: {secret}"
            );
        }
    }
}

#[test]
fn multiple_table_references_preserve_each_object_location() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::ReplaceConnections(vec![config(1)]));
    controller.dispatch(AppCommand::OpenConnection(ConnectionId(1)));
    let database = controller.state().connections[0]
        .objects
        .iter()
        .find(|object| object.path.kind == fluxdb_core::ObjectKind::Database)
        .unwrap()
        .path
        .clone();
    controller.dispatch(AppCommand::LoadObjectChildren(database));
    let references = controller
        .ai_reference_candidates("", None)
        .unwrap()
        .into_iter()
        .filter(|reference| reference.kind == "table")
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    for reference in &references {
        controller.dispatch(AppCommand::AiConversation(
            AiConversationCommand::AddReference {
                reference: reference.clone(),
            },
        ));
    }
    let job = controller
        .prepare_ai_conversation("比较两张表", &empty_pin())
        .unwrap();
    let snapshot = turn_snapshot(&fixture, &job.turn_id);
    let targets = snapshot["context_references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| reference["target"]["object"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_ne!(targets[0], targets[1]);
    for reference in snapshot["context_references"].as_array().unwrap() {
        assert_eq!(reference["target"]["connection_id"], 1);
        assert_eq!(reference["target"]["database"], "main");
        assert!(reference.get("source").is_none());
    }
    let request = text_model_request(&fixture, &job);
    for target in targets {
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.text.contains(&format!(r#""object":"{target}""#)))
        );
    }
}

#[test]
fn legacy_or_unavailable_database_context_is_explicitly_unknown() {
    for legacy in [false, true] {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        let mut pin = empty_pin();
        pin.target.connection_id = Some(if legacy { 17 } else { 999 });
        pin.target_json = pin.target.to_json();
        let job = controller
            .prepare_ai_conversation("解释目标", &pin)
            .unwrap();
        if legacy {
            fixture.storage.with_ai_connection(|conn| {
                conn.execute("UPDATE ai_turns SET runtime_snapshot_json=json_remove(runtime_snapshot_json,'$.database_context') WHERE id=?1", [&job.turn_id]).unwrap();
                Ok(())
            }).unwrap();
        } else {
            assert_eq!(
                turn_snapshot(&fixture, &job.turn_id)["database_context"]["status"],
                "unavailable"
            );
        }
        let request = text_model_request(&fixture, &job);
        assert!(
            request.messages[0]
                .text
                .contains(r#""status":"unavailable""#)
        );
        assert!(request.messages[0].text.contains("数据库类型未知"));
        assert!(
            !request.messages[0]
                .text
                .contains(r#""database_kind":"sqlite""#)
        );
    }
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
#[test]
fn saved_query_and_captured_reference_details_are_bounded_and_scoped() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let same_name = "订单口径";
    fixture
        .storage
        .save_saved_queries(&[
            SavedQuery {
                id: 1,
                connection_id: ConnectionId(17),
                database: Some("shop".into()),
                schema: None,
                name: same_name.into(),
                text: "SELECT 1".into(),
            },
            SavedQuery {
                id: 2,
                connection_id: ConnectionId(18),
                database: None,
                schema: Some("audit".into()),
                name: same_name.into(),
                text: "SELECT 2".into(),
            },
        ])
        .unwrap();
    let saved = controller
        .ai_reference_candidates("@订单口径", None)
        .unwrap()
        .into_iter()
        .filter(|reference| reference.kind == "saved_query")
        .collect::<Vec<_>>();
    assert_eq!(saved.len(), 2);
    let mut ids = saved
        .iter()
        .map(|reference| reference.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    assert!(
        saved
            .iter()
            .any(|reference| reference.summary.contains("shop"))
    );
    assert!(
        saved
            .iter()
            .any(|reference| reference.summary.contains("audit"))
    );

    controller.dispatch(AppCommand::OpenQueryEditor(ConnectionId(17)));
    let tab_id = controller.state().active_tab().unwrap().id;
    controller.dispatch(AppCommand::FinishQueryExecution {
        tab_id,
        result: Ok(fluxdb_core::QueryExecutionResult {
            summaries: Vec::new(),
            rollback_snapshots: Vec::new(),
            results: vec![fluxdb_core::DataPage {
                columns: Vec::new(),
                rows: vec![fluxdb_core::Row {
                    values: vec![fluxdb_core::CellValue::Text("actual-result-marker".into())],
                }],
                offset: 0,
                limit: 100,
                has_more: false,
            }],
        }),
    });
    let target = TargetRef {
        connection_id: Some(17),
        ..Default::default()
    };
    let selected_text = "SELECT count(*) FROM orders";
    let context = fluxdb_app::UiContextSnapshot {
        tab_id: Some(tab_id.0),
        tab_revision: 1,
        target,
        selected_text: Some(selected_text.into()),
        excerpt: None,
        statement_at_cursor: None,
        summary: None,
        last_error: None,
        result_ref: None,
        notes: Vec::new(),
    };
    let candidates = controller
        .ai_reference_candidates("@当前", Some(&context))
        .unwrap();
    let selection = candidates
        .iter()
        .find(|reference| reference.kind == "selection")
        .unwrap();
    assert!(
        selection
            .summary
            .contains(&format!("{} 字符", selected_text.chars().count()))
    );
    let result = candidates
        .iter()
        .find(|reference| reference.kind == "result_set")
        .unwrap();
    assert!(result.source.contains("actual-result-marker"));
    assert!(
        !result.summary.contains("actual-result-marker"),
        "候选摘要不泄露结果正文"
    );
    for reference in [selection, result] {
        assert!(controller.ai_reference_is_valid(reference));
        controller.dispatch(AppCommand::AiConversation(
            AiConversationCommand::AddReference {
                reference: reference.clone(),
            },
        ));
    }
    let job = controller
        .prepare_ai_conversation("解释引用", &empty_pin())
        .unwrap();
    let snapshot = turn_snapshot(&fixture, &job.turn_id);
    assert!(
        !snapshot["context_references"]
            .to_string()
            .contains(selected_text)
    );
    assert!(
        !snapshot["context_references"]
            .to_string()
            .contains("actual-result-marker")
    );
    assert!(
        snapshot["reference_grants"]
            .to_string()
            .contains("actual-result-marker")
    );
    let mut tools = fluxdb_app::ToolCatalog::new(Vec::new());
    fluxdb_app::add_ai_reference_tools(&mut tools, &fixture.storage, &job).unwrap();
    let tool = tools.executable("context.read").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for reference in [selection, result] {
        let args = serde_json::json!({"reference_id": reference.id, "max_bytes": 4096}).to_string();
        tools.prepare("context.read", &args).unwrap();
        let receipt = runtime
            .block_on(tool.execute(
                fluxdb_core::ToolExecutionContext {
                    automatic_approval: false,
                    bounded_write: false,
                    session_id: job.session_id.clone(),
                    turn_id: job.turn_id.clone(),
                    iteration: 1,
                    call_id: "context-read".into(),
                    progress: None,
                    cancel: fluxdb_core::CancelFlag::new(),
                },
                args,
            ))
            .unwrap();
        assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
        assert!(
            receipt
                .result_json
                .contains(if reference.kind == "selection" {
                    selected_text
                } else {
                    "actual-result-marker"
                })
        );
    }
    let receipt = runtime
        .block_on(tool.execute(
            fluxdb_core::ToolExecutionContext {
                automatic_approval: false,
                bounded_write: false,
                session_id: job.session_id.clone(),
                turn_id: job.turn_id.clone(),
                iteration: 1,
                call_id: "unauthorized".into(),
                progress: None,
                cancel: fluxdb_core::CancelFlag::new(),
            },
            r#"{"reference_id":"not-selected"}"#.into(),
        ))
        .unwrap();
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Failed);

    let calls = [selection, result]
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: format!("read-{index}"),
                tool_id: "context.read".into(),
                arguments_json: serde_json::json!({"reference_id": reference.id}).to_string(),
            })
        })
        .chain([ModelEvent::Finished(FinishReason::ToolCalls)])
        .collect();
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            calls,
            vec![
                ModelEvent::TextDelta("读取完成".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ],
    );
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &fluxdb_core::CancelFlag::new(),
        &tx,
        &provider,
        &tools,
        &KernelAuditHook,
        TurnPermissionContext::default(),
    );
    let next = provider.request_at(1).unwrap();
    for marker in [selected_text, "actual-result-marker"] {
        assert!(
            next.messages
                .iter()
                .any(|message| message.role == fluxdb_core::ModelRole::Tool
                    && message.text.contains(marker)),
            "后续模型请求必须收到真实读取回执"
        );
    }
    fixture
        .storage
        .with_ai_connection(|conn| {
            let calls = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?;
            assert_eq!(calls.len(), 2);
            assert!(calls.iter().all(|call| call.status == "succeeded"));
            Ok(())
        })
        .unwrap();
}

#[test]
fn missing_ai_storage_is_retrieval_failure_not_no_match() {
    let controller = AppController::new();
    let error = controller
        .ai_reference_candidates("@anything", None)
        .unwrap_err();
    assert_eq!(error.message, "AI 存储尚未初始化，请重启应用");
}

#[test]
fn saved_query_change_is_visible_and_failed_send_preserves_draft() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let mut saved = SavedQuery {
        id: 3,
        connection_id: ConnectionId(17),
        database: None,
        schema: None,
        name: "saved-reference".into(),
        text: "SELECT 'original-body'".into(),
    };
    fixture
        .storage
        .save_saved_queries(&[saved.clone()])
        .unwrap();
    let reference = controller
        .ai_reference_candidates("saved-reference", None)
        .unwrap()
        .remove(0);
    controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::AddReference {
            reference: reference.clone(),
        },
    ));
    let job = controller
        .prepare_ai_conversation("read saved query", &empty_pin())
        .unwrap();
    let snapshot = turn_snapshot(&fixture, &job.turn_id);
    assert!(
        !snapshot["context_references"]
            .to_string()
            .contains("original-body")
    );
    assert!(
        snapshot["reference_grants"]
            .to_string()
            .contains("original-body")
    );
    text_model_request(&fixture, &job);
    controller.dispatch(AppCommand::AiConversation(AiConversationCommand::New));
    controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::AddReference {
            reference: reference.clone(),
        },
    ));
    saved.text = "SELECT 'new-body'".into();
    fixture.storage.save_saved_queries(&[saved]).unwrap();
    assert!(controller.ai_reference_errors()[&reference.id].contains("修改"));
    assert!(
        controller
            .prepare_ai_conversation("stale query", &empty_pin())
            .is_err()
    );
    assert_eq!(
        controller.state().ai_conversation.context_references.len(),
        1
    );
}

#[test]
fn explicit_reference_never_grants_write_or_bypasses_approval() {
    struct CountedTool(Arc<fluxdb_app::RecordingTool>);
    impl fluxdb_core::AiTool for CountedTool {
        fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
            fluxdb_core::AiTool::descriptor(self.0.as_ref())
        }
        fn execute(
            &self,
            ctx: fluxdb_core::ToolExecutionContext,
            args: String,
        ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
            fluxdb_core::AiTool::execute(self.0.as_ref(), ctx, args)
        }
    }
    for writable in [false, true] {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        let mut config = config(17);
        config.endpoint = Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: !writable,
        };
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        fixture.attach(&mut controller, 17);
        let job = controller
            .prepare_ai_conversation("尝试写入被引用的连接", &empty_pin())
            .unwrap();
        let tool = Arc::new(fluxdb_app::RecordingTool::new(
            "db.execute",
            fluxdb_core::ToolRisk::Write,
        ));
        let mut catalog = fluxdb_app::ToolCatalog::new(Vec::new());
        catalog
            .add_tool(Box::new(CountedTool(tool.clone())))
            .unwrap();
        let provider = ScriptedGenerationProvider::new(
            ModelCapabilities {
                streaming: true,
                tool_calling: true,
                reasoning_summary: false,
            },
            vec![
                vec![
                    ModelEvent::ToolCallRequested(ToolCallRequest {
                        provider_call_id: "try-write".into(),
                        tool_id: "db.execute".into(),
                        arguments_json:
                            r#"{"target":"conn:17/db:main","sql":"DELETE FROM product WHERE id=1"}"#
                                .into(),
                    }),
                    ModelEvent::Finished(FinishReason::ToolCalls),
                ],
                vec![
                    ModelEvent::TextDelta("权限结果以工具回执为准".into()),
                    ModelEvent::Finished(FinishReason::Stop),
                ],
            ],
        );
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        run_ai_conversation_turn(
            &fixture.storage,
            &job,
            &fluxdb_core::CancelFlag::new(),
            &tx,
            &provider,
            &catalog,
            &KernelAuditHook,
            TurnPermissionContext {
                agent_allowed_tools: vec!["db.execute".into()],
                disabled_tools: Default::default(),
                policy: kernel_policy_for_connection(Some(&config)),
                plugin_digest: None,
            },
        );
        assert_eq!(tool.call_count(), 0, "无批准时不能有任何真实工具调用");
        assert!(
            provider
                .request_at(0)
                .unwrap()
                .messages
                .iter()
                .any(|message| message.text.contains("不授予写权限"))
        );
        fixture
            .storage
            .with_ai_connection(|conn| {
                let calls = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?;
                assert_eq!(calls.len(), 1);
                assert_eq!(
                    calls[0].status.to_string(),
                    if writable {
                        "waiting_approval"
                    } else {
                        "denied"
                    }
                );
                Ok(())
            })
            .unwrap();
    }
}
