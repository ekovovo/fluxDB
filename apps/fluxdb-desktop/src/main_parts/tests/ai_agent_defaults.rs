// 在 main.rs 与 tests_ai_ui.rs 同级 include；不依赖子 Agent A 的 fixture。
// 只走生产加载、实际鼠标/输入、保存命令与状态到控件链路（设计 §39.3–39.5）。
#[cfg(test)]
mod ai_agent_defaults_tests {
    use super::*;
    use fluxdb_storage::ai_platform::agents::{self, BUILTIN_AGENT_ID};
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};

    pub(super) struct Fixture {
        pub(super) storage: FileStorage,
        root: std::path::PathBuf,
    }

    impl Fixture {
        pub(super) fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "fluxdb-ai-ui-b-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("测试时钟")
                    .as_nanos()
            ));
            Self {
                storage: FileStorage::new(root.clone()),
                root,
            }
        }

        pub(super) fn controller(&self) -> AppController {
            let mut controller = AppController::new();
            controller.set_ai_storage(self.storage.clone());
            // 复用 app_boot.rs 的生产启动顺序；仅注入存储不会加载已存 AI 实体。
            let event = controller.dispatch(AppCommand::RefreshAiSettings);
            assert!(
                !matches!(event, AppEvent::AiSettingsError(_)),
                "生产加载 fixture 失败：{event:?}"
            );
            controller
        }

        fn builtin(&self) -> agents::AgentView {
            self.storage
                .with_ai_connection(|conn| agents::load_agent(conn, BUILTIN_AGENT_ID))
                .expect("生产加载应已 seed 内置 Agent")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.root.exists() {
                if let Err(error) = std::fs::remove_dir_all(&self.root) {
                    eprintln!("清理隔离 UI fixture 失败：{}: {error}", self.root.display());
                }
            }
        }
    }

    pub(super) fn open<'a>(
        cx: &'a mut TestAppContext,
        fixture: &Fixture,
        mut controller: AppController,
        section: SettingsPanelSection,
    ) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let storage = fixture.storage.clone();
        let _ = cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            let mut main = navicat_main_new(window, cx, controller, storage, ThemeMode::Light);
            main.settings_panel_section = section;
            main
        });
        // 与生产窗口一致安装 Root，官方 Dialog / Select 浮层由 Root 承载。
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
            let _ = window.draw(cx).clear(cx);
        });
        (view, cx)
    }

    fn restart(
        cx: &mut VisualTestContext,
        fixture: &Fixture,
        section: SettingsPanelSection,
    ) -> Entity<NavicatMain> {
        let mut controller = fixture.controller();
        let _ = controller.dispatch(AppCommand::OpenSettings);
        let storage = fixture.storage.clone();
        cx.update(|window, cx| {
            let view = window.replace_root(cx, |window, cx| {
                let mut main = navicat_main_new(window, cx, controller, storage, ThemeMode::Light);
                main.settings_panel_section = section;
                main
            });
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
            let _ = window.draw(cx).clear(cx);
            view
        })
    }

    fn custom_controller(fixture: &Fixture) -> (AppController, String) {
        let mut controller = fixture.controller();
        let event = controller.dispatch(AppCommand::SaveAiAgent(fluxdb_app::AiAgentSaveInput {
            id: None,
            display_name: "B 自定义 Agent 配置标记".into(),
            description: "B 独立 fixture，无远端模型或数据库".into(),
            system_prompt: "B 自定义提示词标记".into(),
            model_profile_id: None,
            allowed_tools: Vec::new(),
            enabled: true,
            compression_trigger_percent: 80,
            expected_revision: 0,
        }));
        let AppEvent::AiAgentSaved { id, .. } = event else {
            panic!("生产保存 custom fixture 失败：{event:?}");
        };
        (controller, id)
    }

    fn stored_agent(fixture: &Fixture, id: &str) -> agents::AgentView {
        fixture
            .storage
            .with_ai_connection(|conn| agents::load_agent(conn, id))
            .expect("独立读取 Agent 行")
    }

    fn percent(view: &Entity<NavicatMain>, cx: &VisualTestContext) -> String {
        view.read_with(cx, |main, app| {
            main.ai_agent_editor
                .as_ref()
                .expect("Agent 编辑器已打开")
                .inputs
                .compression_percent
                .read(app)
                .value()
                .to_string()
        })
    }

    /// 并行全量测试下 GPUI 固定 3s 的 `condition` 会假超时：
    /// 这里用真实时间预算轮询并持续把测试执行器抽干，等待后台命令完成。
    pub(super) async fn wait_for<T: 'static>(
        view: &gpui::Entity<T>,
        cx: &mut VisualTestContext,
        mut predicate: impl FnMut(&T) -> bool,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if view.read_with(cx, |value, _| predicate(value)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "等待后台命令完成的预算 30s 用尽"
            );
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("生产控件未渲染：{selector}"));
        cx.simulate_click(bounds.center(), Modifiers::default());
    }

    pub(super) fn replace(cx: &mut VisualTestContext, selector: &'static str, text: &str) {
        click(cx, selector);
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input(text);
    }

    fn select_agent(view: &Entity<NavicatMain>, cx: &mut VisualTestContext, id: &str) {
        // 生产列表行已带稳定 debug_selector；不再用硬编码坐标，避免并行/不同数据量下点错行。
        assert!(
            view.read_with(cx, |main, _| main
                .controller
                .state()
                .ai_agents
                .iter()
                .any(|agent| agent.id == id)),
            "AppState 必须包含待点击 Agent：{id}"
        );
        // `debug_bounds` 只接受 `&'static str`；测试里按 id 泄漏一个短字符串即可。
        let selector: &'static str = Box::leak(format!("ai-agent-item:{id}").into_boxed_str());
        // 并行全量测试下“渲染帧里的 bounds”与“真正派发鼠标事件”之间可能错开一帧，
        // 这里对真实控件做有界重试点击，直到生产状态确实选中目标行；仍不使用直接调用处理函数。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
            });
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("生产控件未渲染：{selector}"));
            cx.simulate_click(bounds.center(), Modifiers::default());
            if view.read_with(cx, |main, _| {
                main.ai_agent_editor
                    .as_ref()
                    .and_then(|editor| editor.agent_id.clone())
            }) == Some(id.to_string())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "点击列表行 10s 内未选中目标 Agent：{id}"
            );
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        click(cx, "ai-agent-tab:基本");
    }

    fn select_builtin(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        select_agent(view, cx, BUILTIN_AGENT_ID);
    }

    #[gpui::test]
    fn ai_ui_agent_defaults_seed_loads_into_snapshot_and_editor(cx: &mut TestAppContext) {
        let fixture = Fixture::new("builtin-seed");
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiAgents,
        );
        let stored = fixture.builtin();
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        let builtin = snapshot
            .ai_agents
            .iter()
            .find(|agent| agent.id == BUILTIN_AGENT_ID)
            .expect("真实持久化加载后，渲染快照不得丢失内置 Agent");
        assert_eq!(builtin.display_name, stored.display_name);
        assert_eq!(builtin.system_prompt, stored.system_prompt);
        assert_eq!(builtin.compression_trigger_percent, 80);
        assert_eq!(stored.compression_trigger_percent, 80);
        assert_eq!(
            snapshot.ai_agents.len(),
            1,
            "空库仅自动 seed 一条内置 Agent"
        );

        select_builtin(&view, cx);
        click(cx, "ai-field:名称");
        assert_eq!(
            view.read_with(cx, |main, app| main
                .ai_agent_editor
                .as_ref()
                .unwrap()
                .inputs
                .display_name
                .read(app)
                .value()
                .to_string()),
            stored.display_name,
            "实际编辑控件应展示已加载名称"
        );
        click(cx, "ai-agent-tab:模型与提示词");
        click(cx, "ai-field:上下文压缩触发（%）");
        assert_eq!(percent(&view, cx), "80", "默认值也必须展示在实际表单");
        click(cx, "ai-agent-prompt");
        assert_eq!(
            view.read_with(cx, |main, app| main
                .ai_agent_editor
                .as_ref()
                .unwrap()
                .inputs
                .system_prompt
                .read(app)
                .value()
                .to_string()),
            stored.system_prompt
        );
        println!("seed evidence: id={BUILTIN_AGENT_ID}, snapshot=1, compression=80, editor=loaded");
    }

    #[gpui::test]
    fn ai_ui_agent_defaults_builtin_edit_is_cas_saved_and_not_reseeded(cx: &mut TestAppContext) {
        let fixture = Fixture::new("builtin-edit");
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiAgents,
        );
        let before = fixture.builtin();
        select_builtin(&view, cx);
        replace(cx, "ai-field:名称", "B 内置 Agent 修改标记");
        click(cx, "ai-agent-tab:模型与提示词");
        replace(
            cx,
            "ai-agent-prompt",
            "B 内置提示词修改标记\n保留用户编辑，不得 seed 覆盖。",
        );
        click(cx, "ai-agent-save");

        let stored = fixture.builtin();
        assert_eq!(stored.display_name, "B 内置 Agent 修改标记");
        assert_eq!(
            stored.system_prompt,
            "B 内置提示词修改标记\n保留用户编辑，不得 seed 覆盖。"
        );
        assert_eq!(stored.revision, before.revision + 1);
        assert_eq!(stored.compression_trigger_percent, 80);
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_agents.len(), 1, "编辑不得新增行");
        assert_eq!(snapshot.ai_agents[0].display_name, stored.display_name);
        assert_eq!(snapshot.ai_agents[0].system_prompt, stored.system_prompt);
        assert_eq!(snapshot.ai_agents[0].revision, stored.revision);
        assert_eq!(
            view.read_with(cx, |main, _| main
                .ai_agent_editor
                .as_ref()
                .unwrap()
                .expected_revision),
            stored.revision,
            "保存后编辑器也必须更新 CAS revision"
        );

        // 重建整个生产视图，断言持久化 → 状态 → 快照 → 实际控件的恢复链路。
        let restored_view = restart(cx, &fixture, SettingsPanelSection::AiAgents);
        let restored =
            restored_view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(restored.ai_agents.len(), 1);
        assert_eq!(restored.ai_agents[0].system_prompt, stored.system_prompt);
        assert_eq!(fixture.builtin(), stored, "再次加载不得覆写已编辑内置行");
        select_builtin(&restored_view, cx);
        click(cx, "ai-agent-tab:模型与提示词");
        click(cx, "ai-agent-prompt");
        assert_eq!(
            restored_view.read_with(cx, |main, app| main
                .ai_agent_editor
                .as_ref()
                .unwrap()
                .inputs
                .system_prompt
                .read(app)
                .value()
                .to_string()),
            stored.system_prompt
        );
        println!(
            "edit evidence: id={BUILTIN_AGENT_ID}, revision={} -> {}, count=1",
            before.revision, stored.revision
        );
    }

    #[gpui::test]
    fn ai_ui_agent_defaults_builtin_stale_cas_preserves_ui_prompt(cx: &mut TestAppContext) {
        let fixture = Fixture::new("builtin-cas");
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiAgents,
        );
        select_builtin(&view, cx);
        let before = fixture.builtin();
        let competing = fluxdb_app::AiAgentSaveInput {
            id: Some(before.id.clone()),
            display_name: before.display_name.clone(),
            description: before.description.clone(),
            system_prompt: "B 外部抢占提示词".into(),
            model_profile_id: before.default_model_profile_id.clone(),
            allowed_tools: before.allowed_tools.clone(),
            enabled: before.enabled,
            compression_trigger_percent: 90,
            expected_revision: before.revision,
        };
        // 仅作为 CAS 竞争前置：用独立生产控制器模拟其他编辑者，不替代被测保存点击。
        let event = fixture
            .controller()
            .dispatch(AppCommand::SaveAiAgent(competing));
        assert!(
            matches!(event, AppEvent::AiAgentSaved { .. }),
            "前置保存失败：{event:?}"
        );
        let winner = fixture.builtin();
        click(cx, "ai-agent-tab:模型与提示词");
        replace(cx, "ai-agent-prompt", "B 过期草稿不得丢失");
        replace(cx, "ai-field:上下文压缩触发（%）", "73");
        click(cx, "ai-agent-save");
        let (error, draft) = view.read_with(cx, |main, app| {
            let editor = main.ai_agent_editor.as_ref().expect("冲突后保留编辑器");
            (
                editor.error.clone(),
                editor.inputs.system_prompt.read(app).value().to_string(),
            )
        });
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("重新加载")),
            "冲突应内联反馈：{error:?}"
        );
        assert_eq!(draft, "B 过期草稿不得丢失");
        assert_eq!(percent(&view, cx), "73", "冲突必须保留百分比草稿");
        assert_eq!(winner.compression_trigger_percent, 90);
        assert_eq!(fixture.builtin(), winner, "过期 UI 保存不得改写获胜行");
        println!(
            "CAS evidence: id={BUILTIN_AGENT_ID}, winner_revision={}, draft=retained",
            winner.revision
        );
    }

    /// 只验配置绑定与持久化恢复；不表示压缩运行时已实现或已验收。
    #[gpui::test]
    fn ai_ui_agent_defaults_percent_input_save_switch_and_restart_restore(cx: &mut TestAppContext) {
        let fixture = Fixture::new("percent-save-restore");
        let (controller, id) = custom_controller(&fixture);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiAgents);
        let before = stored_agent(&fixture, &id);
        select_agent(&view, cx, &id);
        click(cx, "ai-agent-tab:模型与提示词");
        assert_eq!(percent(&view, cx), "80");
        replace(cx, "ai-field:上下文压缩触发（%）", "73");
        click(cx, "ai-agent-save");
        let stored = stored_agent(&fixture, &id);
        assert_eq!(stored.compression_trigger_percent, 73);
        assert_eq!(stored.revision, before.revision + 1);
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_agents.len(), 2);
        let agent = snapshot
            .ai_agents
            .iter()
            .find(|agent| agent.id == id)
            .unwrap();
        assert_eq!(agent.compression_trigger_percent, 73);
        assert_eq!(agent.revision, stored.revision);
        assert_eq!(
            view.read_with(cx, |main, _| main
                .ai_agent_editor
                .as_ref()
                .unwrap()
                .expected_revision),
            stored.revision
        );

        select_builtin(&view, cx);
        click(cx, "ai-agent-tab:模型与提示词");
        assert_eq!(percent(&view, cx), "80", "不同 Agent 百分比不得串值");
        select_agent(&view, cx, &id);
        click(cx, "ai-agent-tab:模型与提示词");
        assert_eq!(percent(&view, cx), "73", "保存后切换返回无需手动刷新");
        let restored = restart(cx, &fixture, SettingsPanelSection::AiAgents);
        select_agent(&restored, cx, &id);
        click(cx, "ai-agent-tab:模型与提示词");
        click(cx, "ai-field:上下文压缩触发（%）");
        assert_eq!(percent(&restored, cx), "73");
        assert_eq!(stored_agent(&fixture, &id), stored);
        println!(
            "percent configuration evidence: id={id}, saved=73, restored=73, runtime=not-tested"
        );
    }

    #[gpui::test]
    fn ai_ui_agent_defaults_invalid_percent_preserves_draft_and_persisted_row(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("percent-invalid");
        let (controller, id) = custom_controller(&fixture);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiAgents);
        select_agent(&view, cx, &id);
        click(cx, "ai-agent-tab:模型与提示词");
        let before = stored_agent(&fixture, &id);
        for invalid in ["9", "96", "256", "80.5", "NaN"] {
            replace(cx, "ai-field:上下文压缩触发（%）", invalid);
            click(cx, "ai-agent-save");
            let error = view.read_with(cx, |main, _| {
                main.ai_agent_editor.as_ref().unwrap().error.clone()
            });
            assert!(
                error
                    .as_deref()
                    .is_some_and(|message| message.contains("10") && message.contains("95")),
                "应显示范围校验错误：{error:?}"
            );
            assert_eq!(percent(&view, cx), invalid);
            assert_eq!(
                stored_agent(&fixture, &id),
                before,
                "非法百分比不改写持久化"
            );
            let snapshot =
                view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
            let agent = snapshot
                .ai_agents
                .iter()
                .find(|agent| agent.id == id)
                .unwrap();
            assert_eq!(agent.compression_trigger_percent, 80);
            assert_eq!(agent.revision, before.revision);
        }
        // 两端合法值必须均能经真实保存提交，验证错误态能够恢复。
        for valid in [10, 95] {
            replace(cx, "ai-field:上下文压缩触发（%）", &valid.to_string());
            click(cx, "ai-agent-save");
            assert!(view.read_with(cx, |main, _| {
                main.ai_agent_editor.as_ref().unwrap().error.is_none()
            }));
            assert_eq!(
                stored_agent(&fixture, &id).compression_trigger_percent,
                valid
            );
        }
    }

    #[gpui::test]
    async fn ai_ui_agent_defaults_custom_delete_cancel_escape_overlay_then_confirm(
        cx: &mut TestAppContext,
    ) {
        let _dialog_guard = ai_ui_tests::dialog_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fixture = Fixture::new("custom-delete");
        let (controller, id) = custom_controller(&fixture);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiAgents);
        select_agent(&view, cx, &id);
        let before = stored_agent(&fixture, &id);
        for close in ["cancel", "escape", "overlay"] {
            click(cx, "ai-agent-delete");
            assert!(
                cx.debug_bounds("ai-agent-delete-confirm").is_some(),
                "删除先显示确认框，不能直接删除"
            );
            assert_eq!(stored_agent(&fixture, &id), before);
            match close {
                "cancel" => click(cx, "ai-agent-delete-cancel"),
                "escape" => cx.simulate_keystrokes("escape"),
                "overlay" => cx.simulate_click(point(px(100.), px(500.)), Modifiers::default()),
                _ => unreachable!(),
            }
            // Esc/遮罩关闭要经 dialog 层处理并重绘；并行全量下可能慢一帧，用有界等待代替立即断言。
            let close_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                cx.update(|window, cx| {
                    let _ = window.draw(cx).clear(cx);
                });
                if cx.debug_bounds("ai-agent-delete-confirm").is_none() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < close_deadline,
                    "关闭确认框后 5s 仍残留可执行删除"
                );
                cx.run_until_parked();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                stored_agent(&fixture, &id),
                before,
                "取消/Esc/遮罩不能改写数据"
            );
            assert!(view.read_with(cx, |main, _| {
                main.controller
                    .state()
                    .ai_agents
                    .iter()
                    .any(|agent| agent.id == id)
            }));
        }
        click(cx, "ai-agent-delete");
        click(cx, "ai-agent-delete-confirm");
        // 真实确认按钮启动生产后台删除；不派发命令替代点击。
        // `confirm` 按钮派发的是 GPUI 动作：要等弹框处理动作（`on_ok`）才置 `deleting`。
        // 并行全量测试下这一帧可能还没落地，先画出并抽干，等 `deleting` 真正起来再进入循环，
        // 避免把“动作尚未传播”误判成“后台删除已结束”。
        let armed_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let (removed, deleting) = view.read_with(cx, |main, _| {
                (
                    !main
                        .controller
                        .state()
                        .ai_agents
                        .iter()
                        .any(|agent| agent.id == id),
                    main.ai_agent_editor
                        .as_ref()
                        .is_some_and(|editor| editor.deleting),
                )
            });
            if removed || deleting {
                break;
            }
            assert!(
                std::time::Instant::now() < armed_deadline,
                "确认删除后 5s 仍未进入删除流程"
            );
            cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
            });
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // 若后台删除以错误结束，编辑器会解除 deleting 标记但记录仍存在——立即报出原因，不空等到超时。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let (removed, still_deleting, message) =
                view.read_with(cx, |main, _| {
                    (
                        !main
                            .controller
                            .state()
                            .ai_agents
                            .iter()
                            .any(|agent| agent.id == id),
                        main.ai_agent_editor
                            .as_ref()
                            .is_some_and(|editor| editor.deleting),
                        main.ai_agent_editor
                            .as_ref()
                            .and_then(|editor| editor.error.clone()),
                    )
                });
            if removed {
                break;
            }
            assert!(
                still_deleting,
                "后台删除已结束但记录仍在，错误：{message:?}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "等待后台删除完成超过 30s：{message:?}"
            );
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_agents.len(), 1);
        assert_eq!(snapshot.ai_agents[0].id, BUILTIN_AGENT_ID);
        assert!(
            view.read_with(cx, |main, _| main.ai_agent_editor.is_none()),
            "成功删除后被删对象编辑器应清空"
        );
        let visible = fixture
            .storage
            .with_ai_connection(agents::list_agents)
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, BUILTIN_AGENT_ID);
        // 生产契约为删除可编辑配置并保留历史身份，不要求物理删除历史所引用的行。
        let deleted = stored_agent(&fixture, &id);
        assert!(!deleted.enabled);
        assert_eq!(deleted.revision, before.revision + 1);
        let restored = restart(cx, &fixture, SettingsPanelSection::AiAgents);
        let restored_snapshot =
            restored.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(restored_snapshot.ai_agents.len(), 1);
        assert_eq!(restored_snapshot.ai_agents[0].id, BUILTIN_AGENT_ID);
        select_builtin(&restored, cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("ai-agent-delete").is_none());
        println!(
            "delete UI evidence: id={id}, cancel/escape/overlay=unchanged, confirm=removed-from-state-and-list, restart=not-restored"
        );
    }

    #[gpui::test]
    fn ai_ui_agent_defaults_builtin_has_no_delete_and_disabled_switch_cannot_stop_it(
        cx: &mut TestAppContext,
    ) {
        let _dialog_guard = ai_ui_tests::dialog_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fixture = Fixture::new("builtin-protected-ui");
        let (controller, custom_id) = custom_controller(&fixture);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiAgents);
        // 同一真实页面 custom 有删除入口，切换到内置后必须消失，避免空页产生假阳性。
        select_agent(&view, cx, &custom_id);
        click(cx, "ai-agent-delete");
        click(cx, "ai-agent-delete-cancel");
        select_builtin(&view, cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("ai-agent-delete").is_none(),
            "内置无可执行删除入口"
        );
        assert!(cx.debug_bounds("ai-agent-delete-confirm").is_none());
        // Switch 无生产 debug_selector：根据相邻表单 bounds 和真实布局命中默认 36×20 控件。
        // 同一命中方式先能停用 custom，才能证明内置不变不是点击落空造成的假阳性。
        let toggle = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
            });
            let description = cx
                .debug_bounds("ai-field:职责描述")
                .expect("实际职责描述输入已渲染");
            cx.simulate_click(
                point(
                    description.right() - px(18.),
                    description.bottom() + px(22.),
                ),
                Modifiers::default(),
            );
        };
        select_agent(&view, cx, &custom_id);
        assert!(view.read_with(cx, |main, _| main.ai_agent_editor.as_ref().unwrap().enabled));
        toggle(cx);
        assert!(
            !view.read_with(cx, |main, _| main.ai_agent_editor.as_ref().unwrap().enabled),
            "前置：真实点击确实命中可停用的 custom Switch"
        );
        select_builtin(&view, cx);
        let before = fixture.builtin();
        toggle(cx);
        assert!(
            view.read_with(cx, |main, _| main.ai_agent_editor.as_ref().unwrap().enabled),
            "内置 Switch disabled，不得改变草稿"
        );
        click(cx, "ai-agent-save");
        let stored = fixture.builtin();
        assert!(stored.enabled);
        assert_eq!(stored.revision, before.revision + 1);
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert!(
            snapshot
                .ai_agents
                .iter()
                .find(|agent| agent.id == BUILTIN_AGENT_ID)
                .unwrap()
                .enabled
        );
    }
}
