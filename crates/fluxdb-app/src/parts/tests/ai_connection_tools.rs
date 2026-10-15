#[test]
fn ai_connections_list_exposes_only_public_connection_properties() {
    use fluxdb_core::AiTool as _;
    let mut config = ai_sql_exec_demo_config(7);
    config
        .options
        .insert("username".into(), "private-user".into());
    config
        .options
        .insert("password".into(), "private-secret".into());
    config
        .options
        .insert("credential_ref".into(), "private-ref".into());
    let tool = AiConnectionsListTool::new(&[config], Some(ConnectionId(7)));
    let receipt = ai_sql_exec_block_on(
        tool.execute(ai_sql_exec_ctx("list-1", CancelFlag::new()), "{}".into()),
    )
    .unwrap();
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    assert_eq!(receipt.call_id, "list-1");
    assert_eq!(receipt.tool_id, "connections.list");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&receipt.result_json).unwrap(),
        serde_json::json!({
            "connections": [{
                "connection_id": 7,
                "connection_name": "demo-7",
                "database_kind": "Sqlite",
                "connection_read_only": true,
                "is_current": true,
            }],
            "snapshot_scope": "turn",
            "execution_scope": "current_connection_only",
        })
    );
}

#[test]
fn ai_connections_list_accepts_only_an_empty_object() {
    let mut catalog = ToolCatalog::new(Vec::new());
    catalog
        .add_tool(Box::new(AiConnectionsListTool::new(&[], None)))
        .unwrap();
    assert!(catalog.prepare("connections.list", "{}").is_ok());
    for arguments in ["[]", "null", r#"{"target":"conn:1"}"#] {
        assert_eq!(
            catalog
                .prepare("connections.list", arguments)
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    catalog.without_tools(&["connections.list".into()].into_iter().collect());
    assert!(catalog.describe("connections.list").is_none());
    assert!(catalog.executable("connections.list").is_none());
}

#[test]
fn ai_connections_list_empty_and_cancelled_results_do_not_disclose_connections() {
    use fluxdb_core::AiTool as _;
    let tool = AiConnectionsListTool::new(&[], None);
    let receipt = ai_sql_exec_block_on(
        tool.execute(ai_sql_exec_ctx("empty", CancelFlag::new()), "{}".into()),
    )
    .unwrap();
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    let result: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
    assert_eq!(result["connections"], serde_json::json!([]));

    let tool = AiConnectionsListTool::new(&[ai_sql_exec_demo_config(1)], None);
    let cancel = CancelFlag::new();
    cancel.request();
    let receipt =
        ai_sql_exec_block_on(tool.execute(ai_sql_exec_ctx("cancelled", cancel), "{}".into()))
            .unwrap();
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Cancelled);
    assert_eq!(receipt.error_code.as_deref(), Some("cancelled"));
    assert!(!receipt.result_json.contains("demo-1"));
}

#[test]
fn ai_connections_list_large_output_uses_existing_snapshot_paging() {
    let configs = (0..200)
        .map(|id| {
            let mut config = ai_sql_exec_demo_config(id);
            config.name = "connection".repeat(100);
            config
        })
        .collect::<Vec<_>>();
    let mut catalog = ToolCatalog::new(Vec::new())
        .with_results(Arc::new(Mutex::new(ResultStore::new(1024 * 1024))));
    catalog
        .add_tool(Box::new(AiConnectionsListTool::new(&configs, None)))
        .unwrap();
    let receipt = ai_sql_exec_block_on(
        catalog
            .executable("connections.list")
            .unwrap()
            .execute(ai_sql_exec_ctx("large", CancelFlag::new()), "{}".into()),
    )
    .unwrap();
    let original = receipt.result_json.clone();
    let projection = catalog.project("connections.list", &receipt, None).unwrap();
    assert!(projection.truncated);
    assert_eq!(
        projection.result_ref.unwrap().format,
        ResultFormat::OutputFragments
    );
    assert!(projection.text.len() <= TOOL_AUTO_OUTPUT_BYTES);
    assert_eq!(receipt.result_json, original);
}
