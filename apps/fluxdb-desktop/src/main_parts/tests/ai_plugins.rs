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
