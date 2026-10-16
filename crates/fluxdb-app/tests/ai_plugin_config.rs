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

/// 决策模型槽位真值：只由插件管理弹框写入，这里从运行设置直接读。
fn decision_slot(fixture: &Fixture) -> Option<String> {
    fixture
        .storage
        .with_ai_connection(|conn| {
            Ok(
                fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?
                    .default_decision_model_profile_id,
            )
        })
        .unwrap()
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
    assert_eq!(field.resource.as_ref().unwrap().role, "decision");
    assert_eq!(
        field.resource.as_ref().unwrap().api_protocol.as_deref(),
        Some("system-one")
    );
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

/// 上下文压缩声明的是独立的压缩决策槽位：字段非必填，未选择时按确定性切点压缩。
#[test]
fn compaction_declaration_uses_its_own_slot_and_stays_optional() {
    let declaration = fluxdb_app::compaction_config_declaration();
    assert_eq!(declaration.protocol, fluxdb_app::PLUGIN_CONFIG_PROTOCOL);
    let field = declaration
        .field(fluxdb_app::COMPACTION_DECISION_MODEL_FIELD)
        .expect("上下文压缩必须声明压缩决策模型字段");
    assert!(!field.required, "未选择压缩决策模型也必须能正常压缩");
    assert_eq!(field.resource.as_ref().unwrap().role, "compaction_decision");
    assert_eq!(
        field.resource.as_ref().unwrap().api_protocol.as_deref(),
        Some("system-one")
    );
    let reparsed = PluginConfigDeclaration::parse(&serde_json::to_string(declaration).unwrap())
        .expect("内置声明应可回环解析");
    assert_eq!(&reparsed, declaration);
    assert!(
        fluxdb_app::BuiltinRegistry::default()
            .config_declaration("builtin.compaction")
            .is_some(),
        "声明了字段的能力必须有「管理」入口"
    );
}

/// 压缩决策槽位与 Jev 决策槽位各自独立：配一个不会连带改动另一个。
#[test]
fn compaction_slot_is_independent_from_jev_decision_slot() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let jev_model = save_decision_model(&mut controller, "jev");
    let compaction_model = save_decision_model(&mut controller, "compaction");

    // 先在「上下文压缩 · 管理」里选压缩决策模型。
    let event = controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: "builtin.compaction".into(),
        selections: vec![fluxdb_app::AiPluginFieldSelection {
            key: fluxdb_app::COMPACTION_DECISION_MODEL_FIELD.into(),
            value: None,
            resource_id: Some(compaction_model.clone()),
        }],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "{event:?}"
    );
    assert_eq!(
        config_field(
            &plugin_item(&mut controller, "builtin.compaction"),
            fluxdb_app::COMPACTION_DECISION_MODEL_FIELD
        )
        .selection
        .as_deref(),
        Some(compaction_model.as_str())
    );
    assert_eq!(
        decision_slot(&fixture).as_deref(),
        None,
        "压缩决策槽位不得连带写入 Jev 决策槽位"
    );
    assert!(
        config_field(
            &plugin_item(&mut controller, fluxdb_app::JEV_INSTANCE_ID),
            "decision_model"
        )
        .selection
        .is_none()
    );

    // 再选 Jev 决策模型：两个槽位各自保留自己的选择。
    let event = controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: fluxdb_app::JEV_INSTANCE_ID.into(),
        selections: vec![fluxdb_app::AiPluginFieldSelection {
            key: fluxdb_app::JEV_DECISION_MODEL_FIELD.into(),
            value: None,
            resource_id: Some(jev_model.clone()),
        }],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "{event:?}"
    );
    assert_eq!(
        config_field(
            &plugin_item(&mut controller, "builtin.compaction"),
            fluxdb_app::COMPACTION_DECISION_MODEL_FIELD
        )
        .selection
        .as_deref(),
        Some(compaction_model.as_str()),
        "改 Jev 决策模型不得覆盖压缩决策选择"
    );
    assert_eq!(decision_slot(&fixture).as_deref(), Some(jev_model.as_str()));
}

#[test]
fn declaration_rejects_unsupported_or_ambiguous_shapes() {
    let invalid = [
        // 协议主版本不认识：不做猜测兼容（v1 已被 v2 取代）。
        r#"{"protocol":"fluxdb.plugin.config.v1","fields":[]}"#,
        r#"{"protocol":"fluxdb.plugin.config.v9","fields":[]}"#,
        // 资源类型未实现。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"model","label":"模型","resource":{"type":"tool","role":"decision"}}]}"#,
        // 角色存在但没有宿主槽位（没有默认模型列）。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"compaction"}}]}"#,
        // 未知字段：声明必须显式，不能有一半被静默忽略。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"decision"},"secret_path":"/tmp"}]}"#,
        // 字段键重复。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"decision"}},{"key":"model","label":"重复","resource":{"type":"model","role":"decision"}}]}"#,
        // 字段键不是稳定机器键。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"决策模型","label":"模型","resource":{"type":"model","role":"decision"}}]}"#,
        // 标量字段不能带资源需求。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"flag","kind":"switch","label":"开关","resource":{"type":"model","role":"decision"}}]}"#,
        // 资源字段不能声明候选值。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"model","label":"模型","resource":{"type":"model","role":"decision"},"options":[{"value":"a","label":"A"}]}]}"#,
        // 枚举字段必须给出候选值。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"mode","kind":"select","label":"模式"}]}"#,
        // 开关默认值只能是 true/false。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"flag","kind":"switch","label":"开关","default":"yes"}]}"#,
        // 枚举默认值必须在候选里。
        r#"{"protocol":"fluxdb.plugin.config.v2","fields":[{"key":"mode","kind":"select","label":"模式","default":"x","options":[{"value":"a","label":"A"}]}]}"#,
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
        r#"{"config_declaration":{"protocol":"fluxdb.plugin.config.v2","fields":[{
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
            value: None,
            resource_id: Some(model_id.clone()),
        }],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "{event:?}"
    );
    assert_eq!(decision_slot(&fixture).as_deref(), Some(model_id.as_str()));
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

/// 安全审计插件的标量规则字段：按声明读写实例 `config_json`，与模型槽位互不干扰。
#[test]
fn security_audit_scalar_rules_round_trip_through_instance_config() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::RefreshAiSettings);

    let item = plugin_item(&mut controller, "builtin.security-audit");
    assert!(item.config_error.is_none(), "{:?}", item.config_error);
    // 声明的规则字段都在，且都是标量字段（不占模型槽位）。
    let denied = config_field(&item, "denied_tables");
    assert_eq!(denied.kind, fluxdb_app::AiPluginConfigFieldKind::TextList);
    assert!(denied.choices.is_empty(), "标量字段不应有资源候选");
    let flag = config_field(&item, "forbid_ddl");
    assert_eq!(flag.kind, fluxdb_app::AiPluginConfigFieldKind::Switch);
    assert_eq!(flag.selection.as_deref(), Some("true"), "声明默认值应可见");

    let event = controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: "builtin.security-audit".into(),
        selections: vec![
            fluxdb_app::AiPluginFieldSelection {
                key: "denied_tables".into(),
                resource_id: None,
                value: Some("audit_logs\npublic_events".into()),
            },
            fluxdb_app::AiPluginFieldSelection {
                key: "forbid_ddl".into(),
                resource_id: None,
                value: Some("false".into()),
            },
        ],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "保存标量规则应成功：{event:?}"
    );

    // 值落进实例 config_json：多行文本按行拆成数组，开关按布尔落库。
    let values = fixture
        .storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::plugins::load_plugin_instance_config(
                conn,
                "builtin.security-audit",
            )
        })
        .expect("读取实例配置")
        .values;
    let tables = values.get("denied_tables").expect("应有 denied_tables");
    assert_eq!(tables.as_array().map(|items| items.len()), Some(2));
    assert_eq!(
        values.get("forbid_ddl"),
        Some(&serde_json::Value::Bool(false))
    );

    // 规则能真正被求值读出（闭环：写进去 → 求值拿到）。
    let rules = fluxdb_app::load_audit_rules(&fixture.storage).expect("装载规则");
    assert_eq!(rules.denied_tables.len(), 2);
    assert!(!rules.forbid_ddl);

    // 重新读设置页，字段回填的是保存值（含多行文本还原）。
    controller.dispatch(AppCommand::RefreshAiSettings);
    let item = plugin_item(&mut controller, "builtin.security-audit");
    let denied = config_field(&item, "denied_tables");
    assert_eq!(
        denied.selection.as_deref(),
        Some("audit_logs\npublic_events")
    );
    assert_eq!(
        config_field(&item, "forbid_ddl").selection.as_deref(),
        Some("false")
    );
}

/// 安全审计声明带分组：字段按 group 归档，便于弹框分块渲染。
#[test]
fn security_audit_declaration_fields_carry_group_labels() {
    let decl = fluxdb_app::security_audit_config_declaration();
    assert_eq!(decl.protocol, fluxdb_app::PLUGIN_CONFIG_PROTOCOL);
    // 8 条规则全部有分组；分组覆盖三类。
    assert_eq!(decl.fields.len(), 8);
    let groups: std::collections::BTreeSet<&str> = decl
        .fields
        .iter()
        .filter_map(|f| f.group.as_deref())
        .collect();
    assert_eq!(
        groups,
        ["Redis", "写入与结构", "数据范围"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
    // 每个字段都有标签与非空分组名。
    for field in &decl.fields {
        assert!(!field.label.trim().is_empty(), "{} 缺标签", field.key);
        assert!(
            field.group.as_deref().is_some_and(|g| !g.trim().is_empty()),
            "{} 缺分组",
            field.key
        );
    }
}
