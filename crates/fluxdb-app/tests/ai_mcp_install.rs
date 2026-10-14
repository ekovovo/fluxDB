//! 本地 MCP「一键注册到 CLI 客户端」应用链路（T060 派生能力）。
//!
//! 用隔离 HOME（`dirs::home_dir` 在 Unix 读 `$HOME`）驱动 AppController 的
//! InstallMcpClient / UninstallMcpClient，核对真实写入到 `~/.codex/config.toml` 与
//! `~/.claude.json`，以及其它配置键/其它 server 不被破坏。不触网、不读生产配置。
use std::{fs, path::PathBuf, sync::Mutex};

use fluxdb_app::{AppCommand, AppController, AppEvent, DEFAULT_MCP_PROFILE_ID};
use fluxdb_storage::FileStorage;

/// 本文件的所有用例都会改 `HOME`，用进程内互斥串行，避免并行时互相污染。
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// 持有 HOME 互斥并返回守卫（调用方在守卫存活期内设置并使用隔离 HOME）。
fn lock_home() -> std::sync::MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

struct Fixture {
    home: PathBuf,
    storage_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "fluxdb-mcp-install-e2e-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let home = base.join("home");
        let storage_dir = base.join("data");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&storage_dir).unwrap();
        // dirs::home_dir 在 Unix 读 HOME；本测试进程独占该变量。
        // SAFETY: 测试串行（--test-threads=1 时）；这里仅在写入前设置一次。
        unsafe { std::env::set_var("HOME", &home) };
        Self { home, storage_dir }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.storage_dir));
        // 建库/建表。
        FileStorage::new(&self.storage_dir)
            .with_ai_connection(|_| Ok(()))
            .unwrap();
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let base = self.home.parent().unwrap().to_path_buf();
        let _ = fs::remove_dir_all(base);
    }
}

fn binary_path(fixture: &Fixture) -> String {
    let path = fixture.home.join("bin").join("fluxdb-mcp");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"stub").unwrap();
    path.display().to_string()
}

#[test]
fn install_writes_codex_and_claude_configs_without_touching_others() {
    let _home_guard = lock_home();
    let fixture = Fixture::new("both");
    // 预置：Codex 已有其它 server，Claude 有其它键。
    let codex_dir = fixture.home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "model = \"gpt-5\"\n\n[mcp_servers.existing]\ncommand = \"x\"\n",
    )
    .unwrap();
    fs::write(
        fixture.home.join(".claude.json"),
        r#"{"userID":"u1","mcpServers":{"other":{"command":"y"}}}"#,
    )
    .unwrap();

    let mut controller = fixture.controller();
    let binary = binary_path(&fixture);

    for client in ["codex", "claude"] {
        let event = controller.dispatch(AppCommand::InstallMcpClient {
            client: client.into(),
            profile_id: DEFAULT_MCP_PROFILE_ID.into(),
            binary_path: binary.clone(),
            config_root: None,
            preapprove_claude_tools: true,
        });
        assert!(
            matches!(
                event,
                AppEvent::AiMcpClientRegistration {
                    installed: true,
                    ..
                }
            ),
            "注册 {client} 应成功: {event:?}"
        );
    }

    // Codex：保留既有内容，新增 fluxdb 条目。
    let codex = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(codex.contains("model = \"gpt-5\""));
    assert!(codex.contains("[mcp_servers.existing]"));
    assert!(codex.contains("[mcp_servers.fluxdb]"));
    assert!(codex.contains(DEFAULT_MCP_PROFILE_ID));

    // Claude：保留 userID 与其它 server，新增 fluxdb + enabled。
    let claude: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.home.join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(claude["userID"], serde_json::json!("u1"));
    assert_eq!(
        claude["mcpServers"]["other"]["command"],
        serde_json::json!("y")
    );
    assert_eq!(
        claude["mcpServers"]["fluxdb"]["type"],
        serde_json::json!("stdio")
    );
    assert_eq!(
        claude["enabledMcpServers"]["fluxdb"],
        serde_json::json!(true)
    );

    // 自动创建的默认只读 Profile 存在且禁用写。
    let storage = FileStorage::new(&fixture.storage_dir);
    let profile = storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::mcp_server::load_server_profile(
                conn,
                DEFAULT_MCP_PROFILE_ID,
            )
        })
        .unwrap()
        .expect("默认 Profile 应被创建");
    assert!(profile.enabled);
    assert_eq!(
        profile.write_mode,
        fluxdb_storage::ai_platform::mcp_server::WriteMode::Disabled
    );

    // 卸载：只删 fluxdb，保留其它。
    let event = controller.dispatch(AppCommand::UninstallMcpClient {
        client: "codex".into(),
    });
    assert!(matches!(
        event,
        AppEvent::AiMcpClientRegistration {
            installed: false,
            ..
        }
    ));
    let codex = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(!codex.contains("[mcp_servers.fluxdb]"));
    assert!(codex.contains("[mcp_servers.existing]"));
    assert!(codex.contains("model = \"gpt-5\""));
}

#[test]
fn install_is_idempotent_and_reports_status() {
    let _home_guard = lock_home();
    let fixture = Fixture::new("idem");
    let mut controller = fixture.controller();
    let binary = binary_path(&fixture);
    let install = || AppCommand::InstallMcpClient {
        client: "codex".into(),
        profile_id: DEFAULT_MCP_PROFILE_ID.into(),
        binary_path: binary.clone(),
        config_root: None,
        preapprove_claude_tools: false,
    };
    assert!(matches!(
        controller.dispatch(install()),
        AppEvent::AiMcpClientRegistration {
            installed: true,
            ..
        }
    ));
    // 重复注册仍成功且内容稳定。
    assert!(matches!(
        controller.dispatch(install()),
        AppEvent::AiMcpClientRegistration {
            installed: true,
            ..
        }
    ));
    let statuses = fluxdb_app::install_client_statuses(&fixture.home);
    let codex = statuses.iter().find(|s| s.key == "codex").unwrap();
    assert!(codex.registered && codex.config_exists);
}

#[test]
fn install_rejects_unknown_client_and_missing_binary() {
    let _home_guard = lock_home();
    let fixture = Fixture::new("reject");
    let mut controller = fixture.controller();
    let event = controller.dispatch(AppCommand::InstallMcpClient {
        client: "vscode".into(),
        profile_id: DEFAULT_MCP_PROFILE_ID.into(),
        binary_path: "/abs/fluxdb-mcp".into(),
        config_root: None,
        preapprove_claude_tools: false,
    });
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "未知客户端应报错"
    );

    // 相对路径二进制被拒（validate）。
    let event = controller.dispatch(AppCommand::InstallMcpClient {
        client: "codex".into(),
        profile_id: DEFAULT_MCP_PROFILE_ID.into(),
        binary_path: "fluxdb-mcp".into(),
        config_root: None,
        preapprove_claude_tools: false,
    });
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "相对路径应被拒"
    );
}

#[test]
fn startup_auto_registers_only_clients_with_existing_config() {
    let _home_guard = lock_home();
    let fixture = Fixture::new("auto");
    // 只有 Claude Code 的配置已存在；Codex 未安装（无 config.toml）。
    fs::write(fixture.home.join(".claude.json"), r#"{"userID":"u1"}"#).unwrap();

    let mut controller = fixture.controller();
    let binary = binary_path(&fixture);
    let event = controller.dispatch(AppCommand::AutoRegisterMcpClients {
        binary_path: binary,
    });
    match event {
        AppEvent::AiMcpAutoRegistered { clients } => {
            assert_eq!(clients, vec!["claude".to_string()], "只注册已安装的客户端");
        }
        other => panic!("应为自动注册完成事件: {other:?}"),
    }
    let claude: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.home.join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(claude["userID"], serde_json::json!("u1"));
    assert_eq!(
        claude["mcpServers"]["fluxdb"]["type"],
        serde_json::json!("stdio")
    );
    assert!(
        !fixture.home.join(".codex").join("config.toml").is_file(),
        "未安装的 Codex 不应被创建配置"
    );
}

#[test]
fn manual_uninstall_disables_future_auto_registration() {
    let _home_guard = lock_home();
    let fixture = Fixture::new("autoff");
    fs::write(fixture.home.join(".claude.json"), r#"{"userID":"u1"}"#).unwrap();
    let mut controller = fixture.controller();
    let binary = binary_path(&fixture);

    let _ = controller.dispatch(AppCommand::AutoRegisterMcpClients {
        binary_path: binary.clone(),
    });
    let _ = controller.dispatch(AppCommand::UninstallMcpClient {
        client: "claude".into(),
    });
    let after_uninstall: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.home.join(".claude.json")).unwrap())
            .unwrap();
    assert!(after_uninstall["mcpServers"].get("fluxdb").is_none());

    let event = controller.dispatch(AppCommand::AutoRegisterMcpClients {
        binary_path: binary,
    });
    assert!(
        matches!(event, AppEvent::AiMcpAutoRegistered { ref clients } if clients.is_empty()),
        "卸载后自动注册应保持关闭: {event:?}"
    );
    let final_value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.home.join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        final_value["mcpServers"].get("fluxdb").is_none(),
        "不应被加回"
    );
}
