// 界面上下文捕获与 Turn 目标固定测试（设计 §13，T021）。
//
// 全部用例只使用内存 DTO，不读取生产配置/SQLite，不访问网络或凭据。

fn ai_context_sql_tab(
    tab_id: u64,
    revision: u64,
    connection_id: i64,
    database: &str,
    selection: Option<&str>,
    full_text: &str,
    statement: Option<&str>,
) -> CapturedTab {
    CapturedTab::SqlEditor {
        tab_id,
        revision,
        connection_id,
        database: Some(database.to_string()),
        schema: Some("public".to_string()),
        selection: selection.map(str::to_string),
        full_text: full_text.to_string(),
        statement_at_cursor: statement.map(str::to_string),
    }
}

fn ai_context_visible_bytes_without_marker(text: &str) -> usize {
    let marker_start = text.find("\n… [").expect("截断文本必须包含省略标记");
    let after_marker = &text[marker_start + 1..];
    let marker_end = after_marker
        .find('\n')
        .map(|offset| marker_start + 1 + offset);
    let head = &text[..marker_start];
    let tail = marker_end.map(|end| &text[end + 1..]).unwrap_or_default();
    head.len() + tail.len()
}

#[test]
fn ai_context_sql_selection_takes_priority() {
    let tab = ai_context_sql_tab(
        7,
        12,
        42,
        "analytics",
        Some("SELECT customer_id FROM orders"),
        "SELECT * FROM orders; -- 不应该因选区而被发送",
        Some("SELECT * FROM orders"),
    );

    let snapshot = capture(&tab, ContextLimits::default());

    assert_eq!(
        snapshot.selected_text.as_deref(),
        Some("SELECT customer_id FROM orders")
    );
    assert!(snapshot.excerpt.is_none(), "有选区时不应发送全文摘录");
    assert_eq!(
        snapshot.statement_at_cursor.as_deref(),
        Some("SELECT * FROM orders")
    );
    assert_eq!(snapshot.target.connection_id, Some(42));
    assert_eq!(snapshot.target.database.as_deref(), Some("analytics"));
    assert_eq!(snapshot.target.schema.as_deref(), Some("public"));
}

#[test]
fn ai_context_without_selection_uses_bounded_full_text() {
    let full_text = (0..80)
        .map(|index| format!("SELECT {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let tab = ai_context_sql_tab(1, 1, 1, "main", None, &full_text, None);
    let limits = ContextLimits {
        max_excerpt_bytes: 4096,
        max_excerpt_lines: 10,
        head_ratio: 0.6,
    };

    let snapshot = capture(&tab, limits);
    let excerpt = snapshot.excerpt.expect("无选区时应有摘录");

    assert!(excerpt.truncated);
    assert!(excerpt.next_offset > 0);
    assert!(excerpt.text.contains("省略"));
}

#[test]
fn ai_context_short_text_is_not_truncated() {
    let text = "SELECT 1;\nSELECT 2;";
    let bounded = bound_excerpt(text, ContextLimits::default());

    assert!(!bounded.truncated);
    assert_eq!(bounded.text, text);
    assert_eq!(bounded.omitted_lines, 0);
    assert_eq!(bounded.next_offset, 2);
}

#[test]
fn ai_context_head_tail_and_omission_marker_are_preserved() {
    let text = (0..1000)
        .map(|index| format!("line_{index:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let limits = ContextLimits {
        max_excerpt_bytes: 1024 * 1024,
        max_excerpt_lines: 10,
        head_ratio: 0.6,
    };

    let bounded = bound_excerpt(&text, limits);

    assert!(bounded.truncated);
    assert_eq!(bounded.omitted_lines, 990);
    assert!(bounded.text.contains("line_0000"));
    assert!(bounded.text.contains("line_0999"));
    assert!(bounded.text.contains("省略"));
}

#[test]
fn ai_context_byte_limit_is_utf8_safe_and_bounded() {
    let text = "这是一个很长的中文句子".repeat(20);
    let limits = ContextLimits {
        max_excerpt_bytes: 12,
        max_excerpt_lines: 10,
        head_ratio: 0.6,
    };

    let bounded = bound_excerpt(&text, limits);

    assert!(bounded.truncated);
    assert!(bounded.text.contains("内容超限已截断"));
    assert!(
        ai_context_visible_bytes_without_marker(&bounded.text) <= limits.max_excerpt_bytes,
        "去掉标记后的头尾内容不能超过字节上限"
    );
    assert!(std::str::from_utf8(bounded.text.as_bytes()).is_ok());
}

#[test]
fn ai_context_table_data_captures_summary_and_result_ref() {
    let tab = CapturedTab::TableData {
        tab_id: 11,
        revision: 3,
        connection_id: 5,
        database: Some("shop".to_string()),
        schema: Some("public".to_string()),
        object_name: "orders".to_string(),
        columns: vec!["id".to_string(), "amount".to_string()],
        filter: Some("amount > 100".to_string()),
        sort: Some("id DESC".to_string()),
        sample_rows: vec!["1, 120".to_string(), "2, 80".to_string()],
        result_ref: Some("query-result:42".to_string()),
    };

    let snapshot = capture(&tab, ContextLimits::default());
    let summary: serde_json::Value =
        serde_json::from_str(snapshot.summary.as_deref().expect("表摘要不能为空"))
            .expect("表摘要应为合法 JSON");

    assert_eq!(snapshot.target.object.as_deref(), Some("orders"));
    assert_eq!(snapshot.result_ref.as_deref(), Some("query-result:42"));
    assert!(snapshot.excerpt.is_none());
    assert_eq!(summary["columns"], serde_json::json!(["id", "amount"]));
    assert_eq!(summary["filter"], "amount > 100");
    assert_eq!(summary["sort"], "id DESC");
    assert_eq!(
        summary["sample_rows"],
        serde_json::json!(["1, 120", "2, 80"])
    );
}

#[test]
fn ai_context_redis_key_binds_db_and_bounds_preview() {
    let tab = CapturedTab::RedisKey {
        tab_id: 19,
        revision: 4,
        connection_id: 8,
        redis_db: 3,
        key: "session:42".to_string(),
        key_type: "hash".to_string(),
        ttl_seconds: Some(120),
        preview: "field_a=1\nfield_b=2\nfield_c=3".to_string(),
    };
    let limits = ContextLimits {
        max_excerpt_bytes: 1024,
        max_excerpt_lines: 2,
        head_ratio: 0.6,
    };

    let snapshot = capture(&tab, limits);
    let summary: serde_json::Value =
        serde_json::from_str(snapshot.summary.as_deref().expect("Redis 摘要不能为空"))
            .expect("Redis 摘要应为合法 JSON");
    let excerpt = snapshot.excerpt.expect("Redis preview 应有摘录");

    assert_eq!(snapshot.target.redis_db, Some(3));
    assert_eq!(snapshot.target.object.as_deref(), Some("session:42"));
    assert_eq!(summary["key"], "session:42");
    assert_eq!(summary["type"], "hash");
    assert_eq!(summary["ttl_seconds"], 120);
    assert!(excerpt.truncated);
    assert!(excerpt.text.contains("省略"));
}

#[test]
fn ai_context_design_tab_uses_summary_as_excerpt() {
    let summary = "设计对象：用户表\n字段：id、name\n关系：无";
    let tab = CapturedTab::Design {
        tab_id: 23,
        revision: 9,
        connection_id: 3,
        database: Some("app".to_string()),
        schema: Some("public".to_string()),
        object_path: "public.users".to_string(),
        summary: summary.to_string(),
    };

    let snapshot = capture(&tab, ContextLimits::default());
    let excerpt = snapshot.excerpt.expect("设计页应有摘要摘录");

    assert_eq!(snapshot.target.object.as_deref(), Some("public.users"));
    assert_eq!(excerpt.text, summary);
    assert!(!excerpt.truncated);
}

#[test]
fn ai_context_no_tab_notes_empty_target_and_optional_connection() {
    let empty = capture(
        &CapturedTab::NoTab {
            connection_id: None,
            database: None,
            schema: None,
            last_object: None,
        },
        ContextLimits::default(),
    );
    assert!(empty.target.is_empty());
    assert!(!empty.notes.is_empty());
    assert!(empty.can_proceed());

    let connected = capture(
        &CapturedTab::NoTab {
            connection_id: Some(77),
            database: None,
            schema: None,
            last_object: None,
        },
        ContextLimits::default(),
    );
    assert!(!connected.target.is_empty());
    assert_eq!(connected.target.connection_id, Some(77));
    assert!(connected.notes.is_empty());
}

#[test]
fn ai_context_pin_survives_tab_switch() {
    let tab_a = ai_context_sql_tab(1, 1, 10, "db_a", None, "SELECT 1;", None);
    let snapshot_a = capture(&tab_a, ContextLimits::default());
    let pin = pin_target(&snapshot_a);

    let tab_b = ai_context_sql_tab(2, 9, 20, "db_b", None, "SELECT 2;", None);
    let snapshot_b = capture(&tab_b, ContextLimits::default());

    assert_eq!(snapshot_b.target.connection_id, Some(20));
    assert_eq!(pin.target, snapshot_a.target);
    assert_eq!(pin.target_json, snapshot_a.target.to_json());
    assert_eq!(pin.captured_tab_id, Some(1));
    assert_ne!(pin.target, snapshot_b.target);
}

#[test]
fn ai_context_retarget_only_changes_explicit_target() {
    let tab_a = ai_context_sql_tab(1, 1, 10, "db_a", None, "SELECT 1;", None);
    let pin_a = pin_target(&capture(&tab_a, ContextLimits::default()));

    let changed = retarget(
        &pin_a,
        TargetRef {
            connection_id: Some(10),
            database: Some("db_b".to_string()),
            schema: Some("public".to_string()),
            ..TargetRef::default()
        },
    );
    let pin_b = match changed {
        RetargetOutcome::Changed {
            pin,
            invalidated_target_json,
        } => {
            assert_eq!(invalidated_target_json, pin_a.target_json);
            assert_ne!(pin.target_json, pin_a.target_json);
            pin
        }
        RetargetOutcome::Unchanged { .. } => panic!("显式换 database 必须产生 Changed"),
    };

    let unchanged = retarget(&pin_b, pin_b.target.clone());
    match unchanged {
        RetargetOutcome::Unchanged { pin } => assert_eq!(pin, pin_b),
        RetargetOutcome::Changed { .. } => panic!("相同目标必须保持 Unchanged"),
    }
}

#[test]
fn ai_context_target_json_only_contains_non_sensitive_locators() {
    let target = TargetRef {
        connection_id: Some(9),
        database: Some("analytics".to_string()),
        schema: Some("public".to_string()),
        object: Some("orders".to_string()),
        redis_db: Some(2),
    };
    let value: serde_json::Value =
        serde_json::from_str(&target.to_json()).expect("target_json 应为合法 JSON");
    let keys: std::collections::BTreeSet<String> = value
        .as_object()
        .expect("target_json 应为对象")
        .keys()
        .cloned()
        .collect();
    let expected = ["connection_id", "database", "schema", "object", "redis_db"]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(keys, expected);
    for forbidden in ["password", "passwd", "endpoint", "secret", "token", "user"] {
        assert!(
            !keys.contains(forbidden),
            "target_json 不应包含 {forbidden}"
        );
    }
}

#[test]
fn ai_context_default_limits_match_design() {
    let limits = ContextLimits::default();

    assert_eq!(limits.max_excerpt_bytes, 16 * 1024);
    assert_eq!(limits.max_excerpt_lines, 400);
    assert_eq!(limits.head_ratio, 0.6);
}

#[test]
fn ai_editor_read_range_reads_turn_scoped_full_text_by_line_window() {
    // 设计 §13.2：界面摘录超限时插入"可用 editor.read_range 读取"标记，
    // 该工具必须真的能按行区间取回被省略的中段。
    let full = (0..500)
        .map(|i| format!("line-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tools = editor_read_range_tools(&full, 7);
    let spec = tools.describe("editor.read_range").expect("工具应已注册");
    assert_eq!(spec.risk, fluxdb_core::ToolRisk::Read);

    let receipt = run_editor_read(
        &tools,
        r#"{"offset":100,"limit":3}"#,
    );
    let value: serde_json::Value = serde_json::from_str(&receipt.result_json).expect("合法 JSON");
    assert_eq!(value["lines"], serde_json::json!(["line-100", "line-101", "line-102"]));
    assert_eq!(value["total_lines"], 500);
    assert_eq!(value["next_offset"], 103);
    assert_eq!(value["has_more"], true);
}

#[test]
fn ai_editor_read_range_rejects_unknown_tab_and_out_of_range_offset() {
    let tools = editor_read_range_tools("only line", 9);
    // 未知 tab_ref 不能读别的内容：只有本轮装配的那一个可读。
    let bad = run_editor_read(&tools, r#"{"tab_ref":"other-tab","offset":0,"limit":5}"#);
    assert_eq!(bad.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert!(bad.result_json.contains("tab_ref"));

    let out = run_editor_read(&tools, r#"{"offset":99,"limit":5}"#);
    assert_eq!(out.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert!(out.result_json.contains("超出"), "实际: {}", out.result_json);
}

#[test]
fn ai_editor_read_range_marks_snapshot_scope_and_bounds_bytes() {
    // 单行也要受字节上限约束：一行超长时按字节切字符边界，并显式标记截断。
    let full = "x".repeat(EDITOR_READ_MAX_BYTES * 2);
    let tools = editor_read_range_tools(&full, 3);
    let receipt = run_editor_read(&tools, r#"{"offset":0,"limit":10}"#);
    let value: serde_json::Value = serde_json::from_str(&receipt.result_json).expect("合法 JSON");
    assert_eq!(value["truncated"], true);
    // 超长单行必须给出可续读位置，否则模型拿到不完整行却无从继续。
    let partial = &value["partial_line"];
    assert_eq!(partial["line"], 0);
    assert!(
        partial["next_byte_offset"].as_u64().unwrap() > 0,
        "必须给出该行内的续读字节偏移"
    );
    // 不能切出半个 UTF-8 字符：serde 能解析即证明切片合法。
    let first_line = value["lines"][0].as_str().unwrap();
    assert_eq!(first_line.len(), partial["next_byte_offset"].as_u64().unwrap() as usize);
    assert!(
        value["scope"].as_str().unwrap().contains("快照"),
        "必须声明这是发送时的快照而非实时内容，实际: {}",
        value["scope"]
    );
}

fn editor_read_range_tools(full_text: &str, tab_id: u64) -> ToolCatalog {
    let mut catalog = ToolCatalog::new(Vec::new());
    add_ai_editor_read_tools(
        &mut catalog,
        Some(EditorGrant {
            tab_ref: format!("tab-{tab_id}"),
            snapshot: fluxdb_editor_core::EditorBuffer::new_from(full_text).snapshot(),
        }),
    )
    .expect("装配 editor.read_range");
    catalog
}

fn run_editor_read(tools: &ToolCatalog, arguments_json: &str) -> fluxdb_core::ToolReceipt {
    run_editor_tool(tools, "editor.read_range", arguments_json)
}

fn run_editor_tool(
    tools: &ToolCatalog,
    tool_id: &str,
    arguments_json: &str,
) -> fluxdb_core::ToolReceipt {
    let prepared = tools
        .prepare(tool_id, arguments_json)
        .expect("参数应通过 schema 校验");
    let tool = tools.executable(tool_id).expect("应可执行");
    let ctx = fluxdb_core::ToolExecutionContext {
        automatic_approval: false,
        bounded_write: false,
        session_id: "s-1".into(),
        turn_id: "t-1".into(),
        iteration: 1,
        call_id: "call-1".into(),
        cancel: fluxdb_core::CancelFlag::new(),
        progress: None,
    };
    crate::ai_executor::block_on(tool.execute(ctx, prepared.to_json_string()))
        .expect("执行本身不应失败")
        .expect("工具应回执而非 Err")
}

#[test]
fn ai_editor_search_returns_hits_and_read_hints_from_same_snapshot() {
    let full = "SELECT 1;\nALTER TABLE users ADD COLUMN email TEXT;\nSELECT 2;";
    let tools = editor_read_range_tools(full, 7);
    assert!(tools.describe("editor.search").is_some());

    let receipt = run_editor_tool(
        &tools,
        "editor.search",
        r#"{"query":"users","context_lines":1}"#,
    );
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    let value: serde_json::Value = serde_json::from_str(&receipt.result_json).expect("合法 JSON");
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    assert_eq!(value["matches"][0]["line_number"], 2);
    assert_eq!(value["matches"][0]["read_hint"]["offset"], 1);
    assert!(value["matches"][0]["snippet"].as_str().unwrap().contains("ALTER TABLE"));
    assert!(value["search_complete"].as_bool().unwrap());
    assert!(
        value["scope"].as_str().unwrap().contains("快照"),
        "搜索结果必须声明来自发送时快照"
    );

    let rejected = run_editor_tool(
        &tools,
        "editor.search",
        r#"{"tab_ref":"other-tab","query":"users"}"#,
    );
    assert_eq!(rejected.status, fluxdb_core::ToolReceiptStatus::Failed);
}

#[test]
fn ai_editor_read_range_continues_a_truncated_line_without_gap_or_overlap() {
    let full = "a".repeat(EDITOR_READ_MAX_BYTES * 2 + 5);
    let tools = editor_read_range_tools(&full, 3);
    let mut byte_offset = 0u64;
    let mut collected = String::new();
    let mut finished = false;
    for _ in 0..3 {
        let receipt = run_editor_read(
            &tools,
            &format!(r#"{{"offset":0,"limit":1,"byte_offset":{byte_offset}}}"#),
        );
        let value: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        assert_eq!(value["byte_offset"], byte_offset);
        collected.push_str(value["lines"][0].as_str().unwrap());
        if value["has_more"] == false {
            assert!(value["partial_line"].is_null());
            finished = true;
            break;
        }
        assert_eq!(value["next_offset"], 0, "同一超长行续读时行号不得前进");
        byte_offset = value["partial_line"]["next_byte_offset"]
            .as_u64()
            .unwrap();
        assert!(byte_offset > 0);
    }
    assert!(finished, "三段内应能读完这条超长行");
    assert_eq!(collected, full);
}

#[test]
fn ai_editor_grant_is_turn_scoped_taken_once_and_never_persisted() {
    // 全文只在本轮内存里：登记后能被目录装配取走，取走后不可再取，
    // 也不会出现在运行快照里（设计 §13.2 不把整份内容持久化进会话）。
    let grant = EditorGrant {
        tab_ref: "tab-1".into(),
        snapshot: fluxdb_editor_core::EditorBuffer::new_from("select 1;").snapshot(),
    };
    register_editor_grant("turn-a", grant.clone()).expect("登记");

    let taken = take_editor_grant("turn-a").expect("装配时应取到");
    assert_eq!(taken.snapshot, grant.snapshot);
    assert!(
        take_editor_grant("turn-a").is_none(),
        "取走即释放：同一轮不能重复取到原文"
    );
    assert!(
        take_editor_grant("turn-b").is_none(),
        "其它 turn 读不到本轮全文"
    );

    // 真实发送一轮后，运行快照里只有界面摘录，不得出现编辑器全文。
    let dir = std::env::temp_dir().join(format!("fluxdb-editor-grant-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&dir);
    let storage = fluxdb_storage::FileStorage::new(&dir);
    storage.with_ai_connection(|_| Ok(())).unwrap();
    let secret_marker = "select 'UNIQUE-EDITOR-FULLTEXT-MARKER' from t;";
    let mut controller = AppController::new();
    controller.set_ai_storage(fluxdb_storage::FileStorage::new(&dir));
    let model = match controller.dispatch(crate::AppCommand::SaveAiModel(
        crate::AiModelSaveInput {
            display_name: "editor-grant".into(),
            provider: "openai".into(),
            model: "offline-editor".into(),
            enabled: true,
            ..Default::default()
        },
    )) {
        crate::AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save model: {other:?}"),
    };
    let agent = match controller.dispatch(crate::AppCommand::SaveAiAgent(
        crate::AiAgentSaveInput {
            display_name: "editor-grant".into(),
            system_prompt: "editor-grant".into(),
            model_profile_id: Some(model),
            ..Default::default()
        },
    )) {
        crate::AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("save agent: {other:?}"),
    };
    let job = controller
        .prepare_ai_conversation_context(
            "续读编辑器",
            &TurnTargetPin {
                target: TargetRef::default(),
                captured_tab_id: Some(1),
                captured_tab_revision: 0,
                target_json: TargetRef::default().to_json(),
            },
            "界面摘录占位",
        )
        .expect("准备发送");
    let _ = agent;
    register_editor_grant(&job.turn_id, EditorGrant {
        tab_ref: "tab-1".into(),
        snapshot: fluxdb_editor_core::EditorBuffer::new_from(secret_marker).snapshot(),
    })
    .expect("登记全文");
    storage
        .with_ai_connection(|conn| {
            let turn = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)?;
            assert!(
                !turn.runtime_snapshot_json.contains("UNIQUE-EDITOR-FULLTEXT-MARKER"),
                "编辑器全文不得写入运行快照"
            );
            assert!(
                turn.runtime_snapshot_json.contains("界面摘录占位"),
                "快照仍应保留界面摘录"
            );
            Ok(())
        })
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    clear_editor_grant("turn-cleanup");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_editor_grant_accepts_large_shared_snapshot() {
    let huge = EditorGrant {
        tab_ref: "tab-1".into(),
        snapshot: fluxdb_editor_core::EditorBuffer::new_from(&"x".repeat(2 * 1024 * 1024)).snapshot(),
    };
    register_editor_grant("turn-huge", huge).expect("大文档共享快照允许登记");
    assert_eq!(take_editor_grant("turn-huge").unwrap().snapshot.len(), 2 * 1024 * 1024);
}
