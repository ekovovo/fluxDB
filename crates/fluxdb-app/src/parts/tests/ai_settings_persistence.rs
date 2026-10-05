// AI 设置业务与持久化专项测试（子 Agent A，T013/T014）。
//
// 本文件复用 ai_settings.rs 中的隔离 storage/helper，只通过 AppCommand 和真实
// SQLite 验证 Keep/Set/Clear、CAS、插件启停、Agent 保存及公开投影闭环。

#[derive(Debug, PartialEq, Eq)]
struct PaModelRow {
    display_name: String,
    provider: String,
    model_kind: String,
    model: String,
    endpoint: Option<String>,
    enabled: i64,
    auth_mode: String,
    context_window_tokens: Option<i64>,
    max_output_tokens: Option<i64>,
    input_price_micros: Option<i64>,
    output_price_micros: Option<i64>,
    cached_input_price_micros: Option<i64>,
    revision: i64,
    secret_revision: i64,
    ciphertext: Option<Vec<u8>>,
    nonce: Option<Vec<u8>>,
    cipher_version: Option<i64>,
    key_id: Option<String>,
    connection_status: String,
    connection_checked_at_ms: Option<i64>,
    updated_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct PaAgentRow {
    display_name: String,
    description: String,
    enabled: i64,
    default_model_profile_id: Option<String>,
    system_prompt: String,
    budget_policy_id: Option<String>,
    allowed_tools_json: String,
    revision: i64,
    updated_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct PaRuntimeRow {
    default_agent_id: Option<String>,
    default_model_profile_id: Option<String>,
    default_decision_model_profile_id: Option<String>,
    max_active_turns: i64,
    revision: i64,
    updated_at_ms: i64,
}

fn pa_connection(dir: &std::path::Path) -> rusqlite::Connection {
    fluxdb_storage::FileStorage::new(dir.to_path_buf())
        .open_ai_connection()
        .expect("打开隔离 AI SQLite")
}

fn pa_model_row(dir: &std::path::Path, id: &str) -> PaModelRow {
    pa_connection(dir)
        .query_row(
            "SELECT display_name, provider, model_kind, model, endpoint, enabled, auth_mode,
                    context_window_tokens, max_output_tokens,
                    input_price_micros_per_million, output_price_micros_per_million,
                    cached_input_price_micros_per_million, revision, secret_revision,
                    credential_ciphertext, credential_nonce, cipher_version, key_id,
                    connection_status, connection_checked_at_ms, updated_at_ms
               FROM ai_model_profiles WHERE id=?1",
            [id],
            |row| {
                Ok(PaModelRow {
                    display_name: row.get(0)?,
                    provider: row.get(1)?,
                    model_kind: row.get(2)?,
                    model: row.get(3)?,
                    endpoint: row.get(4)?,
                    enabled: row.get(5)?,
                    auth_mode: row.get(6)?,
                    context_window_tokens: row.get(7)?,
                    max_output_tokens: row.get(8)?,
                    input_price_micros: row.get(9)?,
                    output_price_micros: row.get(10)?,
                    cached_input_price_micros: row.get(11)?,
                    revision: row.get(12)?,
                    secret_revision: row.get(13)?,
                    ciphertext: row.get(14)?,
                    nonce: row.get(15)?,
                    cipher_version: row.get(16)?,
                    key_id: row.get(17)?,
                    connection_status: row.get(18)?,
                    connection_checked_at_ms: row.get(19)?,
                    updated_at_ms: row.get(20)?,
                })
            },
        )
        .expect("读取模型私有行快照")
}

fn pa_agent_row(dir: &std::path::Path, id: &str) -> PaAgentRow {
    pa_connection(dir)
        .query_row(
            "SELECT display_name, description, enabled, default_model_profile_id,
                    system_prompt, budget_policy_id, allowed_tools_json, revision, updated_at_ms
               FROM ai_agents WHERE id=?1",
            [id],
            |row| {
                Ok(PaAgentRow {
                    display_name: row.get(0)?,
                    description: row.get(1)?,
                    enabled: row.get(2)?,
                    default_model_profile_id: row.get(3)?,
                    system_prompt: row.get(4)?,
                    budget_policy_id: row.get(5)?,
                    allowed_tools_json: row.get(6)?,
                    revision: row.get(7)?,
                    updated_at_ms: row.get(8)?,
                })
            },
        )
        .expect("读取 Agent 行快照")
}

fn pa_runtime_row(dir: &std::path::Path) -> PaRuntimeRow {
    pa_connection(dir)
        .query_row(
            "SELECT default_agent_id, default_model_profile_id,
                    default_decision_model_profile_id, max_active_turns, revision, updated_at_ms
               FROM ai_runtime_settings WHERE id=1",
            [],
            |row| {
                Ok(PaRuntimeRow {
                    default_agent_id: row.get(0)?,
                    default_model_profile_id: row.get(1)?,
                    default_decision_model_profile_id: row.get(2)?,
                    max_active_turns: row.get(3)?,
                    revision: row.get(4)?,
                    updated_at_ms: row.get(5)?,
                })
            },
        )
        .expect("读取运行设置行快照")
}

fn pa_plugin_row(dir: &std::path::Path, id: &str) -> (i64, i64) {
    pa_connection(dir)
        .query_row(
            "SELECT enabled, revision FROM ai_plugin_instances WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("读取插件实例行")
}

fn pa_model_counts(dir: &std::path::Path) -> (i64, i64) {
    let conn = pa_connection(dir);
    let models = conn
        .query_row("SELECT COUNT(*) FROM ai_model_profiles", [], |row| {
            row.get(0)
        })
        .unwrap();
    let agents = conn
        .query_row("SELECT COUNT(*) FROM ai_agents", [], |row| row.get(0))
        .unwrap();
    (models, agents)
}

fn pa_assert_model_item_public_fields(item: &AiModelItem, row: &PaModelRow) {
    assert_eq!(item.display_name, row.display_name);
    assert_eq!(item.provider, row.provider);
    assert_eq!(item.model, row.model);
    assert_eq!(item.endpoint, row.endpoint);
    assert_eq!(item.enabled, row.enabled != 0);
    assert_eq!(item.context_window_tokens, row.context_window_tokens);
    assert_eq!(item.max_output_tokens, row.max_output_tokens);
    assert_eq!(item.input_price_micros, row.input_price_micros);
    assert_eq!(item.output_price_micros, row.output_price_micros);
    assert_eq!(
        item.cached_input_price_micros,
        row.cached_input_price_micros
    );
    assert_eq!(
        item.kind,
        if row.model_kind == "decision" {
            "决策"
        } else {
            "生成"
        }
    );
    let expected_credential = match (row.auth_mode.as_str(), row.ciphertext.is_some()) {
        ("api_key", true) => "已配置",
        ("api_key", false) => "缺少凭据",
        _ => "无需凭据",
    };
    assert_eq!(item.credential_status, expected_credential);
    assert_eq!(item.connection_status, row.connection_status);
    assert_eq!(item.connection_checked_at_ms, row.connection_checked_at_ms);
    assert_eq!(item.revision, row.revision);
}

#[test]
fn ai_settings_model_keep_set_clear_persist_exact_secret_transitions() {
    let (mut controller, dir) = controller_with_ai_storage("persist-keep-set-clear");
    let first_secret = "sk-first-secret-0123456789";
    let AppEvent::AiModelSaved { id, revision } = controller.dispatch(AppCommand::SaveAiModel(
        sample_model_input(Some(first_secret)),
    )) else {
        panic!("创建带 Key 模型失败");
    };

    let before_keep = pa_model_row(&dir, &id);
    let mut keep = sample_model_input(None);
    keep.id = Some(id.clone());
    keep.display_name = "Keep 改名".into();
    keep.expected_revision = Some(revision);
    let AppEvent::AiModelSaved {
        revision: keep_revision,
        ..
    } = controller.dispatch(AppCommand::SaveAiModel(keep))
    else {
        panic!("Keep 保存失败");
    };
    let after_keep = pa_model_row(&dir, &id);
    assert_ne!(after_keep.display_name, before_keep.display_name);
    assert_eq!(after_keep.secret_revision, before_keep.secret_revision);
    assert_eq!(after_keep.ciphertext, before_keep.ciphertext);
    assert_eq!(after_keep.nonce, before_keep.nonce);
    assert_eq!(after_keep.cipher_version, before_keep.cipher_version);
    assert_eq!(after_keep.key_id, before_keep.key_id);
    assert_eq!(after_keep.revision, keep_revision);

    let second_secret = "sk-second-secret-9876543210";
    let mut set = sample_model_input(Some(second_secret));
    set.id = Some(id.clone());
    set.display_name = "Set 新 Key".into();
    set.expected_revision = Some(keep_revision);
    let AppEvent::AiModelSaved {
        revision: set_revision,
        ..
    } = controller.dispatch(AppCommand::SaveAiModel(set))
    else {
        panic!("Set 保存失败");
    };
    let after_set = pa_model_row(&dir, &id);
    assert_eq!(
        after_set.secret_revision,
        after_keep.secret_revision + 1,
        "Set 应递增 secret_revision"
    );
    assert_ne!(after_set.ciphertext, after_keep.ciphertext);
    assert_ne!(after_set.nonce, after_keep.nonce);
    assert_eq!(after_set.cipher_version, Some(1));
    assert_eq!(after_set.key_id.as_deref(), Some("k1"));

    let conn = pa_connection(&dir);
    let (cipher, _) = fluxdb_storage::ai_platform::load_ai_cipher(&dir).expect("加载数据密钥");
    let sealed = fluxdb_storage::ai_platform::models::load_encrypted_secret(&conn, &id)
        .expect("读取密文")
        .expect("Set 后应有密文");
    assert_eq!(
        fluxdb_storage::ai_platform::model_secret::open(&cipher, &id, &sealed).expect("解密"),
        second_secret
    );

    let mut clear = sample_model_input(None);
    clear.id = Some(id.clone());
    clear.display_name = "Clear 清 Key".into();
    clear.clear_key = true;
    clear.expected_revision = Some(set_revision);
    let AppEvent::AiModelSaved {
        revision: clear_revision,
        ..
    } = controller.dispatch(AppCommand::SaveAiModel(clear))
    else {
        panic!("Clear 保存失败");
    };
    let after_clear = pa_model_row(&dir, &id);
    assert_eq!(
        after_clear.secret_revision,
        after_set.secret_revision + 1,
        "Clear 应递增 secret_revision"
    );
    assert_eq!(after_clear.ciphertext, None);
    assert_eq!(after_clear.nonce, None);
    assert_eq!(after_clear.cipher_version, None);
    assert_eq!(after_clear.key_id, None);
    assert_eq!(after_clear.revision, clear_revision);
    let item = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("Clear 后模型仍在列表");
    assert_eq!(item.credential_status, "缺少凭据");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_stale_model_save_is_rejected_without_overwriting_row() {
    let (mut controller, dir) = controller_with_ai_storage("persist-model-cas");
    let AppEvent::AiModelSaved { id, revision } = controller.dispatch(AppCommand::SaveAiModel(
        sample_model_input(Some("sk-cas-first")),
    )) else {
        panic!("创建模型失败");
    };

    let mut first = sample_model_input(None);
    first.id = Some(id.clone());
    first.display_name = "第一次修改".into();
    first.endpoint = Some("https://first.example.invalid/v1".into());
    first.expected_revision = Some(revision);
    controller.dispatch(AppCommand::SaveAiModel(first));

    let before_stale = pa_model_row(&dir, &id);
    let mut stale = sample_model_input(Some("sk-stale-should-not-write"));
    stale.id = Some(id.clone());
    stale.display_name = "过期覆盖".into();
    stale.expected_revision = Some(revision);
    match controller.dispatch(AppCommand::SaveAiModel(stale)) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("重新加载"), "CAS 错误: {message}");
        }
        other => panic!("过期 revision 必须被拒，实际: {other:?}"),
    }

    let after_stale = pa_model_row(&dir, &id);
    assert_eq!(after_stale, before_stale, "CAS 冲突不得改写旧值或密文");
    assert_eq!(after_stale.display_name, "第一次修改");
    assert_eq!(after_stale.ciphertext, before_stale.ciphertext);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_failed_model_save_preserves_config_and_ciphertext_bytes() {
    let (mut controller, dir) = controller_with_ai_storage("persist-model-failure");
    let secret = "sk-failure-preserve";
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(sample_model_input(Some(secret))))
    else {
        panic!("创建模型失败");
    };
    let before = pa_model_row(&dir, &id);

    let mut invalid = sample_model_input(Some("sk-must-not-replace"));
    invalid.id = Some(id.clone());
    invalid.display_name = "非法保存不得写入".into();
    invalid.context_window_tokens = Some(10);
    invalid.expected_revision = Some(revision);
    match controller.dispatch(AppCommand::SaveAiModel(invalid)) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("上下文窗口"), "校验错误: {message}");
        }
        other => panic!("非法输入必须被拒，实际: {other:?}"),
    }

    let after = pa_model_row(&dir, &id);
    assert_eq!(after, before, "失败保存不得改动配置或任意密文字节");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_default_models_reject_invalid_kind_and_stale_without_overwrite() {
    let (mut controller, dir) = controller_with_ai_storage("persist-defaults");
    let gen_id = create_model(&mut controller, "openai", "主模型", Some("sk-default-main"));
    let dec_id = create_model(&mut controller, "decisions", "决策模型", None);

    let AppEvent::AiDefaultModelsSaved { revision } =
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(gen_id.clone()),
            default_decision_model_id: Some(dec_id.clone()),
            expected_revision: 0,
        })
    else {
        panic!("首次保存默认模型失败");
    };
    assert_eq!(revision, 1);
    let before_invalid = pa_runtime_row(&dir);

    match controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(dec_id.clone()),
        default_decision_model_id: None,
        expected_revision: revision,
    }) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("生成模型"), "非法 kind 错误: {message}");
        }
        other => panic!("决策模型不能作为默认主模型，实际: {other:?}"),
    }
    assert_eq!(
        pa_runtime_row(&dir),
        before_invalid,
        "非法 kind 不得改写运行设置"
    );

    match controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(gen_id.clone()),
        default_decision_model_id: None,
        expected_revision: 0,
    }) {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("重新加载"),
                "过期 revision 错误: {message}"
            );
        }
        other => panic!("过期 revision 必须被拒，实际: {other:?}"),
    }
    assert_eq!(
        pa_runtime_row(&dir),
        before_invalid,
        "过期 revision 不得改写运行设置"
    );

    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let mut reloaded = AppController::new();
    reloaded.set_ai_storage(storage);
    reloaded.dispatch(AppCommand::RefreshAiSettings);
    assert_eq!(
        reloaded.state().ai_default_model_id.as_deref(),
        Some(gen_id.as_str())
    );
    assert_eq!(
        reloaded.state().ai_default_decision_model_id.as_deref(),
        Some(dec_id.as_str())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_plugin_toggle_persists_and_mandatory_instances_cannot_disable() {
    let (mut controller, dir) = controller_with_ai_storage("persist-plugins");
    assert!(
        matches!(
            controller.dispatch(AppCommand::RefreshAiSettings),
            AppEvent::AiSettingsLoaded
        ),
        "刷新应成功加载内置插件"
    );

    let jev = controller
        .state()
        .ai_plugin_instances
        .iter()
        .find(|p| p.id == "builtin.jev")
        .expect("Jev 内置实例应存在");
    assert!(jev.enabled);
    let jev_revision = jev.revision;
    let event = controller.dispatch(AppCommand::SetAiPluginInstanceEnabled {
        id: "builtin.jev".into(),
        enabled: false,
        expected_revision: jev_revision,
    });
    match event {
        AppEvent::AiPluginInstanceEnabled {
            id,
            enabled: false,
            revision,
        } => {
            assert_eq!(id, "builtin.jev");
            assert_eq!(revision, jev_revision + 1);
        }
        other => panic!("Jev 应可关闭，实际: {other:?}"),
    }
    assert_eq!(pa_plugin_row(&dir, "builtin.jev"), (0, jev_revision + 1));
    assert!(
        !controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|p| p.id == "builtin.jev")
            .unwrap()
            .enabled
    );

    for mandatory in ["builtin.security-audit", "builtin.budget"] {
        let before = pa_plugin_row(&dir, mandatory);
        let event = controller.dispatch(AppCommand::SetAiPluginInstanceEnabled {
            id: mandatory.into(),
            enabled: false,
            expected_revision: before.1,
        });
        match event {
            AppEvent::AiSettingsError(message) => {
                assert!(
                    message.contains("内核约束") || message.contains("不可停用"),
                    "{mandatory} 应给出内核约束错误: {message}"
                );
            }
            other => panic!("{mandatory} 不可停用，实际: {other:?}"),
        }
        assert_eq!(
            pa_plugin_row(&dir, mandatory),
            before,
            "{mandatory} 被拒后数据库状态不得变化"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_agent_save_edit_and_failed_cas_are_row_scoped_and_persistent() {
    let (mut controller, dir) = controller_with_ai_storage("persist-agents");
    let target = create_agent(&mut controller, "目标 Agent", "目标提示词");
    let other = create_agent(&mut controller, "其它 Agent", "其它提示词");

    assert!(
        matches!(
            controller.dispatch(AppCommand::RefreshAiSettings),
            AppEvent::AiSettingsLoaded
        ),
        "Agent 刷新应成功"
    );
    let created = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == target)
        .expect("新建 Agent 应出现在列表");
    assert_eq!(created.display_name, "目标 Agent");
    assert_eq!(created.description, "描述");
    assert_eq!(created.system_prompt, "目标提示词");
    assert_eq!(created.revision, 1);

    let before_target = pa_agent_row(&dir, &target);
    let before_other = pa_agent_row(&dir, &other);
    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: Some(target.clone()),
        display_name: "目标 Agent 改名".into(),
        description: before_target.description.clone(),
        system_prompt: before_target.system_prompt.clone(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 1,
    }));
    let AppEvent::AiAgentSaved {
        id: saved_id,
        revision,
    } = event
    else {
        panic!("Agent 编辑失败: {event:?}");
    };
    assert_eq!(saved_id, target);
    assert_eq!(revision, 2);
    let after_edit = pa_agent_row(&dir, &target);
    assert_eq!(after_edit.display_name, "目标 Agent 改名");
    assert_eq!(after_edit.description, before_target.description);
    assert_eq!(after_edit.system_prompt, before_target.system_prompt);
    assert_eq!(after_edit.budget_policy_id, before_target.budget_policy_id);
    assert_eq!(
        after_edit.allowed_tools_json,
        before_target.allowed_tools_json
    );
    assert_eq!(after_edit.revision, 2);
    assert_eq!(
        pa_agent_row(&dir, &other),
        before_other,
        "编辑不得影响其它 Agent"
    );

    let before_blank = pa_agent_row(&dir, &target);
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: Some(target.clone()),
        display_name: "   ".into(),
        description: "不应写入".into(),
        system_prompt: "不应写入".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: false,
        expected_revision: 2,
    })) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("名称不能为空"), "空名称错误: {message}");
        }
        other => panic!("空名称必须被拒，实际: {other:?}"),
    }
    assert_eq!(pa_agent_row(&dir, &target), before_blank, "空名称不得落库");

    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: Some(target.clone()),
        display_name: "过期覆盖".into(),
        description: "不应写入".into(),
        system_prompt: "不应写入".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: false,
        expected_revision: 1,
    })) {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("重新加载"),
                "过期 revision 错误: {message}"
            );
        }
        other => panic!("过期 revision 必须被拒，实际: {other:?}"),
    }
    assert_eq!(
        pa_agent_row(&dir, &target),
        before_blank,
        "CAS 冲突不得落库"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_saved_model_state_matches_public_sqlite_projection_without_secrets() {
    let (mut controller, dir) = controller_with_ai_storage("persist-ui-data");
    let secret = "sk-ui-data-loop-should-not-leak";
    let mut input = sample_model_input(Some(secret));
    input.display_name = "闭环模型".into();
    input.endpoint = Some("https://closed-loop.example.invalid/v1".into());
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(input))
    else {
        panic!("保存闭环模型失败");
    };

    // 保存命令的就地更新路径：不调用 RefreshAiSettings，列表也必须立即包含完整公开字段。
    let item = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("保存后未刷新时列表应已有该行");
    assert_eq!(item.revision, revision);
    let row = pa_model_row(&dir, &id);
    pa_assert_model_item_public_fields(item, &row);
    assert_eq!(item.credential_status, "已配置");
    assert_eq!(row.secret_revision, 1);
    assert!(row.ciphertext.is_some());
    assert!(row.nonce.is_some());

    let state_debug = format!("{:#?}", controller.state().ai_models);
    assert!(!state_debug.contains(secret), "AppState 不得含 Key 明文");
    let lower = state_debug.to_ascii_lowercase();
    assert!(!lower.contains("ciphertext"), "AppState 不得含密文字段");
    assert!(!lower.contains("nonce"), "AppState 不得含 nonce 字段");

    let conn = pa_connection(&dir);
    let public = fluxdb_storage::ai_platform::models::list_models_page(&conn, 200, 0)
        .expect("读取公开模型列表");
    let public_debug = format!("{public:#?}");
    assert!(!public_debug.contains(secret), "公开视图不得含 Key 明文");
    let public_lower = public_debug.to_ascii_lowercase();
    assert!(
        !public_lower.contains("ciphertext"),
        "公开视图不得含密文字段"
    );
    assert!(!public_lower.contains("nonce"), "公开视图不得含 nonce 字段");

    // 刷新后再从公开列表读一次，确保不是只依赖就地更新。
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));
    let refreshed = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("刷新后模型仍应在列表");
    pa_assert_model_item_public_fields(refreshed, &row);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_missing_entities_are_rejected_without_phantom_rows() {
    let (mut controller, dir) = controller_with_ai_storage("persist-missing");
    let sentinel_model = create_model(&mut controller, "openai", "哨兵模型", Some("sk-sentinel"));
    let sentinel_agent = create_agent(&mut controller, "哨兵 Agent", "哨兵提示词");
    let model_before = pa_model_row(&dir, &sentinel_model);
    let agent_before = pa_agent_row(&dir, &sentinel_agent);
    let before_counts = pa_model_counts(&dir);

    let mut missing_model = sample_model_input(Some("sk-missing"));
    missing_model.id = Some("model-does-not-exist".into());
    missing_model.display_name = "不存在".into();
    missing_model.expected_revision = Some(1);
    match controller.dispatch(AppCommand::SaveAiModel(missing_model)) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("不存在"), "缺失模型错误: {message}");
        }
        other => panic!("缺失模型必须被拒，实际: {other:?}"),
    }

    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: Some("agent-does-not-exist".into()),
        display_name: "不存在".into(),
        description: "不应写入".into(),
        system_prompt: "不应写入".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 1,
    })) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("不存在"), "缺失 Agent 错误: {message}");
        }
        other => panic!("缺失 Agent 必须被拒，实际: {other:?}"),
    }

    assert_eq!(pa_model_counts(&dir), before_counts, "不得新增幽灵行");
    assert_eq!(pa_model_row(&dir, &sentinel_model), model_before);
    assert_eq!(pa_agent_row(&dir, &sentinel_agent), agent_before);
    assert_eq!(controller.state().ai_models.len(), 1);
    assert_eq!(controller.state().ai_agents.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_agent_allowed_tools_round_trip_and_narrowing() {
    let (mut controller, dir) = controller_with_ai_storage("agent-tools");
    // 白名单收窄：只允许 db.query。
    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: None,
        display_name: "收窄工具 Agent".into(),
        description: String::new(),
        system_prompt: "只读查询".into(),
        model_profile_id: None,
        allowed_tools: vec!["db.query".into()],
        enabled: true,
        expected_revision: 0,
    }));
    let AppEvent::AiAgentSaved { id, revision } = event else {
        panic!("应返回 AiAgentSaved，实际 {event:?}");
    };
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let item = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == id)
        .expect("Agent 应存在");
    assert_eq!(item.allowed_tools, vec!["db.query".to_string()]);

    // 保存为空 = 不限制：回读为空列表。
    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        id: Some(id.clone()),
        display_name: "收窄工具 Agent".into(),
        description: String::new(),
        system_prompt: "只读查询".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: revision,
    }));
    assert!(
        matches!(event, AppEvent::AiAgentSaved { .. }),
        "取消收窄应保存成功，实际 {event:?}"
    );
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let item = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == id)
        .expect("Agent 应存在");
    assert!(item.allowed_tools.is_empty(), "空白名单 = 不限制");
    let _ = std::fs::remove_dir_all(&dir);
}
