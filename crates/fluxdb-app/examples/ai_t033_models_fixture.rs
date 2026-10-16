//! T033 `#models` 截图/还原度验收的隔离 fixture 生成器（仅 debug 验收使用）。
//!
//! 只接收一个不存在的新目录，不读生产配置库、不写任何真实凭据。通过应用层
//! `AppCommand` 建模型（schema/加密列/revision 由存储层维护），端点指向 RFC 5737
//! 文档保留网段，确定不可路由；准备好截图需要的“经济 / 高能力长上下文 / 常规 /
//! 决策 / 禁止出站”各一行，以及默认模型与已知连接状态。
//!
//! 用法：`cargo run -p fluxdb-app --example ai_t033_models_fixture -- <新目录>`
use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_storage::FileStorage;
use fluxdb_storage::Storage as _;

struct Spec {
    name: &'static str,
    provider: &'static str,
    model: &'static str,
    context: i64,
    output: i64,
    input_price: i64,
    output_price: i64,
    cached_price: Option<i64>,
    egress: bool,
}

const SPECS: &[Spec] = &[
    Spec {
        name: "经济快速模型",
        provider: "openai",
        model: "cheap-flash",
        context: 32_768,
        output: 4_096,
        input_price: 300_000,
        output_price: 1_000,
        cached_price: Some(100_000),
        egress: true,
    },
    Spec {
        name: "高能力长上下文",
        provider: "openai",
        model: "big-premium",
        context: 400_000,
        output: 32_000,
        input_price: 15_000_000,
        output_price: 80_000,
        cached_price: Some(1_500_000),
        egress: true,
    },
    Spec {
        name: "日常通用模型",
        provider: "anthropic",
        model: "claude-daily",
        context: 200_000,
        output: 8_192,
        input_price: 3_000_000,
        output_price: 15_000,
        cached_price: Some(300_000),
        egress: true,
    },
    Spec {
        name: "内部决策模型",
        provider: "decisions",
        model: "choice-score-1",
        context: 8_192,
        output: 1_024,
        input_price: 0,
        output_price: 0,
        cached_price: None,
        egress: true,
    },
    Spec {
        name: "本地敏感模型（禁止出站）",
        provider: "openai",
        model: "local-sensitive",
        context: 32_768,
        output: 8_192,
        input_price: 1_000_000,
        output_price: 2_000_000,
        cached_price: None,
        egress: false,
    },
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("用法: ai_t033_models_fixture <新目录>")?,
    );
    if root.exists() {
        return Err("拒绝覆盖已有目录".into());
    }
    std::fs::create_dir_all(&root)?;
    let storage = FileStorage::new(root.join("config"));
    let mut settings = fluxdb_core::Settings::default();
    settings.log_path = root.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&settings)?;
    storage.with_ai_connection(|_| Ok(()))?; // 首次建库 + seed 内置 Agent/插件

    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));

    let mut ids: Vec<String> = Vec::new();
    for spec in SPECS {
        let endpoint = if spec.provider == "decisions" {
            "http://192.0.2.10:9/v1"
        } else {
            "http://192.0.2.20:9/v1"
        };
        let input = AiModelSaveInput {
            display_name: spec.name.into(),
            provider: spec.provider.into(),
            model: spec.model.into(),
            endpoint: Some(endpoint.into()),
            enabled: true,
            egress: spec.egress,
            context_window_tokens: Some(spec.context),
            max_output_tokens: Some(spec.output),
            input_price_micros: Some(spec.input_price),
            output_price_micros: Some(spec.output_price),
            cached_input_price_micros: spec.cached_price,
            ..Default::default()
        };
        let id = match controller.dispatch(AppCommand::SaveAiModel(input)) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => return Err(format!("模型 {} 保存失败：{other:?}", spec.name).into()),
        };
        ids.push(id);
    }

    // 连接状态是运行时元数据：前两行成功、第三行失败，便于截图核对标签。
    storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::models::set_connection_status(conn, 1_000, &ids[0], "ok")?;
        fluxdb_storage::ai_platform::models::set_connection_status(conn, 1_000, &ids[1], "ok")?;
        fluxdb_storage::ai_platform::models::set_connection_status(conn, 1_000, &ids[2], "failed")
    })?;

    // 默认主模型 = 日常通用模型（列表内「默认」单选）；决策模型由 Jev 插件管理弹框选。
    assert!(matches!(
        controller.dispatch(AppCommand::SetAiDefaultModel {
            model_id: Some(ids[2].clone()),
            expected_revision: controller.state().ai_runtime_revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiPluginConfig {
            instance_id: fluxdb_app::JEV_INSTANCE_ID.into(),
            selections: vec![fluxdb_app::AiPluginFieldSelection {
                key: fluxdb_app::JEV_DECISION_MODEL_FIELD.into(),
                value: None,
                resource_id: Some(ids[3].clone()),
            }],
            expected_revision: controller.state().ai_runtime_revision,
        }),
        AppEvent::AiPluginConfigSaved { .. }
    ));

    // 内置 Agent 绑主模型，避免新会话没有模型。
    let builtin = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::agents::load_agent(
            conn,
            fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID,
        )
    })?;
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: builtin.compression_trigger_percent,
        max_iterations: 24,
        id: Some(builtin.id.clone()),
        display_name: builtin.display_name.clone(),
        description: builtin.description.clone(),
        system_prompt: builtin.system_prompt.clone(),
        model_profile_id: Some(ids[2].clone()),
        allowed_tools: builtin.allowed_tools.clone(),
        enabled: true,
        expected_revision: builtin.revision,
    })) {
        AppEvent::AiAgentSaved { .. } => {}
        other => return Err(format!("Agent 保存失败：{other:?}").into()),
    }

    let config = storage.root_dir().display().to_string();
    println!("隔离配置目录: {config}");
    println!("模型档案: {}", ids.len());
    for (id, spec) in ids.iter().zip(SPECS) {
        println!("  {id}  {}", spec.name);
    }
    println!();
    println!("启动（明/暗各截一张）：");
    println!("  FLUXDB_AI_TEST_ROOT={config} FLUXDB_DEBUG_AI_SETTINGS=models \\");
    println!("  ./target/debug/fluxdb-desktop");
    Ok(())
}
