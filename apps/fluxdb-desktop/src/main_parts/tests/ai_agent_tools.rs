#[gpui_kit::test]
fn ai_ui_agent_tools_exclude_mcp_status_and_preserve_saved_permissions(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    for allowed_tools in [Vec::new(), vec!["operation.status".to_string()]] {
        let root = isolated_root("agent-tool-scope");
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let id = match controller.dispatch(AppCommand::SaveAiAgent(fluxdb_app::AiAgentSaveInput {
            display_name: "Tool scope agent".into(),
            allowed_tools: allowed_tools.clone(),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { id, .. } => id,
            other => panic!("保存测试 Agent 失败: {other:?}"),
        };
        controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(
            cx,
            controller,
            storage.clone(),
            SettingsPanelSection::AiAgents,
        );
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click(SharedString::from(format!("ai-agent-item:{id}")), cx);
        });
        click_selector(cx, "ai-agent-tab:工具与内置能力");
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-agent-tool:connections.list").is_some());
            assert!(window.try_find("ai-agent-tool:operation.status").is_none());
            for id in ["artifact.create", "artifact.add_block", "artifact.finalize"] {
                assert!(
                    window
                        .try_find(SharedString::from(format!("ai-agent-tool:{id}")))
                        .is_some()
                );
            }
        });
        click_selector(cx, "ai-agent-save");
        assert!(view.read_with(cx, |main, _| {
            main.ai_agent_editor.as_ref().unwrap().error.is_none()
        }));
        let saved = storage
            .with_ai_connection(|conn| fluxdb_storage::ai_platform::agents::load_agent(conn, &id))
            .unwrap();
        assert_eq!(saved.allowed_tools, allowed_tools);
        assert_eq!(saved.revision, 2);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[gpui_kit::test]
fn ai_ui_artifact_tools_remain_configurable_when_bi_plugin_is_disabled(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let root = isolated_root("artifact-tool-scope");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    controller.dispatch(AppCommand::RefreshAiSettings);
    controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(
        cx,
        controller,
        storage.clone(),
        SettingsPanelSection::AiToolsMcp,
    );
    click_selector(cx, "ai-tools-tab:内置插件");
    click_selector(cx, "ai-plugin:builtin.bi");
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("ai-plugin:builtin.bi").checked(), Some(false));
    });
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_plugin_instances
            .iter()
            .any(|plugin| plugin.id == "builtin.bi" && !plugin.enabled)
    }));
    click_selector(cx, "ai-tools-tab:工具");
    cx.update(|window, cx| {
        window.render_frame(cx);
        for id in ["artifact.create", "artifact.add_block", "artifact.finalize"] {
            assert_eq!(
                window
                    .find(SharedString::from(format!("ai-tool:{id}")))
                    .checked(),
                Some(true)
            );
        }
        window.scroll(
            "ai-tool:connections.list",
            gpui::ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
    });
    for enabled in [false, true] {
        click_selector(cx, "ai-tool:artifact.create");
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("ai-tool:artifact.create").checked(),
                Some(enabled)
            );
            assert_eq!(
                window.find("ai-tool:artifact.add_block").checked(),
                Some(true)
            );
            assert_eq!(
                window.find("ai-tool:artifact.finalize").checked(),
                Some(true)
            );
        });
        storage
            .with_ai_connection(|conn| {
                let settings =
                    fluxdb_storage::ai_platform::tool_settings::list_tool_settings(conn)?;
                assert!(
                    settings
                        .iter()
                        .any(|setting| setting.tool_id == "artifact.create"
                            && setting.enabled == enabled)
                );
                let plugins = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?;
                assert!(
                    !plugins
                        .iter()
                        .find(|plugin| plugin.id == "builtin.bi")
                        .unwrap()
                        .enabled
                );
                Ok(())
            })
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(root);
}

#[gpui_kit::test]
fn ai_ui_agent_tools_keep_mcp_status_global_switch(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let root = isolated_root("mcp-tool-scope");
    let storage = FileStorage::new(root.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    controller.dispatch(AppCommand::OpenSettings);
    let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiToolsMcp);
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.scroll(
            "ai-tool:connections.list",
            gpui::ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.find("ai-tool:operation.status").visible());
        window.click("ai-tool:operation.status", cx);
    });
    assert!(view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_tool_settings
            .iter()
            .any(|setting| setting.tool_id == "operation.status" && !setting.enabled)
    }));
    let _ = std::fs::remove_dir_all(root);
}
