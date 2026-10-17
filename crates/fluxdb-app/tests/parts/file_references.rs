use super::*;
use fluxdb_app::{AiContextReference, add_ai_file_tools, authorize_ai_file};
use fluxdb_core::ToolReceiptStatus;
use fluxdb_storage::ai_platform::context_references;
use serde_json::{Value, json};

struct Files(Fixture);

impl Files {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("fluxdb-file-ref-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let root = dir.join("config");
        fs::create_dir(&root).unwrap();
        let storage = FileStorage::new(root);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self(Fixture { dir, storage })
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(self.0.storage.root_dir()));
        load(&mut controller, None);
        controller
    }

    fn configured(&self) -> AppController {
        let mut controller = self.controller();
        let model = create_model(&mut controller, "file-fixture", "openai", true);
        select(&mut controller, Some(&model));
        controller
    }

    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn attach(
        &self,
        controller: &mut AppController,
        path: &std::path::Path,
    ) -> AiContextReference {
        let reference = authorize_ai_file(path, self.0.storage.root_dir()).unwrap();
        let view = command(
            controller,
            AiConversationCommand::AddReference { reference },
        );
        view.context_references.last().unwrap().clone()
    }

    fn snapshot(&self, job: &AiConversationJob) -> Value {
        self.0
            .storage
            .with_ai_connection(|conn| {
                let turn = sessions::load_turn(conn, &job.turn_id)?;
                Ok(serde_json::from_str(&turn.runtime_snapshot_json).unwrap())
            })
            .unwrap()
    }

    fn catalog(&self, job: &AiConversationJob) -> ToolCatalog {
        let mut tools = ToolCatalog::new(Vec::new());
        add_ai_file_tools(&mut tools, &self.0.storage, job).unwrap();
        tools
    }

    fn toggle(&self, controller: &mut AppController, enabled: bool) {
        assert!(matches!(controller.dispatch(AppCommand::RefreshAiSettings), AppEvent::AiSettingsLoaded));
        let plugin = controller.state().ai_plugin_instances.iter().find(|p| p.id == "builtin.files").unwrap();
        let event = controller.dispatch(AppCommand::SetAiPluginInstanceEnabled {
            id: plugin.id.clone(), enabled, expected_revision: plugin.revision,
        });
        assert!(matches!(event, AppEvent::AiPluginInstanceEnabled { .. }), "{event:?}");
    }
}

#[test]
fn disabled_file_plugin_rejects_new_grants_and_send_without_losing_the_draft() {
    let files = Files::new();
    let mut controller = files.configured();
    let reference = files.attach(&mut controller, &files.write("plugin.sql", "SELECT 'file-plugin';"));
    files.toggle(&mut controller, false);
    assert!(controller.ai_reference_candidates("", None).unwrap().iter().all(|r| r.kind != "file"));
    assert!(controller.ai_reference_errors()[&reference.id].contains("文件与附件已停用"));
    let error = controller.prepare_ai_conversation("read attachment", &target()).unwrap_err();
    assert!(error.message.contains("文件与附件已停用"));
    assert_eq!(files.0.session_count(), 0);
    assert_eq!(controller.state().ai_conversation.context_references, vec![reference.clone()]);
    let event = controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::AddReference { reference: reference.clone() }));
    assert!(matches!(event, AppEvent::AiSettingsError(ref message) if message.contains("文件与附件已停用")));
    files.toggle(&mut controller, true);
    assert!(controller.ai_reference_errors().is_empty());
    let job = controller.prepare_ai_conversation("read attachment", &target()).unwrap();
    assert!(files.catalog(&job).describe("file.read").is_some());
}

#[test]
fn disabled_file_plugin_removes_tools_and_rejects_calls_from_an_old_catalog() {
    let files = Files::new();
    let mut controller = files.configured();
    let reference = files.attach(&mut controller, &files.write("plugin.sql", "SELECT 'file-plugin';"));
    let job = controller.prepare_ai_conversation("read attachment", &target()).unwrap();
    let old = files.catalog(&job);
    files.toggle(&mut controller, false);
    let current = files.catalog(&job);
    for (tool, arguments) in [
        ("file.read", json!({"file_ref":reference.id})),
        ("file.search", json!({"query":"plugin"})),
        ("file.search_content", json!({"file_ref":reference.id,"query":"plugin"})),
    ] {
        assert!(current.describe(tool).is_none());
        assert!(current.prepare(tool, &arguments.to_string()).is_err());
        let receipt = execute(&old, &job, tool, arguments, CancelFlag::new());
        assert_eq!(receipt.status, ToolReceiptStatus::Failed);
        assert!(receipt.result_json.contains("文件与附件已停用"));
        assert!(!receipt.result_json.contains("SELECT"));
    }
    let provider = ScriptedGenerationProvider::new(ModelCapabilities::default(), vec![
        vec![ModelEvent::TextDelta("attachments disabled".into()), ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let view = run(&files.0, &job, &provider, &current, false);
    assert!(view.error.is_none(), "{:?}", view.error);
    assert!(provider.request_at(0).unwrap().messages.iter()
        .all(|message| !message.text.contains(&reference.id)));
    files.toggle(&mut controller, true);
    assert!(files.catalog(&job).describe("file.read").is_some());
}

fn execute(
    tools: &ToolCatalog,
    job: &AiConversationJob,
    tool_id: &str,
    args: Value,
    cancel: CancelFlag,
) -> ToolReceipt {
    let prepared = tools
        .prepare(tool_id, &args.to_string())
        .expect("production schema validation");
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(tools.executable(tool_id).unwrap().execute(
            ToolExecutionContext {
                automatic_approval: false,
                bounded_write: false,
                session_id: job.session_id.clone(),
                turn_id: job.turn_id.clone(),
                iteration: 1,
                call_id: "file-fixture-call".into(),
                progress: None,
                cancel,
            },
            prepared.to_json_string(),
        ))
        .unwrap()
}

fn payload(receipt: &ToolReceipt) -> Value {
    assert_eq!(receipt.call_id, "file-fixture-call");
    serde_json::from_str(&receipt.result_json).unwrap()
}

#[test]
fn file_reference_draft_transfer_snapshot_and_reopen_preserve_grant_without_body() {
    let files = Files::new();
    let mut controller = files.configured();
    let text = "SELECT 'unique-file-body-not-for-prompt';\n";
    let path = files.write("orders.sql", text);
    let reference = files.attach(&mut controller, &path);
    assert_eq!(reference.label, "orders.sql");
    assert_eq!(reference.kind, "file");
    assert_eq!(files.0.session_count(), 0);
    drop(controller);
    let mut reopened = files.controller();
    assert_eq!(
        reopened.state().ai_conversation.context_references,
        vec![reference.clone()]
    );
    let job = reopened
        .prepare_ai_conversation("inspect attachment", &target())
        .unwrap();
    let snapshot = files.snapshot(&job);
    assert_eq!(snapshot["context_references"][0]["id"], reference.id);
    assert_eq!(snapshot["context_references"][0]["label"], reference.label);
    assert!(snapshot["context_references"][0].get("source").is_none());
    assert!(
        !snapshot["context_references"]
            .to_string()
            .contains(path.to_str().unwrap())
    );
    assert_eq!(snapshot["file_grants"][0]["source"], reference.source);
    assert!(
        !snapshot
            .to_string()
            .contains("unique-file-body-not-for-prompt")
    );
    files
        .0
        .storage
        .with_ai_connection(|conn| {
            assert!(context_references::list(conn, None)?.is_empty());
            assert_eq!(
                context_references::list(conn, Some(&job.session_id))?.len(),
                0
            );
            Ok(())
        })
        .unwrap();
    let tools = files.catalog(&job);
    assert_eq!(tools.describe("file.read").unwrap().risk, ToolRisk::Read);
    assert_eq!(tools.describe("file.search").unwrap().risk, ToolRisk::Read);
    assert_eq!(
        tools.describe("file.search_content").unwrap().risk,
        ToolRisk::Read
    );
    drop(reopened);
    let reopened = files.controller();
    assert!(
        reopened
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    assert!(reopened.state().ai_conversation.messages.iter().flat_map(|message| &message.parts)
        .any(|part| part.kind == "context_ref" && part.payload_json.contains(&reference.id)));
    assert_eq!(files.snapshot(&job), snapshot);
}

#[cfg(unix)]
#[test]
fn file_search_content_finds_text_and_csv_hits_then_reads_them_back() {
    let files = Files::new();
    let mut controller = files.configured();
    let text = "SELECT 1;\nALTER TABLE Users ADD COLUMN email TEXT;\nSELECT 2;";
    let reference = files.attach(&mut controller, &files.write("body.sql", text));
    let csv = files.write(
        "rows.csv",
        "id,name\n1,张三\n2,Users Team\n3,other\n4,users again\n",
    );
    let csv_reference = files.attach(&mut controller, &csv);
    let job = controller
        .prepare_ai_conversation("search attachment body", &target())
        .unwrap();
    let tools = files.catalog(&job);

    let found = execute(
        &tools,
        &job,
        "file.search_content",
        json!({"file_ref":reference.id,"query":"users","context_lines":1}),
        CancelFlag::new(),
    );
    assert_eq!(found.status, ToolReceiptStatus::Succeeded, "{found:?}");
    let value = payload(&found);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    assert_eq!(value["matches"][0]["line_number"], 2);
    assert_eq!(value["content_type"], "text");
    let read_offset = value["matches"][0]["read_hint"]["offset"].as_u64().unwrap();
    let read = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id,"offset":read_offset,"max_bytes":64}),
        CancelFlag::new(),
    );
    assert!(
        payload(&read)["text"]
            .as_str()
            .unwrap()
            .contains("ALTER TABLE Users")
    );

    let csv_found = execute(
        &tools,
        &job,
        "file.search_content",
        json!({"file_ref":csv_reference.id,"query":"users"}),
        CancelFlag::new(),
    );
    assert_eq!(csv_found.status, ToolReceiptStatus::Succeeded);
    let csv_value = payload(&csv_found);
    assert_eq!(csv_value["content_type"], "delimited");
    let records = csv_value["matches"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["record_number"], 2);
    let record = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":csv_reference.id,
            "offset":records[0]["read_hint"]["offset"],"max_records":1}),
        CancelFlag::new(),
    );
    assert_eq!(payload(&record)["records"][0][1], "Users Team");

    let denied = execute(
        &tools,
        &job,
        "file.search_content",
        json!({"file_ref":"not-authorized","query":"users"}),
        CancelFlag::new(),
    );
    assert_eq!(denied.status, ToolReceiptStatus::Failed);
    assert!(payload(&denied).get("matches").is_none());
}

#[cfg(unix)]
#[test]
fn file_read_returns_actual_bytes_bounded_ranges_eof_and_no_implicit_authorization() {
    let files = Files::new();
    let mut controller = files.configured();
    let text = format!("SELECT 'fixture';\n{}", "a".repeat(70_000));
    let path = files.write("bounded.sql", &text);
    files.write("unattached.sql", "must not be visible");
    let reference = files.attach(&mut controller, &path);
    let job = controller
        .prepare_ai_conversation("bounded file", &target())
        .unwrap();
    let tools = files.catalog(&job);
    for (offset, max_bytes) in [(0, 17), (17, 9), (0, 65_536), (text.len() as u64, 8)] {
        let receipt = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":reference.id,
            "offset":offset,"max_bytes":max_bytes}),
            CancelFlag::new(),
        );
        assert_eq!(receipt.status, ToolReceiptStatus::Succeeded, "{receipt:?}");
        let value = payload(&receipt);
        let end = (offset as usize + max_bytes as usize).min(text.len());
        assert_eq!(value["text"], text[offset as usize..end]);
        assert!(value["text"].as_str().unwrap().len() <= max_bytes as usize);
        assert_eq!(value["next_offset"], end as u64);
        assert_eq!(value["truncated"], end < text.len());
        assert_eq!(value["file_ref"], reference.id);
    }
    let default_read = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id}),
        CancelFlag::new(),
    );
    assert_eq!(
        payload(&default_read)["text"].as_str().unwrap().len(),
        16_384
    );
    for args in [
        json!({"file_ref":reference.id,"max_bytes":65_537}),
        json!({"file_ref":reference.id,"max_bytes":0}),
        json!({"file_ref":reference.id,"offset":-1}),
        json!({"file_ref":reference.id,"path":path}),
    ] {
        assert!(tools.prepare("file.read", &args.to_string()).is_err());
    }
    for id in ["unknown-file-id", path.to_str().unwrap()] {
        let receipt = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":id}),
            CancelFlag::new(),
        );
        assert_eq!(receipt.status, ToolReceiptStatus::Failed);
        assert!(payload(&receipt).get("text").is_none());
    }
}

#[test]
fn file_search_is_label_filtered_bounded_and_only_lists_authorized_files() {
    let files = Files::new();
    let mut controller = files.configured();
    let mut ids = Vec::new();
    for index in 0..16 {
        let path = files.write(&format!("authorized-{index:02}.sql"), "SELECT 1;");
        ids.push(files.attach(&mut controller, &path).id);
    }
    files.write("unauthorized.sql", "SELECT 'not allowed';");
    let job = controller
        .prepare_ai_conversation("search attachments", &target())
        .unwrap();
    let tools = files.catalog(&job);
    let receipt = execute(
        &tools,
        &job,
        "file.search",
        json!({"query":"AUTHORIZED-"}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Succeeded);
    let result = payload(&receipt);
    let found = result["files"].as_array().unwrap();
    assert_eq!(found.len(), 16);
    for (file, id) in found.iter().zip(ids) {
        assert_eq!(file["file_ref"], id);
        assert!(file.get("source").is_none());
        assert!(file.get("text").is_none());
    }
    let matched = execute(
        &tools,
        &job,
        "file.search",
        json!({"query":"authorized-07"}),
        CancelFlag::new(),
    );
    assert_eq!(payload(&matched)["files"].as_array().unwrap().len(), 1);
    let empty = execute(
        &tools,
        &job,
        "file.search",
        json!({"query":"unauthorized"}),
        CancelFlag::new(),
    );
    assert!(payload(&empty)["files"].as_array().unwrap().is_empty());
}

#[test]
fn file_tools_cancel_without_returning_text_or_search_results() {
    let files = Files::new();
    let mut controller = files.configured();
    let reference = files.attach(
        &mut controller,
        &files.write("cancel.sql", "SELECT 'cancel';"),
    );
    let job = controller
        .prepare_ai_conversation("cancel read", &target())
        .unwrap();
    let tools = files.catalog(&job);
    for (tool, args) in [
        ("file.read", json!({"file_ref":reference.id})),
        ("file.search", json!({"query":"cancel"})),
        (
            "file.search_content",
            json!({"file_ref":reference.id,"query":"cancel"}),
        ),
    ] {
        let cancel = CancelFlag::new();
        cancel.request();
        let receipt = execute(&tools, &job, tool, args, cancel);
        assert_eq!(receipt.status, ToolReceiptStatus::Cancelled);
        assert!(receipt.error_code.is_some());
        let value = payload(&receipt);
        assert!(value.get("text").is_none());
        assert!(value.get("files").is_none());
        assert!(value.get("matches").is_none());
    }
}

#[cfg(unix)]
#[test]
fn file_read_turn_persists_actual_receipt_and_supplies_it_to_next_model_request() {
    let files = Files::new();
    let mut controller = files.configured();
    let text = "SELECT 'real-file-receipt-unique';\n";
    let reference = files.attach(&mut controller, &files.write("receipt.sql", text));
    let job = controller
        .prepare_ai_conversation("read attached SQL", &target())
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
                    provider_call_id: "read-receipt".into(),
                    tool_id: "file.read".into(),
                    arguments_json: json!({"file_ref":reference.id,"offset":0,"max_bytes":64})
                        .to_string(),
                }),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![
                ModelEvent::TextDelta("read completed".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ],
    );
    let view = run(&files.0, &job, &provider, &files.catalog(&job), false);
    assert!(view.error.is_none(), "{:?}", view.error);
    // 两次是主任务（工具调用 + 回执后继续），第三次才是标题生成。
    assert_eq!(provider.call_count(), 3);
    let first = provider.request_at(0).unwrap();
    assert!(
        first
            .messages
            .iter()
            .any(|message| message.text.contains(&reference.id))
    );
    assert!(
        first
            .messages
            .iter()
            .all(|message| !message.text.contains("real-file-receipt-unique"))
    );
    let grant: Value = serde_json::from_str(&reference.source).unwrap();
    assert!(
        first
            .messages
            .iter()
            .all(|message| !message.text.contains(grant["path"].as_str().unwrap()))
    );
    let next = provider.request_at(1).unwrap();
    let result = next
        .messages
        .iter()
        .find(|message| message.role == fluxdb_core::ModelRole::Tool)
        .unwrap();
    assert!(result.text.contains("real-file-receipt-unique"));
    files
        .0
        .storage
        .with_ai_connection(|conn| {
            let calls = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?;
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].status, "succeeded");
            let value: Value =
                serde_json::from_str(calls[0].receipt_json.as_deref().unwrap()).unwrap();
            assert_eq!(value["text"], text);
            assert_eq!(value["file_ref"], reference.id);
            assert_eq!(
                sessions::load_turn(conn, &job.turn_id)?.status,
                sessions::TurnStatus::Completed
            );
            Ok(())
        })
        .unwrap();
    command(&mut controller, AiConversationCommand::Apply { view });
    assert!(
        files
            .controller()
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
}

#[test]
fn reference_transfer_rolls_back_on_failed_send_and_new_clears_unsent_references() {
    let files = Files::new();
    let mut controller = files.configured();
    let reference = files.attach(&mut controller, &files.write("rollback.sql", "SELECT 1;"));
    files.0.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER fail_file_turn BEFORE INSERT ON ai_turns BEGIN SELECT RAISE(ABORT,'file fixture failure'); END;").unwrap();
        Ok(())
    }).unwrap();
    assert!(
        controller
            .prepare_ai_conversation("rollback references", &target())
            .is_err()
    );
    assert_eq!(files.0.session_count(), 0);
    assert_eq!(
        files
            .controller()
            .state()
            .ai_conversation
            .context_references,
        vec![reference]
    );
    files
        .0
        .storage
        .with_ai_connection(|conn| {
            assert_eq!(context_references::list(conn, None)?.len(), 1);
            let rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM ai_context_references WHERE session_id IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
    command(&mut controller, AiConversationCommand::New);
    assert!(
        controller
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    assert!(
        files
            .controller()
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn file_read_rejects_changed_grant_and_invalid_utf8_or_split_character() {
    let files = Files::new();
    let mut controller = files.configured();
    let path = files.write("changed.sql", "SELECT 1;");
    let reference = files.attach(&mut controller, &path);
    let utf8 = files.attach(
        &mut controller,
        &files.write("unicode.sql", "你好 SELECT 1;"),
    );
    let job = controller
        .prepare_ai_conversation("changed file", &target())
        .unwrap();
    let tools = files.catalog(&job);
    fs::write(&path, "SELECT 'modified-longer';").unwrap();
    let receipt = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Failed);
    assert!(
        payload(&receipt)["error"]
            .as_str()
            .unwrap()
            .contains("变更")
    );
    let split = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":utf8.id,"offset":1,"max_bytes":3}),
        CancelFlag::new(),
    );
    assert_eq!(split.status, ToolReceiptStatus::Failed);
    assert!(payload(&split).get("text").is_none());
    let valid = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":utf8.id,"max_bytes":6}),
        CancelFlag::new(),
    );
    assert_eq!(valid.status, ToolReceiptStatus::Succeeded);
    assert_eq!(payload(&valid)["text"], "你好");
}

#[test]
fn file_authorization_rejects_config_root_database_binary_and_missing_paths() {
    let files = Files::new();
    let mut controller = files.configured();
    let root_file = files.0.storage.root_dir().join("not-an-attachment.sql");
    fs::write(&root_file, "SELECT 'private config';").unwrap();
    for path in [
        root_file,
        files.write("config.toml", "secret_key = 'fixture-only'"),
        files.write("database.sqlite", b"SQLite format 3\0"),
        files.write("binary.bin", [0, 255, 1]),
        files.0.dir.join("missing.sql"),
    ] {
        assert!(
            authorize_ai_file(&path, files.0.storage.root_dir()).is_err(),
            "{path:?}"
        );
        let forged = AiContextReference {
            id: uuid::Uuid::new_v4().to_string(),
            label: "forged.sql".into(),
            kind: "file".into(),
            source: json!({"path":path,"size":0,"modified":null}).to_string(),
            summary: "forged grant".into(),
        };
        let before = controller.state().ai_conversation.clone();
        assert!(matches!(
            controller.dispatch(AppCommand::AiConversation(
                AiConversationCommand::AddReference { reference: forged }
            )),
            AppEvent::AiSettingsError(_)
        ));
        assert_eq!(&controller.state().ai_conversation, &before);
    }
    assert_eq!(files.0.session_count(), 0);
    assert!(
        files
            .controller()
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn binary_disguised_as_sql_is_never_returned_as_text() {
    let files = Files::new();
    let mut controller = files.configured();
    let path = files.write("binary.sql", [0, 255, 254, 1]);
    // 即便扩展名通过授权，内容也不得按文本硬解：只返回类型、大小与预览。
    if let Ok(reference) = authorize_ai_file(&path, files.0.storage.root_dir()) {
        command(
            &mut controller,
            AiConversationCommand::AddReference {
                reference: reference.clone(),
            },
        );
        let job = controller
            .prepare_ai_conversation("binary must fail", &target())
            .unwrap();
        let receipt = execute(
            &files.catalog(&job),
            &job,
            "file.read",
            json!({"file_ref":reference.id}),
            CancelFlag::new(),
        );
        assert_eq!(receipt.status, ToolReceiptStatus::Succeeded);
        assert!(payload(&receipt).get("text").is_none());
        assert_eq!(payload(&receipt)["content_type"], "binary");
        assert_eq!(payload(&receipt)["size"], 4);
        assert!(payload(&receipt)["preview"].as_str().is_some());
    }
}

#[test]
fn csv_attachment_is_segmented_by_record_not_by_raw_byte_offset() {
    let files = Files::new();
    let mut controller = files.configured();
    // 第二条记录含引号转义与嵌入换行：按字节续读会切坏结构，按记录读必须完整。
    let path = files.write(
        "records.csv",
        "id,name\n1,alice\n2,\"bob,\nline2\"\n3,carol\n",
    );
    let reference = files.attach(&mut controller, &path);
    let job = controller
        .prepare_ai_conversation("csv records", &target())
        .unwrap();
    let tools = files.catalog(&job);
    let first = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id,"max_records":3}),
        CancelFlag::new(),
    );
    assert_eq!(first.status, ToolReceiptStatus::Succeeded, "{first:?}");
    let value = payload(&first);
    assert_eq!(value["content_type"], "csv");
    // 表头也作为一条记录返回，模型据此知道列含义。
    assert_eq!(value["record_count"], 3);
    assert_eq!(value["records"][0][1], "name");
    assert_eq!(value["records"][1][1], "alice");
    assert_eq!(value["records"][2][1], "bob,\nline2");
    assert_eq!(value["truncated"], true);
    // 续读位置必须来自解析器实际停下的字节，能接着读到未返回的第三条数据记录。
    let second = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id,"offset":value["next_offset"],"max_records":8}),
        CancelFlag::new(),
    );
    assert_eq!(second.status, ToolReceiptStatus::Succeeded, "{second:?}");
    let tail = payload(&second);
    assert_eq!(tail["record_count"], 1);
    assert_eq!(tail["records"][0][1], "carol");
    assert_eq!(tail["truncated"], false);
}

#[test]
fn csv_unterminated_quote_is_reported_as_data_not_hard_decoded() {
    let files = Files::new();
    let mut controller = files.configured();
    // 奇数引号在 EOF 处被 csv crate 容忍为数据：仍必须走 CSV 记录通道，不退回文本硬解。
    let path = files.write("broken.csv", "id,name\n1,\"unterminated\n");
    let reference = files.attach(&mut controller, &path);
    let job = controller
        .prepare_ai_conversation("csv broken", &target())
        .unwrap();
    let receipt = execute(
        &files.catalog(&job),
        &job,
        "file.read",
        json!({"file_ref":reference.id}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Succeeded, "{receipt:?}");
    let value = payload(&receipt);
    assert_eq!(value["content_type"], "csv");
    assert_eq!(value["record_count"], 2);
    assert_eq!(value["records"][1][1], "unterminated\n");
}

#[test]
fn csv_record_count_limit_is_bounded() {
    let files = Files::new();
    let mut controller = files.configured();
    let mut body = String::from("id\n");
    for index in 0..500 {
        body.push_str(&format!("{index}\n"));
    }
    let reference = files.attach(&mut controller, &files.write("many.csv", &body));
    let job = controller
        .prepare_ai_conversation("csv bounded", &target())
        .unwrap();
    let tools = files.catalog(&job);
    for requested in [1u64, 5, 256] {
        let receipt = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":reference.id,"max_records":requested}),
            CancelFlag::new(),
        );
        assert_eq!(receipt.status, ToolReceiptStatus::Succeeded, "{receipt:?}");
        let count = payload(&receipt)["record_count"].as_u64().unwrap();
        assert!(count <= requested, "requested={requested} count={count}");
        assert!(count >= 1);
    }
    // 超过 schema 上限的参数在准备阶段就被拒绝，不会静默放大读取量。
    assert!(
        tools
            .prepare(
                "file.read",
                &json!({"file_ref":reference.id,"max_records":100_000}).to_string()
            )
            .is_err()
    );
}

#[test]
fn text_segment_never_splits_a_multibyte_character() {
    let files = Files::new();
    let mut controller = files.configured();
    let text = "订单查询备注".repeat(64);
    let reference = files.attach(&mut controller, &files.write("utf8.md", &text));
    let job = controller
        .prepare_ai_conversation("utf8 boundary", &target())
        .unwrap();
    let tools = files.catalog(&job);
    // 17 不是 3 的倍数：切在汉字中间时必须回退到字符边界，而不是返回非法 UTF-8。
    let receipt = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id,"max_bytes":17}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Succeeded, "{receipt:?}");
    let value = payload(&receipt);
    let returned = value["text"].as_str().unwrap();
    assert!(returned.len() <= 17);
    assert!(text.starts_with(returned));
    // 17 字节落在第 6 个汉字中间，回退后应恰好是 5 个完整汉字（15 字节）。
    assert_eq!(returned, "订单查询备");
    assert_eq!(returned.len(), 15);
    assert_eq!(value["next_offset"], returned.len());
}

#[test]
fn authorization_summary_reports_content_type_and_binary_flag_without_exposing_path() {
    let files = Files::new();
    let text = files.write("summary.sql", "SELECT 1;");
    let text_reference = authorize_ai_file(&text, files.0.storage.root_dir()).unwrap();
    assert!(
        text_reference.summary.contains("UTF-8 文本"),
        "{}",
        text_reference.summary
    );
    assert!(text_reference.summary.contains("按需分段读取"));
    let grant: serde_json::Value = serde_json::from_str(&text_reference.source).unwrap();
    assert_eq!(grant["binary"], false);
    let csv = files.write("summary.csv", "a,b\n1,2\n");
    let csv_reference = authorize_ai_file(&csv, files.0.storage.root_dir()).unwrap();
    assert!(
        csv_reference.summary.contains("CSV/TSV"),
        "{}",
        csv_reference.summary
    );
    let binary = files.write("summary.sql", [0u8, 159, 146, 150]);
    let binary_reference = authorize_ai_file(&binary, files.0.storage.root_dir()).unwrap();
    assert!(
        binary_reference.summary.contains("二进制"),
        "{}",
        binary_reference.summary
    );
    assert!(binary_reference.summary.contains("仅返回类型与预览"));
    let binary_grant: serde_json::Value =
        serde_json::from_str(&binary_reference.source).unwrap();
    assert_eq!(binary_grant["binary"], true);
}

#[test]
fn removing_reference_persists_on_reopen_and_excludes_it_from_next_turn() {
    let files = Files::new();
    let mut controller = files.configured();
    let path = files.write("remove.sql", "SELECT 'remove';");
    let reference = files.attach(&mut controller, &path);
    let view = command(
        &mut controller,
        AiConversationCommand::RemoveReference {
            reference_id: reference.id,
        },
    );
    assert!(view.context_references.is_empty());
    assert!(
        files
            .controller()
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    let reference = files.attach(&mut controller, &path);
    let first = controller
        .prepare_ai_conversation("first grant", &target())
        .unwrap();
    let frozen = files.snapshot(&first);
    finish(&files.0, &mut controller, &first, &first.model_profile_id);
    command(
        &mut controller,
        AiConversationCommand::RemoveReference {
            reference_id: reference.id.clone(),
        },
    );
    drop(controller);
    let mut reopened = files.controller();
    assert!(
        reopened
            .state()
            .ai_conversation
            .context_references
            .is_empty()
    );
    assert_eq!(
        files.snapshot(&first),
        frozen,
        "removal must not rewrite a historical turn"
    );
    let next = reopened
        .prepare_ai_conversation("removed grant", &target())
        .unwrap();
    assert!(
        files.snapshot(&next)["file_grants"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        files.snapshot(&next)["context_references"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let tools = files.catalog(&next);
    assert!(tools.executable("file.read").is_none());
    assert!(tools.executable("file.search").is_none());
}

#[cfg(unix)]
#[test]
fn file_read_nofollow_rejects_leaf_and_intermediate_symlink_replacement() {
    use std::os::unix::fs::symlink;
    for intermediate in [false, true] {
        let files = Files::new();
        let mut controller = files.configured();
        let parent = files.0.dir.join("attachments");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("link-test.sql");
        fs::write(&path, "SELECT 'original';").unwrap();
        let reference = files.attach(&mut controller, &path);
        let job = controller
            .prepare_ai_conversation("nofollow", &target())
            .unwrap();
        let tools = files.catalog(&job);
        let before = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":reference.id}),
            CancelFlag::new(),
        );
        assert_eq!(before.status, ToolReceiptStatus::Succeeded);
        // 链接指向原 inode，大小/mtime 完全相同，确保拒绝来自 NOFOLLOW 而非指纹不符。
        if intermediate {
            let moved = files.0.dir.join("moved-attachments");
            fs::rename(&parent, &moved).unwrap();
            symlink(&moved, &parent).unwrap();
        } else {
            let moved = parent.join("moved.sql");
            fs::rename(&path, &moved).unwrap();
            symlink(&moved, &path).unwrap();
        }
        let receipt = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":reference.id}),
            CancelFlag::new(),
        );
        assert_eq!(
            receipt.status,
            ToolReceiptStatus::Failed,
            "intermediate={intermediate}"
        );
        assert!(
            payload(&receipt)["error"]
                .as_str()
                .unwrap()
                .contains("安全打开失败")
        );
        assert!(payload(&receipt).get("text").is_none());
    }
}

#[cfg(windows)]
#[test]
fn windows_file_read_returns_actual_bytes_for_authorized_files() {
    let files = Files::new();
    let mut controller = files.configured();
    let path = files.write("windows.sql", "SELECT 'windows';");
    let reference = files.attach(&mut controller, &path);
    let job = controller
        .prepare_ai_conversation("windows secure open", &target())
        .unwrap();
    let receipt = execute(
        &files.catalog(&job),
        &job,
        "file.read",
        json!({"file_ref":reference.id}),
        CancelFlag::new(),
    );
    assert_eq!(receipt.status, ToolReceiptStatus::Succeeded);
    assert_eq!(payload(&receipt)["text"], "SELECT 'windows';");
}

#[cfg(windows)]
#[test]
fn windows_file_read_rejects_leaf_and_intermediate_reparse_point_replacement() {
    let files = Files::new();
    let mut controller = files.configured();
    let parent = files.0.dir.join("attachments");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("link-test.sql");
    fs::write(&path, "SELECT 'original';").unwrap();
    let reference = files.attach(&mut controller, &path);
    let job = controller
        .prepare_ai_conversation("windows reparse point", &target())
        .unwrap();
    let tools = files.catalog(&job);
    let before = execute(
        &tools,
        &job,
        "file.read",
        json!({"file_ref":reference.id}),
        CancelFlag::new(),
    );
    assert_eq!(before.status, ToolReceiptStatus::Succeeded);
    for intermediate in [false, true] {
        // 链接指向原 inode，大小/mtime 相同，确保拒绝来自 reparse point 检查而非指纹不符。
        let restore = if intermediate {
            let moved = files.0.dir.join("moved-attachments");
            fs::rename(&parent, &moved).unwrap();
            std::os::windows::fs::symlink_dir(&moved, &parent).unwrap();
            parent.clone()
        } else {
            let moved = parent.join("moved.sql");
            fs::rename(&path, &moved).unwrap();
            std::os::windows::fs::symlink_file(&moved, &path).unwrap();
            moved
        };
        let receipt = execute(
            &tools,
            &job,
            "file.read",
            json!({"file_ref":reference.id}),
            CancelFlag::new(),
        );
        assert_eq!(
            receipt.status,
            ToolReceiptStatus::Failed,
            "intermediate={intermediate}"
        );
        assert!(
            payload(&receipt)["error"]
                .as_str()
                .unwrap()
                .contains("安全打开失败")
        );
        assert!(payload(&receipt).get("text").is_none());
        // 还原后再验证另一种替换，避免上一轮状态影响下一轮。
        if intermediate {
            let moved = files.0.dir.join("moved-attachments");
            fs::remove_dir(&parent).unwrap();
            fs::rename(&moved, &parent).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
            fs::rename(&restore, &path).unwrap();
        }
    }
}
