// AI 设置页的读写编排：加载模型/Agent/插件/MCP 列表与启停切换（T013/T014）。
// UI 只发 AppCommand；本文件在控制器副本上访问存储，UI 不接触 SQLite 或凭据。
// 该文件以 include! 引入 crate 根作用域，Error/Result 等已在上方导入。

use fluxdb_storage::ai_platform::{agents, mcp, models, plugins, settings as ai_settings};
use rusqlite::Connection as AiConn;

impl AppController {
    /// 克隆当前 AI 存储句柄；未注入时给出明确错误而不是静默空列表。
    ///
    /// 返回克隆值而不是借用，调用方可在持有它的同时修改 `AppState`。
    pub fn ai_storage(&self) -> fluxdb_core::Result<fluxdb_storage::FileStorage> {
        self.ai_storage.clone().ok_or_else(|| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "AI 存储尚未初始化，请重启应用",
            )
        })
    }

    /// 显式解密模型 API Key，供编辑弹框「眼睛」按钮按需调用。
    ///
    /// 明文只作为本函数返回值短暂存在：不进入 `AppEvent`/`AppState`、任何 DTO、
    /// 日志或错误文本；调用方用完应立即丢弃。
    pub fn reveal_ai_model_secret(&self, id: &str) -> fluxdb_core::Result<String> {
        use fluxdb_storage::ai_platform::model_secret::CredentialStatus;

        let no_key_error = || {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "该模型没有可显示的 API Key",
            )
        };

        let storage = self.ai_storage()?;
        let (view, sealed) = with_ai_storage(&storage, |conn| {
            Ok((
                models::load_model(conn, id)?,
                models::load_encrypted_secret(conn, id)?,
            ))
        })?;
        if view.credential_status != CredentialStatus::Configured {
            return Err(no_key_error());
        }
        let Some(sealed) = sealed else {
            return Err(no_key_error());
        };
        let (cipher, _source) = fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir())
            .map_err(|e| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
            })?;
        fluxdb_storage::ai_platform::model_secret::open(&cipher, id, &sealed).map_err(|e| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
        })
    }
}

/// 在 AI 存储连接上执行一次操作（存储层负责开关连接）。
fn with_ai_storage<T>(
    storage: &fluxdb_storage::FileStorage,
    f: impl FnOnce(&AiConn) -> fluxdb_core::Result<T>,
) -> fluxdb_core::Result<T> {
    storage.with_ai_connection(|conn| f(conn).map_err(Into::into))
}

/// 加载设置页需要的全部 AI 列表（模型/Agent/插件/MCP）。
pub fn refresh_ai_settings(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
) -> fluxdb_core::Result<()> {
    let (models, agents_list, plugins_list, mcp_list, runtime) =
        with_ai_storage(&storage, |conn| {
            Ok((
                models::list_models_page(conn, 200, 0)?,
                agents::list_agents(conn)?,
                plugins::list_plugin_instances(conn)?,
                mcp::list_mcp_settings(conn)?,
                ai_settings::load_runtime_settings(conn)?,
            ))
        })?;
    state.ai_models = models
        .into_iter()
        .map(|m| AiModelItem {
            id: m.id,
            display_name: m.display_name,
            provider: m.provider,
            model: m.model,
            endpoint: m.endpoint,
            kind: match m.kind {
                fluxdb_storage::ai_platform::models::ModelKind::Generation => "生成".into(),
                fluxdb_storage::ai_platform::models::ModelKind::Decision => "决策".into(),
            },
            enabled: m.enabled,
            context_window_tokens: m.context_window_tokens,
            max_output_tokens: m.max_output_tokens,
            input_price_micros: m.input_price_micros_per_million,
            output_price_micros: m.output_price_micros_per_million,
            cached_input_price_micros: m.cached_input_price_micros_per_million,
            credential_status: match m.credential_status {
                fluxdb_storage::ai_platform::model_secret::CredentialStatus::NotRequired => {
                    "无需凭据".into()
                }
                fluxdb_storage::ai_platform::model_secret::CredentialStatus::Missing => {
                    "缺少凭据".into()
                }
                fluxdb_storage::ai_platform::model_secret::CredentialStatus::Configured => {
                    "已配置".into()
                }
                fluxdb_storage::ai_platform::model_secret::CredentialStatus::Unavailable => {
                    "不可用".into()
                }
            },
            connection_status: m.connection_status,
            connection_checked_at_ms: m.connection_checked_at_ms,
            revision: m.revision,
        })
        .collect();
    state.ai_agents = agents_list
        .into_iter()
        .map(|a| AiAgentItem {
            id: a.id,
            display_name: a.display_name,
            description: a.description,
            system_prompt: a.system_prompt,
            model_profile_id: a.default_model_profile_id,
            enabled: a.enabled,
            revision: a.revision,
        })
        .collect();
    state.ai_plugin_instances = plugins_list
        .into_iter()
        .map(|p| {
            let mandatory = plugins::is_mandatory(&p.id);
            AiPluginItem {
                id: p.id,
                display_name: p.display_name,
                package_key: p.package_key,
                enabled: p.enabled,
                mandatory,
                revision: p.revision,
            }
        })
        .collect();
    state.ai_mcp_settings = mcp_list
        .into_iter()
        .map(|m| AiMcpItem {
            id: m.id,
            display_name: m.display_name,
            transport: m.transport,
            enabled: m.enabled,
            revision: m.revision,
        })
        .collect();
    // 默认模型选择与运行设置 revision 一起回填，供「默认主/决策模型」选择器使用。
    state.ai_default_model_id = runtime.default_model_profile_id.clone();
    state.ai_default_decision_model_id = runtime.default_decision_model_profile_id.clone();
    state.ai_runtime_revision = runtime.revision;
    Ok(())
}

/// 切换模型启用状态（对象级 CAS）。
pub fn set_ai_model_enabled(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    id: &str,
    enabled: bool,
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<()> {
    with_ai_storage(storage, |conn| {
        models::set_model_enabled(conn, now_ms, id, enabled, expected_revision)
    })?;
    // 就地更新列表，避免整页重载闪烁。
    if let Some(m) = state.ai_models.iter_mut().find(|m| m.id == id) {
        m.enabled = enabled;
        m.revision = expected_revision + 1;
    }
    Ok(())
}

/// 切换内置插件实例启停。
pub fn set_ai_plugin_enabled(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    id: &str,
    enabled: bool,
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<()> {
    with_ai_storage(storage, |conn| {
        plugins::set_plugin_instance_enabled(conn, now_ms, id, enabled, expected_revision)
    })?;
    if let Some(p) = state
        .ai_plugin_instances
        .iter_mut()
        .find(|p| p.id == id)
    {
        p.enabled = enabled;
        p.revision = expected_revision + 1;
    }
    Ok(())
}

/// 读取「询问 AI」面板偏好（打开面板时用）。
pub fn load_panel_preferences(
    storage: &fluxdb_storage::FileStorage,
) -> fluxdb_core::Result<ai_settings::PanelPreferences> {
    with_ai_storage(storage, |conn| ai_settings::load_panel_preferences(conn))
}

/// 可选模型 Provider。
///
/// Provider 是模型类型与协议的**唯一来源**：UI 不再让用户单独选「模型用途」，
/// 选了 `decisions` 即表示 Jev 决策模型（§18 内置可选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProvider {
    OpenAi,
    Anthropic,
    Decisions,
}

/// Provider 稳定标识（落库到 `ai_model_profiles.provider`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    OpenAi,
    Anthropic,
    Decisions,
}

impl ModelProvider {
    pub const ALL: [ModelProvider; 3] = [
        ModelProvider::OpenAi,
        ModelProvider::Anthropic,
        ModelProvider::Decisions,
    ];

    /// UI 展示名。
    pub fn label(self) -> &'static str {
        match self {
            ModelProvider::OpenAi => "openai",
            ModelProvider::Anthropic => "anthropic",
            ModelProvider::Decisions => "decisions",
        }
    }

    pub fn id(self) -> ProviderId {
        match self {
            ModelProvider::OpenAi => ProviderId::OpenAi,
            ModelProvider::Anthropic => ProviderId::Anthropic,
            ModelProvider::Decisions => ProviderId::Decisions,
        }
    }

    /// 解析落库字符串；未知值回退 OpenAI（老数据兼容）。
    pub fn from_stored(value: &str) -> Self {
        match value {
            "anthropic" => ModelProvider::Anthropic,
            "decisions" => ModelProvider::Decisions,
            _ => ModelProvider::OpenAi,
        }
    }

    fn api_protocol(self) -> &'static str {
        match self {
            ModelProvider::OpenAi => "openai-compatible",
            ModelProvider::Anthropic => "anthropic-messages",
            ModelProvider::Decisions => "system-one",
        }
    }
}

/// 按 Provider 推导模型类型与协议。
pub fn resolve_provider(
    provider: &str,
) -> (
    fluxdb_storage::ai_platform::models::ModelKind,
    &'static str,
) {
    let p = ModelProvider::from_stored(provider);
    let kind = match p {
        // Jev 决策模型：只做 Choice/Score 分类，不做工具调用。
        ModelProvider::Decisions => fluxdb_storage::ai_platform::models::ModelKind::Decision,
        ModelProvider::OpenAi | ModelProvider::Anthropic => {
            fluxdb_storage::ai_platform::models::ModelKind::Generation
        }
    };
    (kind, p.api_protocol())
}

/// 新建 id：AI 实体使用 UUID 文本（连接仍是 INTEGER ID）。
fn new_entity_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// 保存模型档案：新建或按 revision 更新，Key 走 Keep/Set/Clear。
///
/// 明文 Key 只在本次调用内进入加解密路径，不写日志/事件/状态。
pub fn save_ai_model(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    input: &AiModelSaveInput,
) -> fluxdb_core::Result<(String, i64)> {
    let root = storage.root_dir();
    let (cipher, _source) = fluxdb_storage::ai_platform::load_ai_cipher(root)
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let auth_mode = if input.api_key.is_some() || input.clear_key {
        fluxdb_storage::ai_platform::models::AuthMode::ApiKey
    } else {
        // 无 Key 变更时沿用原认证方式；新建默认无需凭据。
        match input.expected_revision {
            None => fluxdb_storage::ai_platform::models::AuthMode::None,
            Some(rev) => state
                .ai_models
                .iter()
                .find(|m| m.id == input.id.clone().unwrap_or_default())
                .filter(|m| m.revision == rev)
                .map(|_| fluxdb_storage::ai_platform::models::AuthMode::ApiKey)
                .unwrap_or(fluxdb_storage::ai_platform::models::AuthMode::None),
        }
    };
    let change = if let Some(key) = &input.api_key {
        let secret = fluxdb_storage::ai_platform::model_secret::SecretInput::new(key.clone())
            .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
        fluxdb_storage::ai_platform::models::ApiKeyChange::Set(secret)
    } else if input.clear_key {
        fluxdb_storage::ai_platform::models::ApiKeyChange::Clear
    } else {
        fluxdb_storage::ai_platform::models::ApiKeyChange::Keep
    };

    let provider = ModelProvider::from_stored(&input.provider);
    let (kind, api_protocol) = resolve_provider(input.provider.as_str());
    let edit = fluxdb_storage::ai_platform::models::ModelEdit {
        id: input.id.clone().unwrap_or_else(|| new_entity_id("model")),
        display_name: input.display_name.clone(),
        provider: provider.label().to_string(),
        api_protocol: api_protocol.to_string(),
        kind,
        model: input.model.clone(),
        endpoint: input.endpoint.clone(),
        enabled: input.enabled,
        auth_mode,
        context_window_tokens: input.context_window_tokens,
        max_output_tokens: input.max_output_tokens,
        input_price_micros_per_million: input.input_price_micros,
        output_price_micros_per_million: input.output_price_micros,
        cached_input_price_micros_per_million: input.cached_input_price_micros,
    };

    let saved = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::models::save_model(
            conn,
            &cipher,
            "k1",
            now,
            &edit,
            input.expected_revision,
            change,
        )
    })?;
    let (id, revision) = (saved.id.clone(), saved.revision);
    // 就地更新列表，避免整页刷新闪烁。
    match state.ai_models.iter_mut().find(|m| m.id == id) {
        Some(existing) => {
            existing.display_name = saved.display_name;
            existing.enabled = saved.enabled;
            existing.provider = saved.provider;
            existing.model = saved.model;
            existing.endpoint = saved.endpoint;
            // Provider 变化会同时改变模型类型（decisions=决策模型），就地列表必须同步。
            existing.kind = if saved.kind == fluxdb_storage::ai_platform::models::ModelKind::Decision {
                "决策".into()
            } else {
                "生成".into()
            };
            existing.context_window_tokens = saved.context_window_tokens;
            existing.max_output_tokens = saved.max_output_tokens;
            existing.input_price_micros = saved.input_price_micros_per_million;
            existing.output_price_micros = saved.output_price_micros_per_million;
            existing.cached_input_price_micros = saved.cached_input_price_micros_per_million;
            existing.revision = revision;
            existing.credential_status = credential_label(saved.credential_status);
            existing.connection_status = saved.connection_status;
            existing.connection_checked_at_ms = saved.connection_checked_at_ms;
        }
        None => state.ai_models.push(AiModelItem {
            id: id.clone(),
            display_name: saved.display_name,
            provider: saved.provider,
            model: saved.model,
            endpoint: saved.endpoint,
            kind: if saved.kind == fluxdb_storage::ai_platform::models::ModelKind::Decision {
                "决策".into()
            } else {
                "生成".into()
            },
            enabled: saved.enabled,
            context_window_tokens: saved.context_window_tokens,
            max_output_tokens: saved.max_output_tokens,
            input_price_micros: saved.input_price_micros_per_million,
            output_price_micros: saved.output_price_micros_per_million,
            cached_input_price_micros: saved.cached_input_price_micros_per_million,
            credential_status: credential_label(saved.credential_status),
            connection_status: saved.connection_status,
            connection_checked_at_ms: saved.connection_checked_at_ms,
            revision,
        }),
    }
    Ok((id, revision))
}

fn credential_label(status: fluxdb_storage::ai_platform::model_secret::CredentialStatus) -> String {
    use fluxdb_storage::ai_platform::model_secret::CredentialStatus as S;
    match status {
        S::NotRequired => "无需凭据".into(),
        S::Missing => "缺少凭据".into(),
        S::Configured => "已配置".into(),
        S::Unavailable => "不可用".into(),
    }
}

/// 新建或保存 Agent 配置（提示词直接随 Agent 行 CAS）。
///
/// `input.id` 为 `None` 走新建，`Some` 走带 `expected_revision` 的对象级 CAS。
/// 成功后返回 agent id，并就地更新 `state.ai_agents`（新增则 push），避免整页重载。
pub fn save_ai_agent(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    input: AiAgentSaveInput,
    now_ms: i64,
) -> fluxdb_core::Result<String> {
    use fluxdb_storage::ai_platform::agents;

    let display_name = input.display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "名称不能为空",
        ));
    }

    let saved = with_ai_storage(storage, |conn| {
        // 编辑时先读当前行，保留本表单不编辑的预算策略与允许工具，避免误清空。
        let existing = match &input.id {
            Some(id) => Some(agents::load_agent(conn, id)?),
            None => None,
        };
        let edit = agents::AgentEdit {
            id: input.id.clone().unwrap_or_else(|| new_entity_id("agent")),
            display_name: display_name.clone(),
            description: input.description.clone(),
            enabled: input.enabled,
            default_model_profile_id: input.model_profile_id.clone(),
            system_prompt: input.system_prompt.clone(),
            budget_policy_id: existing.as_ref().and_then(|a| a.budget_policy_id.clone()),
            allowed_tools: existing
                .as_ref()
                .map(|a| a.allowed_tools.clone())
                .unwrap_or_default(),
        };
        match &input.id {
            None => agents::create_agent(conn, now_ms, &edit),
            Some(_) => agents::save_agent(conn, now_ms, &edit, input.expected_revision),
        }
    })?;

    let id = saved.id.clone();
    let item = AiAgentItem {
        id: saved.id,
        display_name: saved.display_name,
        description: saved.description,
        system_prompt: saved.system_prompt,
        model_profile_id: saved.default_model_profile_id,
        enabled: saved.enabled,
        revision: saved.revision,
    };
    match state.ai_agents.iter_mut().find(|a| a.id == id) {
        Some(existing) => *existing = item,
        None => state.ai_agents.push(item),
    }
    Ok(id)
}

/// 保存默认主模型 / 默认决策模型选择（运行设置单例 CAS）。
///
/// 应用层强校验：主模型必须是存在的 `generation`，决策模型必须是存在的 `decision`；
/// 其余运行设置字段（默认 Agent、最大活动任务数）保持原值不动。
pub fn save_ai_default_models(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    default_model_id: Option<String>,
    default_decision_model_id: Option<String>,
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<i64> {
    use fluxdb_storage::ai_platform::models::ModelKind;
    let saved = with_ai_storage(storage, |conn| {
        if let Some(id) = &default_model_id {
            let model = models::load_model(conn, id)?;
            if model.kind != ModelKind::Generation {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "默认主模型必须是生成模型",
                ));
            }
        }
        if let Some(id) = &default_decision_model_id {
            let model = models::load_model(conn, id)?;
            if model.kind != ModelKind::Decision {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "默认决策模型必须是决策模型",
                ));
            }
        }
        let current = ai_settings::load_runtime_settings(conn)?;
        let next = ai_settings::RuntimeSettings {
            default_agent_id: current.default_agent_id.clone(),
            default_model_profile_id: default_model_id.clone(),
            default_decision_model_profile_id: default_decision_model_id.clone(),
            max_active_turns: current.max_active_turns,
            revision: current.revision,
        };
        ai_settings::save_runtime_settings(conn, now_ms, &next, expected_revision)
    })?;
    // 就地回填，避免整页刷新闪烁。
    state.ai_default_model_id = default_model_id;
    state.ai_default_decision_model_id = default_decision_model_id;
    state.ai_runtime_revision = saved.revision;
    Ok(saved.revision)
}

/// 测试模型连接：每个协议发一条最小聊天请求（提示词「你好」），并落库连接状态。
///
/// 返回 `(id, status)`，status ∈ `ok` / `missing_key` / `not_required` /
/// `unsupported` / `failed:<一句话中文原因>`；任何分支都不会把 API Key 写入结果。
/// 2xx → 落库 `ok`；非 2xx 或网络错误 → 落库 `failed`；
/// `missing_key`/`not_required`/`unsupported` 不落库（保持 `unknown`）。
pub fn test_ai_model(
    _state: &AppState,
    storage: &fluxdb_storage::FileStorage,
    id: &str,
) -> fluxdb_core::Result<(String, String)> {
    use fluxdb_storage::ai_platform::model_secret::CredentialStatus;

    // 只读模型公开视图与密文材料；密文只在本地解密，不进入事件/日志。
    let (view, sealed) = with_ai_storage(storage, |conn| {
        Ok((
            models::load_model(conn, id)?,
            models::load_encrypted_secret(conn, id)?,
        ))
    })?;

    if view.credential_status == CredentialStatus::Missing {
        return Ok((id.to_string(), "missing_key".to_string()));
    }
    let protocol = view.api_protocol.as_str();
    if protocol != "openai-compatible" && protocol != "anthropic-messages" {
        // system-one（Jev 决策）等协议本期不做联网探测。
        return Ok((id.to_string(), "unsupported".to_string()));
    }
    if view.credential_status == CredentialStatus::NotRequired {
        return Ok((id.to_string(), "not_required".to_string()));
    }
    let Some(sealed) = sealed else {
        return Ok((id.to_string(), "missing_key".to_string()));
    };
    let (cipher, _source) = fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir())
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    let key = fluxdb_storage::ai_platform::model_secret::open(&cipher, id, &sealed)
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;

    // base_url：优先模型行 endpoint，否则按协议取官方默认。
    let default_base = match protocol {
        "anthropic-messages" => "https://api.anthropic.com",
        _ => "https://api.openai.com",
    };
    let base = view
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_base)
        .trim_end_matches('/')
        .to_string();

    // 最小聊天请求：提示词固定「你好」，只用于连通性验证。
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();

    // ureq 未启用 json feature，这里自行序列化并显式带 Content-Type。
    let (url, body) = if protocol == "anthropic-messages" {
        (
            format!("{base}/v1/messages"),
            serde_json::json!({
                "model": view.model,
                "max_tokens": 8,
                "messages": [{ "role": "user", "content": "你好" }],
            }),
        )
    } else {
        (
            format!("{base}/v1/chat/completions"),
            serde_json::json!({
                "model": view.model,
                "messages": [{ "role": "user", "content": "你好" }],
                "max_tokens": 8,
                "stream": false,
            }),
        )
    };
    let body = serde_json::to_string(&body).map_err(|e| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("测试请求序列化失败: {e}"),
        )
    })?;
    let request = agent
        .post(&url)
        .header("Content-Type", "application/json");
    let response = if protocol == "anthropic-messages" {
        request
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send(body)
    } else {
        request
            .header("Authorization", &format!("Bearer {key}"))
            .send(body)
    };

    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                write_connection_status(storage, id, "ok")?;
                Ok((id.to_string(), "ok".to_string()))
            } else {
                write_connection_status(storage, id, "failed")?;
                Ok((id.to_string(), format!("failed:HTTP {status}")))
            }
        }
        // 只映射为固定中文原因，绝不透出底层错误文本（避免任何凭据外泄风险）。
        Err(error) => {
            write_connection_status(storage, id, "failed")?;
            Ok((
                id.to_string(),
                format!("failed:{}", ai_net_error_reason(&error)),
            ))
        }
    }
}

/// 记录一次连接探测结果（运行时元数据，不改配置 revision）。
fn write_connection_status(
    storage: &fluxdb_storage::FileStorage,
    id: &str,
    status: &str,
) -> fluxdb_core::Result<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    with_ai_storage(storage, |conn| {
        models::set_connection_status(conn, now_ms, id, status)
    })
}

/// 拉取 Provider 的可用模型 ID 列表（设置页「模型 ID」下拉用）。
///
/// `provider` 用存储里的 Provider 标识（`openai`/`anthropic`/`decisions`）。
/// `api_key` 只作为本次请求头使用：不落库、不写日志、不进入任何事件或错误文本。
pub fn fetch_ai_models(
    provider: &str,
    endpoint: Option<&str>,
    api_key: &str,
) -> fluxdb_core::Result<Vec<String>> {
    let provider = ModelProvider::from_stored(provider);
    let protocol = provider.api_protocol();
    if protocol == "system-one" {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "该 Provider 不支持拉取模型列表，请手工填写模型 ID",
        ));
    }
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "请先填写 API Key 再拉取模型列表",
        ));
    }
    let default_base = match protocol {
        "anthropic-messages" => "https://api.anthropic.com",
        _ => "https://api.openai.com",
    };
    let base = endpoint
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_base);
    let url = format!("{}/v1/models", base.trim_end_matches('/'));

    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(8)))
        .build()
        .new_agent();
    let request = agent.get(&url);
    let request = if protocol == "anthropic-messages" {
        request
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
    } else {
        request.header("Authorization", &format!("Bearer {api_key}"))
    };
    let mut response = match request.call() {
        Ok(response) => response,
        // 只映射为固定中文原因，绝不透出底层错误文本。
        Err(error) => {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                ai_net_error_reason(&error),
            ));
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("拉取模型列表失败：HTTP {status}"),
        ));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "读取响应失败"))?;
    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "响应不是合法 JSON")
    })?;
    // OpenAI 兼容与 Anthropic 都用 `{"data":[{"id": "..."}]}`；兼容 `models[].id`。
    let mut models: Vec<String> = parsed
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| parsed.get("models").and_then(|v| v.as_array()))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "未返回任何模型 ID，请检查 Endpoint 与 API Key",
        ));
    }
    Ok(models)
}

/// 把 ureq 错误映射为不含敏感信息的中文原因。
fn ai_net_error_reason(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::StatusCode(code) => match code {
            401 | 403 => "鉴权失败",
            404 => "接口不存在",
            _ => "服务返回错误",
        },
        ureq::Error::Timeout(_) => "连接超时",
        ureq::Error::HostNotFound => "域名解析失败",
        ureq::Error::ConnectionFailed => "无法连接服务器",
        ureq::Error::Io(_) => "网络读写失败",
        ureq::Error::BadUri(_) => "请求地址无效",
        _ => "网络请求失败",
    }
}
