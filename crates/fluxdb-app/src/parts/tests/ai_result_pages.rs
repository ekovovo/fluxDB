// T027 结果分页工具测试：`result.read_page` 真实可被模型调用。
//
// 覆盖三条硬规则：读已取得的快照（不重跑来源工具）、过期/未知/跨会话是受控错误、
// 分页有界并给出续读游标。不访问数据库或网络。

fn ai_result_pages_tool(
    results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
) -> AiResultReadPageTool {
    AiResultReadPageTool::new(results).expect("构造 result.read_page 工具")
}

fn ai_result_pages_ctx(call_id: &str) -> fluxdb_core::ToolExecutionContext {
    fluxdb_core::ToolExecutionContext {
        session_id: "s-1".into(),
        turn_id: "t-1".into(),
        iteration: 1,
        call_id: call_id.to_string(),
        cancel: fluxdb_core::CancelFlag::new(),
        progress: None,
    }
}

fn ai_result_pages_run(tool: &AiResultReadPageTool, arguments: &str) -> fluxdb_core::ToolReceipt {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(tool.execute(ai_result_pages_ctx("call-read-page"), arguments.to_string()))
        .expect("工具执行")
}

#[test]
fn ai_result_pages_schema_is_registered_and_rejects_bad_arguments() {
    let spec = result_read_page_spec().expect("工具定义");
    assert_eq!(spec.id, "result.read_page");
    assert_eq!(spec.risk, fluxdb_core::ToolRisk::Read);
    assert!(spec.input_schema["properties"]["result_ref"].is_object());

    // catalog 里必须 describe 得到，且有可执行实现（不出现"看得见、调不动"）。
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::new(4096)));
    let mut catalog = ToolCatalog::new(vec![spec]);
    catalog
        .register(Box::new(ai_result_pages_tool(std::sync::Arc::clone(
            &results,
        ))))
        .expect("注册 result.read_page");
    assert!(catalog.describe("result.read_page").is_some());
    assert!(catalog.executable("result.read_page").is_some());
    assert!(
        catalog
            .prepare("result.read_page", r#"{"limit":5}"#)
            .is_err()
    );
}

#[test]
fn ai_result_pages_reads_saved_results_page_by_page_without_recomputing() {
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64,
        "s-1",
        "t-1",
        Some(root.clone()),
    )));
    let rows: Vec<serde_json::Value> = (0..25)
        .map(|n| serde_json::json!({"n": n, "text": "x".repeat(8)}))
        .collect();
    let (result, spill) = results
        .lock()
        .unwrap()
        .put("db.query", "call-rows", &[], 7, rows)
        .expect("保存结果");
    assert!(spill.spilled(), "大结果应走受控 spill");

    let tool = ai_result_pages_tool(std::sync::Arc::clone(&results));
    let mut seen: Vec<i64> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let arguments = match cursor.as_deref() {
            Some(cursor) => serde_json::json!({
                "result_ref": {"id": result.id},
                "cursor": cursor,
                "limit": 7,
            }),
            None => serde_json::json!({
                "result_ref": {"id": result.id},
                "limit": 7,
            }),
        };
        let receipt = ai_result_pages_run(&tool, &arguments.to_string());
        assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
        let payload: serde_json::Value =
            serde_json::from_str(&receipt.result_json).expect("结果载荷");
        assert_eq!(payload["ok"], true, "读取成功不应是空页：{payload}");
        assert_eq!(payload["total_items"], 25);
        for item in payload["items"].as_array().expect("条目数组") {
            seen.push(item["n"].as_i64().expect("行号"));
        }
        match payload["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        seen,
        (0..25).collect::<Vec<i64>>(),
        "分页应读回全部行且不重不漏"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_result_pages_reports_controlled_errors_for_expired_unknown_and_foreign() {
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64 * 1024,
        "s-1",
        "t-1",
        Some(root.clone()),
    )));
    let tool = ai_result_pages_tool(std::sync::Arc::clone(&results));

    // 未知句柄：明确错误，不是空页。
    let unknown = ai_result_pages_run(
        &tool,
        &serde_json::json!({"result_ref": {"id": "result-missing"}}).to_string(),
    );
    assert_eq!(unknown.status, fluxdb_core::ToolReceiptStatus::Failed);
    let payload: serde_json::Value = serde_json::from_str(&unknown.result_json).unwrap();
    assert_eq!(payload["code"], "unknown_result");

    // 释放后读取：受控错误。
    let (result, _) = results
        .lock()
        .unwrap()
        .put(
            "db.query",
            "call-1",
            &[],
            1,
            vec![serde_json::json!({"n": 1})],
        )
        .expect("保存结果");
    assert!(results.lock().unwrap().release(&result.id));
    let released = ai_result_pages_run(
        &tool,
        &serde_json::json!({"result_ref": {"id": result.id}}).to_string(),
    );
    assert_eq!(released.status, fluxdb_core::ToolReceiptStatus::Failed);
    let payload: serde_json::Value = serde_json::from_str(&released.result_json).unwrap();
    assert_eq!(payload["code"], "result_expired");

    // 另一个会话的仓库读不到本会话句柄。
    let foreign = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64 * 1024,
        "s-2",
        "t-2",
        Some(root.clone()),
    )));
    let foreign_tool = ai_result_pages_tool(std::sync::Arc::clone(&foreign));
    let (mine, _) = results
        .lock()
        .unwrap()
        .put(
            "db.query",
            "call-2",
            &[],
            2,
            vec![serde_json::json!({"n": 3})],
        )
        .expect("保存结果");
    let foreign_read = ai_result_pages_run(
        &foreign_tool,
        &serde_json::json!({"result_ref": {"id": mine.id}}).to_string(),
    );
    assert_eq!(foreign_read.status, fluxdb_core::ToolReceiptStatus::Failed);
    let payload: serde_json::Value = serde_json::from_str(&foreign_read.result_json).unwrap();
    assert_eq!(payload["code"], "unknown_result", "跨会话不得读到数据");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_result_pages_damaged_spill_reports_error_instead_of_empty_page() {
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64,
        "s-1",
        "t-1",
        Some(root.clone()),
    )));
    let (result, spill) = results
        .lock()
        .unwrap()
        .put(
            "db.query",
            "call-damaged",
            &[],
            3,
            (0..20).map(|n| serde_json::json!({"n": n, "text": "x".repeat(16)})),
        )
        .expect("保存结果");
    assert!(spill.spilled());
    std::fs::write(root.join(&spill.relative_path.unwrap()), b"broken").expect("破坏文件");

    let tool = ai_result_pages_tool(results);
    let receipt = ai_result_pages_run(
        &tool,
        &serde_json::json!({"result_ref": {"id": result.id}, "limit": 100}).to_string(),
    );
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Failed);
    let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    assert_eq!(payload["code"], "result_damaged");
    assert!(payload["items"].is_null(), "损坏时不得返回空页伪装成功");
    let _ = std::fs::remove_dir_all(&root);
}

/// 回执与落盘事实的对账：溢写过的大结果按 `retained` 登记路径与哈希，未溢写的仍是内存句柄。
/// 这条链一旦断开，重启后溢写文件就成了没有归属的孤儿。
#[test]
fn ai_result_pages_recorded_rows_match_the_store_spill_facts() {
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let storage = fluxdb_storage::FileStorage::new(root.clone());
    storage
        .with_ai_connection(|_| Ok(()))
        .expect("初始化 schema");
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64,
        "s-1",
        "t-1",
        Some(root.join("ai-results")),
    )));

    let receipt_for = |call_id: &str, rows: Vec<serde_json::Value>| {
        let (result, _) = results
            .lock()
            .unwrap()
            .put("db.query", call_id, &[], 1, rows)
            .expect("保存结果");
        fluxdb_core::ToolReceipt {
            call_id: call_id.into(),
            tool_id: "db.query".into(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
            result_json: serde_json::json!({"ok": true, "result_ref": {"id": result.id}})
                .to_string(),
            error_code: None,
            affected_rows: None,
            started_at_ms: 1,
            finished_at_ms: 2,
        }
    };

    let spilled = receipt_for(
        "call-spilled",
        (0..20)
            .map(|n| serde_json::json!({"n": n, "text": "x".repeat(16)}))
            .collect(),
    );
    let in_memory = receipt_for("call-small", vec![serde_json::json!({"n": 1})]);
    let job = AiConversationJob {
        session_id: "s-1".into(),
        turn_id: "t-1".into(),
        assistant_message_id: "m-1".into(),
        model_profile_id: "model-1".into(),
    };
    storage
        .with_ai_connection(|conn| {
            // ai_results 对 ai_turns 有复合外键：必须先有真实会话与轮次，不能凭空插结果行。
            let agent = fluxdb_storage::ai_platform::agents::list_agents(conn)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "缺少内置 Agent")
                })?
                .id;
            fluxdb_storage::ai_platform::conversation::send(
                conn,
                1,
                true,
                "s-1",
                &agent,
                "t-1",
                "m-user",
                "查一下",
                "{}",
                "{}",
            )?;
            fluxdb_storage::ai_platform::sessions::append_message(
                conn,
                1,
                "m-1",
                "s-1",
                Some("t-1"),
                "assistant",
                "{}",
                fluxdb_storage::ai_platform::sessions::PersistenceKind::Checkpoint,
            )
        })
        .expect("准备会话与助手消息");
    for receipt in [&spilled, &in_memory] {
        ai_conversation_record_tool_result(&storage, &job, receipt, 0, Some(&results))
            .expect("登记回执");
    }
    // result.read_page 的回执会回显来源句柄（`result-1`），但翻页只读已有快照、不产生新落盘文件：
    // 它绝不能把来源的 spill 文件登记成自己的落点，否则每个翻页调用都会认领同一个文件。
    let read_page_receipt = fluxdb_core::ToolReceipt {
        call_id: "call-read-page".into(),
        tool_id: "result.read_page".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: serde_json::json!({
            "ok": true,
            "result_ref": {"id": "result-1", "tool_id": "db.query", "created_at_ms": 1},
            "items": [{"n": 0}],
        })
        .to_string(),
        error_code: None,
        affected_rows: None,
        started_at_ms: 3,
        finished_at_ms: 4,
    };
    ai_conversation_record_tool_result(&storage, &job, &read_page_receipt, 0, Some(&results))
        .expect("登记翻页回执");

    storage
        .with_ai_connection(|conn| {
            let row: (String, Option<String>, Option<String>, i64) = conn
                .query_row(
                    "SELECT retention_kind,relative_path,content_hash,byte_count FROM ai_results WHERE id=?1",
                    ["t-1-result-call-spilled"],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))?;
            assert_eq!(row.0, "retained", "溢写结果必须按 retained 登记");
            let relative_path = row.1.expect("retained 必须带相对路径");
            assert!(
                results.lock().unwrap().spill_fact(serde_json::from_str::<serde_json::Value>(&spilled.result_json).unwrap()["result_ref"]["id"].as_str().unwrap()).is_some(),
                "仓库里确有溢写事实"
            );
            let fact = results.lock().unwrap().spill_fact(serde_json::from_str::<serde_json::Value>(&spilled.result_json).unwrap()["result_ref"]["id"].as_str().unwrap()).unwrap();
            assert_eq!(relative_path, fact.relative_path);
            assert_eq!(row.2.as_deref(), Some(fact.content_hash.as_str()));
            assert!(row.3 > 0);
            // 落点必须在受控根内：把入库的相对路径拼回根目录就是真实 spill 文件。
            assert!(root.join("ai-results").join(&relative_path).is_file());

            let paged: (String, Option<String>) = conn
                .query_row(
                    "SELECT retention_kind,relative_path FROM ai_results WHERE id=?1",
                    ["t-1-result-call-read-page"],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))?;
            assert_eq!(paged.0, "memory", "翻页不产生落盘结果，必须按内存登记");
            assert_eq!(paged.1, None, "翻页不得认领来源的 spill 文件");

            let memory: (String, Option<String>) = conn
                .query_row(
                    "SELECT retention_kind,relative_path FROM ai_results WHERE id=?1",
                    ["t-1-result-call-small"],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))?;
            assert_eq!(memory.0, "memory");
            assert_eq!(memory.1, None, "内存结果没有落点");
            Ok(())
        })
        .expect("读取登记事实");
    let mut next_job = job.clone();
    next_job.turn_id = "t-2".into();
    let history = storage.with_ai_connection(|conn| ai_historical_tool_facts(conn, &next_job)).unwrap();
    let facts = history.get("m-1").unwrap();
    assert!(!facts.is_empty());
    assert!(facts.iter().all(|fact| fact["readable"] == false && fact["unavailable_reason"] == "turn_scope"));
    assert!(facts.iter().any(|fact| fact["tool"] == "db.query" && fact["status"] == "succeeded"));
    assert!(!serde_json::json!(facts).to_string().contains("\"items\""), "历史不能回放数据值");
    let handle = serde_json::from_str::<serde_json::Value>(&spilled.result_json).unwrap()["result_ref"]["id"].as_str().unwrap().to_owned();
    let retained_path = root.join("ai-results").join(results.lock().unwrap().spill_fact(&handle).unwrap().relative_path);
    storage.with_ai_connection(|conn| {
        conn.execute("UPDATE ai_results SET expires_at_ms=0 WHERE id='t-1-result-call-spilled'", [])
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        Ok(())
    }).unwrap();
    purge_expired_result_files(&storage).unwrap();
    assert!(!retained_path.exists(), "过期文件必须清理，不延长模型引用寿命");
    let outside_name = format!("{}.json", uuid::Uuid::new_v4());
    let outside = root.join(&outside_name);
    std::fs::write(&outside, "keep").unwrap();
    storage.with_ai_connection(|conn| {
        conn.execute("UPDATE ai_results SET relative_path=?1,metadata_json=json_set(metadata_json,'$.file_purged',0) WHERE id='t-1-result-call-spilled'",
            [format!("../{outside_name}")]).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        Ok(())
    }).unwrap();
    purge_expired_result_files(&storage).unwrap();
    assert!(outside.exists(), "清理不得越过受控根");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_result_pages_read_shape_does_not_return_a_cursor_for_an_omitted_page() {
    let spec = result_read_page_spec().unwrap();
    assert_eq!(spec.output_shape, ToolOutputShape::Read);
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "c1".into(), tool_id: "result.read_page".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: serde_json::json!({"items":["x".repeat(4096)],"next_cursor":"cursor-2"}).to_string(),
        error_code: None, affected_rows: None, started_at_ms: 0, finished_at_ms: 1,
    };
    let projection = project_receipt_with_shape(&receipt, None, 512, &spec.output_shape);
    let value: serde_json::Value = serde_json::from_str(&projection.text).unwrap();
    assert!(projection.truncated);
    assert!(value.get("next_cursor").is_none(), "省略整页时不能跳到下一页");
    assert_eq!(value["delivery_truncated"], true);
}

#[test]
fn ai_result_pages_facts_name_positional_cells_without_changing_page_shape() {
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        64 * 1024,
        "s-1",
        "t-1",
        None,
    )));
    let columns: Vec<String> = [
        "label",
        "amount",
        "text_number",
        "mixed",
        "all_null",
        "nullable",
        "flag",
        "nested",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let rows = vec![
        serde_json::json!(["first", 12, "12", 1, null, null, true, {"n": 1}]),
        serde_json::json!(["second", -2.5, "2.5", "2", null, 0, false, [2]]),
        serde_json::json!(["third", 0, null, null, null, null, null, null]),
    ];
    let (result, spill) = results
        .lock()
        .unwrap()
        .put(
            "db.query",
            "source-call-private",
            &columns,
            42,
            rows.clone(),
        )
        .expect("保存位置行");
    assert!(!spill.spilled());
    let facts = results.lock().unwrap().facts(&result.id).expect("读取事实");
    assert_eq!(facts.result, result);
    assert_eq!(facts.result.call_id, "source-call-private");
    assert_eq!(facts.rows, rows.len());
    assert!(!facts.truncated);
    assert_eq!(
        facts.sample,
        vec![
            serde_json::json!({
                "label": "first", "amount": 12, "text_number": "12", "mixed": 1,
                "all_null": null, "nullable": null, "flag": true, "nested": {"n": 1},
            }),
            serde_json::json!({
                "label": "second", "amount": -2.5, "text_number": "2.5", "mixed": "2",
                "all_null": null, "nullable": 0, "flag": false, "nested": [2],
            }),
            serde_json::json!({
                "label": "third", "amount": 0, "text_number": null, "mixed": null,
                "all_null": null, "nullable": null, "flag": null, "nested": null,
            }),
        ]
    );
    assert_eq!(
        facts
            .fields
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        columns.iter().collect::<std::collections::BTreeSet<_>>(),
    );
    assert_eq!(facts.fields.len(), columns.len(), "字段名不得重复");
    assert_eq!(facts.numeric_fields, vec!["amount", "nullable"]);

    // facts 按名成形不能改写底层快照；模型翻页仍收到位置行，宿主调用 ID 不外泄。
    let tool = ai_result_pages_tool(std::sync::Arc::clone(&results));
    let mut cursor = None;
    let mut seen = Vec::new();
    for offset in 0..rows.len() {
        let receipt = ai_result_pages_run(
            &tool,
            &serde_json::json!({
                "result_ref": {"id": result.id},
                "cursor": cursor,
                "limit": 1,
            })
            .to_string(),
        );
        assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
        assert_eq!(receipt.call_id, "call-read-page");
        let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        assert_eq!(
            payload["result_ref"],
            serde_json::json!({
                "id": result.id, "tool_id": "db.query", "created_at_ms": 42, "scope":"turn", "expires_on":"execution_end","content_type":"items",
            }),
        );
        assert!(!receipt.result_json.contains("source-call-private"));
        assert!(payload.get("call_id").is_none());
        assert_eq!(payload["items"], serde_json::json!([rows[offset]]));
        assert_eq!(payload["returned_items"], 1);
        assert_eq!(payload["total_items"], rows.len());
        let has_more = offset + 1 < rows.len();
        assert_eq!(payload["truncated"], has_more);
        cursor = payload["next_cursor"].as_str().map(str::to_string);
        assert_eq!(
            cursor,
            has_more.then(|| format!("{}:{}", result.id, offset + 1)),
        );
        seen.extend(payload["items"].as_array().unwrap().iter().cloned());
    }
    assert_eq!(seen, rows);
    assert_eq!(results.lock().unwrap().facts(&result.id).unwrap(), facts);
}

#[test]
fn ai_result_pages_facts_count_spill_but_only_sample_memory_without_reading_files() {
    let root = std::env::temp_dir().join(format!("fluxdb-t039-{}", uuid::Uuid::new_v4()));
    let rows: Vec<serde_json::Value> = (0..6).map(|n| serde_json::json!([n, "fixed"])).collect();
    let budget = 2 * serde_json::to_vec(&rows[0]).unwrap().len();
    let mut store = ResultStore::for_turn(budget, "s-1", "t-1", Some(root.clone()));
    let (result, spill) = store
        .put(
            "db.query",
            "call-facts",
            &["n".into(), "label".into()],
            7,
            rows.clone(),
        )
        .expect("保存结果");
    assert!(spill.spilled());
    let facts = store.facts(&result.id).expect("读取事实");
    assert_eq!(facts.result, result);
    assert_eq!(facts.rows, 6, "总数必须包含 spill，不能冒充样本行数");
    assert!(facts.truncated);
    assert_eq!(
        facts.sample,
        vec![
            serde_json::json!({"n": 0, "label": "fixed"}),
            serde_json::json!({"n": 1, "label": "fixed"}),
        ],
    );
    assert_eq!(facts.fields, facts.sample[0].as_object().unwrap().keys().cloned().collect::<Vec<_>>());
    assert_eq!(facts.numeric_fields, vec!["n"]);
    let page = store.read_page(&result.id, None, 10, 4096).unwrap();
    assert_eq!(page.items, rows);
    assert!(!page.truncated);
    assert_eq!(page.next_cursor, None);

    std::fs::remove_file(root.join(spill.relative_path.unwrap())).expect("移除隔离 spill 文件");
    assert_eq!(
        store.facts(&result.id).unwrap(),
        facts,
        "facts 不应触盘或续读"
    );
    assert_eq!(
        store
            .read_page(&result.id, None, 10, 4096)
            .unwrap_err()
            .code,
        "result_damaged",
    );
    std::fs::remove_dir_all(root).expect("清理测试目录");
}

#[test]
fn ai_result_pages_facts_reject_released_expired_unknown_and_foreign_handles() {
    let mut store = ResultStore::for_turn(4096, "s-1", "t-1", None);
    let (result, _) = store
        .put(
            "db.query",
            "call-facts",
            &[],
            1,
            [serde_json::json!({"n": 1})],
        )
        .unwrap();
    assert!(store.facts(&result.id).is_ok());
    assert_eq!(
        store.facts("result-missing").unwrap_err().code,
        "unknown_result"
    );
    for (session, turn) in [("s-2", "t-1"), ("s-1", "t-2")] {
        let foreign = ResultStore::for_turn(4096, session, turn, None);
        assert_eq!(
            foreign.facts(&result.id).unwrap_err().code,
            "unknown_result"
        );
    }
    assert!(store.release(&result.id));
    assert_eq!(store.facts(&result.id).unwrap_err().code, "result_expired");
    assert!(store.expire(&result.id));
    assert_eq!(store.facts(&result.id).unwrap_err().code, "result_expired");

    let (next, _) = store
        .put(
            "db.query",
            "call-next",
            &[],
            2,
            [serde_json::json!({"n": 2})],
        )
        .unwrap();
    store.expire_all();
    assert_eq!(store.facts(&next.id).unwrap_err().code, "result_expired");
}

#[test]
fn ai_result_pages_valid_json_spill_tampering_is_damaged_even_with_same_row_count() {
    let root = std::env::temp_dir().join(format!("fluxdb-t039-{}", uuid::Uuid::new_v4()));
    let results = std::sync::Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        16,
        "s-1",
        "t-1",
        Some(root.clone()),
    )));
    let (result, spill) = results
        .lock()
        .unwrap()
        .put(
            "db.query",
            "call-tampered",
            &["n".into(), "label".into()],
            3,
            (0..6).map(|n| serde_json::json!([n, "fixed"])),
        )
        .unwrap();
    assert!(spill.spilled());
    let path = root.join(spill.relative_path.unwrap());
    let original = std::fs::read(&path).unwrap();
    let original_rows: Vec<serde_json::Value> = serde_json::from_slice(&original).unwrap();
    let tool = ai_result_pages_tool(results);
    let arguments = serde_json::json!({"result_ref": {"id": result.id}, "limit": 100}).to_string();
    assert_eq!(
        ai_result_pages_run(&tool, &arguments).status,
        fluxdb_core::ToolReceiptStatus::Succeeded,
    );
    let mut changed_rows = original_rows.clone();
    changed_rows[0][1] = serde_json::json!("other");
    let changed = serde_json::to_vec(&changed_rows).unwrap();
    assert_eq!(changed.len(), original.len(), "同字节数篡改必须靠指纹检出");
    let mut padded = original.clone();
    padded.push(b' ');
    for modified in [changed, padded] {
        let decoded: Vec<serde_json::Value> = serde_json::from_slice(&modified).unwrap();
        assert_eq!(
            decoded.len(),
            original_rows.len(),
            "夹具必须保留合法 JSON 与行数"
        );
        std::fs::write(&path, modified).unwrap();
        let receipt = ai_result_pages_run(&tool, &arguments);
        assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Failed);
        assert_eq!(receipt.error_code.as_deref(), Some("result_damaged"));
        let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["code"], "result_damaged");
        assert_eq!(payload["retryable"], false);
        assert!(payload.get("items").is_none());
        assert!(payload.get("next_cursor").is_none());
    }
    std::fs::write(path, original).unwrap();
    assert_eq!(
        ai_result_pages_run(&tool, &arguments).status,
        fluxdb_core::ToolReceiptStatus::Succeeded,
    );
    std::fs::remove_dir_all(root).expect("清理测试目录");
}

// T050：受控存储清理——失败产物快照、孤儿文件、用量统计。
#[test]
fn storage_cleanup_purges_failed_artifacts_and_orphans_but_keeps_ready() {
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-ai-storage-cleanup-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    storage.with_ai_connection(|_| Ok(())).unwrap();

    // 建立会话 + Turn（供产物 FK）。
    let agent = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::agents::list_agents(conn)?
                .into_iter()
                .next()
                .map(|agent| agent.id))
        })
        .unwrap()
        .expect("内置 Agent");
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::conversation::send(
                conn, 1_000, true, "session-1", &agent, "turn-1", "msg-1", "会话", "{}", "{}",
            )
        })
        .unwrap();

    let artifacts_dir = storage.root_dir().join("ai-artifacts");
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    let results_dir = storage.root_dir().join("ai-results").join("results");
    std::fs::create_dir_all(&results_dir).unwrap();

    // ready 产物（必须保留）+ failed 产物（带落点，应清理）。
    let ready_file = "aaaaaaaa-0000-0000-0000-000000000001.json";
    let failed_file = "bbbbbbbb-0000-0000-0000-000000000002.json";
    std::fs::write(artifacts_dir.join(ready_file), b"{\"markdown\":\"x\"}").unwrap();
    std::fs::write(artifacts_dir.join(failed_file), b"{\"markdown\":\"y\"}").unwrap();
    // 孤儿文件（DB 未登记）应清理；非 uuid 命名的普通文件不应被误删。
    let orphan_file = "cccccccc-0000-0000-0000-000000000003.json";
    std::fs::write(artifacts_dir.join(orphan_file), b"{}").unwrap();
    std::fs::write(artifacts_dir.join("notes.txt"), b"keep").unwrap();
    // 结果孤儿。
    let result_orphan = "dddddddd-0000-0000-0000-000000000004.json";
    std::fs::write(results_dir.join(result_orphan), b"{}").unwrap();
    storage
        .with_ai_connection(|conn| {
            conn.execute(
                "INSERT INTO ai_artifacts(id,session_id,turn_id,kind,schema_version,title,renderer_id,renderer_version,spec_json,status,relative_path,byte_count,content_hash,revision,created_at_ms,updated_at_ms)
                 VALUES('a-ready','session-1','turn-1','report',1,'t','r','1','{}','ready',?1,10,'h',1,1000,1000),
                       ('a-failed','session-1','turn-1','report',1,'t','r','1','{}','failed',?2,10,'h',1,1000,1000)",
                rusqlite::params![ready_file, failed_file],
            )
            .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(())
        })
        .unwrap();

    let usage = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::artifacts::storage_usage(conn)?)
        })
        .unwrap();
    assert_eq!(usage.artifact_files, 2, "统计计数登记落点（含 failed）");
    assert_eq!(usage.artifact_bytes, 20);
    assert_eq!(usage.result_files, 0);

    let purged = purge_failed_artifact_files(&storage).unwrap();
    assert_eq!(purged, 1, "只清失败产物快照");
    assert!(artifacts_dir.join(ready_file).exists(), "ready 产物必须保留");
    assert!(!artifacts_dir.join(failed_file).exists(), "failed 产物文件应删除");

    let orphans = purge_orphan_controlled_files(&storage).unwrap();
    assert_eq!(orphans, 2, "清理产物与结果两个孤儿");
    assert!(!artifacts_dir.join(orphan_file).exists());
    assert!(artifacts_dir.join("notes.txt").exists(), "非 uuid 文件不得误删");
    assert!(!results_dir.join(result_orphan).exists());

    // 失败产物行仍在（保留追溯），但落点已清空、不再计入用量。
    storage
        .with_ai_connection(|conn| {
            let (status, path): (String, Option<String>) = conn
                .query_row(
                    "SELECT status,relative_path FROM ai_artifacts WHERE id='a-failed'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "failed");
            assert!(path.is_none(), "清理后应清空落点");
            Ok(())
        })
        .unwrap();
    let usage = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::artifacts::storage_usage(conn)?)
        })
        .unwrap();
    assert_eq!(usage.artifact_files, 1, "只剩 ready 产物计费落点");

    let _ = std::fs::remove_dir_all(&dir);
}

// T050：staged 产物崩溃对账。
#[test]
fn staged_artifact_reconcile_promotes_verified_and_fails_missing() {
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-ai-staged-reconcile-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    storage.with_ai_connection(|_| Ok(())).unwrap();
    let agent = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::agents::list_agents(conn)?
                .into_iter()
                .next()
                .map(|a| a.id))
        })
        .unwrap()
        .expect("内置 Agent");
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::conversation::send(
                conn, 1_000, true, "session-1", &agent, "turn-1", "msg-1", "会话", "{}", "{}",
            )
        })
        .unwrap();

    let artifacts_dir = storage.root_dir().join("ai-artifacts");
    std::fs::create_dir_all(&artifacts_dir).unwrap();

    // 1) staged + 文件字节/指纹吻合 → 应提升 ready。
    let verified_name = "11111111-0000-0000-0000-000000000001.json";
    let verified_content = "{\"markdown\":\"verified\"}";
    std::fs::write(artifacts_dir.join(verified_name), verified_content).unwrap();
    let verified_hash = crate::source_hash(&[verified_content]);
    // 2) staged + 文件缺失 → 应置 failed 并清空落点。
    let missing_name = "22222222-0000-0000-0000-000000000002.json";
    storage
        .with_ai_connection(|conn| {
            conn.execute(
                "INSERT INTO ai_artifacts(id,session_id,turn_id,kind,schema_version,title,renderer_id,renderer_version,spec_json,status,relative_path,byte_count,content_hash,revision,created_at_ms,updated_at_ms)
                 VALUES('a-verified','session-1','turn-1','report',1,'t','r','1','{}','staged',?1,?2,?3,1,1000,1000),
                       ('a-missing','session-1','turn-1','report',1,'t','r','1','{}','staged',?4,10,'deadbeef',1,1000,1000)",
                rusqlite::params![
                    verified_name,
                    verified_content.len() as i64,
                    verified_hash,
                    missing_name,
                ],
            )
            .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(())
        })
        .unwrap();

    let candidates = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::recovery::staged_artifact_candidates(conn)?)
        })
        .unwrap();
    assert_eq!(candidates.len(), 2, "两个 staged 产物都应被收集");
    reconcile_staged_artifacts(&storage, &candidates);

    storage
        .with_ai_connection(|conn| {
            let (status, path): (String, Option<String>) = conn
                .query_row(
                    "SELECT status,relative_path FROM ai_artifacts WHERE id='a-verified'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "ready", "核对通过必须提升 ready（不丢已完成文件）");
            assert_eq!(path.as_deref(), Some(verified_name));

            let (status, path): (String, Option<String>) = conn
                .query_row(
                    "SELECT status,relative_path FROM ai_artifacts WHERE id='a-missing'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "failed", "文件缺失必须收敛为 failed");
            assert!(path.is_none(), "失败后应清空悬挂落点");
            Ok(())
        })
        .unwrap();
    assert!(artifacts_dir.join(verified_name).exists(), "ready 文件保留");
    let _ = std::fs::remove_dir_all(&dir);
}
