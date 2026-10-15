//! 插件配置声明协议：解析校验、内置接入与外置（manifest）接入（设计 §24）。
//!
//! 这里验证的是协议本身：同一份声明驱动同一套表单与校验，插件不写具体模型 id，
//! 声明写错时必须可见而不是静默变成「没有可配置项」。

use std::{fs, path::PathBuf};

use fluxdb_app::{
    AiModelSaveInput, AiPluginConfigFieldItem, AppCommand, AppController, AppEvent,
    PluginConfigDeclaration,
};
use fluxdb_storage::FileStorage;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-plugin-cfg-{}", uuid::Uuid::new_v4()));
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

    /// 写入一个非内置（wasm）插件包与实例；manifest 由测试指定。
    fn insert_wasm_plugin(&self, instance_id: &str, manifest_json: &str) {
        let package_id = format!("pkg-{instance_id}");
        self.storage
            .with_ai_connection(|conn| {
                let insert = |sql: &str, params: &[&dyn rusqlite::ToSql]| {
                    conn.execute(sql, params).map_err(|error| {
                        fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            error.to_string(),
                        )
                    })
                };
                insert(
                    "INSERT INTO ai_plugin_packages (id, package_key, version, runtime_kind, digest,
                        host_api_min, host_api_max, manifest_json, relative_path, status, revision,
                        created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, '1.0.0', 'wasm', ?3, 1, 1, ?4, 'plugins/demo.wasm', 'active', 1, 1, 1)",
                    &[
                        &package_id,
                        &format!("fluxdb.demo.{instance_id}"),
                        &format!("digest-{instance_id}"),
                        &manifest_json,
                    ],
                )?;
                insert(
                    "INSERT INTO ai_plugin_instances (id, display_name, package_id, enabled,
                        config_schema_version, config_json, granted_permissions_json, revision,
                        created_at_ms, updated_at_ms)
                     VALUES (?1, 'Demo 决策插件', ?2, 1, 1, '{}', '[]', 1, 1, 1)",
                    &[&instance_id, &package_id],
                )?;
                Ok(())
            })
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 保存一个 System One 决策模型（协议由 Provider 推导，不按名字猜）。
fn save_decision_model(controller: &mut AppController, name: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: "decisions".into(),
        model: format!("fixture-{name}"),
        endpoint: Some("https://decision.example.invalid/v1".into()),
        enabled: true,
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(512),
        api_key: Some("sk-fixture-not-real".into()),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save decision model: {other:?}"),
    }
}

/// 插件列表项（已刷新设置页状态）。
fn plugin_item(controller: &mut AppController, instance_id: &str) -> fluxdb_app::AiPluginItem {
    controller.dispatch(AppCommand::RefreshAiSettings);
    controller
        .state()
        .ai_plugin_instances
        .iter()
        .find(|plugin| plugin.id == instance_id)
        .cloned()
        .unwrap_or_else(|| panic!("插件实例 {instance_id} 应在列表里"))
}

fn config_field(item: &fluxdb_app::AiPluginItem, key: &str) -> AiPluginConfigFieldItem {
    item.config_fields
        .iter()
        .find(|field| field.key == key)
        .cloned()
        .unwrap_or_else(|| panic!("字段 {key} 应在声明里"))
}

#[test]
fn builtin_declaration_only_covers_plugins_that_need_config() {
    let jev = fluxdb_app::jev_config_declaration();
    assert_eq!(jev.protocol, fluxdb_app::PLUGIN_CONFIG_PROTOCOL);
    let field = jev
        .field(fluxdb_app::JEV_DECISION_MODEL_FIELD)
        .expect("Jev 声明必须含决策模型字段");
    assert!(field.required, "决策模型是必填配置");
    assert_eq!(field.resource.role, "decision");
    assert_eq!(field.resource.api_protocol.as_deref(), Some("system-one"));
    // 同一份声明必须能被外置插件路径原样解析：内置不是特权路径。
    let reparsed = PluginConfigDeclaration::parse(&serde_json::to_string(jev).unwrap())
        .expect("内置声明应可回环解析");
    assert_eq!(&reparsed, jev);
    // 没有可配置项的能力不返回声明，宿主因此不显示「管理」入口。
    let registry = fluxdb_app::BuiltinRegistry::default();
    assert!(registry.config_declaration("builtin.router").is_none());
    assert!(
        registry
            .config_declaration(fluxdb_app::JEV_INSTANCE_ID)
            .is_some()
    );
}

#[test]
fn declaration_rejects_unsupported_or_ambiguous_shapes() {
    let invalid = [
        // 协议主版本不认识：不做猜测兼容。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[]}"#,
        // 资源类型未实现。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[{"key":"model","label":"模型","resource":{"type":"tool","role":"decision"}}]}"#,
        // 角色存在但没有宿主槽位（没有默认模型列）。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"compaction"}}]}"#,
        // 未知字段：声明必须显式，不能有一半被静默忽略。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"decision"},"secret_path":"/tmp"}]}"#,
        // 字段键重复。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"decision"}},{"key":"model","label":"重复","resource":{"type":"model","role":"decision"}}]}"#,
        // 字段键不是稳定机器键。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[{"key":"决策模型","label":"模型","resource":{"type":"model","role":"decision"}}]}"#,
    ];
    for declaration in invalid {
        let error = PluginConfigDeclaration::parse(declaration)
            .expect_err(&format!("不应接受: {declaration}"));
        assert!(
            error.message.contains("插件配置声明无效"),
            "实际: {}",
            error.message
        );
    }
}

#[test]
fn wasm_manifest_declaration_uses_the_same_form_and_slot() {
    let fixture = Fixture::new();
    fixture.insert_wasm_plugin(
        "inst-demo",
        r#"{"config_declaration":{"protocol":"fluxdb.plugin.config.v1","fields":[{
             "key":"decision_model","label":"决策模型","required":true,
             "resource":{"type":"model","role":"decision","api_protocol":"system-one"}}]}}"#,
    );
    let mut controller = fixture.controller();
    let model_id = save_decision_model(&mut controller, "demo");

    let item = plugin_item(&mut controller, "inst-demo");
    assert!(item.config_error.is_none(), "{:?}", item.config_error);
    let field = config_field(&item, "decision_model");
    assert_eq!(field.selection, None, "外置插件默认未配置");
    assert_eq!(field.choices.len(), 1, "候选同样由宿主解析");
    assert_eq!(field.choices[0].id, model_id);

    // 选择走同一条命令与同一个校验路径；角色槽位是宿主级设置，同角色的插件共享它。
    let event = controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: "inst-demo".into(),
        selections: vec![fluxdb_app::AiPluginFieldSelection {
            key: "decision_model".into(),
            resource_id: Some(model_id.clone()),
        }],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "{event:?}"
    );
    assert_eq!(
        controller.state().ai_default_decision_model_id.as_deref(),
        Some(model_id.as_str())
    );
    assert_eq!(
        config_field(&plugin_item(&mut controller, "inst-demo"), "decision_model")
            .selection
            .as_deref(),
        Some(model_id.as_str())
    );
}

#[test]
fn broken_manifest_declaration_is_visible_and_does_not_fake_config() {
    let fixture = Fixture::new();
    fixture.insert_wasm_plugin(
        "inst-broken",
        r#"{"config_declaration":{"protocol":"fluxdb.plugin.config.v9","fields":[]}}"#,
    );
    fixture.insert_wasm_plugin("inst-plain", r#"{"capabilities":["tool_provider"]}"#);
    let mut controller = fixture.controller();

    let broken = plugin_item(&mut controller, "inst-broken");
    assert!(
        broken
            .config_error
            .as_deref()
            .is_some_and(|message| message.contains("不支持的配置协议")),
        "实际: {:?}",
        broken.config_error
    );
    assert!(
        broken.config_fields.is_empty(),
        "声明无效时不得冒充「无配置」"
    );

    // 没有 config_declaration 的外置插件：没有可配置项，也没有错误。
    let plain = plugin_item(&mut controller, "inst-plain");
    assert!(plain.config_fields.is_empty());
    assert!(plain.config_error.is_none());
}
