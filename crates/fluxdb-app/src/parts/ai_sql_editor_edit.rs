// SQL 修改只授权宿主选定的文本区间；正文快照仅在本轮内存中保留。
const SQL_EDITOR_EDIT_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct SqlEditorEditGrant {
    pub tab_id: u64,
    pub target: TargetRef,
    pub snapshot: fluxdb_editor_core::BufferSnapshot,
    pub range: fluxdb_editor_core::Range,
    /// true：用户在编辑器显式发起修改/修复，本轮只允许修改、禁止执行 SQL。
    /// false：发送时自动绑定选中文本；仍可正常问答/查询，只有实际改写才写回。
    pub exclusive_edit: bool,
}

impl SqlEditorEditGrant {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.range.start >= self.range.end
            || self.range.end > self.snapshot.len()
            || self.range.len() > SQL_EDITOR_EDIT_MAX_BYTES
            || self.snapshot.clamp_to_char_boundary(self.range.start) != self.range.start
            || self.snapshot.clamp_to_char_boundary(self.range.end) != self.range.end
            || self.snapshot.text_in_range(self.range).trim().is_empty()
        {
            return Err("sql_edit_invalid");
        }
        Ok(())
    }

    pub fn matches(
        &self,
        current: &fluxdb_editor_core::BufferSnapshot,
        target: &TargetRef,
    ) -> bool {
        self.validate().is_ok()
            && &self.target == target
            && self.snapshot.version() == current.version()
            && self.snapshot.len() == current.len()
            && self.snapshot.text_in_range(self.range) == current.text_in_range(self.range)
    }
}

struct SqlEditorEditDelivery {
    result: Option<Result<(), &'static str>>,
    waker: Option<std::task::Waker>,
}

/// 回执等待 UI 的真实结果；超时、取消或 Future 被丢弃后，迟到动作不能继续写入。
pub struct SqlEditorEditRequest {
    pub grant: SqlEditorEditGrant,
    sql: String,
    cancel: fluxdb_core::CancelFlag,
    deadline: std::time::Instant,
    delivery: Mutex<SqlEditorEditDelivery>,
}

impl std::fmt::Debug for SqlEditorEditRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlEditorEditRequest")
            .field("tab_id", &self.grant.tab_id)
            .finish_non_exhaustive()
    }
}

impl SqlEditorEditRequest {
    pub fn reject(&self, code: &'static str) {
        if let Ok(mut delivery) = self.delivery.lock()
            && delivery.result.is_none()
        {
            delivery.result = Some(Err(code));
            if let Some(waker) = delivery.waker.take() {
                waker.wake();
            }
        }
    }

    pub fn apply(
        &self,
        current: &fluxdb_editor_core::BufferSnapshot,
        target: &TargetRef,
        replace: impl FnOnce(fluxdb_editor_core::Range, &str),
    ) -> Result<(), &'static str> {
        let mut delivery = self.delivery.lock().map_err(|_| "sql_edit_unavailable")?;
        if delivery.result.is_some() {
            return Err("sql_edit_unavailable");
        }
        let result = if self.cancel.is_cancelled() {
            Err("sql_edit_cancelled")
        } else if std::time::Instant::now() >= self.deadline {
            Err("sql_edit_timeout")
        } else if !self.grant.matches(current, target) {
            Err("sql_edit_conflict")
        } else {
            replace(self.grant.range, &self.sql);
            Ok(())
        };
        delivery.result = Some(result);
        if let Some(waker) = delivery.waker.take() {
            waker.wake();
        }
        result
    }

    async fn wait(&self) -> Result<(), &'static str> {
        std::future::poll_fn(|cx| {
            if self.cancel.is_cancelled() {
                self.reject("sql_edit_cancelled");
            } else if std::time::Instant::now() >= self.deadline {
                self.reject("sql_edit_timeout");
            }
            let Ok(mut delivery) = self.delivery.lock() else {
                return std::task::Poll::Ready(Err("sql_edit_unavailable"));
            };
            match delivery.result {
                Some(result) => std::task::Poll::Ready(result),
                None => {
                    delivery.waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                }
            }
        })
        .await
    }
}

struct SqlEditorEditWaitGuard(Arc<SqlEditorEditRequest>);

impl Drop for SqlEditorEditWaitGuard {
    fn drop(&mut self) {
        self.0.reject("sql_edit_cancelled");
    }
}

struct SqlEditorEditTool {
    descriptor: fluxdb_core::ToolDescriptor,
    grant: SqlEditorEditGrant,
    actions: Arc<dyn UiActionSink>,
    submitted: std::sync::atomic::AtomicBool,
}

impl fluxdb_core::AiTool for SqlEditorEditTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let started_at_ms = now_ms();
            let result = async {
                if ctx.cancel.is_cancelled() {
                    return Err("sql_edit_cancelled");
                }
                let value: serde_json::Value =
                    serde_json::from_str(&arguments_json).map_err(|_| "sql_edit_invalid")?;
                let schema: serde_json::Value =
                    serde_json::from_str(&self.descriptor.input_schema_json)
                        .map_err(|_| "sql_edit_invalid")?;
                crate::validate_schema(&schema, &value).map_err(|_| "sql_edit_invalid")?;
                let sql = value["sql"].as_str().ok_or("sql_edit_invalid")?;
                if sql.trim().is_empty()
                    || sql.len() > SQL_EDITOR_EDIT_MAX_BYTES
                    || sql.trim_start().starts_with("```")
                {
                    return Err("sql_edit_invalid");
                }
                if self
                    .submitted
                    .swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    return Err("sql_edit_unavailable");
                }
                let request = Arc::new(SqlEditorEditRequest {
                    grant: self.grant.clone(),
                    sql: sql.to_string(),
                    cancel: ctx.cancel.clone(),
                    deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
                    delivery: Mutex::new(SqlEditorEditDelivery {
                        result: None,
                        waker: None,
                    }),
                });
                let _guard = SqlEditorEditWaitGuard(Arc::clone(&request));
                self.actions
                    .submit(UiAction::EditSqlEditor(Arc::clone(&request)))
                    .map_err(|_| "sql_edit_unavailable")?;
                request.wait().await
            }
            .await;
            let (status, value, code) = match result {
                Ok(()) => (
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    serde_json::json!({"status":"applied","executed":false,"undoable":true}),
                    None,
                ),
                Err(code) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error":code,"retryable":false}),
                    Some(code.into()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: value.to_string(),
                error_code: code,
                affected_rows: None,
                started_at_ms,
                finished_at_ms: now_ms(),
            })
        })
    }
}

pub fn add_ai_sql_editor_edit_tool(
    catalog: &mut ToolCatalog,
    grant: SqlEditorEditGrant,
    actions: Arc<dyn UiActionSink>,
) -> fluxdb_core::Result<()> {
    grant.validate().map_err(editor_error)?;
    catalog.add_tool_with_shape_and_effect(
        Box::new(SqlEditorEditTool {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "editor.apply_edit".into(),
                version: 1,
                description: "把修改完成的 SQL 一次性替换到用户本轮授权的选区或语句；可撤销，不保存文件、不执行 SQL。只传纯 SQL，不含 Markdown 围栏或解释。只能提交一次；目标已变化则拒绝写入。".into(),
                // 本地草稿不授予数据库写入权限；实际副作用单独标为 LocalUiMutation。
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: serde_json::json!({
                    "type":"object","additionalProperties":false,"required":["sql"],
                    "properties":{"sql":{"type":"string","minLength":1,"maxLength":65536}}
                }).to_string(),
            },
            grant,
            actions,
            submitted: std::sync::atomic::AtomicBool::new(false),
        }),
        ToolOutputShape::Opaque,
        Some(EffectClass::LocalUiMutation),
    )
}

/// 修复/改写任务只保留元数据与已授权文本能力；未知或猜测的执行工具也无法调用。
pub fn restrict_sql_editor_edit_tools(catalog: &mut ToolCatalog) {
    const ALLOWED: &[&str] = &[
        "editor.apply_edit",
        "editor.search",
        "editor.read_range",
        "db.search_objects",
        "db.describe_object",
        "db.get_relationships",
        "db.find_join_paths",
    ];
    let disabled = catalog
        .specs()
        .iter()
        .filter(|spec| !ALLOWED.contains(&spec.id.as_str()))
        .map(|spec| spec.id.clone())
        .collect();
    catalog.without_tools(&disabled);
}

#[cfg(test)]
mod ai_sql_editor_edit_tests {
    use super::*;
    use fluxdb_editor_core::{EditorBuffer, Range};

    fn grant(buffer: &EditorBuffer, range: Range) -> SqlEditorEditGrant {
        SqlEditorEditGrant {
            tab_id: 7,
            target: TargetRef {
                connection_id: Some(1),
                ..Default::default()
            },
            snapshot: buffer.snapshot(),
            range,
            exclusive_edit: true,
        }
    }

    fn request(grant: SqlEditorEditGrant, sql: &str) -> Arc<SqlEditorEditRequest> {
        Arc::new(SqlEditorEditRequest {
            grant,
            sql: sql.into(),
            cancel: fluxdb_core::CancelFlag::new(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
            delivery: Mutex::new(SqlEditorEditDelivery {
                result: None,
                waker: None,
            }),
        })
    }

    #[test]
    fn selected_unicode_range_is_replaced_once_and_undo_restores_it() {
        let original = "SELECT '中文';\nSELEC 1;\nSELECT 3;";
        let mut buffer = EditorBuffer::new_from(original);
        let start = original.find("SELEC 1;").expect("statement");
        let request = request(grant(&buffer, Range::new(start, start + 8)), "SELECT 2;");
        let snapshot = buffer.snapshot();
        let target = request.grant.target.clone();
        request
            .apply(&snapshot, &target, |range, sql| {
                buffer.edit(
                    range,
                    sql,
                    range.start + sql.len(),
                    range.start + sql.len(),
                    false,
                );
            })
            .expect("apply");
        assert_eq!(buffer.to_string(), "SELECT '中文';\nSELECT 2;\nSELECT 3;");
        assert_eq!(
            request.apply(&buffer.snapshot(), &target, |_, _| panic!("replayed")),
            Err("sql_edit_unavailable")
        );
        assert!(buffer.undo().is_some());
        assert_eq!(buffer.to_string(), original);
    }

    #[test]
    fn changed_text_or_database_target_is_never_overwritten() {
        let mut buffer = EditorBuffer::new_from("SELECT 1;");
        let edit = grant(&buffer, Range::new(0, 9));
        buffer.edit(Range::new(7, 8), "2", 8, 8, false);
        let request = request(edit.clone(), "SELECT 3;");
        assert_eq!(
            request.apply(&buffer.snapshot(), &edit.target, |_, _| panic!(
                "overwritten"
            )),
            Err("sql_edit_conflict")
        );
        assert_eq!(buffer.to_string(), "SELECT 2;");
        let mut target = edit.target.clone();
        target.database = Some("other".into());
        assert!(!edit.matches(&edit.snapshot, &target));
        let recreated = EditorBuffer::new_from("SELECT 9;");
        assert!(!edit.matches(&recreated.snapshot(), &edit.target));
    }

    #[test]
    fn cancelled_expired_or_abandoned_requests_cannot_apply_late() {
        let buffer = EditorBuffer::new_from("SELECT 1;");
        for cause in ["cancel", "timeout", "abandon"] {
            let mut request = request(grant(&buffer, Range::new(0, 9)), "SELECT 2;");
            if cause == "cancel" {
                request.cancel.request();
            } else if cause == "timeout" {
                Arc::get_mut(&mut request).expect("unique request").deadline =
                    std::time::Instant::now();
            } else {
                drop(SqlEditorEditWaitGuard(Arc::clone(&request)));
            }
            assert!(
                request
                    .apply(&buffer.snapshot(), &request.grant.target, |_, _| panic!(
                        "late edit"
                    ))
                    .is_err()
            );
            assert_eq!(buffer.to_string(), "SELECT 1;");
        }
    }

    struct ApplyingSink(Mutex<EditorBuffer>);
    impl UiActionSink for ApplyingSink {
        fn submit(&self, action: UiAction) -> Result<(), UiActionError> {
            let UiAction::EditSqlEditor(request) = action else {
                panic!("wrong action")
            };
            let mut buffer = self.0.lock().expect("buffer");
            request
                .apply(&buffer.snapshot(), &request.grant.target, |range, sql| {
                    buffer.edit(
                        range,
                        sql,
                        range.start + sql.len(),
                        range.start + sql.len(),
                        false,
                    );
                })
                .expect("host apply");
            Ok(())
        }
    }

    #[test]
    fn edit_tool_reports_real_writeback_and_rejects_markdown_and_replay() {
        let buffer = EditorBuffer::new_from("SELEC 1;");
        let edit = grant(&buffer, Range::new(0, 8));
        let sink = Arc::new(ApplyingSink(Mutex::new(buffer)));
        let mut catalog = ToolCatalog::new(Vec::new());
        add_ai_sql_editor_edit_tool(&mut catalog, edit, sink.clone()).expect("tool");
        let tool = catalog.executable("editor.apply_edit").expect("tool");
        let run = |sql: &str| {
            let ctx = fluxdb_core::ToolExecutionContext {
                call_id: "edit".into(),
                session_id: "session".into(),
                turn_id: "turn".into(),
                iteration: 1,
                automatic_approval: false,
                bounded_write: false,
                cancel: fluxdb_core::CancelFlag::new(),
                progress: None,
            };
            ai_executor::block_on(tool.execute(ctx, serde_json::json!({"sql":sql}).to_string()))
                .expect("executor")
                .expect("receipt")
        };
        assert_eq!(
            run("```sql\nSELECT 1;\n```").error_code.as_deref(),
            Some("sql_edit_invalid")
        );
        let receipt = run("SELECT 1;");
        assert!(receipt.is_success());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&receipt.result_json).expect("JSON")["executed"],
            false
        );
        assert_eq!(sink.0.lock().expect("buffer").to_string(), "SELECT 1;");
        assert_eq!(
            run("SELECT 2;").error_code.as_deref(),
            Some("sql_edit_unavailable")
        );
    }

    #[test]
    fn edit_catalog_removes_database_execution_and_configuration_actions() {
        let mut specs = standard_sql_tool_specs().expect("SQL tools");
        for id in ["redis.execute", "config.setting.update", "toolbox.load"] {
            specs.push(
                ToolSpec::from_descriptor(&fluxdb_core::ToolDescriptor {
                    id: id.into(),
                    version: 1,
                    description: id.into(),
                    risk: fluxdb_core::ToolRisk::Read,
                    input_schema_json: "{\"type\":\"object\"}".into(),
                })
                .expect("spec"),
            );
        }
        let mut catalog = ToolCatalog::new(specs);
        restrict_sql_editor_edit_tools(&mut catalog);
        for id in [
            "db.run",
            "redis.execute",
            "config.setting.update",
            "toolbox.load",
        ] {
            assert!(catalog.describe(id).is_none(), "{id}");
            assert!(catalog.executable(id).is_none(), "{id}");
            assert!(catalog.prepare(id, "{}").is_err(), "{id}");
        }
        assert!(catalog.describe("db.describe_object").is_some());
        assert!(catalog.describe("db.search_objects").is_some());
    }

    #[test]
    fn invalid_or_unbounded_grants_are_rejected() {
        let buffer = EditorBuffer::new_from("SELECT '文';");
        for range in [Range::new(0, 0), Range::new(0, 500), Range::new(9, 10)] {
            assert_eq!(grant(&buffer, range).validate(), Err("sql_edit_invalid"));
        }
        let buffer = EditorBuffer::new_from(&"a".repeat(SQL_EDITOR_EDIT_MAX_BYTES + 1));
        assert_eq!(
            grant(&buffer, Range::new(0, buffer.len())).validate(),
            Err("sql_edit_invalid")
        );
    }

    #[test]
    fn missing_edit_grant_fails_closed_before_model_or_database_execution() {
        let path = std::env::temp_dir().join(format!("fluxdb-sql-edit-{}", uuid::Uuid::new_v4()));
        let storage = fluxdb_storage::FileStorage::new(path.clone());
        storage
            .save_settings(&Settings::default())
            .expect("settings");
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let event = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "offline fixture".into(),
            provider: "openai".into(),
            model: "fixture".into(),
            endpoint: Some("https://models.invalid/v1".into()),
            enabled: true,
            ..Default::default()
        }));
        let AppEvent::AiModelSaved { id, .. } = event else {
            panic!("{event:?}")
        };
        let revision = storage
            .with_ai_connection(fluxdb_storage::ai_platform::settings::load_runtime_settings)
            .expect("runtime settings")
            .revision;
        controller.dispatch(AppCommand::SetAiDefaultModel {
            model_id: Some(id),
            expected_revision: revision,
        });
        let buffer = EditorBuffer::new_from("SELEC 1;");
        let edit = grant(&buffer, Range::new(0, 8));
        let snapshot =
            capture_editor_snapshot(7, edit.target.clone(), &buffer.snapshot(), edit.range);
        let pin = pin_target(&snapshot);
        let job = controller
            .prepare_ai_conversation_page(
                "Fix SQL",
                &pin,
                Some(CurrentPageContext {
                    kind: PageContextKind::SqlEditor,
                    label: "SQL".into(),
                    snapshot,
                    details: serde_json::json!({}),
                    data_grant: None,
                    editor_grant: Some(EditorGrant {
                        tab_ref: "tab-7".into(),
                        snapshot: buffer.snapshot(),
                        sql_edit: Some(edit),
                    }),
                }),
            )
            .expect("prepare edit turn");
        // 模拟重启或审批恢复后，内存授权已经丢失；不能退回普通数据库对话。
        clear_current_page_grants(&job.turn_id);
        let mut runtime = AiRuntimeHandle::new(1).expect("runtime");
        let (actions, _rx) = ui_action_channel(1);
        let events = runtime
            .submit_conversation(
                storage.clone(),
                job,
                Vec::new(),
                None,
                Some(Arc::new(actions)),
                None,
                fluxdb_core::CancelFlag::new(),
            )
            .expect("submit production runtime");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let terminal = loop {
            if let Ok(update) = events.try_recv()
                && update.terminal
            {
                break update;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing grant did not terminate"
            );
            std::thread::yield_now();
        };
        assert_eq!(
            terminal.view.expect("terminal view").error.as_deref(),
            Some("sql_edit_unavailable")
        );
        assert!(runtime.shutdown(std::time::Duration::from_secs(2)));
        std::fs::remove_dir_all(path).expect("remove isolated profile");
    }

    #[test]
    fn editor_edit_cannot_use_a_different_turn_target() {
        let buffer = EditorBuffer::new_from("SELECT 1;");
        let edit = grant(&buffer, Range::new(0, 9));
        let snapshot = capture_editor_snapshot(
            7,
            TargetRef {
                connection_id: Some(2),
                ..Default::default()
            },
            &buffer.snapshot(),
            edit.range,
        );
        let pin = pin_target(&snapshot);
        let error = AppController::new()
            .prepare_ai_conversation_page(
                "Edit SQL",
                &pin,
                Some(CurrentPageContext {
                    kind: PageContextKind::SqlEditor,
                    label: "SQL".into(),
                    snapshot,
                    details: serde_json::json!({}),
                    data_grant: None,
                    editor_grant: Some(EditorGrant {
                        tab_ref: "tab-7".into(),
                        snapshot: buffer.snapshot(),
                        sql_edit: Some(edit),
                    }),
                }),
            )
            .expect_err("reject mismatched target before reading settings");
        assert_eq!(error.message, "sql_edit_conflict");
    }
}
