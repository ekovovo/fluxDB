//! T016 真实窗口验收的隔离 fixture 生成器（仅 debug 验收使用）。
//!
//! 通过应用层 `AppCommand` 创建模型与 Agent，而不是直接写 SQLite：schema、加密列与
//! revision 由存储层自己维护，避免 fixture 与真实结构漂移。全程不读生产配置库、
//! 不写入任何凭据；模型端点指向 RFC 5737 文档保留网段，确定不可路由。

use fluxdb_app::{AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_storage::FileStorage;

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("用法: t016_acceptance_fixture <root>");
    let root = std::path::PathBuf::from(root);
    let storage = FileStorage::new(root.clone());

    let mut controller = AppController::new();
    controller.set_ai_storage(storage);
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));

    let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        id: None,
        display_name: "T016 验收模型".into(),
        provider: "openai".into(),
        model: "t016-acceptance-not-a-real-model".into(),
        endpoint: Some("http://192.0.2.1:9/v1".into()),
        endpoint_is_full_url: false,
        enabled: true,
        egress: true,
        context_window_tokens: Some(8192),
        max_output_tokens: Some(1024),
        input_price_micros: None,
        output_price_micros: None,
        cached_input_price_micros: None,
        api_key: None,
        clear_key: false,
        expected_revision: None,
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("模型保存失败: {other:?}"),
    };

    match controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(model_id.clone()),
        default_decision_model_id: None,
        expected_revision: 0,
    }) {
        AppEvent::AiDefaultModelsSaved { .. } => {}
        other => panic!("默认模型设置失败: {other:?}"),
    }

    let agent_id = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: None,
        display_name: "T016 验收 Agent".into(),
        description: "仅用于 T016 后台 actor 真实窗口验收".into(),
        system_prompt: "解释当前页面，不执行任何写操作。".into(),
        model_profile_id: Some(model_id.clone()),
        compression_trigger_percent: 80,
        max_iterations: 24,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 0,
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("Agent 保存失败: {other:?}"),
    };

    println!(
        "FIXTURE_OK root={} model={model_id} agent={agent_id}",
        root.display()
    );
}
