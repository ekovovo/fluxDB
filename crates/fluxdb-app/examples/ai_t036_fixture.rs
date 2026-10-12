//! T036 决策 Provider 的隔离验收夹具（debug 验收用）。
//!
//! 只接收一个不存在的新目录，不读生产配置与凭据；建一个指向本地 mock 的
//! `decisions` 模型（System One 协议），打印可直接复制的冒烟命令。用于黑盒验证
//! 请求体形状 / 认证头 / Choice 解析 / usage 上报；不替代 §37.3 R02 真实模型验收。
//!
//! 用法：`cargo run -p fluxdb-app --example ai_t036_fixture -- <新目录> [mock 端口]`
use fluxdb_app::{AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_core::Settings;
use fluxdb_storage::{FileStorage, Storage as _};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = std::path::PathBuf::from(args.next().ok_or("传入新的隔离目录")?);
    let port: u16 = args
        .next()
        .map(|value| value.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(39110);
    if root.exists() {
        return Err("拒绝覆盖已有目录".into());
    }
    std::fs::create_dir_all(&root)?;
    let storage = FileStorage::new(root.join("config"));
    let mut settings = Settings::default();
    settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&settings)?;
    storage.with_ai_connection(|_| Ok(()))?; // 首次建库 + seed

    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));

    let endpoint = format!("http://127.0.0.1:{port}");
    let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "T036 决策模型（本地 mock）".into(),
        provider: "decisions".into(),
        model: "mock-jev".into(),
        endpoint: Some(endpoint.clone()),
        enabled: true,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(1_024),
        // mock 不校验 Key；非空 Key 让候选过滤不因缺凭据被拒。
        api_key: Some("sk-mock-not-a-real-key".into()),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => return Err(format!("保存决策模型失败：{other:?}").into()),
    };

    let config = storage.root_dir().display().to_string();
    println!("隔离配置目录: {config}");
    println!("决策模型档案: {model_id} (mock-jev, endpoint={endpoint})");
    println!();
    println!("步骤 1（另开终端跑 mock）：");
    println!("  python3 docs/testing/ai-agent/t036_mock_decision.py {port}");
    println!();
    println!("步骤 2（跑冒烟，验证请求/解析/用量，不打印凭据）：");
    println!(
        "  FLUXDB_AI_DECISION_LIVE=1 FLUXDB_AI_DECISION_ROOT={config} \\\n    FLUXDB_AI_DECISION_MODEL={model_id} \\\n    cargo run -p fluxdb-app --offline --example ai_decision_smoke -- alpha beta gamma"
    );
    println!();
    println!(
        "步骤 3（核对 mock 日志：请求路径 /systemone、带 Bearer、questions.decision 是 Choice）："
    );
    println!("  cat /private/tmp/fluxdb-t036/decision-mock.log");
    Ok(())
}
