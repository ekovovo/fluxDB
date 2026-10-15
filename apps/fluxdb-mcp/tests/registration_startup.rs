//! 注册命令必须绑定应用存储根，不能依赖 CLI 继承桌面进程环境。
// Windows 的 home_dir 不使用 HOME；该隔离方式仅在 Unix 上运行，避免触达真实配置。
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use fluxdb_app::{
    AiMcpServerProfileSaveInput, AppCommand, AppController, AppEvent, DEFAULT_MCP_PROFILE_ID,
};
use fluxdb_storage::FileStorage;

#[test]
fn synced_server_starts_without_desktop_profile_environment() {
    let root =
        std::env::temp_dir().join(format!("fluxdb-mcp-registration-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    let data = root.join("custom-profile-data");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    let previous_home = std::env::var_os("HOME");
    // SAFETY: 本集成测试独占测试进程，子进程也显式使用隔离 HOME。
    unsafe { std::env::set_var("HOME", &home) };
    let mut controller = AppController::new();
    controller.set_ai_storage(FileStorage::new(&data));
    // 服务启用与客户端接入分离；显式接入后才能读取到启动配置。
    assert!(matches!(
        controller.dispatch(AppCommand::EnsureMcpControl),
        AppEvent::AiSettingsLoaded
    ));
    let revision = controller
        .state()
        .ai_mcp_server_profiles
        .iter()
        .find(|p| p.id == DEFAULT_MCP_PROFILE_ID)
        .map(|p| p.revision)
        .expect("默认 Profile 应存在");
    let _ = controller.dispatch(AppCommand::SaveAiMcpServerProfile(
        AiMcpServerProfileSaveInput {
            id: Some(DEFAULT_MCP_PROFILE_ID.into()),
            display_name: "本地 MCP".into(),
            enabled: true,
            expected_revision: Some(revision),
        },
    ));
    let event = controller.dispatch(AppCommand::SetMcpClientRegistration {
        client_key: "claude".into(),
        registered: true,
        binary_path: env!("CARGO_BIN_EXE_fluxdb-mcp").into(),
    });
    match previous_home {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    assert!(
        matches!(event, AppEvent::AiMcpClientsSynced { .. }),
        "{event:?}"
    );
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".claude.json")).unwrap()).unwrap();
    let entry = &config["mcpServers"]["fluxdb"];
    let args: Vec<_> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(args.contains(&"--config-root"));
    let mut child = Command::new(entry["command"].as_str().unwrap())
        .args(args)
        .env("HOME", &home)
        .env_remove("FLUXDB_PROFILE")
        .env_remove("FLUXDB_AI_TEST_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params": {
                "protocolVersion":"2024-11-05", "capabilities":{},
                "clientInfo":{"name":"isolated-registration-test","version":"1"}
            }
        })
    )
    .unwrap();
    let response = rx.recv_timeout(Duration::from_secs(10));
    drop(input);
    let _ = child.kill();
    let _ = child.wait();
    reader.join().unwrap();
    let _ = std::fs::remove_dir_all(root);
    let response: serde_json::Value = serde_json::from_str(&response.unwrap().unwrap()).unwrap();
    assert_eq!(response["id"], 1);
    assert!(response.get("error").is_none(), "{response}");
    assert!(response["result"]["serverInfo"].is_object());
}
