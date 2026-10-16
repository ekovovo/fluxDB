//! T039: real SQLite results feed BI artifacts through the production conversation runtime.
//! Only model events and explicitly injected renderer/producer failures are scripted.
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationJob, AiConversationView, AiModelSaveInput, AppCommand,
    AppController, AppEvent, ArtifactError, ArtifactOutcome, ArtifactProducer,
    ArtifactProducerDescriptor, ArtifactRenderer, ArtifactSourceData, BuiltinBiProducer,
    KernelAuditHook, KernelPolicy, MapSqlResolver, RESULT_STORE_MEMORY_BYTES, RenderedArtifact,
    ResultStore, TargetRef, ToolCatalog, TurnPermissionContext, TurnTargetPin,
    add_ai_artifact_tools, add_ai_artifact_tools_with_producer, run_ai_conversation_turn,
    source_hash, standard_sql_catalog_with_results,
};
use fluxdb_core::{
    AiFuture, ArtifactBlock, ArtifactSpec, CancelFlag, ConnectionConfig, ConnectionId,
    DatabaseKind, Endpoint, FinishReason, GenerationProvider, ModelCapabilities, ModelEvent,
    ModelRequest, ModelRole, ToolCallRequest, ToolExecutionContext, ToolReceiptStatus,
};
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{artifacts, plugins, sessions},
};
use serde_json::{Value, json};

include!("support/bi_artifacts_fixture.rs");
include!("support/bi_capability_tests.rs");

#[test]
fn sqlite_query_create_add_table_finalize_has_real_sources_card_file_and_hash() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results.clone()).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    assert_ready(&fixture, &job, &view);
    provider.assert_sql_unchanged();
    let query = fixture.receipt(&job, "query", "succeeded");
    assert_eq!(query["columns"], json!(["channel", "revenue"]));
    assert_eq!(query["rows"], json!([["web", 12], ["store", 34]]));
    assert_eq!(query["row_count"], 2);
    assert!(
        query["result_ref"]["id"]
            .as_str()
            .unwrap()
            .starts_with("result-")
    );
    assert_eq!(
        results
            .lock()
            .unwrap()
            .read_page(query["result_ref"]["id"].as_str().unwrap(), None, 10, 4096)
            .unwrap()
            .items,
        vec![json!(["web", 12]), json!(["store", 34])]
    );
}

fn rejected_create(mut arguments: Value, expected_code: &str) {
    // Keep only JSON arguments supplied by the model; results are always produced by db.query.
    arguments["metric"] = json!({"name":"Revenue","timezone":"UTC"});
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &job,
        vec![
            query(QUERY),
            call("create", "artifact.create", arguments),
            Step::Done,
        ],
    );
    let view = run(&fixture, &job, &catalog, &provider);
    let receipt = fixture.receipt(&job, "create", "failed");
    assert_eq!(receipt["code"], expected_code);
    assert_eq!(receipt["ok"], false);
    assert!(receipt["file"].is_null());
    assert!(fixture.artifacts(&job).is_empty());
    assert!(
        !view
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| part.kind == "artifact_ref")
    );
    provider.assert_sql_unchanged();
}

#[test]
fn unknown_chart_field_is_rejected_without_artifact_or_sql_changes() {
    let mut arguments = create_args();
    arguments["blocks"] = json!([chart("missing_channel", "revenue")]);
    rejected_create(arguments, "unknown_field");
}

#[test]
fn non_numeric_chart_field_is_rejected_without_artifact_or_sql_changes() {
    let mut arguments = create_args();
    arguments["blocks"] = json!([chart("channel", "channel")]);
    rejected_create(arguments, "non_numeric_field");
}

#[test]
fn fake_result_handle_is_rejected_without_artifact_or_sql_changes() {
    let mut arguments = create_args();
    arguments["sources"][0]["result_ref"] = json!("result-fake");
    arguments["blocks"][0]["result_ref"] = json!("result-fake");
    rejected_create(arguments, "source_unavailable");
}

#[test]
fn unknown_renderer_is_visible_and_does_not_modify_sql_results() {
    let mut arguments = create_args();
    arguments["renderer"] = json!("uninstalled.renderer");
    rejected_create(arguments, "renderer_unavailable");
}

#[test]
fn invalid_table_block_leaves_the_draft_usable_and_can_be_followed_by_valid_block() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let mut steps = report_steps(QUERY);
    steps.insert(
        2,
        with_artifact(
            "invalid-add",
            "artifact.add_block",
            json!({"block":{"type":"table","result_ref":"result-1","columns":["invented"]}}),
        ),
    );
    let provider = ReceiptDrivenProvider::new(&fixture, &job, steps);
    let view = run(&fixture, &job, &catalog, &provider);
    assert_eq!(
        fixture.receipt(&job, "invalid-add", "failed")["code"],
        "unknown_field"
    );
    assert_ready(&fixture, &job, &view);
    let created = fixture.receipt(&job, "create", "succeeded");
    let added = fixture.receipt(&job, "add", "succeeded");
    assert_eq!(
        added["revision"].as_i64().unwrap(),
        created["revision"].as_i64().unwrap() + 1,
        "the rejected block must not increment the artifact revision"
    );
    provider.assert_sql_unchanged();
}

#[test]
fn model_written_utc_offset_succeeds_on_first_attempt_and_is_normalized() {
    // 模型常写 `UTC+08:00`；它应被归一化为 `+08:00` 后直接创建成功，而不是先失败一次。
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let mut steps = report_steps(QUERY);
    steps[1] = call("create", "artifact.create", {
        let mut arguments = create_args();
        arguments["metric"]["timezone"] = json!("UTC+08:00");
        arguments
    });
    let provider = ReceiptDrivenProvider::new(&fixture, &job, steps);
    let view = run(&fixture, &job, &catalog, &provider);
    assert_ready(&fixture, &job, &view);
    let created = fixture.receipt(&job, "create", "succeeded");
    assert_eq!(created["metric"]["timezone"], "+08:00");
    let artifact = fixture.artifacts(&job).remove(0);
    let spec: Value = serde_json::from_str(&artifact.spec_json).unwrap();
    assert_eq!(
        spec["metric"]["timezone"], "+08:00",
        "落库 spec 也必须归一化"
    );
    provider.assert_sql_unchanged();
}

#[test]
fn duplicate_block_is_rejected_without_adding_a_second_copy() {
    // 模型可能先在 create 里声明图表、再用 add_block 加同一张图；重复块必须被拒绝，
    // 否则产物会渲染两份完全相同的图表。
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let mut steps = report_steps(QUERY);
    steps.insert(
        2,
        with_artifact(
            "dup-add",
            "artifact.add_block",
            json!({"block": chart("channel", "revenue")}),
        ),
    );
    let provider = ReceiptDrivenProvider::new(&fixture, &job, steps);
    let view = run(&fixture, &job, &catalog, &provider);
    assert_eq!(
        fixture.receipt(&job, "dup-add", "failed")["code"],
        "duplicate_block"
    );
    assert_ready(&fixture, &job, &view);
    let artifact = fixture.artifacts(&job).remove(0);
    let spec: Value = serde_json::from_str(&artifact.spec_json).unwrap();
    let charts = spec["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "chart")
        .count();
    assert_eq!(charts, 1, "重复图表块不能被追加：{}", spec["blocks"]);
    provider.assert_sql_unchanged();
}

#[test]
fn receipts_report_current_blocks_so_the_model_does_not_repeat_declarations() {
    // 方案 2：create 一次声明全部块，add_block 只补遗漏。回执必须回显草稿当前已有的块，
    // 否则模型无从判断该不该再 add_block，只能靠猜并撞上 duplicate_block。
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    run(&fixture, &job, &catalog, &provider);
    let created = fixture.receipt(&job, "create", "succeeded");
    assert_eq!(
        created["blocks"],
        json!([{
            "type": "chart",
            "result_ref": created["sources"][0]["result_ref"].clone(),
            "chart": "bar",
            "x": "channel",
            "y": "revenue",
        }]),
        "create 回执要回显已声明的块"
    );
    let added = fixture.receipt(&job, "add", "succeeded");
    assert_eq!(
        added["blocks"].as_array().unwrap().len(),
        2,
        "add_block 回执要回显追加后的完整块列表"
    );
    assert_eq!(added["blocks"][1]["type"], "table");
    provider.assert_sql_unchanged();
}

#[test]
fn cross_turn_add_block_and_finalize_cannot_modify_another_tasks_draft() {
    let fixture = Fixture::new();
    let first = fixture.job();
    let (mut catalog, results) = fixture.catalog(&first);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &first, results).unwrap();
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &first,
        vec![
            query(QUERY),
            call("create", "artifact.create", create_args()),
            Step::Done,
        ],
    );
    let first_view = run(&fixture, &first, &catalog, &provider);
    let original = fixture.artifacts(&first).remove(0);
    assert_eq!(original.status, "draft");
    let original_sources = fixture.sources(&original.id);
    let original_sql = sql_snapshot(&fixture.storage, &first);
    let second = fixture.job();
    assert_ne!(first.turn_id, second.turn_id);
    let (mut catalog, results) = fixture.catalog(&second);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &second, results).unwrap();
    let attacker = ReceiptDrivenProvider::new(
        &fixture,
        &second,
        vec![
            query(QUERY),
            call(
                "add",
                "artifact.add_block",
                json!({
                    "artifact_id":original.id,"block":{"type":"note","text":"foreign modification"}
                }),
            ),
            call(
                "finalize",
                "artifact.finalize",
                json!({"artifact_id":original.id}),
            ),
            Step::Done,
        ],
    );
    let second_view = run(&fixture, &second, &catalog, &attacker);
    for call in ["add", "finalize"] {
        let receipt = fixture.receipt(&second, call, "failed");
        assert_eq!(receipt["ok"], false);
        assert!(receipt["file"].is_null());
    }
    assert_eq!(fixture.artifacts(&first), vec![original.clone()]);
    assert_eq!(fixture.sources(&original.id), original_sources);
    assert_eq!(sql_snapshot(&fixture.storage, &first), original_sql);
    assert!(fixture.artifacts(&second).is_empty());
    assert!(!fixture.dir.join("ai-artifacts").exists());
    assert_card(&fixture, &first_view, &original, None);
    assert!(
        !second_view
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| part.kind == "artifact_ref")
    );
    attacker.assert_sql_unchanged();
}

struct FailingRenderer;

impl ArtifactRenderer for FailingRenderer {
    fn id(&self) -> &'static str {
        REPORT
    }
    fn version(&self) -> &'static str {
        "test-failure"
    }
    fn render(
        &self,
        _: &ArtifactSpec,
        _: &[ArtifactSourceData],
    ) -> Result<RenderedArtifact, ArtifactError> {
        Err(ArtifactError::new(
            "renderer_failed",
            "Controlled renderer failure",
        ))
    }
}

// This replacement delegates real work to the host producer and records which operations ran.
struct RecordingProducer {
    inner: BuiltinBiProducer,
    finalizer: Option<BuiltinBiProducer>,
    calls: Mutex<Vec<&'static str>>,
}

impl ArtifactProducer for RecordingProducer {
    fn descriptor(&self) -> &ArtifactProducerDescriptor {
        self.inner.descriptor()
    }
    fn create(
        &self,
        spec: ArtifactSpec,
        cancel: &CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError> {
        self.calls.lock().unwrap().push("create");
        self.inner.create(spec, cancel)
    }
    fn add_block(
        &self,
        id: &str,
        block: ArtifactBlock,
        cancel: &CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError> {
        self.calls.lock().unwrap().push("add_block");
        self.inner.add_block(id, block, cancel)
    }
    fn finalize(&self, id: &str, cancel: &CancelFlag) -> Result<ArtifactOutcome, ArtifactError> {
        self.calls.lock().unwrap().push("finalize");
        self.finalizer
            .as_ref()
            .unwrap_or(&self.inner)
            .finalize(id, cancel)
    }
}

#[test]
fn artifact_tools_accept_a_replaceable_trait_object_producer() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    let recording = Arc::new(RecordingProducer {
        inner: fixture.producer(&job, results),
        finalizer: None,
        calls: Mutex::new(Vec::new()),
    });
    let replacement: Arc<dyn ArtifactProducer> = recording.clone();
    add_ai_artifact_tools_with_producer(&mut catalog, replacement).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    assert_ready(&fixture, &job, &view);
    assert_eq!(
        *recording.calls.lock().unwrap(),
        vec!["create", "add_block", "finalize"]
    );
    provider.assert_sql_unchanged();
}

fn failed_finalize(renderer_missing: bool) {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    let producer = if renderer_missing {
        RecordingProducer {
            inner: fixture.producer(&job, results.clone()),
            finalizer: Some(fixture.producer(&job, results).without_renderers()),
            calls: Mutex::new(Vec::new()),
        }
    } else {
        RecordingProducer {
            inner: fixture
                .producer(&job, results)
                .with_renderer(Arc::new(FailingRenderer)),
            finalizer: None,
            calls: Mutex::new(Vec::new()),
        }
    };
    add_ai_artifact_tools_with_producer(&mut catalog, Arc::new(producer)).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "failed");
    assert_eq!(artifact.relative_path, None);
    assert_eq!(artifact.content_hash, None);
    assert_eq!(artifact.byte_count, 0);
    assert_eq!(fixture.sources(&artifact.id).len(), 1);
    let code = if renderer_missing {
        "renderer_unavailable"
    } else {
        "renderer_failed"
    };
    let receipt = fixture.receipt(&job, "finalize", "failed");
    assert_eq!(receipt["code"], code);
    assert_eq!(receipt["ok"], false);
    assert!(receipt["file"].is_null());
    assert_card(&fixture, &view, &artifact, Some(code));
    assert!(!fixture.dir.join("ai-artifacts").exists());
    provider.assert_sql_unchanged();
}

#[test]
fn renderer_removed_before_finalize_marks_failed_without_changing_sql() {
    failed_finalize(true);
}

#[test]
fn renderer_failure_marks_failed_without_changing_sql() {
    failed_finalize(false);
}

#[test]
fn disabled_builtin_bi_keeps_tools_but_rejects_report_creation() {
    let fixture = Fixture::new();
    let job = fixture.job();
    fixture
        .storage
        .with_ai_connection(|conn| {
            let instance = plugins::list_plugin_instances(conn)?
                .into_iter()
                .find(|instance| instance.id == "builtin.bi")
                .unwrap();
            plugins::set_plugin_instance_enabled(conn, 1, &instance.id, false, instance.revision)
        })
        .unwrap();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    for tool in ["artifact.create", "artifact.add_block", "artifact.finalize"] {
        assert!(
            catalog.executable(tool).is_some(),
            "{tool} must remain registered"
        );
    }
    assert!(catalog.executable("db.query").is_some());
    assert!(catalog.executable("result.read_page").is_some());
    let description = &catalog.describe("artifact.create").unwrap().description;
    assert!(!description.contains(REPORT));
    assert!(description.contains("无可用"));
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &job,
        vec![
            query(QUERY),
            call("create", "artifact.create", create_args()),
            Step::Done,
        ],
    );
    run(&fixture, &job, &catalog, &provider);
    assert_eq!(
        fixture.receipt(&job, "create", "failed")["code"],
        "artifact_capability_disabled"
    );
    assert!(fixture.artifacts(&job).is_empty());
    provider.assert_sql_unchanged();
}

#[test]
fn retained_sql_results_keep_positional_paging_shape_after_artifact_generation() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let wide_sql = "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<100)
        SELECT channel,revenue,replace(hex(zeroblob(900)),'00','x') AS padding
        FROM seq CROSS JOIN sales ORDER BY n,revenue";
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(wide_sql));
    let view = run(&fixture, &job, &catalog, &provider);
    let query = fixture.receipt(&job, "query", "succeeded");
    assert_eq!(
        query["rows"].as_array().unwrap().len() as u64,
        query["row_count"].as_u64().unwrap()
    );
    assert_eq!(query["columns"], json!(["channel", "revenue", "padding"]));
    assert_eq!(query["truncated"], true);
    let total = query["row_count"].as_u64().unwrap() as usize;
    assert!(total > 100);
    assert_eq!(query["rows_omitted"].as_u64(), Some(0));
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "ready");
    assert_card(&fixture, &view, &artifact, None);
    let finalized = fixture.receipt(&job, "finalize", "succeeded");
    assert_eq!(finalized["truncated"], true);
    assert_eq!(finalized["sources"][0]["rows"], total);
    let (relative, hash): (String, String) = fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT relative_path,content_hash FROM ai_results
             WHERE id=?1 AND retention_kind='retained'",
                    [format!("{}-result-query", job.turn_id)],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap())
        })
        .unwrap();
    let retained = fs::read_to_string(fixture.dir.join("ai-results").join(relative)).unwrap();
    assert_eq!(hash, source_hash(&[&retained]));
    let original: Vec<Value> = serde_json::from_str(&retained).unwrap();
    let mut items = Vec::new();
    let mut cursor = None;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for _ in 0..32 {
        let mut arguments = json!({"result_ref":{"id":query["result_ref"]["id"]},"limit":16});
        if let Some(next) = cursor.take() {
            arguments["cursor"] = next;
        }
        let receipt = runtime
            .block_on(catalog.executable("result.read_page").unwrap().execute(
                ToolExecutionContext {
                    automatic_approval: false,
                    session_id: job.session_id.clone(),
                    turn_id: job.turn_id.clone(),
                    iteration: 1,
                    call_id: "read-page".into(),
                    cancel: CancelFlag::new(),
                    progress: None,
                },
                arguments.to_string(),
            ))
            .unwrap();
        assert_eq!(receipt.status, ToolReceiptStatus::Succeeded);
        let page: Value = serde_json::from_str(&receipt.result_json).unwrap();
        assert_eq!(page["total_items"], total);
        items.extend(page["items"].as_array().unwrap().iter().cloned());
        if page["next_cursor"].is_null() {
            break;
        }
        cursor = Some(page["next_cursor"].clone());
    }
    assert_eq!(items.len(), total);
    assert!(!original.is_empty());
    assert_eq!(
        &items[total - original.len()..],
        original.as_slice(),
        "retained overflow must remain an unchanged suffix of the positional result"
    );
    for (index, row) in items.iter().enumerate() {
        assert!(row.is_array());
        assert_eq!(row[0], if index % 2 == 0 { "web" } else { "store" });
        assert_eq!(row[1], if index % 2 == 0 { 12 } else { 34 });
        assert_eq!(row[2].as_str().unwrap().len(), 900);
    }
    provider.assert_sql_unchanged();
}

#[test]
fn disabled_agent_binding_keeps_tools_but_rejects_bi_with_global_instance_enabled() {
    let fixture = Fixture::new();
    let job = fixture.job();
    fixture
        .storage
        .with_ai_connection(|conn| {
            let session = sessions::load_session(conn, &job.session_id)?;
            plugins::bind_plugin_system(conn, 1, "bi-binding", &session.agent_id, "builtin.bi")?;
            conn.execute(
                "UPDATE ai_agent_plugin_bindings SET enabled=0 WHERE id='bi-binding'",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    assert!(catalog.executable("artifact.create").is_some());
    assert!(
        !catalog
            .describe("artifact.create")
            .unwrap()
            .description
            .contains(REPORT)
    );
    assert!(catalog.executable("db.query").is_some());
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &job,
        vec![
            query(QUERY),
            call("create", "artifact.create", create_args()),
            Step::Done,
        ],
    );
    run(&fixture, &job, &catalog, &provider);
    assert_eq!(
        fixture.receipt(&job, "create", "failed")["code"],
        "artifact_capability_disabled"
    );
    assert!(fixture.artifacts(&job).is_empty());
    provider.assert_sql_unchanged();
}

#[test]
fn creation_preview_marks_51_points_but_ready_snapshot_stays_complete() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let sql = "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<51)
        SELECT 'channel-' || n AS channel,n AS revenue FROM seq ORDER BY n";
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(sql));
    let view = run(&fixture, &job, &catalog, &provider);
    let created = fixture.receipt(&job, "create", "succeeded");
    assert_eq!(created["points"].as_array().unwrap().len(), 50);
    assert_eq!(created["points_truncated"], true);
    assert_eq!(created["truncated"], true);
    let artifact = fixture.artifacts(&job).remove(0);
    let ready = fixture
        .dir
        .join("ai-artifacts")
        .join(artifact.relative_path.unwrap());
    assert!(
        fs::read_to_string(ready)
            .unwrap()
            .contains("| channel-51 | 51 |")
    );
    let card = view
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find(|part| part.kind == "artifact_ref")
        .unwrap();
    let payload: Value = serde_json::from_str(&card.payload_json).unwrap();
    assert_eq!(
        payload["truncated"], false,
        "preview clipping must not corrupt snapshot completeness"
    );
    provider.assert_sql_unchanged();
}

#[test]
fn zero_row_table_uses_actual_columns_and_finishes_as_empty_snapshot() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let mut arguments = create_args();
    arguments["blocks"] =
        json!([{"type":"table","result_ref":"result-1","columns":["channel","revenue"]}]);
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &job,
        vec![
            query("SELECT channel,revenue FROM sales WHERE 1=0"),
            call("create", "artifact.create", arguments),
            with_artifact("finalize", "artifact.finalize", json!({})),
            Step::Done,
        ],
    );
    run(&fixture, &job, &catalog, &provider);
    let ready = fixture.artifacts(&job).remove(0);
    assert_eq!(ready.status, "ready");
    let content = fs::read_to_string(
        fixture
            .dir
            .join("ai-artifacts")
            .join(ready.relative_path.unwrap()),
    )
    .unwrap();
    assert!(content.contains("| channel | revenue |"));
    assert!(!content.contains("| web |"));
    provider.assert_sql_unchanged();
}

#[test]
fn file_write_failure_persists_failed_card_and_no_fake_file() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    fs::write(fixture.dir.join("ai-artifacts"), "not a directory").unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "failed");
    assert!(artifact.relative_path.is_none());
    let receipt = fixture.receipt(&job, "finalize", "failed");
    assert_eq!(receipt["code"], "artifact_write_failed");
    assert!(receipt["file"].is_null());
    assert_card(&fixture, &view, &artifact, Some("artifact_write_failed"));
    assert_eq!(
        fs::read_to_string(fixture.dir.join("ai-artifacts")).unwrap(),
        "not a directory"
    );
    provider.assert_sql_unchanged();
}

#[test]
fn artifact_schema_rejects_model_values_before_executable_runs() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let mut arguments = create_args();
    arguments["blocks"][0]["data"] = json!([999999]);
    assert_eq!(
        catalog
            .prepare("artifact.create", &arguments.to_string())
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    assert!(fixture.artifacts(&job).is_empty());
    assert_eq!(
        catalog.describe("artifact.create").unwrap().effect,
        fluxdb_app::EffectClass::Mutating
    );
}

#[test]
fn cancelled_operations_do_not_create_artifacts_or_files() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (_, results) = fixture.catalog(&job);
    let producer = fixture.producer(&job, results);
    let cancel = CancelFlag::new();
    cancel.request();
    let spec = ArtifactSpec {
        title: "Cancelled".into(),
        ..Default::default()
    };
    assert_eq!(
        producer.create(spec, &cancel).unwrap_err().code,
        "cancelled"
    );
    assert_eq!(
        producer.finalize("nonexistent", &cancel).unwrap_err().code,
        "cancelled"
    );
    assert!(fixture.artifacts(&job).is_empty());
    assert!(!fixture.dir.join("ai-artifacts").exists());
}

#[test]
fn ready_commit_failure_removes_staged_file_and_persists_failed_card() {
    let fixture = Fixture::new();
    let job = fixture.job();
    fixture
        .storage
        .with_ai_connection(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER reject_artifact_ready BEFORE UPDATE OF status ON ai_artifacts
            WHEN NEW.status='ready' BEGIN SELECT RAISE(ABORT, 'controlled ready rejection'); END;",
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "failed");
    assert!(artifact.relative_path.is_none());
    assert_eq!(
        fs::read_dir(fixture.dir.join("ai-artifacts"))
            .unwrap()
            .count(),
        0
    );
    assert_card(&fixture, &view, &artifact, Some("artifact_store_error"));
    provider.assert_sql_unchanged();
}

#[test]
fn null_chart_values_are_omitted_with_an_explicit_limit_marker() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let sql = "SELECT channel, CASE WHEN channel='store' THEN NULL ELSE revenue END AS revenue FROM sales ORDER BY channel";
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(sql));
    run(&fixture, &job, &catalog, &provider);
    let receipt = fixture.receipt(&job, "create", "succeeded");
    assert_eq!(receipt["points"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["truncated"], true);
    let artifact = fixture.artifacts(&job).remove(0);
    let content = fs::read_to_string(
        fixture
            .dir
            .join("ai-artifacts")
            .join(artifact.relative_path.unwrap()),
    )
    .unwrap();
    assert!(content.contains("数据限制"));
    provider.assert_sql_unchanged();
}

#[test]
fn ready_card_failure_rolls_back_ready_metadata_and_removes_file() {
    let fixture = Fixture::new();
    let job = fixture.job();
    fixture.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER reject_ready_card BEFORE UPDATE OF payload_json ON ai_message_parts
            WHEN NEW.kind='artifact_ref' AND json_extract(NEW.payload_json,'$.artifact_status')='ready'
            BEGIN SELECT RAISE(ABORT, 'controlled ready card rejection'); END;").unwrap();
        Ok(())
    }).unwrap();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "failed");
    assert!(artifact.relative_path.is_none());
    assert!(artifact.content_hash.is_none());
    assert_eq!(
        fs::read_dir(fixture.dir.join("ai-artifacts"))
            .unwrap()
            .count(),
        0
    );
    assert_card(&fixture, &view, &artifact, Some("artifact_store_error"));
    provider.assert_sql_unchanged();
}
