//! 客户端显式安装/卸载、同名槽位接管与通用 stdio 信息；所有配置均位于隔离临时目录。
use std::{fs, path::PathBuf};

use fluxdb_app::{
    DEFAULT_MCP_PROFILE_ID, McpClientKind, McpRegistration, connect_mcp_client_registration,
    mcp_clients_snapshot, remove_mcp_client_registration, sync_mcp_client_registrations,
};
use fluxdb_storage::FileStorage;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    storage: FileStorage,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("fluxdb-mcp-clients-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let storage = FileStorage::new(root.join("data"));
        storage.load_mcp_client_registrations().unwrap();
        Self {
            root,
            home,
            storage,
        }
    }
    fn registration(&self) -> McpRegistration {
        McpRegistration {
            server_name: "fluxdb".into(),
            binary_path: self.root.join("bin").join("fluxdb-mcp"),
            profile_id: DEFAULT_MCP_PROFILE_ID.into(),
            config_root: Some(fs::canonicalize(self.storage.root_dir()).unwrap()),
        }
    }
    fn claude_path(&self) -> PathBuf {
        self.home.join(".claude.json")
    }
    fn claude(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.claude_path()).unwrap()).unwrap()
    }
    fn connect(&self, client: McpClientKind) {
        connect_mcp_client_registration(&self.storage, &self.home, client, &self.registration())
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn discovery_and_startup_do_not_register_detected_clients() {
    let f = Fixture::new();
    fs::write(f.claude_path(), r#"{"userID":"user"}"#).unwrap();
    let before = fs::read(f.claude_path()).unwrap();
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|c| c.key == "claude").unwrap();
    assert!(claude.config_exists && !claude.registered && !claude.managed);
    assert!(
        sync_mcp_client_registrations(&f.storage, &f.home, &f.registration())
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(f.claude_path()).unwrap(), before);
    assert!(!f.home.join(".codex/config.toml").exists());
    assert!(
        f.storage
            .load_mcp_client_registrations()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn explicit_connect_preserves_other_settings_and_does_not_preapprove() {
    let f = Fixture::new();
    fs::write(
        f.claude_path(),
        r#"{"userID":"user","mcpServers":{"other":{"command":"other"}}}"#,
    )
    .unwrap();
    f.connect(McpClientKind::ClaudeCode);
    let value = f.claude();
    assert_eq!(value["userID"], "user");
    assert_eq!(value["mcpServers"]["other"]["command"], "other");
    assert_eq!(
        value["mcpServers"]["fluxdb"]["args"][3],
        fs::canonicalize(f.storage.root_dir())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(!f.home.join(".claude/settings.json").exists());
    assert!(
        f.claude_path()
            .with_file_name(".claude.json.fluxdb.bak")
            .exists()
    );
    assert!(
        FileStorage::new(f.storage.root_dir())
            .load_mcp_client_registrations()
            .unwrap()
            .contains_key("claude")
    );
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    assert!(
        snapshot
            .clients
            .iter()
            .any(|s| s.key == "claude" && s.managed && s.registered && s.up_to_date)
    );
}

#[test]
fn adding_an_absent_client_is_explicit_and_codex_comments_are_preserved() {
    let f = Fixture::new();
    f.connect(McpClientKind::Codex);
    let path = f.home.join(".codex/config.toml");
    assert!(path.exists());
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("# keep comment\nmodel = \"test\"\n{text}")).unwrap();
    f.connect(McpClientKind::Codex);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep comment") && text.contains("model = \"test\""));
    remove_mcp_client_registration(&f.storage, &f.home, McpClientKind::Codex).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("[mcp_servers.fluxdb]"));
    assert!(text.contains("# keep comment"));
    sync_mcp_client_registrations(&f.storage, &f.home, &f.registration()).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn sync_updates_only_managed_entries_and_preserves_entry_options() {
    let f = Fixture::new();
    f.connect(McpClientKind::ClaudeCode);
    let mut value = f.claude();
    value["mcpServers"]["fluxdb"]["env"] = serde_json::json!({"MY_SETTING":"keep"});
    fs::write(f.claude_path(), serde_json::to_vec(&value).unwrap()).unwrap();
    let mut next = f.registration();
    next.binary_path = f.root.join("new-location").join("fluxdb-mcp");
    let changed = sync_mcp_client_registrations(&f.storage, &f.home, &next).unwrap();
    assert_eq!(changed, ["Claude Code"]);
    assert_eq!(
        f.claude()["mcpServers"]["fluxdb"]["command"],
        next.binary_path.to_str().unwrap()
    );
    assert_eq!(
        f.claude()["mcpServers"]["fluxdb"]["env"]["MY_SETTING"],
        "keep"
    );
    let bytes = fs::read(f.claude_path()).unwrap();
    let backup = fs::read(f.claude_path().with_file_name(".claude.json.fluxdb.bak")).unwrap();
    assert!(
        sync_mcp_client_registrations(&f.storage, &f.home, &next)
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(f.claude_path()).unwrap(), bytes);
    assert_eq!(
        fs::read(f.claude_path().with_file_name(".claude.json.fluxdb.bak")).unwrap(),
        backup
    );
}

#[test]
fn explicit_install_takes_over_the_same_name_slot_and_uninstall_clears_it() {
    let f = Fixture::new();
    let original = r#"{"mcpServers":{"fluxdb":{"command":"another-server","args":[]}}}"#;
    fs::write(f.claude_path(), original).unwrap();
    // 尚未安装：启动维护不接管任何条目。
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|s| s.key == "claude").unwrap();
    assert!(claude.registered && !claude.up_to_date && !claude.managed);
    assert!(
        sync_mcp_client_registrations(&f.storage, &f.home, &f.registration())
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_to_string(f.claude_path()).unwrap(), original);

    // 显式安装接管同名键：写成本应用的启动命令并登记管理权。
    f.connect(McpClientKind::ClaudeCode);
    assert_eq!(
        f.claude()["mcpServers"]["fluxdb"]["command"],
        f.registration().binary_path.to_str().unwrap()
    );
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|s| s.key == "claude").unwrap();
    assert!(claude.registered && claude.up_to_date && claude.managed);

    // 卸载删除同名键并放弃管理权，不影响其它 server。
    fs::write(
        f.claude_path(),
        r#"{"mcpServers":{"other":{"command":"other"},"fluxdb":{"command":"another-server","args":[]}}}"#,
    )
    .unwrap();
    remove_mcp_client_registration(&f.storage, &f.home, McpClientKind::ClaudeCode).unwrap();
    let value = f.claude();
    assert!(value["mcpServers"].get("fluxdb").is_none());
    assert_eq!(value["mcpServers"]["other"]["command"], "other");
    assert!(
        f.storage
            .load_mcp_client_registrations()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn same_name_remote_entry_is_never_rewritten() {
    let f = Fixture::new();
    let original =
        r#"{"mcpServers":{"fluxdb":{"url":"https://example.invalid/mcp","type":"http"}}}"#;
    fs::write(f.claude_path(), original).unwrap();
    assert!(
        connect_mcp_client_registration(
            &f.storage,
            &f.home,
            McpClientKind::ClaudeCode,
            &f.registration()
        )
        .is_err()
    );
    assert!(
        f.storage
            .load_mcp_client_registrations()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_to_string(f.claude_path()).unwrap(), original);
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|s| s.key == "claude").unwrap();
    assert!(!claude.up_to_date && claude.error.is_some());
}

#[test]
fn edited_managed_config_stops_auto_maintenance_until_explicit_install() {
    let f = Fixture::new();
    f.connect(McpClientKind::ClaudeCode);
    let mut value = f.claude();
    value["mcpServers"]["fluxdb"]["command"] = serde_json::json!("user-changed");
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(f.claude_path(), &bytes).unwrap();
    assert!(sync_mcp_client_registrations(&f.storage, &f.home, &f.registration()).is_err());
    assert_eq!(fs::read(f.claude_path()).unwrap(), bytes);
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|s| s.key == "claude").unwrap();
    assert!(claude.registered && claude.managed && !claude.up_to_date);
    assert!(
        claude
            .error
            .as_deref()
            .is_some_and(|e| e.contains("重新安装"))
    );

    // 显式重新安装即恢复管理权，后台维护重新可用。
    f.connect(McpClientKind::ClaudeCode);
    assert_eq!(
        f.claude()["mcpServers"]["fluxdb"]["command"],
        f.registration().binary_path.to_str().unwrap()
    );
    assert!(
        sync_mcp_client_registrations(&f.storage, &f.home, &f.registration())
            .unwrap()
            .is_empty()
    );
    let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
    let claude = snapshot.clients.iter().find(|s| s.key == "claude").unwrap();
    assert!(claude.up_to_date && claude.error.is_none());
}

#[test]
fn removed_client_entry_is_not_recreated_at_startup() {
    let f = Fixture::new();
    f.connect(McpClientKind::ClaudeCode);
    fs::write(f.claude_path(), "{}").unwrap();
    sync_mcp_client_registrations(&f.storage, &f.home, &f.registration()).unwrap();
    assert_eq!(fs::read_to_string(f.claude_path()).unwrap(), "{}");
    remove_mcp_client_registration(&f.storage, &f.home, McpClientKind::ClaudeCode).unwrap();
    assert!(
        f.storage
            .load_mcp_client_registrations()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn malformed_config_and_remote_name_conflicts_fail_closed() {
    for text in [
        "invalid json",
        r#"{"mcpServers":{"fluxdb":{"url":"https://example.invalid/mcp"}}}"#,
        r#"{"mcpServers":[]}"#,
    ] {
        let f = Fixture::new();
        fs::write(f.claude_path(), text).unwrap();
        assert!(
            connect_mcp_client_registration(
                &f.storage,
                &f.home,
                McpClientKind::ClaudeCode,
                &f.registration()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(f.claude_path()).unwrap(), text);
        let snapshot = mcp_clients_snapshot(&f.storage, &f.home, Ok(f.registration())).unwrap();
        assert!(
            snapshot
                .clients
                .iter()
                .find(|s| s.key == "claude")
                .unwrap()
                .error
                .is_some()
        );
        assert!(
            f.storage
                .load_mcp_client_registrations()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn generic_launch_matches_registration_and_quotes_paths() {
    let f = Fixture::new();
    let mut registration = f.registration();
    registration.binary_path = f.root.join("a path's executable").join(if cfg!(windows) {
        "fluxdb-desktop.exe"
    } else {
        "fluxdb-desktop"
    });
    let details = registration.connection_details().unwrap();
    assert_eq!(details.args[0], "--mcp-server");
    assert!(details.args.contains(&"--config-root".into()));
    assert!(details.command_line.contains(if cfg!(windows) {
        "path''s"
    } else {
        "path'\\''s"
    }));
    let json: serde_json::Value = serde_json::from_str(&details.json_example).unwrap();
    connect_mcp_client_registration(
        &f.storage,
        &f.home,
        McpClientKind::ClaudeCode,
        &registration,
    )
    .unwrap();
    assert_eq!(
        json["mcpServers"]["fluxdb"]["command"],
        f.claude()["mcpServers"]["fluxdb"]["command"]
    );
    assert_eq!(
        json["mcpServers"]["fluxdb"]["args"],
        f.claude()["mcpServers"]["fluxdb"]["args"]
    );
    registration.binary_path = PathBuf::from("relative");
    assert!(registration.connection_details().is_err());
}

#[test]
fn removal_cleans_legacy_preapproval_but_keeps_other_permissions() {
    let f = Fixture::new();
    f.connect(McpClientKind::ClaudeCode);
    let path = f.home.join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"permissions":{"allow":["Read","mcp__fluxdb","mcp__other"]}}"#,
    )
    .unwrap();
    remove_mcp_client_registration(&f.storage, &f.home, McpClientKind::ClaudeCode).unwrap();
    let settings: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!(["Read", "mcp__other"])
    );
    assert!(f.claude()["mcpServers"].get("fluxdb").is_none());
}

#[cfg(unix)]
#[test]
fn app_commands_require_explicit_choice_and_keep_configuration_when_disabled() {
    use fluxdb_app::{AppCommand, AppController, AppEvent};
    // 本集成测试只有此用例使用进程 HOME；其余用例全部显式传隔离路径。
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }
    let f = Fixture::new();
    let _restore = RestoreHome(std::env::var_os("HOME"));
    unsafe { std::env::set_var("HOME", &f.home) };
    fs::write(f.claude_path(), "{}").unwrap();
    let mut controller = AppController::new();
    controller.set_ai_storage(f.storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::EnsureMcpControl),
        AppEvent::AiSettingsLoaded
    ));
    let binary_path = f.registration().binary_path.display().to_string();
    assert!(matches!(
        controller.dispatch(AppCommand::SyncMcpClients {
            binary_path: binary_path.clone()
        }),
        AppEvent::AiMcpClientsSynced { .. }
    ));
    assert_eq!(fs::read_to_string(f.claude_path()).unwrap(), "{}");
    let event = controller.dispatch(AppCommand::SetMcpClientRegistration {
        client_key: "claude".into(),
        registered: true,
        binary_path: binary_path.clone(),
    });
    assert!(
        matches!(event, AppEvent::AiMcpClientsSynced { .. }),
        "{event:?}"
    );
    let before = fs::read(f.claude_path()).unwrap();
    // Profile 默认关闭；维护配置不得隐式启用服务或删除接入。
    assert!(!controller.state().ai_mcp_server_profiles[0].enabled);
    assert!(matches!(
        controller.dispatch(AppCommand::SyncMcpClients {
            binary_path: binary_path.clone()
        }),
        AppEvent::AiMcpClientsSynced { .. }
    ));
    assert_eq!(fs::read(f.claude_path()).unwrap(), before);
    let event = controller.dispatch(AppCommand::RefreshMcpClients {
        binary_path: binary_path.clone(),
    });
    assert!(
        matches!(event, AppEvent::AiMcpClientsLoaded(_)),
        "{event:?}"
    );
    assert!(matches!(
        controller.dispatch(AppCommand::SetMcpClientRegistration {
            client_key: "unknown".into(),
            registered: true,
            binary_path: binary_path.clone()
        }),
        AppEvent::AiSettingsError(_)
    ));
    // 即便可执行文件不可用，移除操作仍然可执行。
    assert!(matches!(
        controller.dispatch(AppCommand::SetMcpClientRegistration {
            client_key: "claude".into(),
            registered: false,
            binary_path: String::new()
        }),
        AppEvent::AiMcpClientsSynced { .. }
    ));
    assert!(
        f.storage
            .load_mcp_client_registrations()
            .unwrap()
            .is_empty()
    );
}
