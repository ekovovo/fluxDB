// AI 设置端到端测试（T010–T014）：走真实 AppCommand → 真实 SQLite → 真实 AES。
//
// 覆盖：模型新建（含 Key 加密落库）→ 列表状态 → 启停 CAS → 模型编辑 CAS →
// 明文不外泄 → 密钥可解密回原文。

/// 建一个带真实 FileStorage 的控制器；返回存储根目录。
fn controller_with_ai_storage(label: &str) -> (AppController, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-ai-e2e-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage);
    (controller, dir)
}

fn sample_model_input(api_key: Option<&str>) -> AiModelSaveInput {
    AiModelSaveInput {
        id: None,
        display_name: "主模型 · 高能力".into(),
        provider: "openai".into(),
        model: "main-model".into(),
        endpoint: Some("https://model.example.invalid/v1".into()),
        endpoint_is_full_url: false,
        enabled: true,
        context_window_tokens: Some(400_000),
        max_output_tokens: Some(32_000),
        input_price_micros: Some(2_500_000),
        output_price_micros: Some(10_000_000),
        cached_input_price_micros: Some(250_000),
        api_key: api_key.map(str::to_string),
        clear_key: false,
        expected_revision: None,
    }
}

#[test]
fn ai_settings_refresh_starts_empty() {
    let (mut controller, dir) = controller_with_ai_storage("empty");
    controller.dispatch(AppCommand::RefreshAiSettings);
    assert!(controller.state().ai_models.is_empty(), "全新库不应有模型");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_save_model_with_key_encrypts_and_lists_as_configured() {
    let (mut controller, dir) = controller_with_ai_storage("create");
    let secret = "sk-e2e-do-not-leak-0123456789";

    let event = controller.dispatch(AppCommand::SaveAiModel(sample_model_input(Some(secret))));
    let AppEvent::AiModelSaved { id, revision } = event else {
        panic!("保存模型应返回 AiModelSaved，实际: {event:?}");
    };
    assert_eq!(revision, 1);

    // 列表状态：凭据已配置、上下文与计价回读正确。
    let model = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("模型应出现在列表");
    assert_eq!(model.credential_status, "已配置");
    assert_eq!(model.context_window_tokens, Some(400_000));
    assert_eq!(model.input_price_micros, Some(2_500_000));
    // Endpoint 必须回读（修复「保存了再打开就没」的回归）。
    assert_eq!(
        model.endpoint.as_deref(),
        Some("https://model.example.invalid/v1")
    );
    assert!(model.enabled);

    // 数据库里必须是密文，且明文不出现在任何表/文件中。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().expect("打开 AI 连接");
    let (ciphertext, nonce): (Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT credential_ciphertext, credential_nonce FROM ai_model_profiles WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("读取密文");
    assert_ne!(ciphertext, secret.as_bytes(), "落库内容不能是明文");
    assert_eq!(nonce.len(), 12, "nonce 必须是 12 字节");
    assert!(
        !ciphertext.windows(4).any(|w| w == b"sk-e"),
        "密文中不应出现明文片段"
    );
    for table in ["ai_model_profiles", "ai_agents", "ai_plugin_instances"] {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table}"))
            .expect("准备全表扫描");
        let cols = stmt.column_count();
        let mut rows = stmt.query([]).expect("扫描");
        while let Some(row) = rows.next().expect("行") {
            for i in 0..cols {
                if let Ok(text) = row.get::<_, String>(i) {
                    assert!(
                        !text.contains("sk-e2e-do-not-leak"),
                        "{table} 第 {i} 列泄漏了明文 Key"
                    );
                }
            }
        }
    }

    // 用同一把数据密钥能解回原文（真实加解密往返）。
    let (cipher, _source) =
        fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir()).expect("加载数据密钥");
    let sealed = fluxdb_storage::ai_platform::models::load_encrypted_secret(&conn, &id)
        .expect("读取密文行")
        .expect("应有密文");
    let opened =
        fluxdb_storage::ai_platform::model_secret::open(&cipher, &id, &sealed).expect("解密");
    assert_eq!(opened, secret);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_keep_does_not_change_secret_revision_on_rename() {
    let (mut controller, dir) = controller_with_ai_storage("keep");
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(sample_model_input(Some("sk-keep"))))
    else {
        panic!("新建失败");
    };

    let mut edit = sample_model_input(None);
    edit.id = Some(id.clone());
    edit.display_name = "改名后".into();
    edit.expected_revision = Some(revision);
    let AppEvent::AiModelSaved {
        revision: next_revision,
        ..
    } = controller.dispatch(AppCommand::SaveAiModel(edit))
    else {
        panic!("改名失败");
    };
    assert_eq!(next_revision, 2);
    assert_eq!(
        controller
            .state()
            .ai_models
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .credential_status,
        "已配置",
        "Keep 不应清掉凭据"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_stale_revision_save_is_rejected_and_surfaces_error() {
    let (mut controller, dir) = controller_with_ai_storage("cas");
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(sample_model_input(None)))
    else {
        panic!("新建失败");
    };

    // 先用正确 revision 改一次，让磁盘 revision 前进。
    let mut edit = sample_model_input(None);
    edit.id = Some(id.clone());
    edit.expected_revision = Some(revision);
    let _ = controller.dispatch(AppCommand::SaveAiModel(edit.clone()));

    // 再用过期 revision 保存，必须报错而不是静默覆盖。
    let event = controller.dispatch(AppCommand::SaveAiModel(edit));
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("重新加载"),
                "应提示重新加载，实际: {message}"
            );
        }
        other => panic!("过期 revision 应返回错误，实际: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_toggle_model_enabled_round_trips_to_database() {
    let (mut controller, dir) = controller_with_ai_storage("toggle");
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(sample_model_input(None)))
    else {
        panic!("新建失败");
    };

    let event = controller.dispatch(AppCommand::SetAiModelEnabled {
        id: id.clone(),
        enabled: false,
        expected_revision: revision,
    });
    assert!(
        matches!(event, AppEvent::AiModelEnabled { enabled: false, .. }),
        "应返回停用事件，实际: {event:?}"
    );
    assert!(
        !controller
            .state()
            .ai_models
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .enabled,
        "内存列表应同步为停用"
    );

    // 磁盘状态一致。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM ai_model_profiles WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0, "磁盘应为停用");

    // 再次用过期的 revision 切换必须被拒绝。
    let event = controller.dispatch(AppCommand::SetAiModelEnabled {
        id,
        enabled: true,
        expected_revision: revision,
    });
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "过期 revision 应报错"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_ai_storage_missing_reports_clear_error_instead_of_silent_empty() {
    let mut controller = AppController::new();
    let event = controller.dispatch(AppCommand::RefreshAiSettings);
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("AI 存储"),
                "错误应说明存储未初始化: {message}"
            )
        }
        other => panic!("未注入存储应报错，实际: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Provider → 模型类型 / API 协议映射（Provider 是唯一来源）
// ---------------------------------------------------------------------------

/// 直查 `ai_model_profiles` 行，返回 (provider, api_protocol, model_kind)。
fn stored_model_identity(dir: &std::path::Path, id: &str) -> (String, String, String) {
    let storage = fluxdb_storage::FileStorage::new(dir.to_path_buf());
    let conn = storage.open_ai_connection().expect("打开 AI 连接");
    conn.query_row(
        "SELECT provider, api_protocol, model_kind FROM ai_model_profiles WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("读取模型身份列")
}

#[test]
fn ai_settings_provider_maps_to_protocol_and_kind_in_database() {
    let (mut controller, dir) = controller_with_ai_storage("provider-map");
    let cases = [
        ("openai", "openai-compatible", "generation"),
        ("openai-responses", "openai-responses", "generation"),
        ("anthropic", "anthropic-messages", "generation"),
        ("decisions", "system-one", "decision"),
    ];

    let mut ids = Vec::new();
    for (provider, _, _) in cases {
        let mut input = sample_model_input(None);
        input.provider = provider.into();
        input.display_name = format!("模型 {provider}");
        let AppEvent::AiModelSaved { id, .. } = controller.dispatch(AppCommand::SaveAiModel(input))
        else {
            panic!("保存 {provider} 模型失败");
        };
        ids.push(id);
    }

    for ((provider, protocol, kind), id) in cases.iter().zip(&ids) {
        assert_eq!(
            stored_model_identity(&dir, id),
            (provider.to_string(), protocol.to_string(), kind.to_string()),
            "provider={provider} 落库的协议/类型不符"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_unknown_provider_falls_back_to_openai() {
    let (mut controller, dir) = controller_with_ai_storage("provider-unknown");
    let mut input = sample_model_input(None);
    input.provider = "some-unknown-provider".into();
    let AppEvent::AiModelSaved { id, .. } = controller.dispatch(AppCommand::SaveAiModel(input))
    else {
        panic!("未知 Provider 应按 OpenAI 兼容保存，而不是报错");
    };

    assert_eq!(
        stored_model_identity(&dir, &id),
        (
            "openai".to_string(),
            "openai-compatible".to_string(),
            "generation".to_string()
        ),
        "未知 Provider 必须回退 openai/generation"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_decision_provider_exposes_decision_kind_in_state() {
    let (mut controller, dir) = controller_with_ai_storage("provider-decision");
    let mut input = sample_model_input(Some("sk-jev"));
    input.provider = "decisions".into();
    input.display_name = "Jev 决策模型".into();
    let AppEvent::AiModelSaved { id, .. } = controller.dispatch(AppCommand::SaveAiModel(input))
    else {
        panic!("决策模型保存失败");
    };

    let model = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("决策模型应出现在列表");
    assert_eq!(model.kind, "决策", "decisions 应在状态里呈现为决策模型");
    assert_eq!(model.provider, "decisions");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_editing_name_does_not_change_decision_protocol_or_kind() {
    let (mut controller, dir) = controller_with_ai_storage("provider-edit");
    let mut create = sample_model_input(None);
    create.provider = "decisions".into();
    let AppEvent::AiModelSaved {
        id,
        revision: created_revision,
    } = controller.dispatch(AppCommand::SaveAiModel(create))
    else {
        panic!("新建决策模型失败");
    };

    let mut edit = sample_model_input(None);
    edit.id = Some(id.clone());
    edit.provider = "decisions".into();
    edit.display_name = "改名的决策模型".into();
    edit.expected_revision = Some(created_revision);
    let AppEvent::AiModelSaved { revision, .. } =
        controller.dispatch(AppCommand::SaveAiModel(edit))
    else {
        panic!("改名失败");
    };
    assert_eq!(revision, created_revision + 1);

    assert_eq!(
        stored_model_identity(&dir, &id),
        (
            "decisions".to_string(),
            "system-one".to_string(),
            "decision".to_string()
        ),
        "只改名称不应把决策模型变成生成模型"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_invalid_display_name_or_model_is_rejected_without_touching_stored_row() {
    let (mut controller, dir) = controller_with_ai_storage("provider-validate");
    let mut create = sample_model_input(None);
    create.provider = "openai".into();
    create.display_name = "保留的模型".into();
    let AppEvent::AiModelSaved {
        id,
        revision: created_revision,
    } = controller.dispatch(AppCommand::SaveAiModel(create))
    else {
        panic!("新建失败");
    };

    // 名称为空：用正确 revision 编辑也应被拒。
    let mut blank_name = sample_model_input(None);
    blank_name.id = Some(id.clone());
    blank_name.provider = "openai".into();
    blank_name.display_name = "   ".into();
    blank_name.expected_revision = Some(created_revision);
    match controller.dispatch(AppCommand::SaveAiModel(blank_name)) {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("不能为空"),
                "空名称应提示不能为空: {message}"
            );
        }
        other => panic!("空名称应返回 AiSettingsError，实际: {other:?}"),
    }

    // 模型标识为空：新建即失败。
    let mut blank_model = sample_model_input(None);
    blank_model.provider = "openai".into();
    blank_model.model = String::new();
    match controller.dispatch(AppCommand::SaveAiModel(blank_model)) {
        AppEvent::AiSettingsError(message) => {
            assert!(message.contains("模型标识"), "空模型标识应被拒: {message}");
        }
        other => panic!("空模型标识应返回 AiSettingsError，实际: {other:?}"),
    }

    // 两次失败都不得改动磁盘上已有的行。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    let (name, revision, count): (String, i64, i64) = {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_model_profiles", [], |row| {
                row.get(0)
            })
            .unwrap();
        let (name, revision): (String, i64) = conn
            .query_row(
                "SELECT display_name, revision FROM ai_model_profiles WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (name, revision, count)
    };
    assert_eq!(count, 1, "失败的保存不应插入新行");
    assert_eq!(name, "保留的模型");
    assert_eq!(revision, created_revision, "失败的保存不应推进 revision");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 建一个指定 Provider 的模型，返回新 id（走真实 SaveAiModel）。
fn create_model(
    controller: &mut AppController,
    provider: &str,
    display_name: &str,
    key: Option<&str>,
) -> String {
    let input = AiModelSaveInput {
        id: None,
        display_name: display_name.into(),
        provider: provider.into(),
        model: format!("{provider}-model"),
        endpoint: None,
        endpoint_is_full_url: false,
        enabled: true,
        context_window_tokens: Some(8192),
        max_output_tokens: Some(2048),
        input_price_micros: None,
        output_price_micros: None,
        cached_input_price_micros: None,
        api_key: key.map(str::to_string),
        clear_key: false,
        expected_revision: None,
    };
    match controller.dispatch(AppCommand::SaveAiModel(input)) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("创建模型失败: {other:?}"),
    }
}

#[test]
fn ai_settings_default_models_round_trip_and_validate_kind() {
    let (mut controller, dir) = controller_with_ai_storage("defaults");
    let gen_id = create_model(&mut controller, "openai", "主模型", Some("sk-main"));
    let dec_id = create_model(&mut controller, "decisions", "决策模型", None);

    let event = controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(gen_id.clone()),
        default_decision_model_id: Some(dec_id.clone()),
        expected_revision: 0,
    });
    let AppEvent::AiDefaultModelsSaved { revision } = event else {
        panic!("保存默认模型应返回 AiDefaultModelsSaved，实际: {event:?}");
    };
    assert_eq!(revision, 1);
    assert_eq!(
        controller.state().ai_default_model_id.as_deref(),
        Some(gen_id.as_str())
    );
    assert_eq!(
        controller.state().ai_default_decision_model_id.as_deref(),
        Some(dec_id.as_str())
    );

    // 换一个控制器、复用同一存储目录，验证确实落库、可回读。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let mut reloaded = AppController::new();
    reloaded.set_ai_storage(storage);
    reloaded.dispatch(AppCommand::RefreshAiSettings);
    assert_eq!(
        reloaded.state().ai_default_model_id.as_deref(),
        Some(gen_id.as_str()),
        "默认主模型应持久化"
    );
    assert_eq!(
        reloaded.state().ai_default_decision_model_id.as_deref(),
        Some(dec_id.as_str()),
        "默认决策模型应持久化"
    );
    assert_eq!(reloaded.state().ai_runtime_revision, 1);

    // 错误 kind：决策模型不能当默认主模型。
    let event = controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(dec_id.clone()),
        default_decision_model_id: None,
        expected_revision: 1,
    });
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("生成模型"),
                "错误信息应说明 kind: {message}"
            );
        }
        other => panic!("决策模型作为主模型应被拒，实际: {other:?}"),
    }

    // 过期 revision：真实已是 1，这里用 0。
    let event = controller.dispatch(AppCommand::SaveAiDefaultModels {
        default_model_id: Some(gen_id.clone()),
        default_decision_model_id: None,
        expected_revision: 0,
    });
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "过期 revision 保存必须被拒，实际: {event:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_test_ai_model_missing_key_without_network() {
    let (mut controller, dir) = controller_with_ai_storage("probe");

    // openai + ApiKey 认证但没有密文 → missing_key（不触网）。
    let missing_id = {
        let input = AiModelSaveInput {
            id: None,
            display_name: "缺 Key 主模型".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            endpoint: Some("https://example.invalid/v1".into()),
            endpoint_is_full_url: false,
            enabled: true,
            context_window_tokens: Some(8192),
            max_output_tokens: Some(2048),
            input_price_micros: None,
            output_price_micros: None,
            cached_input_price_micros: None,
            api_key: None,
            clear_key: true,
            expected_revision: None,
        };
        match controller.dispatch(AppCommand::SaveAiModel(input)) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("创建无 Key 模型失败: {other:?}"),
        }
    };
    let event = controller.dispatch(AppCommand::TestAiModel {
        id: missing_id.clone(),
    });
    let AppEvent::AiModelTested { id, status } = event else {
        panic!("测试连接应返回 AiModelTested，实际: {event:?}");
    };
    assert_eq!(id, missing_id);
    assert_eq!(status, "missing_key");

    // 决策模型同样要求 API Key，不使用聊天协议假探测。
    let decision_id = create_model(&mut controller, "decisions", "决策模型", None);
    let event = controller.dispatch(AppCommand::TestAiModel {
        id: decision_id.clone(),
    });
    let AppEvent::AiModelTested { id, status } = event else {
        panic!("测试连接应返回 AiModelTested，实际: {event:?}");
    };
    assert_eq!(id, decision_id);
    assert_eq!(status, "missing_key");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 建一个 Agent，返回新 id（走真实 SaveAiAgent）。
fn create_agent(controller: &mut AppController, display_name: &str, system_prompt: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: None,
        display_name: display_name.into(),
        description: "描述".into(),
        system_prompt: system_prompt.into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 0,
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => panic!("创建 Agent 失败: {other:?}"),
    }
}

#[test]
fn ai_settings_agent_create_round_trip() {
    let (mut controller, dir) = controller_with_ai_storage("agent-create");
    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: None,
        display_name: "通用数据库 Agent".into(),
        description: "探索并交付可追溯结果".into(),
        system_prompt: "先确认真实结构再行动。".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 0,
    }));
    let AppEvent::AiAgentSaved { id, revision } = event else {
        panic!("新建 Agent 应返回 AiAgentSaved，实际: {event:?}");
    };
    assert_eq!(revision, 1);

    // 刷新后能从列表读回，字段一致。
    controller.dispatch(AppCommand::RefreshAiSettings);
    let agent = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == id)
        .expect("Agent 应出现在列表");
    assert_eq!(agent.display_name, "通用数据库 Agent");
    assert_eq!(agent.description, "探索并交付可追溯结果");
    assert_eq!(agent.system_prompt, "先确认真实结构再行动。");
    assert_eq!(agent.model_profile_id, None);
    assert!(agent.enabled);
    assert_eq!(agent.revision, 1);

    // system_prompt 真正落到 ai_agents 行。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    let prompt: String = conn
        .query_row(
            "SELECT system_prompt FROM ai_agents WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prompt, "先确认真实结构再行动。");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_agent_edit_updates_prompt_and_revision() {
    let (mut controller, dir) = controller_with_ai_storage("agent-edit");
    let id = create_agent(&mut controller, "旧名", "旧提示词");

    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: Some(id.clone()),
        display_name: "新名".into(),
        description: "新描述".into(),
        system_prompt: "新提示词".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: false,
        expected_revision: 1,
    }));
    let AppEvent::AiAgentSaved {
        id: saved_id,
        revision,
    } = event
    else {
        panic!("编辑 Agent 应返回 AiAgentSaved，实际: {event:?}");
    };
    assert_eq!(saved_id, id);
    assert_eq!(revision, 2, "编辑后 revision 应自增");

    // 刷新后仍与保存值一致（真正落库）。
    controller.dispatch(AppCommand::RefreshAiSettings);
    let agent = controller
        .state()
        .ai_agents
        .iter()
        .find(|a| a.id == id)
        .expect("Agent 应仍在列表");
    assert_eq!(agent.display_name, "新名");
    assert_eq!(agent.description, "新描述");
    assert_eq!(agent.system_prompt, "新提示词");
    assert!(!agent.enabled);
    assert_eq!(agent.revision, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_agent_blank_name_rejected() {
    let (mut controller, dir) = controller_with_ai_storage("agent-blank");
    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: None,
        display_name: "   ".into(),
        description: "".into(),
        system_prompt: "".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 0,
    }));
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("名称不能为空"),
                "错误信息应说明原因: {message}"
            );
        }
        other => panic!("空名称应返回 AiSettingsError，实际: {other:?}"),
    }
    // 初始化已注册内置身份；失败的保存不能插入任何自定义 Agent。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_agents WHERE id<>?1",
            [fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "空名称不应落库");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_agent_stale_revision_rejected() {
    let (mut controller, dir) = controller_with_ai_storage("agent-cas");
    let id = create_agent(&mut controller, "并发", "提示词");

    let event = controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: Some(id.clone()),
        display_name: "改名".into(),
        description: "描述".into(),
        system_prompt: "提示词".into(),
        model_profile_id: None,
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 99,
    }));
    match event {
        AppEvent::AiSettingsError(message) => {
            assert!(
                message.contains("重新加载"),
                "过期 revision 应提示重新加载: {message}"
            );
        }
        other => panic!("过期 revision 应返回 AiSettingsError，实际: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_fetch_models_rejects_blank_key_and_system_one_without_network() {
    // 空 Key：不发请求，直接给出可读错误。
    let err = fetch_ai_models("openai", Some("https://model.example.invalid/v1"), "  ")
        .expect_err("空 API Key 应被拒绝");
    assert!(err.message.contains("API Key"), "错误信息: {}", err.message);

    // decisions（system-one）不支持拉取模型列表，同样不联网。
    let err = fetch_ai_models("decisions", None, "sk-whatever").expect_err("system-one 应被拒绝");
    assert!(err.message.contains("不支持"), "错误信息: {}", err.message);
}

#[test]
fn ai_settings_reveal_model_secret_only_when_configured() {
    let (mut controller, dir) = controller_with_ai_storage("reveal");
    // 配置了 Key 的模型：能原样解出明文。
    let with_key = create_model(&mut controller, "openai", "带 Key", Some("sk-reveal-me"));
    let revealed = controller
        .reveal_ai_model_secret(&with_key)
        .expect("已配置 Key 的模型应可显示");
    assert_eq!(revealed, "sk-reveal-me");

    // 未配置 Key 的模型：明确报错，不返回空串。
    let without_key = create_model(&mut controller, "openai", "无 Key", None);
    let err = controller
        .reveal_ai_model_secret(&without_key)
        .expect_err("未配置 Key 的模型应报错");
    assert!(
        err.message.contains("没有可显示的 API Key"),
        "错误信息应说明无 Key: {}",
        err.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_model_connection_status_persists_and_reloads() {
    let (mut controller, dir) = controller_with_ai_storage("conn-status");
    let id = create_model(&mut controller, "openai", "连接状态模型", Some("sk-cs"));

    // 直接走存储层写状态（不 bump revision）。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    fluxdb_storage::ai_platform::models::set_connection_status(&conn, 12_345, &id, "ok")
        .expect("写入连接状态");
    // 非法状态被拒绝。
    assert!(
        fluxdb_storage::ai_platform::models::set_connection_status(&conn, 1, &id, "bogus").is_err(),
        "非法连接状态必须被拒"
    );

    controller.dispatch(AppCommand::RefreshAiSettings);
    let item = controller
        .state()
        .ai_models
        .iter()
        .find(|m| m.id == id)
        .expect("模型应在列表");
    assert_eq!(item.connection_status, "ok");
    assert_eq!(item.connection_checked_at_ms, Some(12_345));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_settings_test_ai_model_network_failure_is_persisted() {
    let (mut controller, dir) = controller_with_ai_storage("probe-fail");
    // 指向一个必然连不上的本地端口：离线也能稳定复现网络失败路径。
    let id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        id: None,
        display_name: "连通性失败模型".into(),
        provider: "openai".into(),
        model: "gpt-4o".into(),
        endpoint: Some("http://127.0.0.1:1/v1".into()),
        endpoint_is_full_url: false,
        enabled: true,
        context_window_tokens: Some(8192),
        max_output_tokens: Some(2048),
        input_price_micros: None,
        output_price_micros: None,
        cached_input_price_micros: None,
        api_key: Some("sk-offline-test".into()),
        clear_key: false,
        expected_revision: None,
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("创建模型失败: {other:?}"),
    };

    let event = controller.dispatch(AppCommand::TestAiModel { id: id.clone() });
    let AppEvent::AiModelTested { id: got_id, status } = event else {
        panic!("测试连接应返回 AiModelTested，实际: {event:?}");
    };
    assert_eq!(got_id, id);
    assert!(
        status.starts_with("failed:"),
        "离线探测应以 failed: 开头，实际: {status}"
    );

    // 失败状态落到 ai_model_profiles。
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    let (conn_status, checked): (String, Option<i64>) = conn
        .query_row(
            "SELECT connection_status, connection_checked_at_ms FROM ai_model_profiles WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(conn_status, "failed");
    assert!(checked.is_some(), "失败也应记录探测时间");
    let _ = std::fs::remove_dir_all(&dir);
}

include!("ai_model_network.rs");

include!("ai_model_delete.rs");
