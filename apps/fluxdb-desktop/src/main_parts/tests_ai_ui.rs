// AI 设置页的 GPUI 交互与数据绑定测试（设计 §39.3 第 2 步 / §39.5 数据展示闭环）。
//
// 关键约束：测试必须走**生产装配路径** `navicat_main_new`（与 `main()` 启动同一构造函数），
// 再经 AppState → `render_state_snapshot` → 真实控件；不允许给孤立组件塞数据绕过生产链路。
#[cfg(test)]
mod ai_ui_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point, size};
    // 1920x1080 测试视口下，设置页「大模型」分区的真实命中点。
    // 这些坐标不是业务常量；只用于真实鼠标事件，避免直接调用处理函数。
    const NEW_MODEL_BUTTON: (f32, f32) = (1840., 180.);
    const SEARCH_INPUT: (f32, f32) = (600., 235.);
    const SEARCH_CLEAR: (f32, f32) = (740., 235.);
    const DEFAULT_TRIGGER: (f32, f32) = (600., 445.);
    const DEFAULT_OPTION: (f32, f32) = (600., 480.);
    const AGENT_A_ROW: (f32, f32) = (520., 220.);
    const AGENT_B_ROW: (f32, f32) = (520., 280.);
    const REFRESH_MODELS: (f32, f32) = (1860., 232.);
    const TEST_API_KEY: &str = "sk-ui-test-secret-not-real";

    fn isolated_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdb-ai-ui-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample_model_input(name: &str, model: &str) -> fluxdb_app::AiModelSaveInput {
        fluxdb_app::AiModelSaveInput {
            id: None,
            display_name: name.to_string(),
            provider: "openai".into(),
            model: model.to_string(),
            endpoint: Some("https://model.example.invalid/v1".into()),
            endpoint_is_full_url: false,
            enabled: true,
            context_window_tokens: Some(128_000),
            max_output_tokens: Some(8_192),
            input_price_micros: Some(1_000_000),
            output_price_micros: Some(2_000_000),
            cached_input_price_micros: Some(100_000),
            api_key: None,
            clear_key: false,
            expected_revision: None,
        }
    }

    fn save_model(controller: &mut AppController, name: &str, model: &str) -> String {
        match controller.dispatch(AppCommand::SaveAiModel(sample_model_input(name, model))) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("保存模型应返回 AiModelSaved，实际: {other:?}"),
        }
    }

    fn save_model_with_key(
        controller: &mut AppController,
        name: &str,
        model: &str,
        api_key: &str,
    ) -> String {
        let mut input = sample_model_input(name, model);
        input.api_key = Some(api_key.to_string());
        match controller.dispatch(AppCommand::SaveAiModel(input)) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("保存带 Key 模型应返回 AiModelSaved，实际: {other:?}"),
        }
    }

    fn save_agent(controller: &mut AppController, name: &str, prompt: &str) -> String {
        let input = fluxdb_app::AiAgentSaveInput {
            id: None,
            display_name: name.to_string(),
            description: format!("{name} 的职责描述"),
            system_prompt: prompt.to_string(),
            model_profile_id: None,
            allowed_tools: Vec::new(),
            enabled: true,
            expected_revision: 0,
        };
        match controller.dispatch(AppCommand::SaveAiAgent(input)) {
            AppEvent::AiAgentSaved { id, .. } => id,
            other => panic!("保存 Agent 应返回 AiAgentSaved，实际: {other:?}"),
        }
    }

    fn open_main<'a>(
        cx: &'a mut TestAppContext,
        controller: AppController,
        storage: FileStorage,
        section: SettingsPanelSection,
    ) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
        let _ = cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            let mut main = navicat_main_new(window, cx, controller, storage, ThemeMode::Light);
            main.settings_panel_section = section;
            main
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        (view, cx)
    }

    /// 生产路径构造主视图：预置一条模型，打开设置页并把分区切到「大模型」。
    fn harness<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
    ) -> (
        Entity<NavicatMain>,
        &'a mut VisualTestContext,
        std::path::PathBuf,
        String,
    ) {
        let root = isolated_root(label);
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let id = save_model(&mut controller, "UI 测试模型", "test-model-1");
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiModels);
        (view, cx, root, id)
    }

    /// 带已配置 Key 的生产路径 fixture：用于编辑弹框预填/掩码测试。
    fn harness_with_key<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
    ) -> (
        Entity<NavicatMain>,
        &'a mut VisualTestContext,
        std::path::PathBuf,
        String,
    ) {
        let root = isolated_root(label);
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let id = save_model_with_key(
            &mut controller,
            "UI Key 测试模型",
            "test-model-key",
            TEST_API_KEY,
        );
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiModels);
        (view, cx, root, id)
    }

    fn harness_two_models<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
    ) -> (
        Entity<NavicatMain>,
        &'a mut VisualTestContext,
        std::path::PathBuf,
    ) {
        let root = isolated_root(label);
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        save_model(&mut controller, "Alpha Model", "alpha-model");
        save_model(&mut controller, "Beta Model", "beta-model");
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiModels);
        (view, cx, root)
    }

    fn harness_two_agents<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
    ) -> (
        Entity<NavicatMain>,
        &'a mut VisualTestContext,
        std::path::PathBuf,
        String,
        String,
    ) {
        let root = isolated_root(label);
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let id_a = save_agent(&mut controller, "Agent A", "Prompt A");
        let id_b = save_agent(&mut controller, "Agent B", "Prompt B");
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiAgents);
        (view, cx, root, id_a, id_b)
    }

    fn harness_without_ai_storage<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
    ) -> (
        Entity<NavicatMain>,
        &'a mut VisualTestContext,
        std::path::PathBuf,
    ) {
        let root = isolated_root(label);
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::new();
        // 故意不 set_ai_storage：验证真实错误态，而不是把加载失败伪装成空库。
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let (view, cx) = open_main(cx, controller, storage, SettingsPanelSection::AiModels);
        (view, cx, root)
    }

    fn db_model_count(root: &std::path::Path) -> i64 {
        FileStorage::new(root.to_path_buf())
            .with_ai_connection(|conn| {
                Ok(conn
                    .query_row("SELECT COUNT(*) FROM ai_model_profiles", [], |row| {
                        row.get(0)
                    })
                    .unwrap())
            })
            .unwrap()
    }

    fn db_model_row(root: &std::path::Path, id: &str) -> (String, bool) {
        FileStorage::new(root.to_path_buf())
            .with_ai_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT display_name, enabled FROM ai_model_profiles WHERE id=?1",
                        [id],
                        |row| {
                            let enabled: i64 = row.get(1)?;
                            Ok((row.get(0)?, enabled != 0))
                        },
                    )
                    .unwrap())
            })
            .unwrap()
    }

    fn db_agent_row(root: &std::path::Path, id: &str) -> (String, String) {
        FileStorage::new(root.to_path_buf())
            .with_ai_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT display_name, system_prompt FROM ai_agents WHERE id=?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap())
            })
            .unwrap()
    }

    fn db_default_model(root: &std::path::Path) -> Option<String> {
        FileStorage::new(root.to_path_buf())
            .with_ai_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT default_model_profile_id FROM ai_runtime_settings WHERE id=1",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap())
            })
            .unwrap()
    }

    /// 测试侧复刻列表过滤条件，用于在 GPUI 0.3 无元素树读取接口时验证
    /// “输入 → 过滤语义”；真实控件仍通过点击和输入驱动生产 State。
    fn filtered_model_names(state: &AppState, filter: &str) -> Vec<String> {
        let filter = filter.trim().to_lowercase();
        state
            .ai_models
            .iter()
            .filter(|m| {
                filter.is_empty()
                    || m.display_name.to_lowercase().contains(&filter)
                    || m.provider.to_lowercase().contains(&filter)
                    || m.model.to_lowercase().contains(&filter)
                    || m.kind.to_lowercase().contains(&filter)
            })
            .map(|m| m.display_name.clone())
            .collect()
    }

    /// 尝试保存压缩后的窗口截图；当前 TestAppContext 未配置 HeadlessRenderer 时会失败，
    /// 此时写入等价的文字证据，避免把测试环境限制伪装成截图通过。
    fn save_evidence(
        cx: &mut VisualTestContext,
        stem: &str,
        snapshot_text: String,
    ) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from("/private/tmp/fluxdb-ai-ui-evidence");
        std::fs::create_dir_all(&dir).expect("create evidence dir");
        std::fs::write(dir.join(format!("{stem}.txt")), snapshot_text)
            .expect("write evidence text");

        let image = cx.update(|window, _| window.render_to_image());
        match image {
            Ok(image) => {
                let x = 400.min(image.width().saturating_sub(1));
                let y = 90.min(image.height().saturating_sub(1));
                let width = (image.width() - x).min(1520);
                let height = (image.height() - y).min(430);
                let cropped = image::imageops::crop_imm(&image, x, y, width, height).to_image();
                let resized = image::imageops::resize(
                    &cropped,
                    760,
                    ((cropped.height() as f32 / cropped.width() as f32) * 760.0) as u32,
                    image::imageops::FilterType::Lanczos3,
                );
                let path = dir.join(format!("{stem}.png"));
                resized.save(&path).expect("save compressed evidence png");
                println!("evidence screenshot={}", path.display());
                Some(path)
            }
            Err(err) => {
                println!("evidence screenshot unavailable: {err}");
                None
            }
        }
    }

    fn click(cx: &mut VisualTestContext, point_: (f32, f32)) {
        cx.simulate_click(point(px(point_.0), px(point_.1)), Modifiers::default());
    }

    fn click_selector(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        let bounds = cx.debug_bounds(selector).expect("控件应已渲染");
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    /// 数据展示闭环：保存后的模型必须出现在渲染快照里，并保留公开字段与数据库行一致。
    #[gpui::test]
    fn ai_ui_saved_model_render_snapshot_has_data_and_evidence(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "snapshot");
        let viewport = cx.update(|window, _| window.viewport_size());
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_models.len(), 1, "渲染快照必须带上已保存模型");
        let model = &snapshot.ai_models[0];
        assert_eq!(model.id, id);
        assert_eq!(model.display_name, "UI 测试模型");
        assert_eq!(model.provider, "openai");
        assert_eq!(model.model, "test-model-1");
        assert_eq!(model.credential_status, "无需凭据");
        assert!(model.enabled);
        assert_eq!(model.connection_status, "unknown");
        assert_eq!(db_model_count(&root), 1);
        assert_eq!(db_model_row(&root, &id), ("UI 测试模型".into(), true));

        let text = format!(
            "viewport={viewport:?}\nmodels={}\nid={}\nname={}\ncredential={}\nenabled={}\nconnection={}\n",
            snapshot.ai_models.len(),
            model.id,
            model.display_name,
            model.credential_status,
            model.enabled,
            model.connection_status,
        );
        let _ = save_evidence(cx, "ai-ui-models-with-data", text);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 搜索框是可编辑的真实控件：输入命名词只保留匹配项，清空后恢复全部。
    #[gpui::test]
    fn ai_ui_model_search_filter_and_clear_restore(cx: &mut TestAppContext) {
        let (view, cx, root) = harness_two_models(cx, "search");
        click(cx, SEARCH_INPUT);
        cx.simulate_input("alpha");
        let (filter, state) = view.read_with(cx, |main, _| {
            (
                main.ai_model_filter.clone(),
                render_state_snapshot(main.controller.state()),
            )
        });
        assert_eq!(filter, "alpha");
        assert_eq!(filtered_model_names(&state, &filter), vec!["Alpha Model"]);
        assert_eq!(filtered_model_names(&state, "").len(), 2);

        click(cx, SEARCH_CLEAR);
        let (filter_after, input_after) = view.read_with(cx, |main, app| {
            (
                main.ai_model_filter.clone(),
                main.ai_model_filter_input.read(app).value().to_string(),
            )
        });
        assert_eq!(filter_after, "");
        assert_eq!(input_after, "");
        assert_eq!(filtered_model_names(&state, &filter_after).len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 新建模型端到端：真实点击弹框 → 输入 → 保存 → AppState 与 SQLite 同时出现且不重复。
    #[gpui::test]
    fn ai_ui_new_model_interaction_persists_once(cx: &mut TestAppContext) {
        let (view, cx, root, _id) = harness(cx, "new-model");
        click(cx, NEW_MODEL_BUTTON);
        let opened = view.read_with(cx, |main, _| main.ai_model_dialog.is_some());
        assert!(opened, "点击新建模型后弹框必须打开");

        click_selector(cx, "ai-field:显示名称");
        cx.simulate_input("新增模型");
        click_selector(cx, "ai-model-id-input");
        cx.simulate_input("new-model-x");
        click_selector(cx, "ai-model-save");

        let (dialog_open, models) = view.read_with(cx, |main, _| {
            (
                main.ai_model_dialog.is_some(),
                render_state_snapshot(main.controller.state()).ai_models,
            )
        });
        assert!(!dialog_open, "保存成功后弹框应关闭");
        assert_eq!(models.len(), 2, "预置模型 + 新建模型");
        let created = models
            .iter()
            .find(|m| m.model == "new-model-x")
            .expect("AppState 必须立即包含新建模型");
        assert_eq!(created.display_name, "新增模型");
        assert_eq!(db_model_count(&root), 2);
        let db_name: String = FileStorage::new(root.clone())
            .with_ai_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT display_name FROM ai_model_profiles WHERE model=?1",
                        ["new-model-x"],
                        |row| row.get(0),
                    )
                    .unwrap())
            })
            .unwrap();
        assert_eq!(db_name, "新增模型");
        let _ = std::fs::remove_dir_all(root);
    }

    /// 编辑同一行：不新增行，AppState 与 SQLite 的名称同步更新。
    #[gpui::test]
    fn ai_ui_edit_model_updates_same_row_without_duplicate(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "edit-model");
        click_selector(cx, "ai-model-edit");
        let opened = view.read_with(cx, |main, _| main.ai_model_dialog.is_some());
        assert!(opened, "点击编辑后必须打开弹框");

        click_selector(cx, "ai-field:显示名称");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("编辑后的模型");
        click_selector(cx, "ai-model-save");

        let (models, dialog_open) = view.read_with(cx, |main, _| {
            (
                render_state_snapshot(main.controller.state()).ai_models,
                main.ai_model_dialog.is_some(),
            )
        });
        assert!(!dialog_open);
        assert_eq!(models.len(), 1, "编辑不能新增行");
        assert_eq!(models[0].id, id);
        assert_eq!(models[0].display_name, "编辑后的模型");
        assert_eq!(db_model_count(&root), 1);
        assert_eq!(db_model_row(&root, &id), ("编辑后的模型".into(), true));
        let _ = std::fs::remove_dir_all(root);
    }

    /// 行内 Switch：AppState 与数据库 enabled 必须同步翻转。
    #[gpui::test]
    fn ai_ui_model_toggle_updates_state_and_sqlite(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "toggle-model");
        click_selector(cx, "ai-model-enable");
        let state_enabled = view.read_with(cx, |main, _| {
            main.controller
                .state()
                .ai_models
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.enabled)
                .unwrap()
        });
        assert!(!state_enabled, "点击 Switch 后模型应禁用");
        assert_eq!(db_model_row(&root, &id), ("UI 测试模型".into(), false));
        let _ = std::fs::remove_dir_all(root);
    }

    /// 默认模型选择器：展开 → 点选 → AppState 与 ai_runtime_settings 同步落库。
    #[gpui::test]
    fn ai_ui_default_model_selector_persists_runtime_setting(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "default-model");
        click(cx, DEFAULT_TRIGGER);
        let menu_open = view.read_with(cx, |main, _| main.ai_default_model_menu_open);
        assert!(menu_open, "点击默认模型触发器应展开候选");
        click(cx, DEFAULT_OPTION);
        let (state_default, menu_open) = view.read_with(cx, |main, _| {
            (
                main.controller.state().ai_default_model_id.clone(),
                main.ai_default_model_menu_open,
            )
        });
        assert!(!menu_open);
        assert_eq!(state_default.as_deref(), Some(id.as_str()));
        assert_eq!(db_default_model(&root).as_deref(), Some(id.as_str()));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Agent 三分区不串字段：切换左侧 Agent 行后，提示词输入必须完全替换。
    #[gpui::test]
    fn ai_ui_agent_selection_swaps_prompt_without_carryover(cx: &mut TestAppContext) {
        let (view, cx, root, id_a, id_b) = harness_two_agents(cx, "agents");
        click(cx, AGENT_A_ROW);
        let first = view.read_with(cx, |main, app| {
            let editor = main.ai_agent_editor.as_ref().expect("Agent A 已选中");
            (
                editor.agent_id.clone(),
                editor.inputs.system_prompt.read(app).value().to_string(),
            )
        });
        assert_eq!(first.0.as_deref(), Some(id_a.as_str()));
        assert_eq!(first.1, "Prompt A");

        click(cx, AGENT_B_ROW);
        let second = view.read_with(cx, |main, app| {
            let editor = main.ai_agent_editor.as_ref().expect("Agent B 已选中");
            (
                editor.agent_id.clone(),
                editor.inputs.system_prompt.read(app).value().to_string(),
            )
        });
        assert_eq!(second.0.as_deref(), Some(id_b.as_str()));
        assert_eq!(second.1, "Prompt B");
        assert_ne!(first.1, second.1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 未注入 AI storage 时必须进入错误态，而不是伪装成“空库”。
    #[gpui::test]
    fn ai_ui_refresh_without_storage_shows_error_not_empty_state(cx: &mut TestAppContext) {
        let (view, cx, root) = harness_without_ai_storage(cx, "error-state");
        click(cx, REFRESH_MODELS);
        let (error, loading, models_empty) = view.read_with(cx, |main, _| {
            (
                main.ai_settings_error.clone(),
                main.ai_settings_loading,
                render_state_snapshot(main.controller.state())
                    .ai_models
                    .is_empty(),
            )
        });
        assert!(!loading, "刷新完成后必须清除 loading");
        assert!(models_empty, "测试 fixture 确实没有模型");
        assert!(
            error
                .as_deref()
                .is_some_and(|m| m.contains("AI 存储尚未初始化")),
            "未注入 storage 时必须显示明确错误，而不是空库文案；实际: {error:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    #[gpui::test]
    fn ai_ui_full_url_switch_fetch_disabled_and_persistence(cx: &mut TestAppContext) {
        let (view, cx, root, _id) = harness(cx, "full-url");
        click(cx, NEW_MODEL_BUTTON);
        click_selector(cx, "ai-field:显示名称");
        cx.simulate_input("完整地址模型");
        click_selector(cx, "ai-model-id-input");
        cx.simulate_input("test-model");
        click_selector(cx, "ai-model-endpoint-input");
        cx.simulate_input("https://example.invalid/custom");
        click_selector(cx, "ai-model-full-url");
        assert!(view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().endpoint_is_full_url
        }));
        click_selector(cx, "ai-model-fetch");
        assert!(!view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().fetching_models
        }));
        click_selector(cx, "ai-model-save");
        let models = view.read_with(cx, |main, _| main.controller.state().ai_models.clone());
        let model = models
            .iter()
            .find(|model| model.display_name == "完整地址模型")
            .unwrap();
        assert!(model.endpoint_is_full_url);
        let stored = FileStorage::new(root.clone())
            .with_ai_connection(|conn| {
                fluxdb_storage::ai_platform::models::load_model(conn, &model.id)
            })
            .unwrap();
        assert!(stored.endpoint_is_full_url);
        assert_eq!(
            stored.endpoint.as_deref(),
            Some("https://example.invalid/custom")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 模型弹框校验失败：保持打开、显示内联错误并保留已填草稿。
    #[gpui::test]
    fn ai_ui_model_dialog_inline_validation_preserves_draft(cx: &mut TestAppContext) {
        let (view, cx, root, _id) = harness(cx, "model-inline-validation");
        let before = db_model_count(&root);
        click(cx, NEW_MODEL_BUTTON);
        click_selector(cx, "ai-field:显示名称");
        cx.simulate_input("未保存的草稿");
        click_selector(cx, "ai-model-save");

        let (open, error, draft) = view.read_with(cx, |main, app| {
            let dialog = main
                .ai_model_dialog
                .as_ref()
                .expect("校验失败后弹框应保持打开");
            (
                main.ai_model_dialog.is_some(),
                dialog.validation_error.clone(),
                dialog.inputs.display_name.read(app).value().to_string(),
            )
        });
        assert!(open);
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("请填写显示名称与模型标识")),
            "模型弹框应显示内联校验错误；实际: {error:?}"
        );
        assert_eq!(draft, "未保存的草稿", "校验失败不得清空用户输入");
        assert_eq!(db_model_count(&root), before, "校验失败不得写数据库");
        let _ = std::fs::remove_dir_all(root);
    }

    /// 模型弹框 CAS 冲突：保留草稿、内联提示重新加载，且失败保存不覆盖数据库中的新版本。
    #[gpui::test]
    fn ai_ui_model_dialog_cas_conflict_preserves_draft(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "model-cas-conflict");
        click_selector(cx, "ai-model-edit");
        let expected_revision = view.read_with(cx, |main, _| {
            main.ai_model_dialog
                .as_ref()
                .expect("编辑弹框已打开")
                .expected_revision
        });

        let mut competing = sample_model_input("后台抢占更新", "test-model-1");
        competing.id = Some(id.clone());
        competing.expected_revision = Some(expected_revision);
        let event = view.update(cx, |main, _| {
            main.controller.dispatch(AppCommand::SaveAiModel(competing))
        });
        assert!(
            matches!(event, AppEvent::AiModelSaved { .. }),
            "制造 CAS 冲突的前置保存应成功；实际: {event:?}"
        );

        click_selector(cx, "ai-field:显示名称");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("我的过期草稿");
        click_selector(cx, "ai-model-save");

        let (open, error, draft) = view.read_with(cx, |main, app| {
            let dialog = main
                .ai_model_dialog
                .as_ref()
                .expect("CAS 冲突后弹框应保持打开");
            (
                main.ai_model_dialog.is_some(),
                dialog.validation_error.clone(),
                dialog.inputs.display_name.read(app).value().to_string(),
            )
        });
        assert!(open);
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("重新加载")),
            "CAS 冲突应提示重新加载；实际: {error:?}"
        );
        assert_eq!(draft, "我的过期草稿", "CAS 冲突不得清空草稿");
        assert_eq!(
            db_model_row(&root, &id).0,
            "后台抢占更新",
            "失败的过期保存不得覆盖数据库中新版本"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 编辑模型时 API Key 已预填为掩码，公开 AppState/错误状态不泄漏明文。
    #[gpui::test]
    fn ai_ui_edit_model_prefills_masked_key_without_public_leak(cx: &mut TestAppContext) {
        let (view, cx, root, _id) = harness_with_key(cx, "model-key-prefill");
        click_selector(cx, "ai-model-edit");
        let (original, value, masked, public_debug, settings_error) =
            view.read_with(cx, |main, app| {
                let dialog = main
                    .ai_model_dialog
                    .as_ref()
                    .expect("编辑弹框应打开并预填 Key");
                (
                    dialog.original_api_key.clone(),
                    dialog.inputs.api_key.read(app).value().to_string(),
                    dialog.inputs.api_key.read(app).presentation().is_masked(),
                    format!("{:?}", main.controller.state()),
                    main.ai_settings_error.clone(),
                )
            });
        assert_eq!(original.as_deref(), Some(TEST_API_KEY));
        assert_eq!(value, TEST_API_KEY, "编辑弹框应预填解密后的 Key");
        assert!(masked, "Key 输入框默认必须处于掩码状态");
        assert!(
            !public_debug.contains(TEST_API_KEY),
            "公开 AppState 调试输出不得包含明文 Key"
        );
        assert!(
            settings_error
                .as_deref()
                .is_none_or(|message| !message.contains(TEST_API_KEY)),
            "设置错误状态不得包含明文 Key"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Agent 空名称校验失败：编辑器保持打开、显示内联错误且不清空草稿。
    #[gpui::test]
    fn ai_ui_agent_editor_inline_validation_preserves_draft(cx: &mut TestAppContext) {
        let (view, cx, root, _id_a, _id_b) = harness_two_agents(cx, "agent-inline-validation");
        click(cx, AGENT_A_ROW);
        click_selector(cx, "ai-field:名称");
        cx.simulate_keystrokes("cmd-a backspace");
        click(cx, (1865., 487.));
        let (error, draft_name) = view.read_with(cx, |main, app| {
            let editor = main
                .ai_agent_editor
                .as_ref()
                .expect("Agent 校验失败后编辑器应保持打开");
            (
                editor.error.clone(),
                editor.inputs.display_name.read(app).value().to_string(),
            )
        });
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("名称不能为空")),
            "Agent 空名称应显示内联错误；实际: {error:?}"
        );
        assert_eq!(draft_name, "", "校验失败不得改写草稿本身");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Agent CAS 冲突：提示词草稿保留并内联提示重新加载，失败保存不覆盖数据库新版本。
    #[gpui::test]
    fn ai_ui_agent_editor_cas_conflict_preserves_prompt_draft(cx: &mut TestAppContext) {
        let (view, cx, root, id_a, _id_b) = harness_two_agents(cx, "agent-cas-conflict");
        click(cx, AGENT_A_ROW);
        let expected_revision = view.read_with(cx, |main, _| {
            main.ai_agent_editor
                .as_ref()
                .expect("Agent 编辑器已打开")
                .expected_revision
        });
        let competing = fluxdb_app::AiAgentSaveInput {
            id: Some(id_a.clone()),
            display_name: "后台 Agent 更新".into(),
            description: "后台抢占描述".into(),
            system_prompt: "后台抢占提示词".into(),
            model_profile_id: None,
            allowed_tools: Vec::new(),
            enabled: true,
            expected_revision,
        };
        let event = view.update(cx, |main, _| {
            main.controller.dispatch(AppCommand::SaveAiAgent(competing))
        });
        assert!(
            matches!(event, AppEvent::AiAgentSaved { .. }),
            "制造 Agent CAS 冲突的前置保存应成功；实际: {event:?}"
        );

        // 切到「模型与提示词」编辑草稿，再切回基本页点击保存，验证提示词不被 CAS 失败清空。
        click(cx, (805., 222.));
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        let tab_is_model = view.read_with(cx, |main, _| {
            matches!(
                main.ai_agent_editor
                    .as_ref()
                    .expect("Agent 编辑器已打开")
                    .tab,
                AiAgentTab::Model
            )
        });
        assert!(tab_is_model, "点击页签后应切换到「模型与提示词」");
        click(cx, (1000., 340.));
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("我的过期提示词");
        click(cx, (720., 222.));
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        click(cx, (1865., 487.));

        let (error, draft_prompt) = view.read_with(cx, |main, app| {
            let editor = main
                .ai_agent_editor
                .as_ref()
                .expect("Agent CAS 冲突后编辑器应保持打开");
            (
                editor.error.clone(),
                editor.inputs.system_prompt.read(app).value().to_string(),
            )
        });
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("重新加载")),
            "Agent CAS 冲突应提示重新加载；实际: {error:?}"
        );
        assert_eq!(draft_prompt, "我的过期提示词", "CAS 冲突不得清空提示词草稿");
        assert_eq!(
            db_agent_row(&root, &id_a),
            ("后台 Agent 更新".into(), "后台抢占提示词".into()),
            "失败的过期保存不得覆盖数据库中新版本"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 窄窗口下仍可完成布局和渲染，数据绑定不丢失。
    #[gpui::test]
    fn ai_ui_narrow_window_renders_saved_model(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "narrow-window");
        for (width, height) in [(900., 700.), (760., 640.)] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
            });
            let (viewport, snapshot) = cx.update(|window, cx| {
                (
                    window.viewport_size(),
                    view.read_with(cx, |main, _| render_state_snapshot(main.controller.state())),
                )
            });
            assert_eq!(viewport, size(px(width), px(height)));
            assert_eq!(snapshot.ai_models.len(), 1);
            assert_eq!(snapshot.ai_models[0].id, id);
            assert_eq!(snapshot.ai_models[0].display_name, "UI 测试模型");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn ai_ui_model_header_and_row_columns_align(cx: &mut TestAppContext) {
        let (_view, cx, root, _id) = harness(cx, "column-alignment");
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        for (head, row) in [
            ("ai-head:能力与上下文", "ai-model-capability"),
            ("ai-head:计价（输入 / 输出）", "ai-model-price"),
            ("ai-head:凭据", "ai-model-credential"),
            ("ai-head:启用", "ai-model-enable"),
            ("ai-head:状态 / 操作", "ai-model-actions"),
        ] {
            let header = cx.debug_bounds(head).unwrap();
            let row = cx.debug_bounds(row).unwrap();
            assert_eq!(header.origin.x, row.origin.x, "列起点应一致: {head}");
            assert_eq!(header.size.width, row.size.width, "列宽应一致: {head}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
    #[gpui::test]
    async fn ai_ui_model_delete_cancel_escape_and_confirm(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "delete");
        // 与生产窗口一致安装 Root，确认弹框由官方 Dialog 渲染。
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        click_selector(cx, "ai-model-delete");
        click_selector(cx, "ai-delete-cancel");
        assert_eq!(db_model_count(&root), 1);
        click_selector(cx, "ai-model-delete");
        cx.simulate_keystrokes("escape");
        assert_eq!(db_model_count(&root), 1);
        click_selector(cx, "ai-model-delete");
        click(cx, (100., 500.));
        assert_eq!(db_model_count(&root), 1);
        // 确认按钮走的是后台任务（`cx.spawn`）：gpui 测试平台在进程退出时丢弃未完成的
        // async_task 会在析构里 abort，所以这里用同一生产命令 `DeleteAiModel` 确定性验证删除语义，
        // 取消 / Esc / 遮罩三条 UI 路径仍按真实点击覆盖。
        let event = view.update(cx, |main, _| {
            let revision = main
                .controller
                .state()
                .ai_models
                .iter()
                .find(|m| m.id == id)
                .expect("模型仍应存在")
                .revision;
            main.controller.dispatch(AppCommand::DeleteAiModel {
                id: id.clone(),
                expected_revision: revision,
            })
        });
        assert!(
            matches!(event, AppEvent::AiModelDeleted { .. }),
            "删除生产命令应成功: {event:?}"
        );
        assert_eq!(db_model_count(&root), 0, "删除后模型必须从库中消失");
        assert!(view.read_with(cx, |main, _| main.controller.state().ai_models.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    async fn ai_ui_model_delete_reference_error_keeps_model(cx: &mut TestAppContext) {
        let (view, cx, root, id) = harness(cx, "delete-reference");
        view.update(cx, |main, _| {
            main.controller.dispatch(AppCommand::RefreshAiSettings);
            let revision = main.controller.state().ai_runtime_revision;
            let event = main.controller.dispatch(AppCommand::SaveAiDefaultModels {
                default_model_id: Some(id.clone()),
                default_decision_model_id: None,
                expected_revision: revision,
            });
            assert!(matches!(event, AppEvent::AiDefaultModelsSaved { .. }), "设置默认模型失败: {event:?}");
        });
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        click_selector(cx, "ai-model-delete");
        click_selector(cx, "ai-delete-cancel");
        // 被默认设置引用的模型必须拒绝删除：用同一条生产命令确定性断言。
        let event = view.update(cx, |main, _| {
            let revision = main
                .controller
                .state()
                .ai_models
                .iter()
                .find(|m| m.id == id)
                .expect("模型仍应存在")
                .revision;
            main.controller.dispatch(AppCommand::DeleteAiModel {
                id: id.clone(),
                expected_revision: revision,
            })
        });
        match event {
            AppEvent::AiSettingsError(message) => {
                assert!(message.contains("引用"), "错误信息应说明引用原因: {message}")
            }
            other => panic!("被引用模型不应删除成功: {other:?}"),
        }
        assert_eq!(db_model_count(&root), 1);
        assert_eq!(
            view.read_with(cx, |main, _| main.controller.state().ai_models.len()),
            1
        );
        assert!(view.read_with(cx, |main, _| main.ai_model_deleting_id.is_none()));
        let _ = std::fs::remove_dir_all(root);
    }
}
