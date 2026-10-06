// AI SQL 执行服务测试（设计 §20–21，T023/T024）。
//
// 用 demo（内存 MockConnector）连接验证执行链：单条语句、只读保证、参数拒绝、
// 取消语义、结果有界与受控错误；不触网、不读用户凭据。

fn ai_sql_exec_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(future)
}

fn ai_sql_exec_demo_config(id: u64) -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(id),
        name: format!("demo-{id}"),
        kind: DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: true,
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

fn ai_sql_exec_ctx(call_id: &str, cancel: CancelFlag) -> ToolExecutionContext {
    ToolExecutionContext {
        session_id: "s-sql".into(),
        turn_id: "t-sql".into(),
        iteration: 1,
        call_id: call_id.to_string(),
        cancel,
    }
}

#[test]
fn ai_sql_exec_parses_target_handle_and_rejects_garbage() {
    let target = AiSqlTarget::parse("conn:7/db:sales/schema:public").expect("合法句柄");
    assert_eq!(target.connection_id, ConnectionId(7));
    assert_eq!(target.database.as_deref(), Some("sales"));
    assert_eq!(target.schema.as_deref(), Some("public"));

    let short = AiSqlTarget::parse("connection:3").expect("connection: 前缀");
    assert_eq!(short.connection_id, ConnectionId(3));
    assert!(short.database.is_none() && short.schema.is_none());

    // 连接字符串不是合法句柄：不允许模型自己提交 endpoint。
    assert_eq!(
        AiSqlTarget::parse("mysql://root:secret@127.0.0.1:3306/sales")
            .unwrap_err()
            .code,
        "invalid_target"
    );
    assert_eq!(
        AiSqlTarget::parse("conn:abc").unwrap_err().code,
        "invalid_target"
    );
    assert_eq!(
        AiSqlTarget::parse("conn:1/other:x").unwrap_err().code,
        "invalid_target"
    );
}

#[test]
fn ai_sql_exec_read_only_entry_rejects_writes_and_multi_statement() {
    let resolver = MapSqlResolver::new([ai_sql_exec_demo_config(1)]);
    let target = AiSqlTarget::parse("conn:1").expect("target");

    let write = execute_sql(
        &resolver,
        &target,
        "INSERT INTO product(id, name) VALUES(1, 'x')",
        10,
        true,
        &|| false,
    )
    .expect_err("只读入口必须拒绝写语句");
    assert_eq!(write.code, "not_read_only");

    let multi = execute_sql(&resolver, &target, "SELECT 1; SELECT 2", 10, true, &|| {
        false
    })
    .expect_err("多语句必须被拒绝");
    assert_eq!(multi.code, "rejected_sql");

    let unknown = execute_sql(&resolver, &target, "SELEC * FROM", 10, true, &|| false)
        .expect_err("未知语法不能当只读自动执行");
    assert_eq!(unknown.code, "not_read_only");

    let ok = execute_sql(
        &resolver,
        &target,
        "SELECT * FROM product",
        10,
        true,
        &|| false,
    )
    .expect("只读查询");
    assert!(!ok.columns.is_empty(), "结果必须有列名");
    assert_eq!(ok.rows.len(), 2, "demo 返回固定 2 行");
    assert!(!ok.truncated);
}

#[test]
fn ai_sql_exec_write_path_runs_only_when_caller_drops_read_only_guard() {
    let resolver = MapSqlResolver::new([ai_sql_exec_demo_config(1)]);
    let target = AiSqlTarget::parse("conn:1").expect("target");
    let sql = "UPDATE product SET name = 'x' WHERE id = 1";

    // 只读入口拒绝；写入口（审批层已经放行）才真正执行。
    assert!(
        execute_sql(&resolver, &target, sql, 10, true, &|| false).is_err(),
        "只读入口不得执行写语句"
    );
    let outcome = execute_sql(&resolver, &target, sql, 10, false, &|| false)
        .expect("写入口在调用方已授权时应执行");
    assert!(outcome.elapsed_ms < 60_000);
}

#[test]
fn ai_sql_exec_unknown_connection_is_a_controlled_error() {
    let resolver = MapSqlResolver::new([ai_sql_exec_demo_config(1)]);
    let target = AiSqlTarget::parse("conn:999").expect("target");
    let error = execute_sql(&resolver, &target, "SELECT 1", 10, true, &|| false)
        .expect_err("未知连接必须受控失败");
    assert_eq!(error.code, "connection_not_found");
}

#[test]
fn ai_sql_exec_query_tool_rejects_parameters_until_connector_bind_exists() {
    let resolver = Arc::new(MapSqlResolver::new([ai_sql_exec_demo_config(1)]));
    let tool = AiSqlReadTool::query(resolver).expect("db.query 工具");
    assert_eq!(tool.descriptor().id, "db.query");

    let receipt = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-param", CancelFlag::new()),
        r#"{"target":"conn:1","sql":"SELECT * FROM product WHERE id = ?","parameters":[{"type":"i64","value":"1"}]}"#
            .into(),
    ))
    .expect("工具调用");

    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("parameters_unsupported")
    );
    assert_eq!(receipt.tool_id, "db.query");
}

#[test]
fn ai_sql_exec_query_tool_returns_bounded_rows_for_read_only_select() {
    let resolver = Arc::new(MapSqlResolver::new([ai_sql_exec_demo_config(1)]));
    let tool = AiSqlReadTool::query(resolver).expect("db.query 工具");

    let receipt = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-ok", CancelFlag::new()),
        r#"{"target":"conn:1/db:main","sql":"SELECT * FROM product","max_rows":1}"#.into(),
    ))
    .expect("工具调用");

    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    let payload: serde_json::Value =
        serde_json::from_str(&receipt.result_json).expect("回执载荷是 JSON");
    assert!(payload["columns"].as_array().is_some_and(|c| !c.is_empty()));
    assert!(payload["row_count"].as_u64().unwrap_or(0) <= 1);
}

#[test]
fn ai_sql_exec_cancel_flag_yields_cancelled_receipt() {
    let resolver = Arc::new(MapSqlResolver::new([ai_sql_exec_demo_config(1)]));
    let tool = AiSqlReadTool::query(resolver).expect("db.query 工具");
    let cancel = CancelFlag::new();
    cancel.request();

    let receipt = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-cancel", cancel),
        r#"{"target":"conn:1","sql":"SELECT * FROM product"}"#.into(),
    ))
    .expect("工具调用");

    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Cancelled);
    assert_eq!(receipt.error_code.as_deref(), Some("cancelled"));
}

#[test]
fn ai_sql_exec_search_tool_returns_bounded_object_list() {
    let resolver = Arc::new(MapSqlResolver::new([ai_sql_exec_demo_config(1)]));
    let tool = AiSqlSearchTool::new(resolver).expect("db.search_objects 工具");
    assert_eq!(tool.descriptor().id, "db.search_objects");

    let receipt = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-search", CancelFlag::new()),
        r#"{"target":"conn:1","query":"","limit":2}"#.into(),
    ))
    .expect("工具调用");

    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    let payload: serde_json::Value =
        serde_json::from_str(&receipt.result_json).expect("回执载荷是 JSON");
    // limit=2 必须真正生效（有界），不是全量返回。
    assert_eq!(payload["count"].as_u64(), Some(2));
}

#[test]
fn ai_sql_exec_projection_is_bounded_by_rows() {
    let page = fluxdb_core::DataPage {
        columns: vec![fluxdb_core::Column {
            name: "id".into(),
            type_name: Some("INTEGER".into()),
            nullable: false,
            primary_key: true,
            comment: None,
        }],
        rows: (0..500)
            .map(|i| fluxdb_core::Row {
                values: vec![fluxdb_core::CellValue::I64(i)],
            })
            .collect(),
        offset: 0,
        limit: 500,
        has_more: true,
    };
    let execution = fluxdb_core::QueryExecutionResult {
        summaries: Vec::new(),
        results: vec![page],
        rollback_snapshots: Vec::new(),
    };

    let outcome = project_execution(&execution, 10, 3);
    assert_eq!(outcome.row_count, 10, "行数必须按 max_rows 截断");
    assert_eq!(outcome.rows.len(), 10);
    assert!(outcome.truncated, "被截断必须标记");
    assert_eq!(outcome.columns, vec!["id".to_string()]);
}

#[test]
fn ai_sql_exec_projection_is_bounded_by_bytes() {
    let big = "x".repeat(20_000);
    let page = fluxdb_core::DataPage {
        columns: vec![fluxdb_core::Column {
            name: "blob".into(),
            type_name: Some("TEXT".into()),
            nullable: true,
            primary_key: false,
            comment: None,
        }],
        rows: (0..50)
            .map(|_| fluxdb_core::Row {
                values: vec![fluxdb_core::CellValue::Text(big.clone())],
            })
            .collect(),
        offset: 0,
        limit: 50,
        has_more: false,
    };
    let execution = fluxdb_core::QueryExecutionResult {
        summaries: Vec::new(),
        results: vec![page],
        rollback_snapshots: Vec::new(),
    };

    let outcome = project_execution(&execution, 1000, 1);
    assert!(
        outcome.rows.len() < 50,
        "字节上限必须先于行数上限生效，实际保留 {} 行",
        outcome.rows.len()
    );
    assert!(outcome.truncated);
    let serialized = serde_json::to_string(&outcome.rows).expect("序列化");
    assert!(
        serialized.len() <= SQL_RESULT_MAX_BYTES,
        "载荷不得超过字节上限: {}",
        serialized.len()
    );
}

#[test]
fn ai_sql_exec_explain_tool_builds_plan_and_refuses_execute_mode() {
    let resolver = Arc::new(MapSqlResolver::new([ai_sql_exec_demo_config(1)]));
    let tool = AiSqlReadTool::explain(resolver).expect("db.explain 工具");
    assert_eq!(tool.descriptor().id, "db.explain");

    // 纯 SELECT 会被包成 EXPLAIN（plan），可以只读执行。
    let plan = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-plan", CancelFlag::new()),
        r#"{"target":"conn:1","sql":"SELECT * FROM product"}"#.into(),
    ))
    .expect("工具调用");
    assert_eq!(plan.status, fluxdb_core::ToolReceiptStatus::Succeeded);

    // 包成 EXPLAIN 后仍然是"被解释语句是写"的情况必须拒绝（证明真的做了包装而不是原样执行）。
    let write = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-plan-write", CancelFlag::new()),
        r#"{"target":"conn:1","sql":"INSERT INTO product(id) VALUES(1)"}"#.into(),
    ))
    .expect("工具调用");
    assert_eq!(write.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert_eq!(write.error_code.as_deref(), Some("not_read_only"));

    // 执行式计划默认不开放。
    let execute = ai_sql_exec_block_on(tool.execute(
        ai_sql_exec_ctx("call-plan-exec", CancelFlag::new()),
        r#"{"target":"conn:1","sql":"SELECT 1","mode":"execute"}"#.into(),
    ))
    .expect("工具调用");
    assert_eq!(execute.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert_eq!(
        execute.error_code.as_deref(),
        Some("explain_execute_unsupported")
    );
}
