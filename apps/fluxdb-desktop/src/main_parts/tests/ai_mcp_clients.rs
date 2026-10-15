// Windows 的 dirs::home_dir 不读取 HOME，禁止在那里运行此隔离方式。
#[cfg(unix)]
mod mcp_clients {
    use super::*;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};

    struct IsolatedHome {
        previous: Option<std::ffi::OsString>,
        root: std::path::PathBuf,
        home: std::path::PathBuf,
    }

    impl IsolatedHome {
        fn new() -> Self {
            let root = isolated_root("mcp-clients");
            let home = root.join("client home");
            std::fs::create_dir_all(&home).unwrap();
            let guard = Self {
                previous: std::env::var_os("HOME"),
                root,
                home,
            };
            // SAFETY: 此用例必须在独立测试进程中以精确过滤器和 --test-threads=1
            // 运行；在创建 controller 和启动客户端后台操作之前设置，禁止并发运行。
            unsafe { std::env::set_var("HOME", &guard.home) };
            assert_eq!(fluxdb_app::home_dir().unwrap(), guard.home);
            guard
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            // SAFETY: 与设置时使用相同的独立进程约束；panic 展开时也恢复原值。
            unsafe {
                match &self.previous {
                    Some(previous) => std::env::set_var("HOME", previous),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    async fn wait_clients_idle(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        let handle = cx.update(|window, _| window.window_handle());
        cx.wait_for(handle, Duration::from_secs(8), |window, cx| {
            view.read(cx).ai_mcp_clients_busy.is_none()
                && window.try_find("ai-mcp-clients-loading").is_none()
        })
        .await;
    }

    // 只能精确运行本用例，不与其他读取用户目录的测试并行。所有操作使用生产 View
    // 的 controller；不调用回调或自行 dispatch 客户端命令替代真实点击。
    #[gpui_kit::test]
    async fn ai_ui_mcp_clients_explicit_registration_lifecycle(cx: &mut TestAppContext) {
        let _dialog_guard = dialog_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.executor().allow_parking();
        let isolated = IsolatedHome::new();
        let storage = FileStorage::new(isolated.root.join("app data"));
        let claude = fluxdb_app::McpClientKind::ClaudeCode.config_path(&isolated.home);
        let codex = fluxdb_app::McpClientKind::Codex.config_path(&isolated.home);
        let approvals = fluxdb_app::McpClientKind::claude_allow_settings_path(&isolated.home);
        let binary = isolated.root.join("binary path").join("fluxdb-desktop");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"not executed by registration").unwrap();
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
        view.update(cx, |main, _| {
            main.ai_mcp_binary_override = Some(binary.display().to_string());
        });
        cx.simulate_resize(size(px(1600.), px(2400.)));
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
            window.render_frame(cx);
            window.click("ai-tools-tab:本地 MCP", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).ai_mcp_clients_busy.as_deref(),
                Some("refresh")
            );
            assert!(window.try_find("ai-mcp-clients-loading").is_some());
        });
        wait_clients_idle(&view, cx).await;

        // 未检测、未添加的客户端不占行；刷新只是检测，不创建客户端文件。
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client:codex").is_none());
            assert!(window.try_find("ai-mcp-client:claude").is_none());
            assert!(window.try_find("ai-mcp-clients-error").is_none());
            assert!(view.read(cx).ai_mcp_connection_details.is_some());
            assert!(window.try_find("ai-mcp-config-preview").is_none());
            assert!(window.try_find("ai-mcp-connection-command").is_none());
            assert!(window.try_find("ai-mcp-config-details").is_some());
            assert!(
                view.read(cx).ai_mcp_install_status.iter().all(|status| {
                    !status.config_exists && !status.managed && !status.registered
                })
            );
        });
        assert!(!claude.exists() && !codex.exists());
        assert!(storage.load_mcp_client_registrations().unwrap().is_empty());

        let original = r#"{"userID":"ui-test","mcpServers":{"other":{"command":"other"}}}"#;
        std::fs::write(&claude, original).unwrap();
        cx.update(|window, cx| {
            window.click("ai-mcp-client-refresh", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).ai_mcp_clients_busy.as_deref(),
                Some("refresh")
            );
            assert!(window.try_find("ai-mcp-clients-loading").is_some());
            window.click("ai-mcp-client-refresh", cx);
            window.click("ai-mcp-client-add", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).ai_mcp_clients_busy.as_deref(),
                Some("refresh")
            );
            assert!(window.try_find("ai-mcp-client-add:codex").is_none());
        });
        wait_clients_idle(&view, cx).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client:claude").is_some());
            assert!(window.try_find("ai-mcp-client-connect:claude").is_some());
            assert!(window.try_find("ai-mcp-client:codex").is_none());
            let status = view
                .read(cx)
                .ai_mcp_install_status
                .iter()
                .find(|status| status.key == "claude")
                .unwrap();
            assert!(status.config_exists && !status.managed && !status.registered);

            // 服务许可与客户端接入独立：打开开关必须落库，但不注册任何客户端。
            assert_eq!(window.find("ai-mcp-profile-enabled").checked(), Some(false));
            window.click("ai-mcp-profile-enabled", cx);
            window.render_frame(cx);
            assert_eq!(window.find("ai-mcp-profile-enabled").checked(), Some(true));
            assert!(view.read(cx).ai_mcp_clients_busy.is_none());
        });
        let profile = storage
            .with_ai_connection(|conn| {
                fluxdb_storage::ai_platform::mcp_server::load_server_profile(
                    conn,
                    fluxdb_app::DEFAULT_MCP_PROFILE_ID,
                )
            })
            .unwrap()
            .unwrap();
        assert!(profile.enabled);
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), original);
        assert!(!codex.exists() && !approvals.exists());
        assert!(storage.load_mcp_client_registrations().unwrap().is_empty());

        // 显式接入走后台写入与快照回桥；忙时所有入口不得覆盖当前操作。
        cx.update(|window, cx| {
            window.click("ai-mcp-client-connect:claude", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
            assert!(window.try_find("ai-mcp-clients-loading").is_some());
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("busy sentinel".into()));
            window.click("ai-mcp-client-connect:claude", cx);
            window.click("ai-mcp-client-refresh", cx);
            window.click("ai-mcp-client-add", cx);
            window.click("ai-mcp-copy-command", cx);
            window.click("ai-mcp-copy-config", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
            assert!(window.try_find("ai-mcp-client-add:codex").is_none());
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("busy sentinel")
            );
        });
        wait_clients_idle(&view, cx).await;
        let details = view.read_with(cx, |main, _| {
            assert!(main.ai_mcp_clients_error.is_none());
            let status = main
                .ai_mcp_install_status
                .iter()
                .find(|status| status.key == "claude")
                .unwrap();
            assert!(status.managed && status.registered && status.error.is_none());
            assert_eq!(
                main.app_message.as_ref().unwrap().kind,
                AppMessageKind::Success
            );
            main.ai_mcp_connection_details.clone().unwrap()
        });
        let connected = std::fs::read_to_string(&claude).unwrap();
        let value: serde_json::Value = serde_json::from_str(&connected).unwrap();
        assert_eq!(value["userID"], "ui-test");
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            value["mcpServers"]["fluxdb"]["command"],
            binary.display().to_string()
        );
        let expected_args = vec![
            "--mcp-server".to_string(),
            "--server-profile".into(),
            fluxdb_app::DEFAULT_MCP_PROFILE_ID.into(),
            "--config-root".into(),
            std::fs::canonicalize(storage.root_dir())
                .unwrap()
                .display()
                .to_string(),
        ];
        assert_eq!(
            value["mcpServers"]["fluxdb"]["args"],
            serde_json::json!(expected_args)
        );
        assert_eq!(details.command, binary.display().to_string());
        assert_eq!(details.args, expected_args);
        let managed = storage.load_mcp_client_registrations().unwrap();
        assert_eq!(managed.len(), 1);
        assert_eq!(managed["claude"].command, details.command);
        assert_eq!(managed["claude"].args, expected_args);
        assert!(
            !codex.exists() && !approvals.exists(),
            "接入不创建其他客户端或审批规则"
        );

        // 关闭服务只撤销调用许可；配置和管理权均保留，重新启用也不改写配置。
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-remove:claude").is_some());
            window.click("ai-mcp-profile-enabled", cx);
            window.render_frame(cx);
            assert_eq!(window.find("ai-mcp-profile-enabled").checked(), Some(false));
            assert!(window.try_find("ai-mcp-client-remove:claude").is_some());
            assert!(view.read(cx).ai_mcp_clients_busy.is_none());
        });
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), connected);
        assert_eq!(storage.load_mcp_client_registrations().unwrap(), managed);
        assert!(
            !storage
                .with_ai_connection(|conn| {
                    fluxdb_storage::ai_platform::mcp_server::load_server_profile(
                        conn,
                        fluxdb_app::DEFAULT_MCP_PROFILE_ID,
                    )
                })
                .unwrap()
                .unwrap()
                .enabled
        );
        cx.update(|window, cx| {
            window.click("ai-mcp-profile-enabled", cx);
            window.render_frame(cx);
            assert_eq!(window.find("ai-mcp-profile-enabled").checked(), Some(true));
        });
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), connected);

        // 菜单只排除已由本应用接入的客户端，未检测到的 Codex 仍可主动添加。
        cx.update(|window, cx| {
            window.click("ai-mcp-client-add", cx);
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-add:codex").is_some());
            assert!(window.try_find("ai-mcp-client-add:claude").is_none());
            window.press("escape", cx);
        });
        // PopupMenu 的 DismissEvent 在窗口更新结束后回收浮层。
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-add:codex").is_none());
            window.click("ai-mcp-client-add", cx);
            window.render_frame(cx);
            window.click("ai-mcp-client-add:codex", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("codex"));
            window.click("ai-mcp-client-remove:claude", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("codex"));
        });
        wait_clients_idle(&view, cx).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-remove:codex").is_some());
            assert!(window.try_find("ai-mcp-client-remove:claude").is_some());
            assert!(view.read(cx).ai_mcp_clients_error.is_none());
        });
        assert!(codex.exists());
        assert_eq!(storage.load_mcp_client_registrations().unwrap().len(), 2);
        let codex_text = std::fs::read_to_string(&codex).unwrap();
        assert!(codex_text.contains("[mcp_servers.fluxdb]"));

        // 通用命令和 JSON 经真实按钮进入 Headless 剪贴板，不写客户端或审批文件。
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-config-preview").is_none());
            window.click("ai-mcp-config-details", cx);
            window.render_frame(cx);
            assert!(view.read(cx).ai_mcp_details_open);
            assert!(window.try_find("ai-mcp-config-preview").is_some());
            assert!(window.try_find("ai-mcp-connection-command").is_some());
            window.click("ai-mcp-config-details", cx);
            window.render_frame(cx);
            assert!(!view.read(cx).ai_mcp_details_open);
            assert!(window.try_find("ai-mcp-config-preview").is_none());
            assert!(window.try_find("ai-mcp-connection-command").is_none());
            window.click("ai-mcp-copy-command", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(details.command_line.as_str())
            );
            assert_eq!(
                view.read(cx).app_message.as_ref().unwrap().kind,
                AppMessageKind::Success
            );
            window.click("ai-mcp-copy-config", cx);
            let copied = cx.read_from_clipboard().unwrap().text().unwrap();
            assert_eq!(copied, details.json_example);
            let config: serde_json::Value = serde_json::from_str(&copied).unwrap();
            assert_eq!(
                config,
                serde_json::json!({
                    "mcpServers": {
                        "fluxdb": {"command": details.command, "args": expected_args}
                    }
                })
            );
        });
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), connected);
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), codex_text);
        assert!(!approvals.exists());

        // 显式移除保留用户其他配置；检测到的配置文件仍显示为可接入行。
        cx.update(|window, cx| {
            window.click("ai-mcp-client-remove:claude", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
            window.click("ai-mcp-client-remove:claude", cx);
            window.click("ai-mcp-client-remove:codex", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
        });
        wait_clients_idle(&view, cx).await;
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
        assert_eq!(value["userID"], "ui-test");
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert!(value["mcpServers"].get("fluxdb").is_none());
        let managed = storage.load_mcp_client_registrations().unwrap();
        assert_eq!(managed.len(), 1);
        assert!(managed.contains_key("codex"));
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-connect:claude").is_some());
            assert!(window.try_find("ai-mcp-client-remove:claude").is_none());
            assert!(window.try_find("ai-mcp-client-remove:codex").is_some());
            window.click("ai-mcp-client-add", cx);
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-add:claude").is_some());
            assert!(window.try_find("ai-mcp-client-add:codex").is_none());
            window.press("escape", cx);
        });

        // 损坏配置先由刷新显示行内错误，接入失败再显示全局错误；原文不被覆盖。
        std::fs::write(&claude, "invalid json").unwrap();
        cx.update(|window, cx| window.click("ai-mcp-client-refresh", cx));
        wait_clients_idle(&view, cx).await;
        assert!(view.read_with(cx, |main, _| {
            main.ai_mcp_install_status
                .iter()
                .any(|status| status.key == "claude" && status.error.is_some())
        }));
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-connect:claude").is_some());
            window.click("ai-mcp-client-connect:claude", cx);
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-clients-loading").is_some());
            window.click("ai-mcp-client-connect:claude", cx);
            window.click("ai-mcp-client-refresh", cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
        });
        wait_clients_idle(&view, cx).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-clients-error").is_some());
            let main = view.read(cx);
            assert!(
                main.ai_mcp_clients_error
                    .as_deref()
                    .is_some_and(|e| e.contains("JSON"))
            );
            assert_eq!(
                main.app_message.as_ref().unwrap().kind,
                AppMessageKind::Error
            );
        });
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), "invalid json");
        assert_eq!(storage.load_mcp_client_registrations().unwrap(), managed);
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), codex_text);

        // 失败后可重试：修复配置，通过添加菜单重新接入，不残留错误或 busy。
        std::fs::write(&claude, original).unwrap();
        cx.update(|window, cx| {
            window.click("ai-mcp-client-add", cx);
            window.render_frame(cx);
            window.click("ai-mcp-client-add:claude", cx);
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-clients-error").is_none());
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
        });
        wait_clients_idle(&view, cx).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-remove:claude").is_some());
            assert!(window.try_find("ai-mcp-clients-error").is_none());
            assert!(
                view.read(cx)
                    .ai_mcp_install_status
                    .iter()
                    .any(|status| status.key == "claude"
                        && status.managed
                        && status.registered
                        && status.error.is_none())
            );
        });
        assert_eq!(storage.load_mcp_client_registrations().unwrap().len(), 2);
        assert!(!approvals.exists());

        // 手工改过的同名条目停止自动维护：行内不再提供卸载，改为可覆盖的重新安装。
        let mut edited: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
        edited["mcpServers"]["fluxdb"]["command"] = serde_json::json!("/edited/by-hand");
        std::fs::write(&claude, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
        cx.update(|window, cx| window.click("ai-mcp-client-refresh", cx));
        wait_clients_idle(&view, cx).await;
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-connect:claude").is_some());
            assert!(window.try_find("ai-mcp-client-remove:claude").is_none());
            assert!(view.read(cx).ai_mcp_install_status.iter().any(|status| {
                status.key == "claude" && status.registered && !status.up_to_date
                    && status.error.is_some()
            }));
            window.click("ai-mcp-client-connect:claude", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).ai_mcp_clients_busy.as_deref(), Some("claude"));
        });
        wait_clients_idle(&view, cx).await;
        let repaired: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
        assert_eq!(repaired["userID"], "ui-test");
        assert_eq!(repaired["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            repaired["mcpServers"]["fluxdb"]["command"],
            binary.display().to_string()
        );
        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("ai-mcp-client-remove:claude").is_some());
            assert!(view.read(cx).ai_mcp_clients_error.is_none());
        });
        cx.executor().forbid_parking();
    }
}
