#[cfg(test)]
mod mcp_install_tests {
    use super::*;

    #[test]
    fn desktop_mcp_entry_is_available_without_sidecar() {
        let home = temp_home("desktop-entry");
        for name in ["fluxdb-desktop", "fluxdb-desktop.exe", "fluxdb"] {
            let exe = home.join(name);
            std::fs::write(&exe, b"stub").unwrap();
            assert_eq!(mcp_binary_next_to(&exe), Some(exe.clone()));
            let mut reg = registration(&home);
            reg.binary_path = exe;
            assert_eq!(reg.command_args().1[0], "--mcp-server");
        }
        let sidecar = home.join(if cfg!(windows) {
            "fluxdb-mcp.exe"
        } else {
            "fluxdb-mcp"
        });
        std::fs::write(&sidecar, b"stub").unwrap();
        assert_eq!(
            mcp_binary_next_to(&home.join("fluxdb-desktop")),
            Some(sidecar)
        );
        assert_eq!(registration(&home).command_args().1[0], "--server-profile");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fluxdb-mcp-install-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn registration(home: &Path) -> McpRegistration {
        McpRegistration {
            server_name: "fluxdb".into(),
            binary_path: home.join("bin").join("fluxdb-mcp"),
            profile_id: "local-readonly".into(),
            config_root: Some(home.join("data")),
        }
    }

    #[test]
    fn validate_rejects_relative_paths_and_bad_names() {
        let home = temp_home("validate");
        let mut reg = registration(&home);
        assert!(reg.validate().is_ok());
        reg.binary_path = PathBuf::from("fluxdb-mcp");
        assert!(reg.validate().is_err(), "相对路径必须拒绝");
        reg = registration(&home);
        reg.server_name = "bad name!".into();
        assert!(reg.validate().is_err(), "非法名称必须拒绝");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn codex_register_preserves_existing_content_and_is_idempotent() {
        let home = temp_home("codex");
        let config = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            "model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        let reg = registration(&home);
        let fact = register(McpClientKind::Codex, &home, &reg).unwrap();
        assert!(fact.changed);
        assert!(fact.backup_path.is_some(), "首次写入应保留备份");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("model = \"gpt-5\""), "保留用户既有配置");
        assert!(text.contains("[mcp_servers.other]"), "保留其它 server");
        assert!(text.contains("[mcp_servers.fluxdb]"));
        assert!(text.contains("--server-profile"));
        assert!(text.contains("local-readonly"));
        // 二次注册幂等：内容一致则 changed=false（覆盖为相同内容）。
        let again = register(McpClientKind::Codex, &home, &reg).unwrap();
        assert!(!again.changed, "重复注册相同内容不应重写");
        // 状态查询。
        let status = client_status(McpClientKind::Codex, &home, "fluxdb");
        assert!(status.registered && status.config_exists);
        // 卸载只删自己的条目。
        let removed = unregister(McpClientKind::Codex, &home, "fluxdb").unwrap();
        assert!(removed.changed);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("[mcp_servers.fluxdb]"));
        assert!(text.contains("[mcp_servers.other]"), "不误删其它 server");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn codex_register_rejects_invalid_toml() {
        let home = temp_home("codex-bad");
        let config = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "this is = = not toml\n").unwrap();
        let reg = registration(&home);
        assert!(
            register(McpClientKind::Codex, &home, &reg).is_err(),
            "非法 TOML 必须拒绝改写"
        );
        // 原文件未被破坏。
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "this is = = not toml\n"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn claude_register_roundtrip_and_allow_rule() {
        let home = temp_home("claude");
        std::fs::write(
            home.join(".claude.json"),
            r#"{"userID":"u1","mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        let reg = registration(&home);
        let fact = register(McpClientKind::ClaudeCode, &home, &reg).unwrap();
        assert!(fact.changed);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(value["userID"], serde_json::json!("u1"), "保留其它键");
        assert_eq!(
            value["mcpServers"]["other"]["command"],
            serde_json::json!("x")
        );
        assert_eq!(
            value["mcpServers"]["fluxdb"]["type"],
            serde_json::json!("stdio")
        );
        assert!(
            value["mcpServers"]["fluxdb"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "local-readonly")
        );
        assert_eq!(
            value["enabledMcpServers"]["fluxdb"],
            serde_json::json!(true)
        );

        // 卸载同时清掉 allow 与条目。
        unregister(McpClientKind::ClaudeCode, &home, "fluxdb").unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert!(value["mcpServers"].get("fluxdb").is_none());
        assert!(value["mcpServers"].get("other").is_some());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn claude_register_rejects_invalid_json() {
        let home = temp_home("claude-bad");
        std::fs::write(home.join(".claude.json"), "{ not json").unwrap();
        let reg = registration(&home);
        assert!(register(McpClientKind::ClaudeCode, &home, &reg).is_err());
        assert_eq!(
            std::fs::read_to_string(home.join(".claude.json")).unwrap(),
            "{ not json"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn status_reports_absence_for_missing_config() {
        let home = temp_home("missing");
        let status = client_status(McpClientKind::Codex, &home, "fluxdb");
        assert!(!status.config_exists && !status.registered);
        std::fs::remove_dir_all(&home).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_updates_preserve_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("permissions");
        let reg = registration(&home);
        register(McpClientKind::ClaudeCode, &home, &reg).unwrap();
        let path = home.join(".claude.json");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut next = reg;
        next.binary_path = home.join("new-binary");
        register(McpClientKind::ClaudeCode, &home, &next).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
