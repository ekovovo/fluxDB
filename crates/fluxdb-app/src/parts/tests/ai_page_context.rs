#[test]
fn current_page_large_sql_preview_is_bounded_and_full_snapshot_is_searchable() {
    let text = format!(
        "{}\nUNIQUE-MIDDLE-KEYWORD\n{}",
        "a".repeat(1024 * 1024),
        "z".repeat(1024 * 1024)
    );
    let snapshot = fluxdb_editor_core::EditorBuffer::new_from(&text).snapshot();
    let preview = capture_editor_snapshot(
        9,
        TargetRef::default(),
        &snapshot,
        fluxdb_editor_core::Range::new(0, 0),
    );
    let excerpt = preview.excerpt.as_ref().unwrap();
    assert!(excerpt.truncated);
    assert!(excerpt.text.len() < 17 * 1024);
    assert!(!excerpt.text.contains("UNIQUE-MIDDLE-KEYWORD"));
    let tools = editor_read_range_tools(&text, 9);
    let receipt = run_editor_tool(
        &tools,
        "editor.search",
        r#"{"query":"UNIQUE-MIDDLE-KEYWORD"}"#,
    );
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    assert!(receipt.result_json.contains("UNIQUE-MIDDLE-KEYWORD"));
}

#[test]
fn current_page_reference_has_type_scope_and_only_real_read_capabilities() {
    let text = fluxdb_editor_core::EditorBuffer::new_from("select 1;").snapshot();
    let snapshot = capture_editor_snapshot(
        2,
        TargetRef::default(),
        &text,
        fluxdb_editor_core::Range::new(0, 0),
    );
    let page = CurrentPageContext {
        kind: PageContextKind::SqlEditor,
        label: "query.sql".into(),
        snapshot,
        details: serde_json::json!({"total_bytes":text.len()}),
        editor_grant: Some(EditorGrant {
            tab_ref: "tab-2".into(),
            snapshot: text,
        }),
        data_grant: None,
    };
    let value = page.public_json();
    assert_eq!(value["kind"], "current_page");
    assert_eq!(value["delivery"], "full_text");
    assert_eq!(value["content"], "select 1;");
    assert_eq!(
        value["capabilities"],
        serde_json::json!(["editor.search", "editor.read_range"])
    );
    assert!(!value.to_string().contains("page.read_rows"));
}

#[test]
fn current_page_rows_require_authorization_and_keep_snapshot_immutable() {
    let mut page = DataPage {
        columns: vec![fluxdb_core::Column {
            name: "name".into(),
            type_name: None,
            nullable: true,
            primary_key: false,
            comment: None,
        }],
        rows: vec![fluxdb_core::Row {
            values: vec![fluxdb_core::CellValue::Text("PUBLIC-FIXTURE".into())],
        }],
        offset: 0,
        limit: 10,
        has_more: false,
    };
    let grant = PageDataGrant {
        reference_id: "page:2".into(),
        target: TargetRef::default(),
        page: Arc::new(page.clone()),
    };
    page.rows.clear();
    let job = AiConversationJob {
        session_id: "s-1".into(),
        turn_id: "t-1".into(),
        assistant_message_id: "a".into(),
        model_profile_id: "m".into(),
    };
    let mut catalog = ToolCatalog::new(vec![]);
    add_ai_page_tools(&mut catalog, Some(grant), &job).unwrap();
    let receipt = run_editor_tool(&catalog, "page.read_rows", r#"{"reference_id":"page:2"}"#);
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    assert!(receipt.result_json.contains("PUBLIC-FIXTURE"));
    let denied = run_editor_tool(
        &catalog,
        "page.read_rows",
        r#"{"reference_id":"page:other"}"#,
    );
    assert_eq!(denied.status, fluxdb_core::ToolReceiptStatus::Failed);
    let mut empty = ToolCatalog::new(vec![]);
    add_ai_page_tools(&mut empty, None, &job).unwrap();
    assert!(empty.describe("page.read_rows").is_none());
}

#[test]
fn local_sql_file_reader_handles_bom_empty_and_rejects_binary() {
    let root = std::env::temp_dir().join(format!("fluxdb-open-sql-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("test.SQL");
    std::fs::write(&path, b"\xef\xbb\xbfselect 1;\r\n").unwrap();
    assert_eq!(read_sql_file(&path).unwrap(), "select 1;\r\n");
    std::fs::write(&path, b"").unwrap();
    assert_eq!(read_sql_file(&path).unwrap(), "");
    std::fs::write(&path, b"select\0binary").unwrap();
    assert!(read_sql_file(&path).is_err());
    assert!(read_sql_file(&root.join("missing.sql")).is_err());
    assert!(read_sql_file(&root.join("data.txt")).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn current_page_data_cannot_bypass_config_database_boundary() {
    let root = std::env::temp_dir().join(format!("fluxdb-page-boundary-{}", uuid::Uuid::new_v4()));
    let config_root = root.join("config");
    std::fs::create_dir_all(&config_root).unwrap();
    let protected = config_root.join("settings.sqlite");
    let public = root.join("public.sqlite");
    std::fs::write(&protected, b"").unwrap();
    std::fs::write(&public, b"").unwrap();
    let mut controller = AppController::new();
    controller.set_ai_storage(fluxdb_storage::FileStorage::new(&config_root));
    let mut connection = ConnectionConfig {
        id: ConnectionId(1),
        name: "fixture".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: protected,
            read_only: true,
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    assert!(
        controller
            .validate_ai_database_resource(&connection)
            .is_err()
    );
    connection.endpoint = Endpoint::SqliteFile {
        path: public,
        read_only: true,
    };
    assert!(
        controller
            .validate_ai_database_resource(&connection)
            .is_ok()
    );
    std::fs::remove_dir_all(root).unwrap();
}
