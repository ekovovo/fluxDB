mod tool_output_tests {
    use super::*;

    fn receipt(value: serde_json::Value) -> fluxdb_core::ToolReceipt {
        fluxdb_core::ToolReceipt {
            call_id: "call-output".into(),
            tool_id: "external.inspect".into(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
            result_json: value.to_string(),
            error_code: None,
            affected_rows: None,
            started_at_ms: 1,
            finished_at_ms: 2,
        }
    }

    fn catalog(shape: ToolOutputShape, store: Arc<Mutex<ResultStore>>) -> ToolCatalog {
        let mut spec = ToolSpec::from_descriptor(&fluxdb_core::ToolDescriptor {
            id: "external.inspect".into(),
            version: 1,
            description: "Read a controlled result".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: r#"{"type":"object"}"#.into(),
        })
        .unwrap();
        spec.output_shape = shape;
        ToolCatalog::new(vec![spec]).with_results(store)
    }

    #[test]
    fn short_collections_are_complete_and_large_ones_keep_a_budgeted_prefix() {
        let store = Arc::new(Mutex::new(ResultStore::new(4096)));
        let catalog = catalog(
            ToolOutputShape::Sample {
                omit_field: "rows".into(),
                rows_omitted_field: Some("rows_omitted".into()),
            },
            store,
        );
        let small =
            receipt(serde_json::json!({"rows":(0..30).collect::<Vec<_>>(),"rows_omitted":0}));
        let projection = catalog.project("external.inspect", &small, None).unwrap();
        assert!(!projection.truncated);
        assert_eq!(projection.text, small.result_json);
        let large = receipt(
            serde_json::json!({"rows":(0..500).map(|id| serde_json::json!({"id":id,"text":"x".repeat(60)})).collect::<Vec<_>>(),"rows_omitted":0}),
        );
        let projection = catalog.project("external.inspect", &large, None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&projection.text).unwrap();
        let count = value["rows"].as_array().unwrap().len();
        assert!(count > 5 && count < 500);
        assert_eq!(value["rows_omitted"], 500 - count);
        assert!(projection.text.len() < TOOL_AUTO_OUTPUT_BYTES);
        assert!(estimate_tokens(&projection.text) < TOOL_AUTO_OUTPUT_TOKENS as i64);
        assert!(projection.result_ref.is_some());
        assert!(
            value.get("next_cursor").is_none(),
            "JSON 片段快照不能使用集合行数作为游标"
        );
        let page_spec = result_read_page_spec().unwrap();
        assert!(
            validate_schema(
                &page_spec.input_schema,
                &serde_json::json!({"result_ref":value["result_ref"]})
            )
            .is_ok()
        );
    }

    #[test]
    fn token_limit_applies_even_when_utf8_bytes_fit_and_snapshot_is_lossless() {
        let store = Arc::new(Mutex::new(ResultStore::new(4096)));
        let catalog = catalog(ToolOutputShape::Opaque, store.clone());
        // CJK 9000 字 ≈ 27 KB（未超 32 KiB 字节上限），但估算 token ≈ 9000 > 8192，
        // 正好验证"字节够、token 不够"时仍按 token 约束触发快照续读。
        let source = receipt(serde_json::json!({"body":"文".repeat(9000)}));
        assert!(source.result_json.len() < TOOL_AUTO_OUTPUT_BYTES);
        let projection = catalog.project("external.inspect", &source, None).unwrap();
        assert!(projection.truncated);
        let reference = projection.result_ref.unwrap();
        // 快照可能跨多页；逐页读到没有游标为止再重组，验证无损且不重不漏。
        let mut reconstructed = String::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .lock()
                .unwrap()
                .read_page(&reference.id, cursor.as_deref(), 50, 16384)
                .unwrap();
            for item in &page.items {
                reconstructed.push_str(item["text"].as_str().unwrap());
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(reconstructed, source.result_json);
        let repeated = catalog.project("external.inspect", &source, None).unwrap();
        assert_eq!(
            repeated.result_ref.unwrap().id,
            reference.id,
            "不重复建立快照"
        );
    }

    #[test]
    fn oversized_execution_receipt_keeps_failure_and_marks_missing_warning_details() {
        let store = Arc::new(Mutex::new(ResultStore::new(4096)));
        let catalog = catalog(ToolOutputShape::Opaque, store);
        let mut source = receipt(serde_json::json!({
            "ok":false,"code":"partial_failure","partial":true,
            "warnings":["x".repeat(40000)],"affected_rows":12,
        }));
        source.status = fluxdb_core::ToolReceiptStatus::Failed;
        source.error_code = Some("partial_failure".into());
        source.affected_rows = Some(12);
        let before = source.result_json.clone();
        let projection = catalog.project("external.inspect", &source, None).unwrap();
        let value: serde_json::Value = serde_json::from_str(&projection.text).unwrap();
        assert_eq!(value["status"], "failed");
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], "partial_failure");
        assert_eq!(value["partial"], true);
        assert_eq!(value["affected_rows"], 12);
        assert_eq!(value["details_complete"], false);
        assert_eq!(
            value["omitted_detail_fields"],
            serde_json::json!(["warnings"])
        );
        assert!(projection.result_ref.is_some());
        assert_eq!(source.result_json, before);
    }

    #[test]
    fn active_reads_keep_the_page_and_source_cursor_under_their_own_budget() {
        let store = Arc::new(Mutex::new(ResultStore::new(4096)));
        let catalog = catalog(ToolOutputShape::Read, store);
        let source = receipt(serde_json::json!({"content":"x".repeat(12000),"next_offset":12000}));
        let projection = catalog.project("external.inspect", &source, None).unwrap();
        assert!(!projection.truncated);
        assert_eq!(projection.text, source.result_json);
    }

    #[test]
    fn old_handle_never_aliases_a_new_turn_and_foreign_context_is_rejected() {
        let mut old = ResultStore::for_turn(4096, "s", "old", None);
        let (old_ref, _) = old
            .put(
                "external.inspect",
                "old-call",
                &[],
                1,
                [serde_json::json!(1)],
            )
            .unwrap();
        let mut current = ResultStore::for_turn(4096, "s", "current", None);
        let (current_ref, _) = current
            .put(
                "external.inspect",
                "new-call",
                &[],
                1,
                [serde_json::json!(2)],
            )
            .unwrap();
        assert_ne!(old_ref.id, current_ref.id);
        assert_eq!(
            current
                .read_page(&old_ref.id, None, 10, 4096)
                .unwrap_err()
                .code,
            "unknown_result"
        );
        let tool = AiResultReadPageTool::new(Arc::new(Mutex::new(current))).unwrap();
        let result = ai_result_pages_run(
            &tool,
            &serde_json::json!({"result_ref":{"id":current_ref.id}}).to_string(),
        );
        assert_eq!(result.error_code.as_deref(), Some("unknown_result"));
    }

    #[test]
    fn oversized_single_item_is_an_error_not_an_overbudget_page_or_empty_cursor() {
        let mut store = ResultStore::new(4096);
        let (reference, _) = store
            .put(
                "external.inspect",
                "c",
                &[],
                1,
                [serde_json::json!("x".repeat(100))],
            )
            .unwrap();
        let error = store.read_page(&reference.id, None, 1, 32).unwrap_err();
        assert_eq!(error.code, "result_item_too_large");
        let page = store.read_page(&reference.id, None, 1, 128).unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn history_preserves_latest_read_and_tool_pairing_without_changing_original_messages() {
        let mut messages = Vec::new();
        for i in 0..8 {
            messages.push(fluxdb_core::ModelMessage::assistant_with_tool_calls(
                String::new(),
                vec![fluxdb_core::ModelToolCall {
                    provider_call_id: format!("c{i}"),
                    tool_id: "file.read".into(),
                    arguments_json: "{}".into(),
                }],
            ));
            messages.push(fluxdb_core::ModelMessage::tool_result(
                format!("c{i}"),
                "file.read",
                serde_json::json!({"status":"succeeded","result":{"content":"x".repeat(16000)}})
                    .to_string(),
            ));
        }
        let before = messages.clone();
        let bounded = bounded_tool_history(&messages).unwrap();
        assert_eq!(bounded.len(), messages.len());
        assert_eq!(bounded.last(), messages.last());
        assert!(
            bounded
                .iter()
                .any(|message| message.text.contains("history_details_omitted"))
        );
        assert_eq!(messages, before);
        let tool_bytes: usize = bounded
            .iter()
            .filter(|message| message.role == fluxdb_core::ModelRole::Tool)
            .map(|message| message.text.len())
            .sum();
        // 收紧后的轮内历史不得超过其自身预算；用常量断言，避免手工数字随预算漂移。
        assert!(
            tool_bytes <= TOOL_HISTORY_BYTES,
            "轮内工具历史必须收在预算内，实际 {tool_bytes}"
        );
    }

    #[test]
    fn runtime_continues_non_database_output_without_reexecuting_the_source() {
        struct Source {
            descriptor: fluxdb_core::ToolDescriptor,
            calls: Arc<std::sync::atomic::AtomicUsize>,
            body: String,
            invalid_identity: bool,
        }
        impl fluxdb_core::AiTool for Source {
            fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
                &self.descriptor
            }
            fn execute(
                &self,
                ctx: fluxdb_core::ToolExecutionContext,
                _: String,
            ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
                Box::pin(async move {
                    self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut result = receipt(serde_json::Value::Null);
                    result.call_id = ctx.call_id;
                    if self.invalid_identity {
                        result.call_id = "foreign-call".into();
                    }
                    result.result_json = self.body.clone();
                    Ok(result)
                })
            }
        }
        struct Reader {
            step: Mutex<usize>,
            reference: Mutex<String>,
            collected: Mutex<String>,
        }
        impl fluxdb_core::GenerationProvider for Reader {
            fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
                fluxdb_core::ModelCapabilities {
                    streaming: true,
                    tool_calling: true,
                    reasoning_summary: false,
                }
            }
            fn stream(
                &self,
                request: fluxdb_core::ModelRequest,
                _: fluxdb_core::CancelFlag,
            ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
                Box::pin(async move {
                    let mut step = self.step.lock().unwrap();
                    let mut args = serde_json::json!({});
                    let tool = if *step == 0 {
                        "external.inspect"
                    } else {
                        let last = request.messages.last().unwrap();
                        let envelope: serde_json::Value = serde_json::from_str(&last.text).unwrap();
                        assert_eq!(envelope["status"], "succeeded");
                        assert!(last.text.len() <= TOOL_READ_OUTPUT_BYTES);
                        let value = &envelope["result"];
                        if *step == 1 {
                            *self.reference.lock().unwrap() =
                                value["result_ref"]["id"].as_str().unwrap().into();
                            assert_eq!(value["delivery_truncated"], true);
                        } else {
                            let mut collected = self.collected.lock().unwrap();
                            for item in value["items"].as_array().unwrap() {
                                assert_eq!(item["offset_bytes"], collected.len());
                                collected.push_str(item["text"].as_str().unwrap());
                            }
                            if value["next_cursor"].is_null() {
                                return Ok(vec![
                                    fluxdb_core::ModelEvent::TextDelta("Read complete".into()),
                                    fluxdb_core::ModelEvent::Finished(
                                        fluxdb_core::FinishReason::Stop,
                                    ),
                                ]);
                            }
                            args["cursor"] = value["next_cursor"].clone();
                        }
                        args["result_ref"] =
                            serde_json::json!({"id":self.reference.lock().unwrap().clone()});
                        args["limit"] = serde_json::json!(4);
                        args["max_bytes"] = serde_json::json!(8192);
                        "result.read_page"
                    };
                    *step += 1;
                    Ok(vec![
                        fluxdb_core::ModelEvent::ToolCallRequested(fluxdb_core::ToolCallRequest {
                            provider_call_id: format!("call-{}", *step),
                            tool_id: tool.into(),
                            arguments_json: args.to_string(),
                        }),
                        fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
                    ])
                })
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body =
            serde_json::json!({"nested":{"log":"line\n".repeat(9000)},"done":true}).to_string();
        let source = Source {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "external.inspect".into(),
                version: 1,
                description: "Controlled non-database output".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: r#"{"type":"object","additionalProperties":false}"#.into(),
            },
            calls: calls.clone(),
            body: body.clone(),
            invalid_identity: false,
        };
        let results = Arc::new(Mutex::new(ResultStore::for_turn(
            128,
            "loop-session",
            "loop-turn",
            None,
        )));
        let spec = ToolSpec::from_descriptor(fluxdb_core::AiTool::descriptor(&source)).unwrap();
        let mut catalog = ToolCatalog::new(vec![spec]).with_results(results.clone());
        catalog.register(Box::new(source)).unwrap();
        catalog
            .add_tool_with_shape(
                Box::new(AiResultReadPageTool::new(results).unwrap()),
                ToolOutputShape::Read,
            )
            .unwrap();
        let reader = Reader {
            step: Mutex::new(0),
            reference: Mutex::new(String::new()),
            collected: Mutex::new(String::new()),
        };
        let (outcome, _) = ai_loop_test_support::run(
            &reader,
            &catalog,
            LoopLimits::default(),
            "offline",
            vec![ai_loop_test_support::user_message("Read the saved output")],
            &mut Vec::new(),
            &fluxdb_core::CancelFlag::new(),
        );
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{outcome:?}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(*reader.collected.lock().unwrap(), body);
        let descriptor = loop_tool_descriptors(&catalog)
            .into_iter()
            .find(|tool| tool.id == "external.inspect")
            .unwrap();
        let mut bad_catalog =
            ToolCatalog::new(vec![ToolSpec::from_descriptor(&descriptor).unwrap()]);
        bad_catalog
            .register(Box::new(Source {
                descriptor,
                calls: calls.clone(),
                body: "{}".into(),
                invalid_identity: true,
            }))
            .unwrap();
        let provider = ScriptedGenerationProvider::new(
            fluxdb_core::GenerationProvider::capabilities(&reader),
            vec![vec![
                fluxdb_core::ModelEvent::ToolCallRequested(fluxdb_core::ToolCallRequest {
                    provider_call_id: "authorized-call".into(),
                    tool_id: "external.inspect".into(),
                    arguments_json: "{}".into(),
                }),
                fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
            ]],
        );
        let (outcome, events) = ai_loop_test_support::run(
            &provider,
            &bad_catalog,
            LoopLimits::default(),
            "offline",
            vec![ai_loop_test_support::user_message("Read once")],
            &mut Vec::new(),
            &fluxdb_core::CancelFlag::new(),
        );
        assert!(matches!(outcome, TurnOutcome::Failed { .. }));
        assert_eq!(
            provider.call_count(),
            1,
            "归属不符不能再次请求模型或重放工具"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(events.iter().any(|event| matches!(event, LoopEvent::ToolFinished { receipt, .. }
            if receipt.call_id == "authorized-call" && receipt.status == fluxdb_core::ToolReceiptStatus::Unknown)));
    }

    #[test]
    fn context_estimation_includes_tool_arguments_and_definitions_not_only_text() {
        let plain = fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::Assistant, "");
        let call = fluxdb_core::ModelMessage::assistant_with_tool_calls(
            String::new(),
            vec![fluxdb_core::ModelToolCall {
                provider_call_id: "c".into(),
                tool_id: "external.inspect".into(),
                arguments_json: serde_json::json!({"query":"x".repeat(8000)}).to_string(),
            }],
        );
        assert!(estimate_messages_tokens(&[call]) > estimate_messages_tokens(&[plain]) + 2000);
        let spec = result_read_page_spec().unwrap();
        let tools = loop_tool_descriptors(&ToolCatalog::new(vec![spec]));
        assert!(estimate_tools_tokens(&tools) > 100);
    }
}
