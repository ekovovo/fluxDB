// 隔离 SQLite + 真实回执驱动的产物验收夹具。
const QUERY: &str = "SELECT channel,revenue FROM sales ORDER BY revenue";
const REPORT: &str = "fluxdb.bi.report";
type Results = Arc<Mutex<ResultStore>>;
type SqlSnapshot = Vec<Vec<rusqlite::types::Value>>;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t039-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        let path = dir.join("sales.sqlite");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE sales(channel TEXT,revenue INTEGER);
                 INSERT INTO sales VALUES ('web',12),('store',34);",
            )
            .unwrap();
        storage
            .save_connections(&[ConnectionConfig {
                id: ConnectionId(1),
                name: "T039 isolated sales".into(),
                kind: DatabaseKind::Sqlite,
                endpoint: Endpoint::SqliteFile {
                    path,
                    read_only: true,
                },
                options: BTreeMap::new(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
            }])
            .unwrap();
        Self { dir, storage }
    }

    fn job(&self) -> AiConversationJob {
        let mut controller = AppController::new();
        controller.set_ai_storage(self.storage.clone());
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T039 offline model".into(),
            provider: "openai".into(),
            model: "t039-scripted".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens: Some(16_384),
            max_output_tokens: Some(1_024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            event => panic!("model setup failed: {event:?}"),
        };
        assert!(matches!(
            controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                display_name: "T039 sales agent".into(),
                system_prompt: "Use real source results for the sales report.".into(),
                model_profile_id: Some(model),
                enabled: true,
                ..Default::default()
            })),
            AppEvent::AiAgentSaved { .. }
        ));
        let target = TargetRef {
            connection_id: Some(1),
            database: Some("main".into()),
            schema: None,
            object: Some("sales".into()),
            redis_db: None,
        };
        controller
            .prepare_ai_conversation(
                "Query sales and create a report with a chart and table.",
                &TurnTargetPin {
                    target_json: target.to_json(),
                    target,
                    captured_tab_id: Some(1),
                    captured_tab_revision: 1,
                },
            )
            .unwrap()
    }

    fn catalog(&self, job: &AiConversationJob) -> (ToolCatalog, Results) {
        let results = Arc::new(Mutex::new(ResultStore::for_turn(
            RESULT_STORE_MEMORY_BYTES,
            &job.session_id,
            &job.turn_id,
            Some(self.dir.join("ai-results")),
        )));
        let catalog = standard_sql_catalog_with_results(
            Arc::new(MapSqlResolver::new(
                self.storage.load_connections().unwrap(),
            )),
            results.clone(),
            false,
        )
        .unwrap();
        (catalog, results)
    }

    fn producer(&self, job: &AiConversationJob, results: Results) -> BuiltinBiProducer {
        BuiltinBiProducer::new(
            self.storage.clone(),
            job.session_id.clone(),
            job.turn_id.clone(),
            job.assistant_message_id.clone(),
            results,
        )
    }

    fn artifacts(&self, job: &AiConversationJob) -> Vec<artifacts::ArtifactView> {
        self.storage
            .with_ai_connection(|conn| artifacts::list_artifacts_for_turn(conn, &job.turn_id))
            .unwrap()
    }

    fn sources(&self, id: &str) -> Vec<artifacts::ArtifactSourceView> {
        self.storage
            .with_ai_connection(|conn| artifacts::list_artifact_sources(conn, id))
            .unwrap()
    }

    fn receipt(&self, job: &AiConversationJob, call_id: &str, status: &str) -> Value {
        let call = self
            .storage
            .with_ai_connection(|conn| sessions::list_tool_calls_for_turn(conn, &job.turn_id))
            .unwrap()
            .into_iter()
            .find(|call| call.provider_call_id == call_id)
            .expect("scripted call must really execute");
        assert_eq!(call.status.as_str(), status, "{:?}", call.receipt_json);
        serde_json::from_str(call.receipt_json.as_deref().unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// Capture every persisted SQL result/call column, including timestamps and retention facts.
fn sql_snapshot(storage: &FileStorage, job: &AiConversationJob) -> SqlSnapshot {
    storage
        .with_ai_connection(|conn| {
            let mut snapshot = Vec::new();
            for (sql, id) in [
                (
                    "SELECT * FROM ai_tool_calls WHERE id=?1",
                    format!("{}-toolcall-query", job.turn_id),
                ),
                (
                    "SELECT * FROM ai_results WHERE id=?1",
                    format!("{}-result-query", job.turn_id),
                ),
            ] {
                let mut stmt = conn.prepare(sql).unwrap();
                let count = stmt.column_count();
                let row = stmt
                    .query_row([id], |row| {
                        (0..count)
                            .map(|index| row.get(index))
                            .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
                    })
                    .unwrap();
                snapshot.push(row);
            }
            Ok(snapshot)
        })
        .unwrap()
}

enum Step {
    Call {
        id: String,
        tool: String,
        arguments: Value,
    },
    WithArtifact {
        id: String,
        tool: String,
        arguments: Value,
    },
    Done,
}

fn call(id: &str, tool: &str, arguments: Value) -> Step {
    Step::Call {
        id: id.into(),
        tool: tool.into(),
        arguments,
    }
}

fn with_artifact(id: &str, tool: &str, arguments: Value) -> Step {
    Step::WithArtifact {
        id: id.into(),
        tool: tool.into(),
        arguments,
    }
}

fn query(sql: &str) -> Step {
    call(
        "query",
        "db.query",
        json!({"target":"conn:1/db:main","sql":sql,"max_rows":200}),
    )
}

fn chart(x: &str, y: &str) -> Value {
    json!({"type":"chart","result_ref":"result-1","chart":"bar","x":x,"y":y})
}

fn create_args() -> Value {
    json!({
        "title":"Sales by channel","kind":"report","renderer":REPORT,
        "metric":{"name":"Revenue","timezone":"UTC"},
        "sources":[{"result_ref":"result-1"}],
        "blocks":[chart("channel","revenue")]
    })
}

fn report_steps(sql: &str) -> Vec<Step> {
    vec![
        query(sql),
        call("create", "artifact.create", create_args()),
        with_artifact(
            "add",
            "artifact.add_block",
            json!({"block":{"type":"table","result_ref":"result-1","columns":["channel","revenue"]}}),
        ),
        with_artifact("finalize", "artifact.finalize", json!({})),
        Step::Done,
    ]
}

struct ReceiptDrivenProvider {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ModelRequest>>,
    query_before_artifacts: Mutex<Option<SqlSnapshot>>,
    storage: FileStorage,
    job: AiConversationJob,
}

impl ReceiptDrivenProvider {
    fn new(fixture: &Fixture, job: &AiConversationJob, steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
            query_before_artifacts: Mutex::new(None),
            storage: fixture.storage.clone(),
            job: job.clone(),
        }
    }

    fn assert_sql_unchanged(&self) {
        assert_eq!(
            self.query_before_artifacts
                .lock()
                .unwrap()
                .as_ref()
                .expect("query must finish before artifact operations"),
            &sql_snapshot(&self.storage, &self.job),
            "artifact operations must not rewrite any SQL result/call fact"
        );
        let requests = self.requests.lock().unwrap();
        let receipts: Vec<_> = requests
            .iter()
            .flat_map(|request| {
                request
                    .messages
                    .iter()
                    .filter(|message| message.tool_call_id.as_deref() == Some("query"))
            })
            .collect();
        assert!(!receipts.is_empty());
        assert!(receipts.iter().all(|receipt| *receipt == receipts[0]));
    }
}

impl GenerationProvider for ReceiptDrivenProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        }
    }

    fn stream(&self, request: ModelRequest, cancel: CancelFlag) -> AiFuture<'_, Vec<ModelEvent>> {
        Box::pin(async move {
            assert!(!cancel.is_cancelled());
            // The runtime also requests a title after a completed first turn.
            if request.tools.is_empty() {
                assert_eq!(
                    request.max_output_tokens,
                    Some(64),
                    "unexpected auxiliary model request"
                );
                assert!(self.steps.lock().unwrap().is_empty());
                self.requests.lock().unwrap().push(request);
                return Ok(vec![
                    ModelEvent::TextDelta("Sales report".into()),
                    ModelEvent::Finished(FinishReason::Stop),
                ]);
            }
            if request.messages.iter().any(|message| {
                message.role == ModelRole::Tool && message.tool_call_id.as_deref() == Some("query")
            }) {
                let mut before = self.query_before_artifacts.lock().unwrap();
                if before.is_none() {
                    *before = Some(sql_snapshot(&self.storage, &self.job));
                }
            }
            let step = self.steps.lock().unwrap().pop_front().unwrap_or_else(|| {
                panic!(
                    "script exhausted after {} requests; last request tool receipts: {:?}",
                    self.requests.lock().unwrap().len(),
                    request
                        .messages
                        .iter()
                        .filter(|message| message.role == ModelRole::Tool)
                        .map(|message| (&message.tool_call_id, &message.tool_name, &message.text))
                        .collect::<Vec<_>>()
                )
            });
            let source_ref = request.messages.iter().find_map(|message| {
                if message.tool_call_id.as_deref() != Some("query") { return None; }
                let value: Value = serde_json::from_str(&message.text).ok()?;
                value["result"]["result_ref"]["id"].as_str().map(str::to_owned)
            });
            let substitute_source = |mut arguments: Value| {
                fn substitute(value: &mut Value, reference: &str) {
                    match value {
                        Value::String(text) if text == "result-1" => *text = reference.to_owned(),
                        Value::Array(items) => { for item in items { substitute(item, reference); } },
                        Value::Object(object) => { for item in object.values_mut() { substitute(item, reference); } },
                        _ => {}
                    }
                }
                if let Some(reference) = &source_ref { substitute(&mut arguments, reference); }
                arguments
            };
            let events = match step {
                Step::Done => vec![
                    ModelEvent::TextDelta(
                        "Report workflow finished using actual tool receipts.".into(),
                    ),
                    ModelEvent::Finished(FinishReason::Stop),
                ],
                Step::Call {
                    id,
                    tool,
                    arguments,
                } => tool_events(id, tool, substitute_source(arguments)),
                Step::WithArtifact {
                    id,
                    tool,
                    mut arguments,
                } => {
                    // Host IDs are unknowable until the next model request carries a real receipt.
                    let message = request
                        .messages
                        .iter()
                        .rev()
                        .find(|message| {
                            message.role == ModelRole::Tool
                                && message.tool_name.as_deref() == Some("artifact.create")
                        })
                        .expect("create tool receipt must be replayed");
                    let receipt: Value = serde_json::from_str(&message.text).unwrap();
                    assert_eq!(receipt["status"], "succeeded", "{receipt}");
                    let artifact_id = receipt["result"]["artifact_id"]
                        .as_str()
                        .expect("successful create must return the host artifact ID");
                    arguments["artifact_id"] = json!(artifact_id);
                    tool_events(id, tool, substitute_source(arguments))
                }
            };
            self.requests.lock().unwrap().push(request);
            Ok(events)
        })
    }
}

fn tool_events(id: String, tool: String, arguments: Value) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: id,
            tool_id: tool,
            arguments_json: arguments.to_string(),
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn run(
    fixture: &Fixture,
    job: &AiConversationJob,
    catalog: &ToolCatalog,
    provider: &ReceiptDrivenProvider,
) -> AiConversationView {
    let (tx, rx) = std::sync::mpsc::sync_channel(128);
    run_ai_conversation_turn(
        &fixture.storage,
        job,
        &CancelFlag::new(),
        &tx,
        provider,
        catalog,
        &KernelAuditHook,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );
    let view = rx
        .try_iter()
        .filter(|update| update.terminal)
        .filter_map(|update| update.view)
        .last()
        .expect("runtime must emit a terminal conversation view");
    assert!(view.error.is_none(), "{:?}", view.error);
    assert!(
        provider.steps.lock().unwrap().is_empty(),
        "all model steps must run"
    );
    view
}

fn assert_card(
    fixture: &Fixture,
    view: &AiConversationView,
    artifact: &artifacts::ArtifactView,
    failure: Option<&str>,
) {
    let cards: Vec<_> = view
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| part.kind == "artifact_ref")
        .collect();
    assert_eq!(
        cards.len(),
        1,
        "finalize must update, not duplicate, the artifact card"
    );
    let payload: Value = serde_json::from_str(&cards[0].payload_json).unwrap();
    assert_eq!(payload["artifact_id"], artifact.id);
    assert_eq!(payload["artifact_status"], artifact.status);
    assert_eq!(payload["failure_code"], json!(failure));
    let stored = fixture
        .storage
        .with_ai_connection(|conn| sessions::load_part(conn, &cards[0].id))
        .unwrap();
    assert_eq!(stored.kind, sessions::PartKind::ArtifactRef);
    assert_eq!(stored.artifact_id.as_deref(), Some(artifact.id.as_str()));
    assert_eq!(stored.payload_json, cards[0].payload_json);
}

fn assert_ready(fixture: &Fixture, job: &AiConversationJob, view: &AiConversationView) {
    let artifacts = fixture.artifacts(job);
    assert_eq!(artifacts.len(), 1);
    let artifact = &artifacts[0];
    assert_eq!(artifact.status, "ready");
    assert_eq!(artifact.session_id, job.session_id);
    assert_eq!(artifact.turn_id, job.turn_id);
    assert_eq!(artifact.renderer_id, REPORT);
    let spec: Value = serde_json::from_str(&artifact.spec_json).unwrap();
    assert!(
        ["UTC", "+08:00"].contains(&spec["metric"]["timezone"].as_str().unwrap_or_default()),
        "时区必须是校验通过的规范写法，实际 {}",
        spec["metric"]["timezone"]
    );
    assert_eq!(spec["blocks"].as_array().unwrap().len(), 2);
    assert_card(fixture, view, artifact, None);
    let sources = fixture.sources(&artifact.id);
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].result_id,
        Some(format!("{}-result-query", job.turn_id))
    );
    assert_eq!(
        sources[0].tool_call_id,
        Some(format!("{}-toolcall-query", job.turn_id))
    );
    assert_eq!(
        serde_json::from_str::<Value>(&sources[0].metadata_json).unwrap()["result_ref"],
        fixture.receipt(job, "query", "succeeded")["result_ref"]["id"]
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            let owner: (String, String, String) = conn
                .query_row(
                    "SELECT session_id,turn_id,source_call_id FROM ai_results WHERE id=?1",
                    [sources[0].result_id.as_deref().unwrap()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                owner,
                (
                    job.session_id.clone(),
                    job.turn_id.clone(),
                    sources[0].tool_call_id.clone().unwrap()
                )
            );
            let call_owner: (String, String, String) = conn
                .query_row(
                    "SELECT session_id,turn_id,tool_id FROM ai_tool_calls WHERE id=?1",
                    [sources[0].tool_call_id.as_deref().unwrap()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                call_owner,
                (
                    job.session_id.clone(),
                    job.turn_id.clone(),
                    "db.query".into()
                )
            );
            Ok(())
        })
        .unwrap();
    let relative = Path::new(artifact.relative_path.as_deref().unwrap());
    assert!(!relative.is_absolute());
    assert!(
        relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    );
    let content = fs::read_to_string(fixture.dir.join("ai-artifacts").join(relative)).unwrap();
    let snapshot: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["spec"]["title"], "Sales by channel");
    assert_eq!(snapshot["sources"][0]["columns"], json!(["channel", "revenue"]));
    assert_eq!(snapshot["sources"][0]["items"], json!([
        {"channel":"web","revenue":12}, {"channel":"store","revenue":34}
    ]));
    assert_eq!(artifact.byte_count, content.len() as i64);
    assert_eq!(artifact.content_hash, Some(source_hash(&[&content])));
    assert_eq!(content.matches("| web | 12 |").count(), 2, "{content}");
    assert_eq!(content.matches("| store | 34 |").count(), 2, "{content}");
    assert!(!content.contains(&fixture.dir.to_string_lossy().to_string()));
    let created = fixture.receipt(job, "create", "succeeded");
    assert_eq!(
        created["points"],
        json!([{"x":"web","y":12},{"x":"store","y":34}])
    );
    assert_eq!(
        created["sources"][0]["result_id"],
        sources[0].result_id.as_deref().unwrap()
    );
    assert_eq!(
        created["sources"][0]["tool_call_id"],
        sources[0].tool_call_id.as_deref().unwrap()
    );
    fixture.receipt(job, "add", "succeeded");
    let finalized = fixture.receipt(job, "finalize", "succeeded");
    assert_eq!(finalized["status"], "ready");
    assert_eq!(
        finalized["file"]["relative_path"],
        artifact.relative_path.as_deref().unwrap()
    );
    assert_eq!(
        finalized["file"]["content_hash"],
        artifact.content_hash.as_deref().unwrap()
    );
    assert_eq!(finalized["file"]["byte_count"], artifact.byte_count);
}

