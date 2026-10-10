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

fn ai_result_pages_run(
    tool: &AiResultReadPageTool,
    arguments: &str,
) -> fluxdb_core::ToolReceipt {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(tool.execute(
            ai_result_pages_ctx("call-read-page"),
            arguments.to_string(),
        ))
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
        .register(Box::new(ai_result_pages_tool(std::sync::Arc::clone(&results))))
        .expect("注册 result.read_page");
    assert!(catalog.describe("result.read_page").is_some());
    assert!(catalog.executable("result.read_page").is_some());
    assert!(catalog
        .prepare("result.read_page", r#"{"limit":5}"#)
        .is_err());
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
        .put("db.query", 7, rows)
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
    assert_eq!(seen, (0..25).collect::<Vec<i64>>(), "分页应读回全部行且不重不漏");
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
        .put("db.query", 1, vec![serde_json::json!({"n": 1})])
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
        .put("db.query", 2, vec![serde_json::json!({"n": 3})])
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
    storage.with_ai_connection(|_| Ok(())).expect("初始化 schema");
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
            .put("db.query", 1, rows)
            .expect("保存结果");
        fluxdb_core::ToolReceipt {
            call_id: call_id.into(),
            tool_id: "db.query".into(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
            result_json: serde_json::json!({"ok": true, "result_ref": {"id": result.id}}).to_string(),
            error_code: None,
            affected_rows: None,
            started_at_ms: 1,
            finished_at_ms: 2,
        }
    };

    let spilled = receipt_for(
        "call-spilled",
        (0..20).map(|n| serde_json::json!({"n": n, "text": "x".repeat(16)})).collect(),
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
                conn, 1, true, "s-1", &agent, "t-1", "m-user", "查一下", "{}", "{}",
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
                results.lock().unwrap().spill_fact("result-1").is_some(),
                "仓库里确有溢写事实"
            );
            let fact = results.lock().unwrap().spill_fact("result-1").unwrap();
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
    let _ = std::fs::remove_dir_all(&root);
}
