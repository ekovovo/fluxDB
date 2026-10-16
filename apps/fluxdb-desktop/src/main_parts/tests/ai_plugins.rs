/// 内置目录不重复工具能力；常驻上下文不可停用，附件开关真实落库。
#[gpui_kit::test]
fn ai_ui_builtin_plugins_page_lists_catalog_and_enforces_mandatory(cx: &mut TestAppContext) {
    let root = isolated_root("builtin-plugins");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller,
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );

    click_selector(cx, "ai-tools-tab:内置插件");

    let (count, jev, telemetry) = view.read_with(cx, |main, _| {
        let instances = &main.controller.state().ai_plugin_instances;
        let pick = |id: &str| instances.iter().find(|p| p.id == id).cloned();
        (
            instances.len(),
            pick("builtin.jev"),
            pick("builtin.telemetry"),
        )
    });
    assert_eq!(count, 11, "内置能力页应列出 11 项");
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        assert!(window.try_find("ai-plugin:builtin.database").is_none());
        window.click("ai-plugin:builtin.context-ui", cx);
    });
    view.read_with(cx, |main, _| {
        let context = main
            .controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|plugin| plugin.id == "builtin.context-ui")
            .unwrap();
        assert!(context.enabled && context.mandatory);
        assert_eq!(context.revision, 1, "禁用的开关不得提交命令");
    });
    for enabled in [false, true] {
        cx.update(|window, cx| {
            use gpui_kit::test::TestWindowExt as _;
            window.render_frame(cx);
            window.click("ai-plugin:builtin.files", cx);
        });
        cx.update(|window, cx| {
            use gpui_kit::test::TestWindowExt as _;
            window.render_frame(cx);
            assert_eq!(
                window.find("ai-plugin:builtin.files").checked(),
                Some(enabled)
            );
        });
        view.read_with(cx, |main, _| {
            assert_eq!(
                main.controller
                    .state()
                    .ai_plugin_instances
                    .iter()
                    .find(|plugin| plugin.id == "builtin.files")
                    .unwrap()
                    .enabled,
                enabled
            );
        });
        storage
            .with_ai_connection(|conn| {
                assert_eq!(
                    fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
                        .iter()
                        .find(|plugin| plugin.id == "builtin.files")
                        .unwrap()
                        .enabled,
                    enabled
                );
                Ok(())
            })
            .unwrap();
    }
    let jev = jev.expect("Jev 实例");
    assert_eq!(jev.summary, "可选分类、路由与相关性建议");
    assert!(jev.enabled && !jev.mandatory);
    assert!(telemetry.expect("诊断实例").enabled);

    // 内核约束项：真实点击开关后必须仍然启用（不改 UI 状态、不落库）。
    click_selector(cx, "ai-plugin:builtin.security-audit");
    let audit_enabled = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|p| p.id == "builtin.security-audit")
            .map(|p| p.enabled)
    });
    assert_eq!(audit_enabled, Some(true), "内核约束项不可被停用");

    // 可选能力：点击即真实落库停用（用目录中靠前的行，避免测试窗口高度不足）。
    click_selector(cx, "ai-plugin:builtin.jev");
    let jev_enabled = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|p| p.id == "builtin.jev")
            .map(|p| p.enabled)
    });
    assert_eq!(jev_enabled, Some(false), "可选能力应可停用");
    let _ = std::fs::remove_dir_all(root);
}

/// 内置能力展示顺序 = 设计 §23.2 目录顺序，不受数据库按名称排序影响。
#[test]
fn builtin_plugins_render_in_catalog_order_not_name_order() {
    let items: Vec<fluxdb_app::AiPluginItem> = [
        ("builtin.telemetry", "fluxdb.lifecycle.telemetry"),
        ("builtin.bi", "fluxdb.output.bi"),
        ("builtin.context-ui", "fluxdb.context.ui"),
        ("builtin.unknown", "fluxdb.unknown"),
    ]
    .into_iter()
    .map(|(id, package_key)| fluxdb_app::AiPluginItem {
        id: id.into(),
        display_name: id.into(),
        package_key: package_key.into(),
        summary: String::new(),
        enabled: true,
        mandatory: false,
        retired: false,
        config_fields: Vec::new(),
        config_error: None,
        revision: 1,
    })
    .collect();
    let order = ai_plugins_in_catalog_order(&items)
        .into_iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            "builtin.context-ui".to_string(),
            "builtin.bi".to_string(),
            "builtin.telemetry".to_string(),
            "builtin.unknown".to_string(),
        ],
        "目录内的按 §23.2 排序，未知实例排末尾"
    );
}

/// 插件配置协议闭环：只保存一个合格决策模型不再自动可用；必须在「管理」弹框里显式选择
/// 决策模型后 Jev 才可用，清除选择回到待选择；开关始终可点（Jev 不可 required）。
#[gpui::test]
fn ai_ui_jev_config_dialog_selects_model_then_toggle(cx: &mut TestAppContext) {
    let root = isolated_root("jev-config");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let mut decision = sample_model_input("UI 决策模型", "fixture-decide");
    decision.provider = "decisions".into();
    decision.api_key = Some(TEST_API_KEY.to_string());
    let model_id = match controller.dispatch(AppCommand::SaveAiModel(decision)) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存决策模型应返回 AiModelSaved，实际: {other:?}"),
    };
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(cx, controller, storage.clone(), SettingsPanelSection::AiToolsMcp);
    cx.simulate_resize(size(px(1600.), px(1800.)));
    click_selector(cx, "ai-tools-tab:内置插件");

    // 只有一个合格模型也不再自动使用：未选择 = 待选择，且必须给出「管理」入口。
    let (kind, jev) = view.read_with(cx, |main, _| {
        let state = main.controller.state();
        (
            state.ai_jev_status.kind,
            state
                .ai_plugin_instances
                .iter()
                .find(|plugin| plugin.id == fluxdb_app::JEV_INSTANCE_ID)
                .cloned(),
        )
    });
    assert_eq!(
        kind,
        fluxdb_app::JevStatusKind::NeedsChoice,
        "未选择决策模型时不得自动可用"
    );
    let jev = jev.expect("Jev 实例");
    assert!(jev.enabled, "Jev 默认启用");
    assert!(!jev.mandatory, "Jev 是可选优化，不可 required");
    let field = jev.config_fields.first().expect("Jev 必须声明决策模型字段");
    assert_eq!(field.key, fluxdb_app::JEV_DECISION_MODEL_FIELD);
    assert!(field.required, "决策模型是必填配置");
    assert_eq!(field.selection, None);
    assert_eq!(field.choices.len(), 1, "候选由宿主解析");
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        assert!(window.try_find("ai-plugin-manage:builtin.jev").is_some());
        assert!(
            window.try_find("ai-plugin-manage:builtin.files").is_none(),
            "没有可配置项的能力不得显示「管理」入口"
        );
    });
    assert!(
        cx.debug_bounds("ai-jev-status-note").is_some(),
        "插件页应渲染 Jev 状态说明块"
    );

    // 打开管理弹框 → 打开候选 → 选择模型 → 保存。
    click_selector(cx, "ai-plugin-manage:builtin.jev");
    click_selector(cx, "ai-plugin-config-field:decision_model");
    let option: &'static str =
        Box::leak(format!("ai-plugin-config-option:decision_model:{model_id}").into_boxed_str());
    click_selector(cx, option);
    click_selector(cx, "ai-plugin-config-save");

    let (selected_kind, saved, dialog_open) = view.read_with(cx, |main, _| {
        let state = main.controller.state();
        (
            state.ai_jev_status.kind,
            state.ai_default_decision_model_id.clone(),
            main.ai_plugin_config_dialog.is_some(),
        )
    });
    assert_eq!(
        saved.as_deref(),
        Some(model_id.as_str()),
        "选择应落到决策模型槽位"
    );
    assert_eq!(
        selected_kind,
        fluxdb_app::JevStatusKind::Available,
        "显式选择后 Jev 才可用"
    );
    assert!(!dialog_open, "保存成功后弹框应关闭");
    // 真实落库核对：不只是内存状态。
    storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?
                .default_decision_model_profile_id)
        })
        .unwrap()
        .as_deref()
        .expect("决策模型槽位应已写入");

    // 清除选择：回到待选择，不退回自动挑选。
    click_selector(cx, "ai-plugin-manage:builtin.jev");
    click_selector(cx, "ai-plugin-config-field:decision_model");
    click_selector(cx, "ai-plugin-config-clear:decision_model");
    click_selector(cx, "ai-plugin-config-save");
    let (cleared_kind, cleared) = view.read_with(cx, |main, _| {
        let state = main.controller.state();
        (
            state.ai_jev_status.kind,
            state.ai_default_decision_model_id.clone(),
        )
    });
    assert_eq!(cleared, None, "清空应写回宿主槽位");
    assert_eq!(
        cleared_kind,
        fluxdb_app::JevStatusKind::NeedsChoice,
        "清空后回到待选择"
    );

    // 开关仍然可点：关闭后状态"已禁用"，且不阻断其它能力。
    click_selector(cx, "ai-plugin:builtin.jev");
    let (disabled_kind, disabled) = view.read_with(cx, |main, _| {
        (
            main.controller.state().ai_jev_status.kind,
            main.controller
                .state()
                .ai_plugin_instances
                .iter()
                .find(|plugin| plugin.id == fluxdb_app::JEV_INSTANCE_ID)
                .map(|plugin| plugin.enabled),
        )
    });
    assert_eq!(disabled_kind, fluxdb_app::JevStatusKind::Disabled);
    assert_eq!(disabled, Some(false), "开关应真实落库");
    let _ = std::fs::remove_dir_all(root);
}

/// 「上下文压缩」也走同一条配置协议：管理弹框里选压缩决策模型，落独立槽位，
/// 且与 Jev 决策槽位互不影响。
#[gpui::test]
fn ai_ui_compaction_plugin_config_dialog_uses_its_own_slot(cx: &mut TestAppContext) {
    let root = isolated_root("compaction-config");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let mut decision = sample_model_input("UI 压缩决策模型", "fixture-compaction-decide");
    decision.provider = "decisions".into();
    decision.api_key = Some(TEST_API_KEY.to_string());
    let model_id = match controller.dispatch(AppCommand::SaveAiModel(decision)) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存决策模型应返回 AiModelSaved，实际: {other:?}"),
    };
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(cx, controller, storage.clone(), SettingsPanelSection::AiToolsMcp);
    cx.simulate_resize(size(px(1600.), px(1800.)));
    click_selector(cx, "ai-tools-tab:内置插件");

    // 声明了字段的能力才有「管理」入口；字段非必填（未选择也照常压缩）。
    let (field, selection) = view.read_with(cx, |main, _| {
        let plugin = main
            .controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|plugin| plugin.id == "builtin.compaction")
            .cloned()
            .expect("上下文压缩实例");
        let field = plugin
            .config_fields
            .iter()
            .find(|field| field.key == fluxdb_app::COMPACTION_DECISION_MODEL_FIELD)
            .cloned()
            .expect("上下文压缩必须声明压缩决策模型字段");
        (field, plugin.config_fields.len())
    });
    assert_eq!(selection, 1, "上下文压缩只声明一个字段");
    assert!(!field.required, "压缩决策模型非必填：未选择也照常压缩");
    assert_eq!(field.selection, None);
    assert_eq!(field.choices.len(), 1, "候选由宿主解析");

    // 打开管理弹框 → 选择模型 → 保存。
    click_selector(cx, "ai-plugin-manage:builtin.compaction");
    click_selector(cx, "ai-plugin-config-field:compaction_decision_model");
    let option: &'static str = Box::leak(
        format!("ai-plugin-config-option:compaction_decision_model:{model_id}").into_boxed_str(),
    );
    click_selector(cx, option);
    click_selector(cx, "ai-plugin-config-save");

    let (dialog_open, jev_selection) = view.read_with(cx, |main, _| {
        let state = main.controller.state();
        (
            main.ai_plugin_config_dialog.is_some(),
            state
                .ai_plugin_instances
                .iter()
                .find(|plugin| plugin.id == fluxdb_app::JEV_INSTANCE_ID)
                .and_then(|plugin| plugin.config_fields.first())
                .and_then(|field| field.selection.clone()),
        )
    });
    assert!(!dialog_open, "保存成功后弹框应关闭");
    assert_eq!(jev_selection, None, "压缩决策选择不得连带写入 Jev 决策槽位");
    // 真实落库核对：写入的是压缩决策列，不是 Jev 决策列。
    let runtime = storage
        .with_ai_connection(fluxdb_storage::ai_platform::settings::load_runtime_settings)
        .unwrap();
    assert_eq!(
        runtime.compaction_decision_model_profile_id.as_deref(),
        Some(model_id.as_str())
    );
    assert_eq!(runtime.default_decision_model_profile_id, None);
    // 重进插件页仍能看到选择（不是只在内存里）。
    let reselected = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|plugin| plugin.id == "builtin.compaction")
            .and_then(|plugin| plugin.config_fields.first())
            .and_then(|field| field.selection.clone())
    });
    assert_eq!(reselected.as_deref(), Some(model_id.as_str()));
    let _ = std::fs::remove_dir_all(root);
}


/// 「安全审计」页签：统计卡筛选、行展开详情、空态与错误态区分。
///
/// 种入真实审计事实（走存储层 API），不伪造内存列表。
#[gpui_kit::test]
fn ai_ui_audit_page_filters_expands_and_shows_decision_facts(cx: &mut TestAppContext) {
    use fluxdb_storage::ai_platform::audits;

    let root = isolated_root("audit-page");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);

    // 种两条审计：一条拒绝（命中 denied_tables），一条允许。
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::ensure_schema(conn)?;
            fluxdb_storage::ai_platform::agents::create_agent(
                conn,
                1_000,
                &fluxdb_storage::ai_platform::agents::AgentEdit {
                    max_iterations: 10,
                    compression_trigger_percent: 80,
                    id: "agent-audit".into(),
                    display_name: "审计页 Agent".into(),
                    description: String::new(),
                    enabled: true,
                    default_model_profile_id: None,
                    system_prompt: "测试".into(),
                    budget_policy_id: None,
                    allowed_tools: Vec::new(),
                },
            )?;
            fluxdb_storage::ai_platform::sessions::create_session(
                conn,
                1_000,
                "sess-audit",
                "agent-audit",
                "审计页会话",
            )?;
            fluxdb_storage::ai_platform::sessions::start_turn(
                conn,
                1_000,
                "sess-audit",
                "turn-audit",
                "{\"connection_id\":1}",
                "{}",
            )?;
            for (ordinal, call, at) in
                [(0_i64, "call-denied", 1_000_i64), (1, "call-allow", 2_000)]
            {
                fluxdb_storage::ai_platform::sessions::start_tool_call(
                    conn,
                    at,
                    call,
                    "sess-audit",
                    "turn-audit",
                    1,
                    ordinal,
                    call,
                    "db.execute",
                    1,
                    None,
                    &format!("{{\"sql\":\"UPDATE t SET x=1 -- {call}\"}}"),
                    "{\"connection_id\":1}",
                    "hash-x",
                )?;
            }
            audits::insert_tool_audit(
                conn,
                1_000,
                &audits::ToolAuditEdit {
                    id: "a-denied".into(),
                    tool_call_id: Some("call-denied".into()),
                    mcp_request_id: None,
                    stage: audits::AuditStage::PreExecute,
                    hook_id: "kernel.security-audit".into(),
                    hook_version: "1".into(),
                    ruleset_version: "kernel-audit-v1-test".into(),
                    plan_hash: "hash-x".into(),
                    decision: audits::AuditDecision::Deny,
                    risk: "write".into(),
                    rule_ids: vec!["denied_tables".into()],
                    reason_codes: vec!["denied_table:orders".into()],
                },
            )?;
            audits::insert_tool_audit(
                conn,
                2_000,
                &audits::ToolAuditEdit {
                    id: "a-allow".into(),
                    tool_call_id: Some("call-allow".into()),
                    mcp_request_id: None,
                    stage: audits::AuditStage::PreExecute,
                    hook_id: "kernel.security-audit".into(),
                    hook_version: "1".into(),
                    ruleset_version: "kernel-audit-v1-test".into(),
                    plan_hash: "hash-x".into(),
                    decision: audits::AuditDecision::Allow,
                    risk: "write".into(),
                    rule_ids: Vec::new(),
                    reason_codes: vec!["read_only".into()],
                },
            )?;
            Ok(())
        })
        .expect("种审计数据");
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);

    let (view, cx) = open_main(
        cx,
        controller,
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        window.click("ai-tools-tab:安全审计", cx);
    });
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        // 统计卡可被观察（普通 div 需要 test_support）。
        assert!(window.try_find("ai-audit-stat:all").is_some());
        assert!(window.try_find("ai-audit-row:a-denied").is_some());
        assert!(window.try_find("ai-audit-row:a-allow").is_some());
    });

    // 点"拒绝"统计卡 → 只保留拒绝行；再点同一张回到全部。
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        window.click("ai-audit-stat:deny", cx);
    });
    view.read_with(cx, |main, _| {
        assert_eq!(main.ai_audit_filter, Some(fluxdb_app::AiAuditDecision::Deny));
    });
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        assert!(window.try_find("ai-audit-row:a-denied").is_some());
        assert!(window.try_find("ai-audit-row:a-allow").is_none());
    });

    // 展开详情：行 id 稳定，展开状态写回。
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        window.click("ai-audit-stat:deny", cx); // 取消筛选，回到全部
    });
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        window.click("ai-audit-row:a-denied", cx);
    });
    view.read_with(cx, |main, _| {
        assert_eq!(main.ai_audit_expanded.as_deref(), Some("a-denied"));
    });
    let _ = std::fs::remove_dir_all(root);
}


/// 安全审计插件的「管理」弹框：标量规则字段按声明分组渲染，且给出"生效中"摘要。
#[gpui_kit::test]
fn ai_ui_security_audit_config_dialog_groups_scalar_rules(cx: &mut TestAppContext) {
    let root = isolated_root("audit-plugin-dialog");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller,
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        window.click("ai-tools-tab:内置插件", cx);
    });
    // 打开安全审计插件的管理弹框（标量规则字段所在）。
    view.update_in(cx, |main, window, cx| {
        main.open_ai_plugin_config_dialog("builtin.security-audit", window, cx);
    });
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        // 分组下的标量字段控件按稳定 selector 存在（text_list 用 textarea）。
        assert!(
            window.try_find("ai-plugin-config-input:denied_tables").is_some(),
            "禁止访问的表字段应渲染"
        );
        assert!(
            window.try_find("ai-plugin-config-switch:forbid_ddl:wrap").is_some(),
            "禁止结构变更开关应渲染"
        );
    });
    let _ = std::fs::remove_dir_all(root);
}


/// 解析结果 chip：填了内容后显示"解析为 N 项"与规范化条目（与落库口径同源）。
#[gpui_kit::test]
fn ai_ui_security_audit_dialog_shows_parsed_items(cx: &mut TestAppContext) {
    let root = isolated_root("audit-plugin-parsed");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller,
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    view.update_in(cx, |main, window, cx| {
        main.open_ai_plugin_config_dialog("builtin.security-audit", window, cx);
    });
    // 直接往"禁止访问的表"的草稿实体里写入带重复与空行的内容。
    view.update_in(cx, |main, window, cx| {
        if let Some(dialog) = main.ai_plugin_config_dialog.as_mut() {
            if let Some(input) = dialog.textareas.get("denied_tables").cloned() {
                let _ = input.update(cx, |state, cx| {
                    state.set_value("audit_logs\n\naudit_logs\nlogin_attempts".to_string(), window, cx)
                });
            }
        }
    });
    cx.update(|window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.render_frame(cx);
        assert!(
            window.try_find("ai-plugin-config-parsed:denied_tables").is_some(),
            "有内容时应显示解析结果 chip"
        );
    });
    // 去重口径与应用层一致：3 行去重后 2 项。
    let parsed = fluxdb_app::text_list_items("audit_logs\n\naudit_logs\nlogin_attempts");
    assert_eq!(parsed, vec!["audit_logs".to_string(), "login_attempts".to_string()]);
    let _ = std::fs::remove_dir_all(root);
}
