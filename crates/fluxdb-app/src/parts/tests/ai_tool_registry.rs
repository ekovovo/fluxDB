// 工具目录、JSON Schema 子集与结果协议测试（设计 §20，T019）。
//
// 本文件只使用内存中的 ToolCatalog / ResultStore / RecordingTool，不访问
// 生产配置、SQLite 或网络。重点验证“发现不等于授权、先校验再执行、
// 回执与投影分离”三条硬规则。

fn tool_spec(id: &str, input_schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        id: id.to_string(),
        version: 1,
        description: format!("测试工具 {id}"),
        input_schema,
        output_schema: serde_json::json!({"type": "object"}),
        risk: fluxdb_core::ToolRisk::Read,
        effect: EffectClass::PureRead,
        timeout_ms: 30_000,
        output_limit_bytes: 4096,
        output_shape: ToolOutputShape::Opaque,
    }
}

fn assert_invalid_arguments(catalog: &ToolCatalog, id: &str, arguments_json: &str) {
    let error = catalog
        .prepare(id, arguments_json)
        .expect_err("非法参数必须被拒绝");
    assert_eq!(error.code, "invalid_arguments", "错误码必须受控: {error:?}");
    assert!(error.retryable, "参数错误应允许模型修正后重试: {error:?}");
}

/// 将 `RecordingTool` 交给目录后仍保留外部句柄，用于断言没有发生真实执行。
struct SharedRecordingTool(Arc<crate::RecordingTool>);

impl fluxdb_core::AiTool for SharedRecordingTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        self.0.descriptor()
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        self.0.execute(ctx, arguments_json)
    }
}

#[test]
fn ai_tool_registry_schema_passes_and_rejects_without_coercion() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "sql"],
        "properties": {
            "target": {"type": "string", "minLength": 1},
            "sql": {"type": "string", "maxLength": 8},
            "max_rows": {"type": "integer", "minimum": 1, "maximum": 10},
            "mode": {"enum": ["safe", "fast"]},
            "parameters": {
                "type": "array",
                "maxItems": 2,
                "items": {"type": "object"}
            }
        }
    });
    let catalog = ToolCatalog::new(vec![tool_spec("db.query", schema)]);
    let valid = serde_json::json!({
        "target": "orders",
        "sql": "select 1",
        "max_rows": 5,
        "mode": "safe",
        "parameters": [{}]
    });
    let prepared = catalog
        .prepare("db.query", &valid.to_string())
        .expect("合法参数应通过 schema 校验");
    assert_eq!(prepared.as_value(), &valid);

    for invalid in [
        serde_json::json!({"sql": "select 1"}),
        serde_json::json!({"target": "orders", "sql": "select 1", "max_rows": "5"}),
        serde_json::json!({"target": "orders", "sql": "select 1", "extra": true}),
        serde_json::json!({"target": "orders", "sql": "select 1", "mode": "slow"}),
        serde_json::json!({"target": "orders", "sql": "123456789"}),
        serde_json::json!({
            "target": "orders",
            "sql": "select 1",
            "parameters": [{}, {}, {}]
        }),
    ] {
        assert_invalid_arguments(&catalog, "db.query", &invalid.to_string());
    }
}

#[test]
fn ai_tool_registry_ref_resolution_is_local_and_failure_is_controlled() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["parameters"],
        "properties": {
            "parameters": {
                "type": "array",
                "items": {"$ref": "#/$defs/parameter"}
            }
        },
        "$defs": {
            "parameter": {"type": "string", "minLength": 1}
        }
    });
    let catalog = ToolCatalog::new(vec![tool_spec("db.query", schema)]);
    let valid = serde_json::json!({"parameters": ["one", "two"]});
    assert!(catalog.prepare("db.query", &valid.to_string()).is_ok());

    let wrong_type = serde_json::json!({"parameters": [1]});
    assert_invalid_arguments(&catalog, "db.query", &wrong_type.to_string());

    let missing_ref_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "parameters": {
                "type": "array",
                "items": {"$ref": "#/$defs/missing"}
            }
        }
    });
    let missing_ref_catalog = ToolCatalog::new(vec![tool_spec("db.query", missing_ref_schema)]);
    let error = missing_ref_catalog
        .prepare("db.query", r#"{"parameters": [1]}"#)
        .expect_err("未知 $ref 目标必须拒绝而不是 panic");
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("$ref"),
        "错误应指出引用问题: {error:?}"
    );
}

#[test]
fn ai_tool_registry_unknown_tool_is_non_retryable_and_not_executable() {
    let catalog = ToolCatalog::new(vec![tool_spec(
        "db.query",
        serde_json::json!({"type": "object"}),
    )]);
    assert!(catalog.executable("nope.tool").is_none());

    let error = catalog
        .prepare("nope.tool", "{}")
        .expect_err("未知工具必须被拒绝");
    assert_eq!(error.code, "unknown_tool");
    assert!(!error.retryable, "未知工具不应盲目重试: {error:?}");
}

#[test]
fn ai_tool_registry_discovery_returns_summary_without_granting_execution() {
    let catalog = ToolCatalog::new(vec![
        ToolSpec {
            description: "执行只读查询".into(),
            ..tool_spec("db.query", serde_json::json!({"type": "object"}))
        },
        ToolSpec {
            risk: fluxdb_core::ToolRisk::Write,
            effect: EffectClass::Mutating,
            description: "执行写入".into(),
            ..tool_spec("db.write", serde_json::json!({"type": "object"}))
        },
    ]);

    let summaries = catalog.discover("db", 10);
    assert_eq!(summaries.len(), 2);
    let ToolSummary {
        id,
        version,
        description,
        risk,
        effect,
    } = &summaries[0];
    assert_eq!(id, "db.query");
    assert_eq!(*version, 1);
    assert_eq!(description, "执行只读查询");
    assert_eq!(*risk, fluxdb_core::ToolRisk::Read);
    assert_eq!(*effect, EffectClass::PureRead);
    assert_eq!(
        catalog
            .describe("db.query")
            .expect("describe 应能取得 schema")
            .input_schema,
        serde_json::json!({"type": "object"})
    );
    assert!(
        catalog.executable("db.query").is_none(),
        "未注册实现时，看见工具不等于可执行"
    );
}

#[test]
fn ai_tool_registry_argument_limit_rejects_before_execution() {
    let catalog = ToolCatalog::new(vec![tool_spec(
        "db.query",
        serde_json::json!({"type": "object"}),
    )])
    .with_argument_limit(64);
    let tool = Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let mut catalog = catalog;
    catalog
        .register(Box::new(SharedRecordingTool(tool.clone())))
        .expect("有对应 spec 时应可注册");
    assert!(catalog.executable("db.query").is_some());

    let oversized = serde_json::json!({"query": "x".repeat(100)}).to_string();
    assert!(oversized.len() > 64);
    let error = catalog
        .prepare("db.query", &oversized)
        .expect_err("超过参数体积上限必须拒绝");
    assert_eq!(error.code, "arguments_too_large");
    assert!(error.retryable);
    assert_eq!(tool.call_count(), 0, "参数校验失败不得执行");
}

#[test]
fn ai_tool_registry_result_pages_are_bounded_and_cursor_checked() {
    let mut store = ResultStore::new(1024 * 1024);
    let items: Vec<serde_json::Value> = (0..10)
        .map(|n| serde_json::json!({"n": n, "text": "x"}))
        .collect();
    let (result, spill) = store
        .put("db.query", "call-1", &[], 123, items)
        .expect("保存结果");
    assert!(!spill.spilled(), "大字节上限应完整留在内存，不触发 spill");

    let first = store
        .read_page(&result.id, None, 3, 10 * 1024 * 1024)
        .expect("第一页应可读");
    assert_eq!(first.items.len(), 3);
    assert!(first.truncated);
    assert_eq!(first.next_cursor, Some(format!("{}:3", result.id)));
    assert_eq!(first.total_items, 10);

    let mut seen = first.items.clone();
    let mut cursor = first.next_cursor.clone();
    let mut pages = 1;
    while let Some(next) = cursor {
        let page = store
            .read_page(&result.id, Some(&next), 3, 10 * 1024 * 1024)
            .expect("后续页应可读");
        seen.extend(page.items);
        cursor = page.next_cursor;
        pages += 1;
    }
    assert_eq!(pages, 4);
    assert_eq!(seen.len(), 10);

    let small = store
        .read_page(&result.id, None, 10, 20)
        .expect("小字节上限也应返回有界单条结果");
    assert_eq!(small.items.len(), 1);
    assert!(small.truncated);
    assert!(small.next_cursor.is_some());

    let bad = store
        .read_page(&result.id, Some("not-a-cursor"), 3, 1024)
        .expect_err("格式非法游标必须拒绝");
    assert_eq!(bad.code, "invalid_cursor");
    let out_of_range = store
        .read_page(&result.id, Some(&format!("{}:11", result.id)), 3, 1024)
        .expect_err("越界游标必须拒绝");
    assert_eq!(out_of_range.code, "invalid_cursor");
}

#[test]
fn ai_tool_registry_released_and_expired_results_are_visible_errors() {
    let mut released_store = ResultStore::new(1024);
    let (released, _) = released_store
        .put(
            "db.query",
            "call-1",
            &[],
            1,
            vec![serde_json::json!({"n": 1})],
        )
        .expect("保存结果");
    assert!(released_store.release(&released.id));
    let released_error = released_store
        .read_page(&released.id, None, 10, 1024)
        .expect_err("释放后的结果必须报错");
    assert_eq!(released_error.code, "result_expired");

    let mut expired_store = ResultStore::new(1024);
    let (expired, _) = expired_store
        .put(
            "db.query",
            "call-2",
            &[],
            2,
            vec![serde_json::json!({"n": 2})],
        )
        .expect("保存结果");
    assert!(expired_store.expire(&expired.id));
    let expired_error = expired_store
        .read_page(&expired.id, None, 10, 1024)
        .expect_err("过期后的结果必须报错");
    assert_eq!(expired_error.code, "result_expired");
}

#[test]
fn ai_tool_registry_receipt_projection_is_bounded_and_immutable() {
    let source = "数据库查询成功，返回了一行包含中文字段的数据。".repeat(8);
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "call-1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: source.clone(),
        error_code: None,
        affected_rows: Some(1),
        started_at_ms: 10,
        finished_at_ms: 20,
    };
    let result_ref = ResultRef {
        id: "result-1".into(),
        tool_id: "db.query".into(),
        call_id: "call-1".into(),
        created_at_ms: 20,
        session_id: "s-1".into(),
        turn_id: "t-1".into(),
        format: ResultFormat::Items,
    };
    let before = receipt.result_json.clone();

    let projection = project_receipt(&receipt, Some(result_ref.clone()), 10);
    assert!(projection.truncated);
    assert!(projection.text.len() <= 10);
    assert!(projection.text.is_char_boundary(projection.text.len()));
    assert_eq!(projection.result_ref, Some(result_ref));
    assert_eq!(receipt.result_json, before, "投影不得修改原始回执");

    let full = project_receipt(&receipt, None, before.len() + 32);
    assert!(!full.truncated);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&full.text).unwrap(), serde_json::json!(before));
}

#[test]
fn ai_tool_registry_register_rejects_id_without_spec() {
    let mut catalog = ToolCatalog::new(vec![tool_spec(
        "db.query",
        serde_json::json!({"type": "object"}),
    )]);
    let tool = crate::RecordingTool::new("missing.tool", fluxdb_core::ToolRisk::Read);
    let result = catalog.register(Box::new(tool));
    assert!(
        result.is_err(),
        "没有对应 ToolSpec 的可执行实现必须拒绝注册"
    );
    assert!(catalog.executable("missing.tool").is_none());
}

/// 主 agent 补测：`ToolSpec.output_limit_bytes` 必须真正生效（此前只声明未使用）。
#[test]
fn ai_tool_registry_projection_honors_spec_output_limit() {
    let mut spec = tool_spec("db.query", serde_json::json!({"type":"object"}));
    spec.output_limit_bytes = 32;
    let catalog = crate::ToolCatalog::new(vec![spec]);

    let receipt = fluxdb_core::ToolReceipt {
        call_id: "call-1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: serde_json::json!({"rows": (0..50).collect::<Vec<_>>()}).to_string(),
        error_code: None,
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 1,
    };
    let before = receipt.result_json.clone();

    let projection = catalog
        .project("db.query", &receipt, None)
        .expect("已注册工具应能投影");
    assert!(projection.truncated, "超过 spec 上限必须标记截断");
    assert!(
        projection.text.len() <= 32,
        "投影不得超过 spec 声明的字节上限，实际 {}",
        projection.text.len()
    );
    assert_eq!(receipt.result_json, before, "投影不得修改原始回执");

    // 未知工具的投影同样是受控错误。
    let error = catalog.project("nope.tool", &receipt, None).unwrap_err();
    assert_eq!(error.code, "unknown_tool");
}

#[test]
fn ai_tool_registry_spilled_results_are_readable_page_by_page() {
    // 大结果 spill 到受控文件后，分页必须能读回**全部**行，而不是只报"还有更多"。
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let mut store = ResultStore::for_turn(64, "s-1", "t-1", Some(root.clone()));
    let rows: Vec<serde_json::Value> = (0..40)
        .map(|n| serde_json::json!({"n": n, "text": "x".repeat(16)}))
        .collect();
    let (result, spill) = store
        .put("db.query", "call-9", &[], 42, rows)
        .expect("保存结果");
    assert!(spill.spilled(), "超出内存预算应写受控 spill 文件");
    assert!(spill.byte_count > 0 && !spill.content_hash.is_empty());

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = store
            .read_page(&result.id, cursor.as_deref(), 7, 64 * 1024)
            .expect("分页读取");
        seen.extend(page.items);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 40, "spill 后仍应能读回全部行");
    assert_eq!(seen[0], serde_json::json!({"n": 0, "text": "x".repeat(16)}));
    assert_eq!(seen[39]["n"], 39);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_tool_registry_spill_file_damage_is_reported_not_silently_empty() {
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let mut store = ResultStore::for_turn(64, "s-1", "t-1", Some(root.clone()));
    let rows: Vec<serde_json::Value> = (0..20)
        .map(|n| serde_json::json!({"n": n, "text": "x".repeat(16)}))
        .collect();
    let (result, spill) = store
        .put("db.query", "call-9", &[], 42, rows)
        .expect("保存结果");
    assert!(spill.spilled());

    // 人为破坏 spill 文件：读取必须显式报错，不能伪装成空页。
    std::fs::write(root.join(&spill.relative_path.unwrap()), b"not-json").expect("破坏文件");
    let error = store
        .read_page(&result.id, None, 50, 64 * 1024)
        .expect_err("损坏的 spill 文件必须报错");
    assert_eq!(error.code, "result_damaged");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_tool_registry_result_refs_are_scoped_to_the_owning_session() {
    // 另一个会话创建的 ResultStore 不能读到本会话的结果，反向亦然。
    let root = std::env::temp_dir().join(format!("fluxdb-t027-{}", uuid::Uuid::new_v4()));
    let mut mine = ResultStore::for_turn(64 * 1024, "s-1", "t-1", Some(root.clone()));
    let (result, _) = mine
        .put(
            "db.query",
            "call-1",
            &[],
            1,
            vec![serde_json::json!({"n": 1})],
        )
        .expect("保存结果");
    assert!(mine.read_page(&result.id, None, 10, 4096).is_ok());

    let other = ResultStore::for_turn(64 * 1024, "s-2", "t-2", Some(root.clone()));
    let foreign = other
        .read_page(&result.id, None, 10, 4096)
        .expect_err("跨会话不得读取");
    assert_eq!(foreign.code, "unknown_result");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ai_tool_registry_globally_disabled_tool_is_neither_discoverable_nor_executable() {
    // 全局关闭必须同时挡住"发现"和"直接调用"：只藏描述的话，模型仍可凭
    // 记忆里的工具名发起调用，prepare 就会失败而不是被静默放行。
    let resolver = Arc::new(MapSqlResolver::new(Vec::new()));
    let mut catalog = standard_sql_catalog(resolver, true).expect("装配只读目录");
    assert!(catalog.describe("db.query").is_some());
    assert!(catalog.executable("db.query").is_some());

    let disabled: std::collections::BTreeSet<String> = ["db.query".to_string()].into();
    catalog.without_tools(&disabled);

    assert!(
        catalog.describe("db.query").is_none(),
        "全局关闭后不应再有 spec"
    );
    assert!(
        catalog.executable("db.query").is_none(),
        "全局关闭后不应保留可执行实现"
    );
    assert!(
        catalog.discover("db.query", 10).is_empty(),
        "全局关闭的工具不应被工具发现返回"
    );
    assert!(
        matches!(
            catalog.prepare("db.query", r#"{"target":"c1","sql":"select 1"}"#),
            Err(ToolError {
                code: "unknown_tool",
                ..
            })
        ),
        "绕过发现直接调用必须返回受控 unknown_tool"
    );
    // 未停用的工具不受影响。
    assert!(catalog.describe("db.describe_object").is_some());
}

#[test]
fn ai_tool_registry_truncated_projection_is_still_valid_json() {
    // 回执超过工具 output 上限时，投影会按字节截断。截断后的片段必须仍是
    // 合法 JSON：ai_loop 会把它原样嵌进 {"tool":..,"result":<投影>} 信封发给模型，
    // 半截 JSON 会让模型收到不可解析的内容，而不是"结果被截断"这个事实。
    let rows: Vec<serde_json::Value> = (0..20000)
        .map(|i| serde_json::json!(format!("value-{i}")))
        .collect();
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "c1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: serde_json::json!({ "columns": ["a"], "rows": rows }).to_string(),
        error_code: None,
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 1,
    };
    let limit = 2048usize;
    let projection = project_receipt(&receipt, None, limit);
    assert!(projection.truncated, "fixture 必须真的超限");

    // 与 ai_loop 构造信封的方式保持一致。
    let envelope = format!(
        "{{\"tool\":\"{}\",\"status\":\"succeeded\",\"truncated\":{},\"result\":{}}}",
        receipt.tool_id, projection.truncated, projection.text
    );
    serde_json::from_str::<serde_json::Value>(&envelope).unwrap_or_else(|error| {
        panic!(
            "截断后信封必须是合法 JSON: {error}; tail={}",
            &envelope[envelope.len().saturating_sub(80)..]
        )
    });
}

#[test]
fn ai_tool_registry_truncation_keeps_structure_and_marks_omitted_fields() {
    // 设计 §15：大结果只带列结构、行数/截断标记与少量样本。截断应优先丢弃
    // 体积最大的 rows，保留 columns/row_count/truncated 这些小事实，并明确
    // 标注被省略的字段，而不是只留一段无法解释的残缺 JSON。
    let rows: Vec<serde_json::Value> = (0..5000)
        .map(|i| serde_json::json!(format!("row-value-{i}")))
        .collect();
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "c1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: serde_json::json!({
            "columns": ["id", "name"],
            "rows": rows,
            "row_count": 5000,
            "truncated": true,
        })
        .to_string(),
        error_code: None,
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 1,
    };
    let limit = 512usize;
    let projection = project_receipt_with_shape(&receipt, None, limit,
        &ToolOutputShape::Sample { omit_field: "rows".into(), rows_omitted_field: None });
    assert!(projection.truncated);
    assert!(
        projection.text.len() <= limit,
        "投影必须遵守工具声明上限，实际 {}",
        projection.text.len()
    );
    let value: serde_json::Value =
        serde_json::from_str(&projection.text).expect("截断后仍是合法 JSON");
    assert_eq!(value["columns"], serde_json::json!(["id", "name"]));
    assert_eq!(value["row_count"], 5000);
    let shown = value["rows"].as_array().unwrap().len();
    assert!(shown > 5 && shown < 5000, "按预算保留完整前缀，不固定 5 行");
    assert_eq!(value["returned_items"], shown);
    assert_eq!(value["delivery_truncated"], true);

}

#[test]
fn ai_tool_registry_non_json_receipt_truncation_stays_valid_and_bounded() {
    // 受控夹具可能返回非 JSON 文本；截断后必须是合法 JSON 字符串且不超上限。
    let source = "数据库查询成功，返回了中文内容。".repeat(64);
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "c1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded,
        result_json: source,
        error_code: None,
        affected_rows: None,
        started_at_ms: 0,
        finished_at_ms: 1,
    };
    // 0/1 字节预算装不下任何合法 JSON（`""` 本身 2 字节），从 2 开始。
    for limit in [2usize, 8, 32, 128] {
        let projection = project_receipt(&receipt, None, limit);
        assert!(projection.truncated);
        assert!(
            projection.text.len() <= limit,
            "limit={limit} 时投影 {} 超出上限",
            projection.text.len()
        );
        serde_json::from_str::<serde_json::Value>(&projection.text)
            .unwrap_or_else(|error| panic!("limit={limit} 截断后必须是合法 JSON: {error}"));
    }
}

#[test]
fn ai_tool_registry_sample_shape_sheds_declared_field_first() {
    // 声明了 Sample 形状的工具，兜底裁剪必须优先丢声明字段（rows），
    // 即使它在 JSON 里不是最后一个键；否则 rows 会留在上下文里把结构字段挤掉。
    let rows: Vec<serde_json::Value> = (0..4000)
        .map(|i| serde_json::json!(format!("row-{i}")))
        .collect();
    let raw = serde_json::json!({
        "rows": rows,
        "columns": ["id", "name"],
        "row_count": 4000,
        "truncated": true,
        "rows_omitted": 3995,
    })
    .to_string();
    let shape = ToolOutputShape::Sample {
        omit_field: "rows".into(),
        rows_omitted_field: Some("rows_omitted".into()),
    };
    let receipt = fluxdb_core::ToolReceipt {
        call_id: "c1".into(), tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Succeeded, result_json: raw,
        error_code: None, affected_rows: None, started_at_ms: 0, finished_at_ms: 1,
    };
    let projection = project_receipt_with_shape(&receipt, None, 512, &shape);
    let value: serde_json::Value = serde_json::from_str(&projection.text).unwrap();
    let shown = value["rows"].as_array().unwrap().len();
    assert!(shown > 0 && shown < 4000);
    assert_eq!(value["columns"], serde_json::json!(["id", "name"]));
    assert_eq!(value["row_count"], 4000);
    assert_eq!(value["rows_omitted"], 3995 + 4000 - shown);

}

#[test]
fn ai_tool_registry_opaque_shape_does_not_invent_structure() {
    // 未声明形状的工具按 Opaque 处理：超限时给受控错误，而不是猜字段删内容。
    let raw = serde_json::json!({"payload": "x".repeat(4096)}).to_string();
    let text = project_receipt_with_shape(
        &fluxdb_core::ToolReceipt {
            call_id: "c1".into(),
            tool_id: "file.read".into(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
            result_json: raw,
            error_code: None,
            affected_rows: None,
            started_at_ms: 0,
            finished_at_ms: 1,
        },
        None,
        128,
        &ToolOutputShape::Opaque,
    )
    .text;
    serde_json::from_str::<serde_json::Value>(&text).expect("受控错误必须是合法 JSON");
}

#[test]
fn ai_tool_registry_one_of_matches_source_branches_with_local_refs() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["source"],
        "properties": {
            "source": {
                "oneOf": [
                    {"$ref": "#/$defs/result"},
                    {"$ref": "#/$defs/query"},
                ],
            },
        },
        "$defs": {
            "result": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "result_ref"],
                "properties": {
                    "kind": {"enum": ["result"]},
                    "result_ref": {"type": "string", "minLength": 1},
                },
            },
            "query": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "sql"],
                "properties": {
                    "kind": {"enum": ["query"]},
                    "sql": {"type": "string", "minLength": 1},
                },
            },
        },
    });
    let catalog = ToolCatalog::new(vec![tool_spec("artifact.test", schema)]);
    for source in [
        serde_json::json!({"kind": "result", "result_ref": "result-1"}),
        serde_json::json!({"kind": "query", "sql": "select 1"}),
    ] {
        let arguments = serde_json::json!({"source": source});
        let prepared = catalog
            .prepare("artifact.test", &arguments.to_string())
            .unwrap();
        assert_eq!(prepared.as_value(), &arguments);
    }
    for source in [
        serde_json::json!({"kind": "unknown", "result_ref": "result-1"}),
        serde_json::json!({"kind": "result"}),
        serde_json::json!({"kind": "result", "result_ref": ""}),
        serde_json::json!({"kind": "result", "result_ref": 1}),
        serde_json::json!({"kind": "result", "sql": "select 1"}),
        serde_json::json!({"kind": "query", "sql": "select 1", "result_ref": "result-1"}),
        serde_json::json!(null),
    ] {
        assert_invalid_arguments(
            &catalog,
            "artifact.test",
            &serde_json::json!({"source": source}).to_string(),
        );
    }
}

#[test]
fn ai_tool_registry_one_of_rejects_zero_and_multiple_matches_without_execution() {
    let catalog = ToolCatalog::new(vec![tool_spec(
        "db.query",
        serde_json::json!({
            "type": "object",
            "required": ["value"],
            "properties": {
                "value": {"oneOf": [{"type": "integer"}, {"type": "number"}]},
            },
        }),
    )]);
    let tool = Arc::new(crate::RecordingTool::new(
        "db.query",
        fluxdb_core::ToolRisk::Read,
    ));
    let mut catalog = catalog;
    catalog
        .register(Box::new(SharedRecordingTool(tool.clone())))
        .unwrap();
    assert!(catalog.executable("db.query").is_some());
    let valid = serde_json::json!({"value": 1.5});
    assert_eq!(
        catalog
            .prepare("db.query", &valid.to_string())
            .unwrap()
            .as_value(),
        &valid,
    );
    for value in [
        serde_json::json!(1),
        serde_json::json!(0),
        serde_json::json!("1"),
        serde_json::json!(null),
        serde_json::json!(true),
    ] {
        assert_invalid_arguments(
            &catalog,
            "db.query",
            &serde_json::json!({"value": value}).to_string(),
        );
    }
    assert_eq!(tool.call_count(), 0, "oneOf 校验或失败不得执行工具");
}

#[test]
fn ai_tool_registry_one_of_keeps_sibling_constraints_and_validates_array_items() {
    let catalog = ToolCatalog::new(vec![tool_spec(
        "artifact.test",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["values"],
            "properties": {
                "values": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "oneOf": [{"type": "string"}, {"type": "integer"}],
                        "minLength": 2,
                        "minimum": 0,
                    },
                },
            },
        }),
    )]);
    let valid = serde_json::json!({"values": ["ab", 0, 12]});
    assert_eq!(
        catalog
            .prepare("artifact.test", &valid.to_string())
            .unwrap()
            .as_value(),
        &valid,
    );
    for invalid in [
        serde_json::json!({"values": ["a"]}),
        serde_json::json!({"values": [-1]}),
        serde_json::json!({"values": [1.5]}),
        serde_json::json!({"values": ["ab", null]}),
        serde_json::json!({"values": []}),
        serde_json::json!({"values": ["ab"], "extra": true}),
    ] {
        assert_invalid_arguments(&catalog, "artifact.test", &invalid.to_string());
    }
}

#[test]
fn ai_tool_registry_facts_preserve_object_samples_and_strict_numeric_types() {
    let mut store = ResultStore::new(4096);
    let rows = vec![
        serde_json::json!({
            "number": null, "text_number": "1", "mixed": 1, "all_null": null,
            "mixed_bool": 1, "nested": {"n": 1}, "sparse": null,
        }),
        serde_json::json!({
            "number": -1.5, "text_number": "2", "mixed": "2", "all_null": null,
            "mixed_bool": true, "nested": [2], "sparse": 0,
        }),
        serde_json::json!({"number": 0, "mixed": null, "all_null": null}),
        serde_json::json!([99]),
        serde_json::json!("99"),
    ];
    let (result, _) = store
        .put("db.query", "call-object", &[], 3, rows.clone())
        .unwrap();
    let facts = store.facts(&result.id).unwrap();
    assert_eq!(facts.result, result);
    assert_eq!(facts.rows, rows.len());
    assert!(!facts.truncated);
    assert_eq!(facts.sample, rows);
    assert_eq!(facts.fields, rows[0].as_object().unwrap().keys().cloned().collect::<Vec<_>>());
    assert_eq!(facts.numeric_fields, vec!["number", "sparse"]);
    assert_eq!(
        store.read_page(&result.id, None, 10, 4096).unwrap().items,
        rows
    );

    let (empty, _) = store
        .put("db.query", "call-empty", &["n".into()], 4, Vec::new())
        .unwrap();
    let empty_facts = store.facts(&empty.id).unwrap();
    assert_eq!(empty_facts.rows, 0);
    assert!(!empty_facts.truncated);
    assert!(empty_facts.sample.is_empty());
    assert!(empty_facts.fields.is_empty());
    assert!(empty_facts.numeric_fields.is_empty());
}

#[test]
fn ai_tool_registry_one_of_const_discriminates_real_sql_parameter_types() {
    let catalog = ToolCatalog::new(standard_sql_tool_specs().unwrap());
    for parameter in [
        serde_json::json!({"type":"null"}),
        serde_json::json!({"type":"bool","value":true}),
        serde_json::json!({"type":"i64","value":"12"}),
        serde_json::json!({"type":"text","value":"channel"}),
    ] {
        let args = serde_json::json!({"target":"conn:1/db:main","sql":"SELECT ?","parameters":[parameter]});
        assert!(catalog.prepare("db.query", &args.to_string()).is_ok());
    }
    let invalid = serde_json::json!({"target":"conn:1/db:main","sql":"SELECT ?",
        "parameters":[{"type":"not-supported","value":"12"}]});
    assert!(catalog.prepare("db.query", &invalid.to_string()).is_err());
}

#[test]
fn ai_tool_registry_nested_argument_redaction_keeps_only_public_structure() {
    let args = serde_json::json!({"sql":"SELECT 1","target":"conn:1/db:main",
        "nested":[{"password":"redaction-fixture","api_key":"redaction-fixture",
            "deep":{"token":"redaction-fixture","public":"keep"}}]});
    let redacted = ai_conversation_redact_arguments(&args.to_string());
    assert!(!redacted.contains("redaction-fixture"));
    let value: serde_json::Value = serde_json::from_str(&redacted).unwrap();
    assert_eq!(value["sql"], "SELECT 1");
    assert_eq!(value["nested"][0]["deep"]["public"], "keep");
}
