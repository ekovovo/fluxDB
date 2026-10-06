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
