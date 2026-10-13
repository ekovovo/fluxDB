// AI 设置页的读写编排：加载模型/Agent/插件/MCP 列表与启停切换（T013/T014）。
// UI 只发 AppCommand；本文件在控制器副本上访问存储，UI 不接触 SQLite 或凭据。
// 该文件以 include! 引入 crate 根作用域，Error/Result 等已在上方导入。

use fluxdb_storage::ai_platform::{
    agents, mcp, models, plugins, settings as ai_settings, tool_settings,
};
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
        fluxdb_storage::ai_platform::model_secret::open(&cipher, id, &sealed)
            .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))
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
    let (models, agents_list, plugins_list, mcp_list, sessions_list, runtime, tools_list, privacy) =
        with_ai_storage(&storage, |conn| {
            Ok((
                models::list_models_page(conn, 200, 0)?,
                agents::list_agents(conn)?,
                plugins::list_plugin_instances(conn)?,
                mcp::list_mcp_settings(conn)?,
                fluxdb_storage::ai_platform::sessions::list_session_overviews(conn, 200)?,
                ai_settings::load_runtime_settings(conn)?,
                tool_settings::list_tool_settings(conn)?,
                ai_settings::load_privacy_policy(conn)?,
            ))
        })?;
    state.ai_models = models
        .into_iter()
        .map(|m| {
            let capability_labels = crate::model_capability_labels(&m)
                .into_iter()
                .map(str::to_string)
                .collect();
            AiModelItem {
            id: m.id,
            display_name: m.display_name,
            provider: m.provider,
            model: m.model,
            endpoint: m.endpoint,
            endpoint_is_full_url: m.endpoint_is_full_url,
            kind: match m.kind {
                fluxdb_storage::ai_platform::models::ModelKind::Generation => "生成".into(),
                fluxdb_storage::ai_platform::models::ModelKind::Decision => "决策".into(),
            },
            enabled: m.enabled,
            egress: m.egress == fluxdb_storage::ai_platform::models::EgressPermission::Allow,
            capability_labels,
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
            }
        })
        .collect();
    state.ai_tool_settings = tools_list
        .into_iter()
        .map(|t| AiToolSettingItem {
            tool_id: t.tool_id,
            enabled: t.enabled,
            revision: t.revision,
        })
        .collect();
    state.ai_agents = agents_list
        .into_iter()
        .map(|a| AiAgentItem {
            compression_trigger_percent: a.compression_trigger_percent,
            max_iterations: a.max_iterations,
            id: a.id,
            display_name: a.display_name,
            description: a.description,
            system_prompt: a.system_prompt,
            model_profile_id: a.default_model_profile_id,
            allowed_tools: a.allowed_tools,
            enabled: a.enabled,
            revision: a.revision,
        })
        .collect();
    state.ai_plugin_instances = plugins_list
        .into_iter()
        .map(|p| {
            let mandatory = plugins::is_mandatory(&p.id);
            let summary = BuiltinRegistry::default()
                .find_by_package(&p.package_key)
                .map(|descriptor| descriptor.summary.to_string())
                .unwrap_or_default();
            AiPluginItem {
                id: p.id,
                display_name: p.display_name,
                package_key: p.package_key,
                summary,
                enabled: p.enabled,
                mandatory,
                revision: p.revision,
            }
        })
        .collect();
    state.ai_model_egress_enabled = privacy.model_egress_enabled;
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
    state.ai_sessions = sessions_list
        .into_iter()
        .map(|s| AiSessionItem {
            id: s.id,
            title: s.title,
            agent_id: s.agent_id,
            status: s.status.as_str().to_string(),
            last_turn_status: s.last_turn_status.map(|status| status.as_str().to_string()),
            last_target_json: s.last_target_json,
            message_count: s.message_count,
            turn_count: s.turn_count,
            updated_at_ms: s.updated_at_ms,
            revision: s.revision,
        })
        .collect();
    // 默认模型选择与运行设置 revision 一起回填，供「默认主/决策模型」选择器使用。
    state.ai_default_model_id = runtime.default_model_profile_id.clone();
    state.ai_default_decision_model_id = runtime.default_decision_model_profile_id.clone();
    state.ai_runtime_revision = runtime.revision;
    state.ai_jev_status = with_ai_storage(storage, |conn| {
        reconcile_jev_builtins(conn, ai_chat_now())
    })?;
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
        models::set_model_enabled(conn, now_ms, id, enabled, expected_revision)?;
        reconcile_jev_builtins(conn, now_ms)
    })?;
    refresh_jev_status(state, storage)?;
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
        plugins::set_plugin_instance_enabled(conn, now_ms, id, enabled, expected_revision)?;
        reconcile_jev_builtins(conn, now_ms)
    })?;
    refresh_jev_status(state, storage)?;
    if let Some(p) = state.ai_plugin_instances.iter_mut().find(|p| p.id == id) {
        p.enabled = enabled;
        p.revision = expected_revision + 1;
    }
    Ok(())
}

/// 切换全局模型数据出站闸门（`privacy_policy_json`，复用运行设置行 CAS）。
///
/// 这是"怀疑数据外泄先全停"的应急开关：关闭后任何模型都不再发送会话数据，
/// 打开后各自模型的单模型许可恢复生效。
pub fn set_ai_model_egress_enabled(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    enabled: bool,
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<()> {
    let saved = with_ai_storage(storage, |conn| {
        let saved = ai_settings::save_privacy_policy(
            conn,
            now_ms,
            ai_settings::PrivacyPolicy {
                model_egress_enabled: enabled,
            },
            expected_revision,
        )?;
        reconcile_jev_builtins(conn, now_ms)?;
        Ok(saved)
    })?;
    refresh_jev_status(state, storage)?;
    state.ai_model_egress_enabled = saved.model_egress_enabled;
    state.ai_runtime_revision = expected_revision + 1;
    Ok(())
}

/// 切换标准工具的全局启停。
///
/// 全局关闭时，同一事务里把所有 Agent 白名单中的该工具一并移除：Agent 只能
/// 在全局允许的工具里再收窄，不能把已全局关闭的工具重新打开（§31.1 工具启停
/// 是全局能力开关，Agent 白名单是其子集）。
pub fn set_ai_tool_enabled(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
    tool_id: &str,
    enabled: bool,
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<Vec<String>> {
    let mut conn = storage.open_ai_connection()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
    let saved = tool_settings::save_tool_setting(
        &tx,
        now_ms,
        tool_id,
        enabled,
        expected_revision,
    )?;
    // 返回被收窄的 Agent id；关闭失败时整笔回滚，不留"全局关了但白名单还开着"的中间态。
    let mut affected = Vec::new();
    if !enabled {
        for agent in agents::list_agents(&tx)? {
            if !agent.allowed_tools.iter().any(|id| id == tool_id) {
                continue;
            }
            let remaining: Vec<String> = agent
                .allowed_tools
                .iter()
                .filter(|id| *id != tool_id)
                .cloned()
                .collect();
            let edit = agents::AgentEdit {
                compression_trigger_percent: agent.compression_trigger_percent,
                max_iterations: agent.max_iterations,
                id: agent.id.clone(),
                display_name: agent.display_name,
                description: agent.description,
                enabled: agent.enabled,
                default_model_profile_id: agent.default_model_profile_id,
                system_prompt: agent.system_prompt,
                budget_policy_id: agent.budget_policy_id,
                allowed_tools: remaining,
            };
            agents::save_agent(&tx, now_ms, &edit, agent.revision)?;
            affected.push(agent.id);
        }
    }
    tx.commit()
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;

    // 就地回填，避免整页刷新闪烁。
    match state
        .ai_tool_settings
        .iter_mut()
        .find(|t| t.tool_id == tool_id)
    {
        Some(item) => {
            item.enabled = enabled;
            item.revision = saved.revision;
        }
        None => state.ai_tool_settings.push(AiToolSettingItem {
            tool_id: tool_id.to_string(),
            enabled,
            revision: saved.revision,
        }),
    }
    for agent in state.ai_agents.iter_mut() {
        if affected.iter().any(|id| id == &agent.id) {
            agent.allowed_tools.retain(|id| id != tool_id);
            agent.revision += 1;
        }
    }
    Ok(affected)
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
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Decisions,
}

/// Provider 稳定标识（落库到 `ai_model_profiles.provider`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Decisions,
}

impl ModelProvider {
    pub const ALL: [ModelProvider; 4] = [
        ModelProvider::OpenAiChat,
        ModelProvider::OpenAiResponses,
        ModelProvider::Anthropic,
        ModelProvider::Decisions,
    ];

    /// UI 展示名。
    pub fn label(self) -> &'static str {
        match self {
            ModelProvider::OpenAiChat => "OpenAI Chat Completions",
            ModelProvider::OpenAiResponses => "OpenAI Responses",
            ModelProvider::Anthropic => "Anthropic Messages",
            ModelProvider::Decisions => "Decisions",
        }
    }

    pub fn id(self) -> ProviderId {
        match self {
            ModelProvider::OpenAiChat => ProviderId::OpenAiChat,
            ModelProvider::OpenAiResponses => ProviderId::OpenAiResponses,
            ModelProvider::Anthropic => ProviderId::Anthropic,
            ModelProvider::Decisions => ProviderId::Decisions,
        }
    }

    /// 解析落库字符串或 UI 展示名；未知值回退 OpenAI Chat（老数据兼容）。
    pub fn from_stored(value: &str) -> Self {
        match value {
            "openai-responses" | "OpenAI Responses" => ModelProvider::OpenAiResponses,
            "anthropic" | "Anthropic Messages" => ModelProvider::Anthropic,
            "decisions" | "Decisions" => ModelProvider::Decisions,
            _ => ModelProvider::OpenAiChat,
        }
    }

    /// 数据库中的稳定 Provider 标识；UI 改名不能改变历史值语义。
    pub fn stored_id(self) -> &'static str {
        match self {
            ModelProvider::OpenAiChat => "openai",
            ModelProvider::OpenAiResponses => "openai-responses",
            ModelProvider::Anthropic => "anthropic",
            ModelProvider::Decisions => "decisions",
        }
    }

    fn api_protocol(self) -> &'static str {
        match self {
            ModelProvider::OpenAiChat => "openai-compatible",
            ModelProvider::OpenAiResponses => "openai-responses",
            ModelProvider::Anthropic => "anthropic-messages",
            ModelProvider::Decisions => "system-one",
        }
    }
}

/// 按 Provider 推导模型类型与协议。
pub fn resolve_provider(
    provider: &str,
) -> (fluxdb_storage::ai_platform::models::ModelKind, &'static str) {
    let p = ModelProvider::from_stored(provider);
    let kind = match p {
        // Jev 决策模型：只做 Choice/Score 分类，不做工具调用。
        ModelProvider::Decisions => fluxdb_storage::ai_platform::models::ModelKind::Decision,
        ModelProvider::OpenAiChat | ModelProvider::OpenAiResponses | ModelProvider::Anthropic => {
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
            .map_err(|e| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
            })?;
        fluxdb_storage::ai_platform::models::ApiKeyChange::Set(secret)
    } else if input.clear_key {
        fluxdb_storage::ai_platform::models::ApiKeyChange::Clear
    } else {
        fluxdb_storage::ai_platform::models::ApiKeyChange::Keep
    };

    if let Some(endpoint) = &input.endpoint {
        ai_endpoint_url(endpoint)?;
    } else if input.endpoint_is_full_url {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "完整 URL 模式必须填写请求地址",
        ));
    }
    let provider = ModelProvider::from_stored(&input.provider);
    let (kind, api_protocol) = resolve_provider(input.provider.as_str());
    let edit = fluxdb_storage::ai_platform::models::ModelEdit {
        id: input.id.clone().unwrap_or_else(|| new_entity_id("model")),
        display_name: input.display_name.clone(),
        provider: provider.stored_id().to_string(),
        api_protocol: api_protocol.to_string(),
        kind,
        model: input.model.clone(),
        endpoint: input.endpoint.clone(),
        endpoint_is_full_url: input.endpoint_is_full_url,
        enabled: input.enabled,
        auth_mode,
        egress: if input.egress {
            fluxdb_storage::ai_platform::models::EgressPermission::Allow
        } else {
            fluxdb_storage::ai_platform::models::EgressPermission::Deny
        },
        context_window_tokens: input.context_window_tokens,
        max_output_tokens: input.max_output_tokens,
        input_price_micros_per_million: input.input_price_micros,
        output_price_micros_per_million: input.output_price_micros,
        cached_input_price_micros_per_million: input.cached_input_price_micros,
    };

    let saved = with_ai_storage(storage, |conn| {
        let saved = fluxdb_storage::ai_platform::models::save_model(
            conn,
            &cipher,
            "k1",
            now,
            &edit,
            input.expected_revision,
            change,
        )?;
        // 模型保存会改用途/协议/启停/凭据（Keep/Set/Clear），任一变化都要重新装配。
        reconcile_jev_builtins(conn, now)?;
        Ok(saved)
    })?;
    refresh_jev_status(state, storage)?;
    let (id, revision) = (saved.id.clone(), saved.revision);
    // 就地更新列表，避免整页刷新闪烁。
    match state.ai_models.iter_mut().find(|m| m.id == id) {
        Some(existing) => {
            let capability_labels = crate::model_capability_labels(&saved)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            existing.display_name = saved.display_name;
            existing.enabled = saved.enabled;
            existing.egress = saved.egress
                == fluxdb_storage::ai_platform::models::EgressPermission::Allow;
            existing.provider = saved.provider;
            existing.model = saved.model;
            existing.endpoint = saved.endpoint;
            existing.endpoint_is_full_url = saved.endpoint_is_full_url;
            // Provider 变化会同时改变模型类型（decisions=决策模型），就地列表必须同步。
            existing.kind =
                if saved.kind == fluxdb_storage::ai_platform::models::ModelKind::Decision {
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
            existing.capability_labels = capability_labels;
            existing.credential_status = credential_label(saved.credential_status);
            existing.connection_status = saved.connection_status;
            existing.connection_checked_at_ms = saved.connection_checked_at_ms;
        }
        None => {
            let capability_labels = crate::model_capability_labels(&saved)
                .into_iter()
                .map(str::to_string)
                .collect();
            state.ai_models.push(AiModelItem {
            id: id.clone(),
            display_name: saved.display_name,
            provider: saved.provider,
            model: saved.model,
            endpoint: saved.endpoint,
            endpoint_is_full_url: saved.endpoint_is_full_url,
            kind: if saved.kind == fluxdb_storage::ai_platform::models::ModelKind::Decision {
                "决策".into()
            } else {
                "生成".into()
            },
            enabled: saved.enabled,
            egress: saved.egress
                == fluxdb_storage::ai_platform::models::EgressPermission::Allow,
            capability_labels,
            context_window_tokens: saved.context_window_tokens,
            max_output_tokens: saved.max_output_tokens,
            input_price_micros: saved.input_price_micros_per_million,
            output_price_micros: saved.output_price_micros_per_million,
            cached_input_price_micros: saved.cached_input_price_micros_per_million,
            credential_status: credential_label(saved.credential_status),
            connection_status: saved.connection_status,
            connection_checked_at_ms: saved.connection_checked_at_ms,
            revision,
            });
        }
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

/// 重新读取 Jev 状态（reconcile 后调用，保证 UI 与运行期看到同一判定）。
pub fn refresh_jev_status(
    state: &mut AppState,
    storage: &fluxdb_storage::FileStorage,
) -> fluxdb_core::Result<()> {
    state.ai_jev_status = with_ai_storage(storage, |conn| jev_status(conn, ai_chat_now()))?;
    Ok(())
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
            compression_trigger_percent: input.compression_trigger_percent,
            max_iterations: input.max_iterations,
            id: input.id.clone().unwrap_or_else(|| new_entity_id("agent")),
            display_name: display_name.clone(),
            description: input.description.clone(),
            enabled: input.enabled,
            default_model_profile_id: input.model_profile_id.clone(),
            system_prompt: input.system_prompt.clone(),
            budget_policy_id: existing.as_ref().and_then(|a| a.budget_policy_id.clone()),
            allowed_tools: input.allowed_tools.clone(),
        };
        let saved = match &input.id {
            None => agents::create_agent(conn, now_ms, &edit),
            Some(_) => agents::save_agent(conn, now_ms, &edit, input.expected_revision),
        };
        // Agent 新建/编辑是设计 §18.1 列出的 reconcile 触发点。
        saved.and_then(|agent| {
            reconcile_jev_builtins(conn, now_ms)?;
            Ok(agent)
        })
    })?;
    refresh_jev_status(state, storage)?;

    let id = saved.id.clone();
    let item = AiAgentItem {
        compression_trigger_percent: saved.compression_trigger_percent,
        max_iterations: saved.max_iterations,
        id: saved.id,
        display_name: saved.display_name,
        description: saved.description,
        system_prompt: saved.system_prompt,
        model_profile_id: saved.default_model_profile_id,
        allowed_tools: saved.allowed_tools,
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
        let saved = ai_settings::save_runtime_settings(conn, now_ms, &next, expected_revision)?;
        // 默认决策模型变更按设计 §18.1 触发 reconcile：默认优先于"唯一合格候选"。
        reconcile_jev_builtins(conn, now_ms)?;
        Ok(saved)
    })?;
    refresh_jev_status(state, storage)?;
    // 就地回填，避免整页刷新闪烁。
    state.ai_default_model_id = default_model_id;
    state.ai_default_decision_model_id = default_decision_model_id;
    state.ai_runtime_revision = saved.revision;
    Ok(saved.revision)
}

include!("ai_model_network.rs");
