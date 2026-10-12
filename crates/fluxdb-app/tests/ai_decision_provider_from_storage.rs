//! T036 `decision_provider_from_storage` 的候选过滤：走真实 SQLite + 生产保存路径，
//! 只准备模型/隐私元数据，不发请求、不解密真实凭据。
use std::{fs, path::PathBuf};

use fluxdb_app::{AiModelSaveInput, AppCommand, AppController, AppEvent};
use fluxdb_storage::FileStorage;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t036-ctor-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn save_decision_model(controller: &mut AppController, name: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: "decisions".into(),
        model: "jev-offline".into(),
        endpoint: Some("https://decision.example.invalid/v1".into()),
        enabled: true,
        api_key: Some("sk-mock-not-a-real-key".into()),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存决策模型失败: {other:?}"),
    }
}

#[test]
fn system_one_decision_provider_builds_for_configured_model() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let id = save_decision_model(&mut controller, "T036 决策模型");
    let provider = fluxdb_app::decision_provider_from_storage(&fixture.storage, &id)
        .expect("合格决策模型应能构造 Provider");
    // Debug 只显示非敏感信息，不含凭据。
    let debug = format!("{provider:?}");
    assert!(
        debug.contains(&id) && !debug.contains("sk-mock"),
        "Debug 不得含凭据: {debug}"
    );
}

#[test]
fn system_one_decision_provider_rejects_generation_model() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "生成模型".into(),
        provider: "openai".into(),
        model: "gpt-offline".into(),
        endpoint: Some("https://model.example.invalid/v1".into()),
        enabled: true,
        api_key: Some("sk-mock-not-a-real-key".into()),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存生成模型失败: {other:?}"),
    };
    let error = fluxdb_app::decision_provider_from_storage(&fixture.storage, &model)
        .expect_err("生成模型不得当作决策模型");
    assert!(
        error.message.contains("决策"),
        "实际错误：{}",
        error.message
    );
}

#[test]
fn system_one_decision_provider_rejects_disabled_model() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let id = save_decision_model(&mut controller, "T036 决策模型");
    let revision = match controller.dispatch(AppCommand::SetAiModelEnabled {
        id: id.clone(),
        enabled: false,
        expected_revision: 1,
    }) {
        AppEvent::AiModelEnabled { revision, .. } => revision,
        other => panic!("停用失败: {other:?}"),
    };
    assert!(revision > 1);
    let error = fluxdb_app::decision_provider_from_storage(&fixture.storage, &id)
        .expect_err("停用模型不得作为决策 Provider");
    assert!(
        error.message.contains("停用"),
        "实际错误：{}",
        error.message
    );
}

#[test]
fn system_one_decision_provider_rejects_model_without_credential() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    // 决策模型必须带 Key；用无 Key 的 decisions 模型构造应被候选过滤拒绝。
    let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "无凭据决策模型".into(),
        provider: "decisions".into(),
        model: "jev-no-key".into(),
        endpoint: Some("https://decision.example.invalid/v1".into()),
        enabled: true,
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("保存无 Key 决策模型失败: {other:?}"),
    };
    // 该模型 auth_mode=none → 直接拒绝（System One 需要凭据）。
    let error = fluxdb_app::decision_provider_from_storage(&fixture.storage, &model)
        .expect_err("无凭据决策模型不得构造 Provider");
    assert!(
        error.message.contains("凭据") || error.message.contains("不是 System One"),
        "实际错误：{}",
        error.message
    );
}
