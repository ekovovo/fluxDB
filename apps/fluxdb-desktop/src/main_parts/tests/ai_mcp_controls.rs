// 由 ai_ui_tests include，沿用生产 View fixture。
/// 本地 MCP：全局默认策略下拉落库；数据源行按写入策略落库。
#[gpui_kit::test]
fn ai_ui_mcp_control_persists_global_policy_and_datasource(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let root = isolated_root("mcp-control");
    let storage = FileStorage::new(&root);
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller.clone(),
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    cx.simulate_resize(size(px(1600.), px(1800.)));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("ai-tools-tab:本地 MCP", cx);
        window.render_frame(cx);
        // 页签直接呈现三个面板：没有额外的「加载本地 MCP 设置」入口。
        assert!(window.try_find("ai-mcp-profile-enabled").is_some());
        assert!(window.try_find("ai-mcp-datasource-new").is_some());
        assert!(window.try_find("ai-mcp-client-refresh").is_some());
    });
    // 全局默认策略默认只读；切到允许读写。
    click_selector(cx, "ai-mcp-global-policy");
    click_selector(cx, "ai-mcp-global-policy-option:ReadWrite");
    assert_eq!(
        view.read_with(cx, |main, _| main.controller.state().ai_execution_policy.mcp_default),
        fluxdb_core::McpWritePolicy::ReadWrite
    );
    assert_eq!(
        storage
            .with_ai_connection(fluxdb_storage::ai_platform::execution_policy::load)
            .unwrap()
            .0
            .mcp_default,
        fluxdb_core::McpWritePolicy::ReadWrite
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 数据源：真实点击「新增数据源」下拉 → 默认策略落库；策略下拉切到允许读写 → 落库并更新 revision。
#[gpui::test]
fn ai_ui_mcp_datasource_policy_persists(cx: &mut TestAppContext) {
    let root = isolated_root("mcp-datasource");
    let storage = FileStorage::new(root.clone());
    let config = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "数据源测试库".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: root.join("ds.sqlite"),
            read_only: false,
        },
        options: std::collections::BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    storage
        .save_connections(std::slice::from_ref(&config))
        .expect("保存连接");
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::EnsureMcpControl),
        AppEvent::AiSettingsLoaded
    ));
    let profile_id = fluxdb_app::DEFAULT_MCP_PROFILE_ID.to_string();
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let _ = controller.dispatch(AppCommand::ReplaceConnections(vec![config]));
    let (view, cx) = open_main(
        cx,
        controller.clone(),
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    cx.simulate_resize(size(px(1600.), px(1800.)));
    click_selector(cx, "ai-tools-tab:本地 MCP");
    // 真实点击「新增数据源」下拉并选中候选连接。
    click_selector(cx, "ai-mcp-datasource-new");
    click_selector(cx, "ai-mcp-datasource-add:1");
    // 数据源行渲染。
    cx.update(|window, cx| {
        let _ = window.draw(cx).clear(cx);
    });
    assert!(
        cx.debug_bounds("ai-mcp-datasource-policy:1").is_some(),
        "数据源行应渲染"
    );
    assert_eq!(
        view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_mcp_datasources
                .iter()
                .find(|d| d.connection_id == 1)
                .map(|d| d.write_policy.clone())
        }),
        Some("default".to_string())
    );
    // 切到允许读写。
    click_selector(cx, "ai-mcp-datasource-policy:1");
    click_selector(cx, "ai-mcp-datasource-policy-option:1:read_write");
    let stored = storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_datasource(conn, &profile_id, 1)
        })
        .unwrap()
        .expect("数据源存在");
    assert_eq!(
        stored.write_policy,
        fluxdb_storage::ai_platform::mcp_server::DatasourceWritePolicy::ReadWrite
    );
    assert_eq!(stored.revision, 2, "策略更新后 revision 递增");
    let _ = std::fs::remove_dir_all(root);
}

/// 「工具」页签：写入风险阈值与评审权重下拉真实落库，并在 AppState 上同步。
#[gpui::test]
fn ai_ui_write_risk_threshold_and_weight_persist(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let root = isolated_root("write-risk-policy");
    let storage = FileStorage::new(&root);
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    let _ = controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller.clone(),
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    cx.simulate_resize(size(px(1600.), px(1800.)));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("ai-tools-tab:工具", cx);
        window.render_frame(cx);
        assert!(window.try_find("execution-policy-threshold").is_some());
        assert!(window.try_find("execution-policy-weight").is_some());
    });
    // 默认阈值 0.35，切到 0.50。
    click_selector(cx, "execution-policy-threshold");
    click_selector(cx, "execution-policy-threshold-option:0.5");
    assert_eq!(
        view.read_with(cx, |main, _| {
            main.controller.state().ai_execution_policy.write_risk_threshold
        }),
        0.5
    );
    // 评审权重默认 0.5，切到 0.25。
    click_selector(cx, "execution-policy-weight");
    click_selector(cx, "execution-policy-weight-option:0.25");
    let stored = storage
        .with_ai_connection(fluxdb_storage::ai_platform::execution_policy::load)
        .unwrap()
        .0;
    assert_eq!(stored.write_risk_threshold, 0.5);
    assert_eq!(stored.write_review_weight, 0.25);
    let _ = std::fs::remove_dir_all(root);
}
