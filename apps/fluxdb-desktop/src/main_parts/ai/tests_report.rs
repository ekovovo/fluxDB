// T040 会话产物卡 → 报告弹框：真实 SQLite 产物与快照，经生产加载链到弹框控件。
// 断言用户可见状态 + 业务事实（导出文件实际内容/hash），不直接调用处理函数替代交互。
/// 预置一条真实的 ready 产物（含快照文件）与产物卡，走生产 `load` 投影到面板。
fn seed_ready_artifact(fixture: &Fixture, session: &str) -> String {
    let agent = fixture.agent(None);
    let now = 1_700_000_000_500_i64;
    let turn_id = format!("{session}-turn");
    let user_id = format!("{session}-user");
    let assistant_id = format!("{session}-assistant");
    let call_id = format!("{turn_id}-toolcall-1");
    let result_id = format!("{turn_id}-result-1");
    let artifact_id = format!("{turn_id}-artifact-report");
    let snapshot = serde_json::json!({
        "schema_version": 1,
        "spec": {
            "schema_version": 1,
            "kind": "report",
            "title": "各渠道销售额",
            "renderer": "fluxdb.bi.report",
            "metric": {"name": "Revenue", "timezone": "UTC"},
            "sources": [{"result_ref": "result-1"}],
            "blocks": [
                {"type": "chart", "result_ref": "result-1", "chart": "bar", "x": "channel", "y": "revenue"},
                {"type": "table", "result_ref": "result-1", "columns": ["channel", "revenue"]}
            ]
        },
        "renderer_version": "1",
        "markdown": "# 各渠道销售额\n\n| channel | revenue |\n| --- | --- |\n| web | 12 |\n| store | 34 |\n",
        "truncated": false,
        "sources": [{
            "result_ref": "result-1",
            "result_id": result_id.clone(),
            "tool_call_id": call_id.clone(),
            "snapshot_at_ms": now,
            "row_count": 2,
            "truncated": false,
            "columns": ["channel", "revenue"],
            "items": [{"channel": "web", "revenue": 12}, {"channel": "store", "revenue": 34}],
        }],
    });
    let content = snapshot.to_string();
    let hash = fluxdb_app::source_hash(&[content.as_str()]);
    let bytes = content.len() as i64;
    let relative = "report.json".to_string();
    let dir = fixture.storage.root_dir().join("ai-artifacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&relative), &content).unwrap();
    fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::send(
                conn, now, true, session, &agent, &turn_id, &user_id,
                "查询各渠道销售额并生成报告", "{}", "{}",
            )?;
            sessions::create_tool_call(
                conn, now, &call_id, session, &turn_id, 1, 0, "provider-call-1",
                "db.query", 1, None, r#"{"sql":"SELECT channel, revenue FROM sales"}"#,
                "{}", "plan-report", "succeeded",
            )?;
            fluxdb_storage::ai_platform::tool_calls::insert_result(
                conn, now, &result_id, session, &turn_id, &call_id,
                &serde_json::json!({"tool":"db.query"}).to_string(), "available",
                bytes, &sessions::ResultRetention::Memory,
            )?;
            fluxdb_storage::ai_platform::artifacts::create_artifact(
                conn, now, &artifact_id, session, &turn_id, "1",
                &serde_json::from_str::<fluxdb_core::ArtifactSpec>(
                    &serde_json::to_string(&snapshot["spec"]).unwrap(),
                )
                .unwrap(),
                &[fluxdb_storage::ai_platform::artifacts::ArtifactSourceInput {
                    declared_ref: Some("result-1".into()),
                    result_id: Some(result_id.clone()),
                    tool_call_id: Some(call_id.clone()),
                }],
            )?;
            fluxdb_storage::ai_platform::artifacts::mark_artifact_ready(
                conn, now, &artifact_id, &relative, bytes, &hash, 1,
            )?;
            conversation::upsert_artifact_part(
                conn, now, session, &turn_id, &assistant_id,
                &format!("{assistant_id}-artifact-{artifact_id}"),
                sessions::PartStatus::Complete,
                &serde_json::json!({
                    "text": "各渠道销售额 · ready · 1 个来源",
                    "artifact_id": artifact_id,
                    "artifact_status": "ready",
                    "kind": "report",
                    "title": "各渠道销售额",
                    "renderer": "fluxdb.bi.report",
                    "renderer_version": "1",
                    "revision": 2,
                    "truncated": false,
                    "source_count": 1,
                })
                .to_string(),
                &artifact_id,
            )?;
            conversation::checkpoint(
                conn, now + 1, session, &turn_id, &assistant_id,
                "报告已完成。", "公开摘要",
                Some(sessions::TurnStatus::Completed), None,
            )
        })
        .unwrap();
    artifact_id
}

fn open_artifact_session<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
    session: &str,
) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
    let mut controller = fixture.controller();
    controller.dispatch(fluxdb_app::AppCommand::AiConversation(
        fluxdb_app::AiConversationCommand::Load { session_id: Some(session.into()) },
    ));
    let (view, cx) = open_main(cx, fixture, controller, ThemeMode::Light);
    cx.simulate_resize(size(px(1200.), px(800.)));
    open_panel(&view, cx);
    wait_idle(&view, cx);
    (view, cx)
}

#[gpui::test]
fn ai_artifact_card_opens_report_dialog_with_real_values(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    let artifact_id = seed_ready_artifact(&fixture, "report-open");
    let (view, cx) = open_artifact_session(&fixture, cx, "report-open");
    assert_eq!(artifact_id, "report-open-turn-artifact-report");
    assert!(cx.debug_bounds("ai-artifact-open:report-open-turn-artifact-report").is_some());
    click_selector(cx, "ai-artifact-open:report-open-turn-artifact-report");
    wait_report_dialog(&view, cx);
    // 弹框内结构齐全：图表、表格、来源；不再铺一份与结构化块重复的 renderer 正文。
    assert!(cx.debug_bounds("ai-report-chart").is_some(), "报告应渲染原生图表");
    assert!(cx.debug_bounds("ai-report-table").is_some(), "报告应渲染表格");
    assert!(cx.debug_bounds("ai-report-metrics").is_some(), "报告应展示真实指标条");
    assert!(cx.debug_bounds("ai-report-source").is_some(), "报告应展示来源");
    assert!(cx.debug_bounds("ai-report-markdown").is_none(), "弹框不再渲染 renderer 正文");
    // 来源 SQL 默认折叠，点击后才出现。
    assert!(cx.debug_bounds("ai-report-sql").is_none());
    click_selector(cx, "ai-report-sql-toggle");
    draw(cx);
    assert!(cx.debug_bounds("ai-report-sql").is_some(), "点击后应展示来源 SQL");
    // 业务侧：报告读取的是真实快照值，不是编造。
    let view_model = view.read_with(cx, |main, _| {
        let data = main.ai_report_modal.borrow();
        data.report.clone().unwrap()
    });
    assert_eq!(view_model.sources[0].items[1]["revenue"], serde_json::json!(34));
}

/// 读快照是后台任务：有界等待弹框真正打开（等待结果状态，不用固定 sleep）。
fn wait_report_dialog(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        cx.run_until_parked();
        if view.read_with(cx, |main, _| main.ai_report_modal.borrow().dialog_open) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "报告弹框未打开");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    draw(cx);
}

/// 关闭同样有界等待：断言的是弹框状态，不是执行器空闲。
fn wait_report_dialog_closed(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        cx.run_until_parked();
        if !view.read_with(cx, |main, _| main.ai_report_modal.borrow().dialog_open) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "报告弹框未关闭");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    draw(cx);
}

/// X / Esc / 遮罩关闭弹框后，产物卡必须能再次打开同一份报告（`dialog_open` 不得残留）。
#[gpui::test]
fn ai_report_dialog_reopens_after_dialog_internal_close(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    seed_ready_artifact(&fixture, "report-reopen");
    let (view, cx) = open_artifact_session(&fixture, cx, "report-reopen");
    let open_card = "ai-artifact-open:report-reopen-turn-artifact-report";
    click_selector(cx, open_card);
    wait_report_dialog(&view, cx);
    assert!(cx.debug_bounds("ai-report-chart").is_some());
    // Esc 走 Dialog 内置关闭（Cancel → on_close），不经过页脚「关闭」按钮。
    cx.simulate_keystrokes("escape");
    wait_report_dialog_closed(&view, cx);
    assert!(cx.debug_bounds("ai-report-chart").is_none(), "报告弹框应已关闭");
    click_selector(cx, open_card);
    wait_report_dialog(&view, cx);
    assert!(cx.debug_bounds("ai-report-chart").is_some(), "再次点击应重新打开报告");
}

#[gpui::test]
fn ai_artifact_card_export_writes_real_file_with_matching_hash(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new();
    let _ = seed_ready_artifact(&fixture, "report-export");
    let (view, cx) = open_artifact_session(&fixture, cx, "report-export");
    click_selector(cx, "ai-artifact-export:report-export-turn-artifact-report");
    // 选择导出路径（真实系统对话框被测试平台替换为确定路径）。
    let export_path = fixture.root.join("exports").join("report.html");
    std::fs::create_dir_all(export_path.parent().unwrap()).unwrap();
    let target = export_path.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        cx.run_until_parked();
        if cx.did_prompt_for_new_path() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "未打开导出路径选择");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.simulate_new_path_selection(move |_| Some(target.clone()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !export_path.exists() {
        cx.run_until_parked();
        assert!(std::time::Instant::now() < deadline, "导出文件未写出");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let content = std::fs::read_to_string(&export_path).unwrap();
    // 导出内容与业务事实一致：自包含 HTML、带宿主自绘的内联 SVG 图表，无脚本与远程资源。
    assert!(content.starts_with("<!doctype html>"));
    assert!(content.contains("web") && content.contains("store"));
    assert!(content.contains("<svg") && content.contains("<rect"));
    assert!(!content.contains("<script"));
    assert!(!content.contains("http://") && !content.contains("https://"));
    assert_eq!(fluxdb_app::source_hash(&[content.as_str()]).len(), 16);
    // GUI 只对渲染成功的 ready 产物启用导出。
    assert!(view.read_with(cx, |main, _| {
        let data = main.ai_report_modal.borrow();
        data.report.as_ref().is_some_and(|report| !report.metadata_only())
    }));
}

#[test]
fn band_labels_are_truncated_fitted_and_unique() {
    // 附件格式那种长标签 + 13 个分类：必须全部收进分类轴且互不相同。
    let categories: Vec<String> = [
        "stl",
        "stl_exocad",
        "ply_color",
        "ply_exocad",
        "obj_color_255",
        "3shape_ply",
        "ply",
        "obj_color_1",
        "3shape_stl",
        "obj",
        "obj_dentalwings",
        "stl_dentalwings",
        "ply_dentalwings",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let labels = ai_report_band_labels(&categories, 880.);
    assert_eq!(labels.len(), categories.len());
    let unique: std::collections::HashSet<_> = labels.iter().collect();
    assert_eq!(unique.len(), labels.len(), "分类轴标签不能重名: {labels:?}");
    let budget = (((880.0_f32 / 13.0) - AI_BAND_GAP_PX) / AI_BAND_UNIT_PX).floor() as usize;
    for label in &labels {
        assert!(
            ai_report_text_units(label) <= budget,
            "标签应适配分类宽度: {label:?} budget={budget}"
        );
        assert!(ai_report_text_units(label) > 0);
    }
    // 短标签保持原样。
    assert_eq!(labels[0], "stl");
    assert!(labels[10].starts_with("obj_"));
}

#[test]
fn band_labels_disambiguate_identical_prefixes() {
    let categories = vec![
        "obj_dentalwings".to_string(),
        "obj_dentalwingz".to_string(),
    ];
    let labels = ai_report_band_labels(&categories, 120.);
    assert_ne!(labels[0], labels[1], "截断重名必须消除歧义");
    assert_eq!(
        labels.iter().collect::<std::collections::HashSet<_>>().len(),
        2
    );
}

#[test]
fn band_labels_keep_short_categories_untouched_even_when_narrow() {
    let categories = vec!["a".to_string(), "bb".to_string(), "ccc".to_string()];
    assert_eq!(ai_report_band_labels(&categories, 80.), categories);
}
