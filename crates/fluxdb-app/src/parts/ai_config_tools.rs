// 全局配置读取与持久化提案。
//
// 工具只读取非敏感配置摘要、提交待确认提案；真正落库由桌面端确认后
// 调用既有 AppCommand / app service 完成。

pub const CONFIG_INSPECT_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["kind"],
  "properties": {
    "kind": {
      "type": "string",
      "enum": ["settings", "models", "agents", "tools", "mcp", "sessions"]
    },
    "section": {
      "type": "string",
      "enum": ["editor", "appearance", "data", "system"]
    },
    "offset": { "type": "integer", "minimum": 0, "maximum": 4294967295 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
  }
}"#;

fn action_variant(
    action: &str,
    required: &[&str],
    mut properties: serde_json::Value,
) -> serde_json::Value {
    let object = properties
        .as_object_mut()
        .expect("action properties must be an object");
    object.insert(
        "action".into(),
        serde_json::json!({ "type": "string", "const": action }),
    );
    let mut required = required.to_vec();
    required.push("action");
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties
    })
}

fn config_propose_schema() -> String {
    let setting = action_variant(
        "setting.update",
        &["section", "fields", "expected_revision"],
        serde_json::json!({
            "section": { "type": "string", "enum": ["editor", "appearance", "data", "system"] },
            "fields": crate::settings_fields_schema(),
            "expected_revision": { "type": "integer", "minimum": 0 }
        }),
    );
    let tool = action_variant(
        "tool.set_enabled",
        &["tool_id", "enabled", "expected_revision"],
        serde_json::json!({
            "tool_id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "enabled": { "type": "boolean" },
            "expected_revision": { "type": "integer", "minimum": 0 }
        }),
    );
    let agent = action_variant(
        "agent.create",
        &["display_name", "system_prompt"],
        serde_json::json!({
            "display_name": { "type": "string", "minLength": 1, "maxLength": 128 },
            "description": { "type": "string", "maxLength": 1024 },
            "system_prompt": { "type": "string", "minLength": 1, "maxLength": 65536 },
            "model_profile_id": { "type": "string", "maxLength": 128 },
            "allowed_tools": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 128 } },
            "enabled": { "type": "boolean" },
            "compression_trigger_percent": { "type": "integer", "minimum": 10, "maximum": 95 },
            "max_iterations": { "type": "integer", "minimum": 1, "maximum": 1000 }
        }),
    );
    let session_archive = action_variant(
        "session.archive",
        &["session_id", "expected_revision"],
        serde_json::json!({
            "session_id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "expected_revision": { "type": "integer", "minimum": 0 }
        }),
    );
    let session_purge = action_variant(
        "session.purge",
        &["session_id", "expected_revision"],
        serde_json::json!({
            "session_id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "expected_revision": { "type": "integer", "minimum": 0 }
        }),
    );
    let mut provider_values = crate::ModelProvider::ALL
        .iter()
        .map(|provider| provider.stored_id())
        .collect::<Vec<_>>();
    provider_values.extend([
        "deepseek",
        "OpenAI Chat Completions",
        "OpenAI Responses",
        "Anthropic Messages",
    ]);
    let model = action_variant(
        "model.create",
        &["display_name", "provider", "model"],
        serde_json::json!({
            "display_name": { "type": "string", "minLength": 1, "maxLength": 128 },
            "provider": {
                "type": "string",
                "enum": provider_values,
                "description": "API protocol, not vendor. Use openai for OpenAI-compatible endpoints such as DeepSeek."
            },
            "endpoint": { "type": "string", "maxLength": 2048 },
            "endpoint_is_full_url": { "type": "boolean" },
            "model": { "type": "string", "minLength": 1, "maxLength": 256 },
            "context_window_tokens": { "type": "integer", "minimum": 1 },
            "max_output_tokens": { "type": "integer", "minimum": 1 },
            "input_price_micros": { "type": "integer", "minimum": 0 },
            "output_price_micros": { "type": "integer", "minimum": 0 },
            "cached_input_price_micros": { "type": "integer", "minimum": 0 },
            "enabled": { "type": "boolean" },
            "egress": { "type": "boolean" }
        }),
    );
    let model_update = update_variant("model.update", &model);
    let agent_update = update_variant("agent.update", &agent);
    let model_delete = delete_variant("model.delete");
    let agent_delete = delete_variant("agent.delete");
    let mcp_server = action_variant(
        "mcp.server_set_enabled",
        &["id", "enabled", "expected_revision"],
        serde_json::json!({
            "id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "enabled": { "type": "boolean" },
            "expected_revision": { "type": "integer", "minimum": 0 }
        }),
    );
    let mcp_sync = action_variant(
        "mcp.sync_client",
        &["client_key", "registered"],
        serde_json::json!({
            "client_key": { "type": "string", "minLength": 1, "maxLength": 128 },
            "registered": { "type": "boolean" }
        }),
    );
    serde_json::json!({
        "type": "object",
        "oneOf": [
            setting, tool, agent, session_archive, session_purge, model, model_update, model_delete, agent_update, agent_delete, mcp_server, mcp_sync
        ]
    })
    .to_string()
}

fn update_variant(action: &str, create: &serde_json::Value) -> serde_json::Value {
    let mut fields = create["properties"].clone();
    if let Some(fields) = fields.as_object_mut() {
        fields.remove("action");
    }
    action_variant(
        action,
        &["id", "expected_revision", "changes"],
        serde_json::json!({
            "id": {"type":"string", "minLength":1, "maxLength":128},
            "expected_revision":{"type":"integer", "minimum":0, "maximum":i64::MAX},
            "changes":{"type":"object", "additionalProperties":false, "properties":fields}
        }),
    )
}

fn delete_variant(action: &str) -> serde_json::Value {
    action_variant(
        action,
        &["id", "expected_revision"],
        serde_json::json!({
            "id":{"type":"string", "minLength":1, "maxLength":128},
            "expected_revision":{"type":"integer", "minimum":0, "maximum":i64::MAX}
        }),
    )
}

struct ConfigInspectTool {
    descriptor: fluxdb_core::ToolDescriptor,
    storage: fluxdb_storage::FileStorage,
}

struct ConfigProposeTool {
    descriptor: fluxdb_core::ToolDescriptor,
    actions: std::sync::Arc<dyn crate::UiActionSink>,
}

impl ConfigInspectTool {
    fn new(storage: fluxdb_storage::FileStorage) -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "config.inspect".into(),
                version: 1,
                description: "读取非敏感配置摘要：设置、模型、Agent、工具、MCP、会话。任何页面都可调用；凭据只返回状态。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: CONFIG_INSPECT_INPUT_SCHEMA.into(),
            },
            storage,
        }
    }

    fn inspect(&self, value: &serde_json::Value) -> Result<serde_json::Value, String> {
        let kind = value
            .get("kind")
            .and_then(|kind| kind.as_str())
            .ok_or_else(|| "缺少 kind".to_string())?;
        let limit = value
            .get("limit")
            .and_then(|limit| limit.as_u64())
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let offset = usize::try_from(value.get("offset").and_then(|v| v.as_u64()).unwrap_or(0))
            .map_err(|_| "offset out of range".to_string())?;
        let next_offset = offset.saturating_add(limit);
        match kind {
            "settings" => {
                let section = value
                    .get("section")
                    .and_then(|section| section.as_str())
                    .unwrap_or("editor");
                let section = crate::SettingsSection::parse(section)
                    .ok_or_else(|| format!("不支持的设置分区: {section}"))?;
                let settings = self
                    .storage
                    .load_settings()
                    .map_err(|error| error.message)?;
                let public = crate::settings_section_public_json(&settings, section);
                Ok(serde_json::json!({
                    "kind": "settings",
                    "revision": crate::settings_fingerprint(&settings),
                    "section": public["section"],
                    "values": public["values"],
                    "editable_fields": public["editable_fields"],
                }))
            }
            "models" => {
                let models = self
                    .storage
                    .with_ai_connection(|conn| {
                        fluxdb_storage::ai_platform::models::list_models_page(
                            conn,
                            limit as u32,
                            offset.min(u32::MAX as usize) as u32,
                        )
                    })
                    .map_err(|error| error.message)?;
                Ok(serde_json::json!({
                    "kind": "models",
                    "providers": crate::ModelProvider::ALL.iter().map(|provider| serde_json::json!({
                        "id": provider.stored_id(),
                        "label": provider.label(),
                    })).collect::<Vec<_>>(),
                    "items": models.into_iter().map(|model| serde_json::json!({
                        "id": model.id,
                        "display_name": model.display_name,
                        "provider": model.provider,
                        "model": model.model,
                        "endpoint": model.endpoint,
                        "enabled": model.enabled,
                        "context_window_tokens": model.context_window_tokens,
                        "max_output_tokens": model.max_output_tokens,
                        "credential_status": credential_status_key(model.credential_status),
                        "revision": model.revision,
                    })).collect::<Vec<_>>(),
                }))
            }
            "agents" => {
                let agents = self
                    .storage
                    .with_ai_connection(fluxdb_storage::ai_platform::agents::list_agents)
                    .map_err(|error| error.message)?;
                Ok(serde_json::json!({
                    "kind": "agents",
                    "items": agents.into_iter().skip(offset).take(limit).map(|agent| serde_json::json!({
                        "id": agent.id,
                        "display_name": agent.display_name,
                        "description": agent.description,
                        "enabled": agent.enabled,
                        "model_profile_id": agent.default_model_profile_id,
                        "allowed_tools": agent.allowed_tools,
                        "revision": agent.revision,
                    })).collect::<Vec<_>>(),
                }))
            }
            "tools" => {
                let settings = self
                    .storage
                    .with_ai_connection(
                        fluxdb_storage::ai_platform::tool_settings::list_tool_settings,
                    )
                    .map_err(|error| error.message)?;
                let catalog = crate::AI_AGENT_TOOL_CATALOG
                    .iter()
                    .chain(crate::AI_MCP_ONLY_TOOL_CATALOG);
                let total = catalog.clone().count();
                let items = catalog
                    .skip(offset)
                    .take(limit)
                    .map(|spec| {
                        let setting = settings.iter().find(|setting| setting.tool_id == spec.0);
                        serde_json::json!({
                            "id": spec.0,
                            "description": spec.2,
                            "may_mutate": spec.3,
                            "enabled": setting.map(|setting| setting.enabled).unwrap_or(true),
                            "revision": setting.map(|setting| setting.revision).unwrap_or(0),
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(
                    serde_json::json!({ "kind": "tools", "items": items, "total": total, "next_offset": (next_offset < total).then_some(next_offset) }),
                )
            }
            "mcp" => {
                let (servers, clients) = self
                    .storage
                    .with_ai_connection(|conn| {
                        let servers =
                            fluxdb_storage::ai_platform::mcp_server::list_server_profiles(conn)?;
                        let clients = fluxdb_storage::ai_platform::mcp::list_mcp_settings(conn)?;
                        Ok((servers, clients))
                    })
                    .map_err(|error| error.message)?;
                Ok(serde_json::json!({
                    "kind": "mcp",
                    "registration_clients": crate::McpClientKind::all().iter().map(|client| serde_json::json!({
                        "key": client.key(), "label": client.label(),
                        "action": "mcp.sync_client"
                    })).collect::<Vec<_>>(),
                    "servers": servers.into_iter().take(limit).map(|server| serde_json::json!({
                        "id": server.id,
                        "display_name": server.display_name,
                        "enabled": server.enabled,
                        "revision": server.revision,
                    })).collect::<Vec<_>>(),
                    "clients": clients.into_iter().take(limit).map(|client| serde_json::json!({
                        "id": client.id,
                        "display_name": client.display_name,
                        "transport": client.transport,
                        "enabled": client.enabled,
                        "revision": client.revision,
                    })).collect::<Vec<_>>(),
                }))
            }
            "sessions" => {
                let sessions = self
                    .storage
                    .with_ai_connection(|conn| {
                        fluxdb_storage::ai_platform::sessions::list_session_overviews(
                            conn,
                            limit as i64,
                        )
                    })
                    .map_err(|error| error.message)?;
                Ok(serde_json::json!({
                    "kind": "sessions",
                    "items": sessions.into_iter().map(|session| serde_json::json!({
                        "id": session.id,
                        "title": session.title,
                        "agent_id": session.agent_id,
                        "status": session.status.as_str(),
                        "message_count": session.message_count,
                        "turn_count": session.turn_count,
                        "updated_at_ms": session.updated_at_ms,
                        "revision": session.revision,
                    })).collect::<Vec<_>>(),
                }))
            }
            other => Err(format!("不支持的配置类型: {other}")),
        }
    }
}

impl fluxdb_core::AiTool for ConfigInspectTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let now = now_ms();
            if ctx.cancel.is_cancelled() {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Cancelled,
                    "配置读取已取消",
                ));
            }
            let value = serde_json::from_str::<serde_json::Value>(&arguments_json);
            let (status, result, error_code) = match value
                .map_err(|error| format!("参数不是合法 JSON: {error}"))
                .and_then(|value| self.inspect(&value))
            {
                Ok(value) => (fluxdb_core::ToolReceiptStatus::Succeeded, value, None),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({ "error": error, "retryable": false }),
                    Some("config_inspect_failed".to_string()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: result.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: now_ms(),
            })
        })
    }
}

impl ConfigProposeTool {
    fn new(actions: std::sync::Arc<dyn crate::UiActionSink>) -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "config.propose".into(),
                version: 1,
                description: "提交待用户确认的持久化配置变更；不直接落库。支持 setting.update、tool.set_enabled、agent.create/update/delete、session.archive/purge、model.create/update/delete、mcp.*。编辑只传 changes 中明确修改的字段；不传 API Key。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: config_propose_schema(),
            },
            actions,
        }
    }

    fn proposal(
        &self,
        ctx: &fluxdb_core::ToolExecutionContext,
        value: &serde_json::Value,
    ) -> Result<crate::ConfigProposal, String> {
        crate::validate_schema(
            &serde_json::from_str(&config_propose_schema()).map_err(|e| e.to_string())?,
            value,
        )?;
        let action = value
            .get("action")
            .and_then(|action| action.as_str())
            .ok_or_else(|| "缺少 action".to_string())?;
        let expected_revision = value
            .get("expected_revision")
            .and_then(|revision| revision.as_u64())
            .unwrap_or(0);
        let kind = match action {
            "setting.update" => {
                crate::ConfigProposalKind::SettingUpdate(crate::parse_settings_prefill(value)?)
            }
            "tool.set_enabled" => crate::ConfigProposalKind::ToolSetEnabled {
                tool_id: required_str(value, "tool_id")?,
                enabled: required_bool(value, "enabled")?,
            },
            "agent.create" => crate::ConfigProposalKind::AgentCreate(crate::AgentCreateProposal {
                display_name: required_str(value, "display_name")?,
                description: value
                    .get("description")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                system_prompt: required_str(value, "system_prompt")?,
                model_profile_id: value
                    .get("model_profile_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                allowed_tools: value
                    .get("allowed_tools")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                enabled: value
                    .get("enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                compression_trigger_percent: value
                    .get("compression_trigger_percent")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(80)
                    .clamp(10, 95) as u8,
                max_iterations: value
                    .get("max_iterations")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(100)
                    .clamp(1, 1000),
            }),
            "session.archive" => crate::ConfigProposalKind::SessionArchive {
                id: required_str(value, "session_id")?,
            },
            "session.purge" => crate::ConfigProposalKind::SessionPurge {
                id: required_str(value, "session_id")?,
            },
            "model.create" => {
                let raw_provider = required_str(value, "provider")?;
                let provider = normalize_model_provider(&raw_provider);
                if !crate::ModelProvider::ALL
                    .iter()
                    .any(|candidate| candidate.stored_id() == provider)
                {
                    return Err(format!("不支持的 Provider: {raw_provider}"));
                }
                let endpoint = value
                    .get("endpoint")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        (raw_provider.eq_ignore_ascii_case("deepseek"))
                            .then(|| "https://api.deepseek.com".to_string())
                    });
                crate::ConfigProposalKind::ModelCreate(crate::ModelCreateProposal {
                    display_name: required_str(value, "display_name")?,
                    provider,
                    endpoint,
                    endpoint_is_full_url: value
                        .get("endpoint_is_full_url")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    model: required_str(value, "model")?,
                    context_window_tokens: optional_i64(value, "context_window_tokens"),
                    max_output_tokens: optional_i64(value, "max_output_tokens"),
                    input_price_micros: optional_i64(value, "input_price_micros"),
                    output_price_micros: optional_i64(value, "output_price_micros"),
                    cached_input_price_micros: optional_i64(value, "cached_input_price_micros"),
                    enabled: value
                        .get("enabled")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true),
                    egress: value
                        .get("egress")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true),
                })
            }
            "model.update" => crate::ConfigProposalKind::ModelUpdate {
                id: required_str(value, "id")?,
                changes: normalized_model_changes(value)?,
            },
            "model.delete" => crate::ConfigProposalKind::ModelDelete {
                id: required_str(value, "id")?,
            },
            "agent.update" => crate::ConfigProposalKind::AgentUpdate {
                id: required_str(value, "id")?,
                changes: required_changes(value)?,
            },
            "agent.delete" => crate::ConfigProposalKind::AgentDelete {
                id: required_str(value, "id")?,
            },
            "mcp.server_set_enabled" => crate::ConfigProposalKind::McpServerSetEnabled {
                id: required_str(value, "id")?,
                enabled: required_bool(value, "enabled")?,
            },
            "mcp.sync_client" => crate::ConfigProposalKind::McpSyncClient {
                client_key: required_str(value, "client_key")?,
                registered: required_bool(value, "registered")?,
            },
            other => return Err(format!("不支持的配置动作: {other}")),
        };
        Ok(crate::ConfigProposal {
            request_id: ctx.call_id.clone(),
            turn_id: ctx.turn_id.clone(),
            expected_revision,
            created_at_ms: now_ms(),
            kind,
        })
    }
}

fn normalize_model_provider(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" | "openai chat completions" | "openai-chat" | "deepseek" => "openai".into(),
        "openai-responses" | "openai responses" => "openai-responses".into(),
        "anthropic" | "anthropic messages" => "anthropic".into(),
        "decisions" => "decisions".into(),
        _ => value.trim().to_string(),
    }
}

fn normalized_model_changes(
    value: &serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut changes = required_changes(value)?;
    if let Some(provider) = changes.get("provider").and_then(|value| value.as_str()) {
        let normalized = normalize_model_provider(provider);
        if !crate::ModelProvider::ALL
            .iter()
            .any(|candidate| candidate.stored_id() == normalized)
        {
            return Err(format!("不支持的 Provider: {provider}"));
        }
        changes.insert("provider".into(), serde_json::Value::String(normalized));
    }
    Ok(changes)
}

impl fluxdb_core::AiTool for ConfigProposeTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let now = now_ms();
            if ctx.cancel.is_cancelled() {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Cancelled,
                    "配置提案已取消",
                ));
            }
            let value = serde_json::from_str::<serde_json::Value>(&arguments_json);
            let (status, result, error_code) = match value
                .map_err(|error| format!("参数不是合法 JSON: {error}"))
                .and_then(|value| self.proposal(&ctx, &value))
                .and_then(|proposal| {
                    self.actions
                        .submit(crate::UiAction::ProposeConfig(proposal))
                        .map_err(|error| {
                            format!("{}（retryable={}）", error.message(), error.retryable())
                        })
                }) {
                Ok(()) => (
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    serde_json::json!({
                        "status": "proposed",
                        "note": "已提交配置提案；必须等用户确认后才会落库"
                    }),
                    None,
                ),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({ "error": error, "retryable": false }),
                    Some("config_proposal_failed".to_string()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: result.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: now_ms(),
            })
        })
    }
}

pub fn add_ai_config_tools(
    catalog: &mut crate::ToolCatalog,
    storage: fluxdb_storage::FileStorage,
    actions: std::sync::Arc<dyn crate::UiActionSink>,
) -> fluxdb_core::Result<()> {
    catalog.add_tool_with_shape_and_effect(
        Box::new(ConfigInspectTool::new(storage)),
        crate::ToolOutputShape::Opaque,
        Some(crate::EffectClass::PureRead),
    )?;
    catalog.add_tool_with_shape_and_effect(
        Box::new(ConfigProposeTool::new(actions)),
        crate::ToolOutputShape::Opaque,
        Some(crate::EffectClass::LocalUiMutation),
    )
}

fn required_str(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {key}"))
}

fn required_changes(
    value: &serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let changes = value["changes"]
        .as_object()
        .ok_or("changes must be an object")?;
    if changes.is_empty() {
        return Err("changes must not be empty".into());
    }
    Ok(changes.clone())
}

fn required_bool(value: &serde_json::Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(|value| value.as_bool())
        .ok_or_else(|| format!("缺少 {key}"))
}

fn optional_i64(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}

fn credential_status_key(
    status: fluxdb_storage::ai_platform::model_secret::CredentialStatus,
) -> &'static str {
    use fluxdb_storage::ai_platform::model_secret::CredentialStatus;
    match status {
        CredentialStatus::NotRequired => "not_required",
        CredentialStatus::Missing => "missing",
        CredentialStatus::Configured => "configured",
        CredentialStatus::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod ai_config_tools_tests {
    use super::*;

    #[test]
    fn propose_schema_accepts_action_specific_shapes() {
        let schema: serde_json::Value =
            serde_json::from_str(&config_propose_schema()).expect("schema");
        crate::validate_schema(
            &schema,
            &serde_json::json!({
                "action": "tool.set_enabled",
                "tool_id": "db.run",
                "enabled": false,
                "expected_revision": 2
            }),
        )
        .expect("tool action");
        crate::validate_schema(
            &schema,
            &serde_json::json!({
                "action": "model.create",
                "display_name": "DeepSeek V4",
                "provider": "openai",
                "model": "deepseek-v4",
                "endpoint": "https://api.deepseek.com/v1"
            }),
        )
        .expect("model action");
    }

    #[test]
    fn model_create_accepts_compatible_provider_aliases() {
        let schema: serde_json::Value =
            serde_json::from_str(&config_propose_schema()).expect("schema");
        for provider in ["openai", "deepseek", "OpenAI Chat Completions"] {
            crate::validate_schema(
                &schema,
                &serde_json::json!({
                    "action": "model.create",
                    "display_name": "DeepSeek",
                    "provider": provider,
                    "model": "deepseek-chat"
                }),
            )
            .unwrap_or_else(|error| panic!("{provider}: {error}"));
        }
    }

    #[test]
    fn model_create_infers_deepseek_endpoint_but_preserves_explicit_endpoint() {
        let tool = ConfigProposeTool::new(std::sync::Arc::new(NoopSink));
        let ctx = fluxdb_core::ToolExecutionContext {
            automatic_approval: false,
            bounded_write: false,
            session_id: "session".into(),
            turn_id: "turn".into(),
            iteration: 1,
            call_id: "call".into(),
            cancel: fluxdb_core::CancelFlag::new(),
            progress: None,
        };
        let inferred = tool
            .proposal(
                &ctx,
                &serde_json::json!({
                    "action": "model.create",
                    "display_name": "DeepSeek",
                    "provider": "deepseek",
                    "model": "deepseek-chat"
                }),
            )
            .expect("deepseek proposal");
        let crate::ConfigProposalKind::ModelCreate(draft) = inferred.kind else {
            panic!("expected model create");
        };
        assert_eq!(draft.provider, "openai");
        assert_eq!(
            draft.endpoint.as_deref(),
            Some("https://api.deepseek.com")
        );

        let explicit = tool
            .proposal(
                &ctx,
                &serde_json::json!({
                    "action": "model.create",
                    "display_name": "DeepSeek",
                    "provider": "deepseek",
                    "model": "deepseek-chat",
                    "endpoint": "https://proxy.example.com/v1"
                }),
            )
            .expect("explicit endpoint proposal");
        let crate::ConfigProposalKind::ModelCreate(draft) = explicit.kind else {
            panic!("expected model create");
        };
        assert_eq!(
            draft.endpoint.as_deref(),
            Some("https://proxy.example.com/v1")
        );
    }

    #[test]
    fn model_create_reports_the_matching_action_field_error() {
        let schema: serde_json::Value =
            serde_json::from_str(&config_propose_schema()).expect("schema");
        let error = crate::validate_schema(
            &schema,
            &serde_json::json!({
                "action": "model.create",
                "display_name": "DeepSeek",
                "provider": "openai"
            }),
        )
        .expect_err("missing model must fail");
        assert!(error.contains("action model.create"), "{error}");
        assert!(error.contains("缺少必填字段: model"), "{error}");
    }

    #[test]
    fn model_update_normalizes_provider_alias() {
        let tool = ConfigProposeTool::new(std::sync::Arc::new(NoopSink));
        let ctx = fluxdb_core::ToolExecutionContext {
            automatic_approval: false,
            bounded_write: false,
            session_id: "session".into(),
            turn_id: "turn".into(),
            iteration: 1,
            call_id: "call".into(),
            cancel: fluxdb_core::CancelFlag::new(),
            progress: None,
        };
        let proposal = tool
            .proposal(
                &ctx,
                &serde_json::json!({
                    "action": "model.update",
                    "id": "model-1",
                    "expected_revision": 2,
                    "changes": {"provider": "deepseek"}
                }),
            )
            .expect("model update");
        let crate::ConfigProposalKind::ModelUpdate { changes, .. } = proposal.kind else {
            panic!("expected model update");
        };
        assert_eq!(changes["provider"], "openai");
    }

    #[test]
    fn proposal_parses_global_actions() {
        let tool = ConfigProposeTool::new(std::sync::Arc::new(NoopSink));
        let ctx = fluxdb_core::ToolExecutionContext {
            automatic_approval: false,
            bounded_write: false,
            session_id: "session".into(),
            turn_id: "turn".into(),
            iteration: 1,
            call_id: "call".into(),
            cancel: fluxdb_core::CancelFlag::new(),
            progress: None,
        };
        let proposal = tool
            .proposal(
                &ctx,
                &serde_json::json!({
                    "action": "session.archive",
                    "session_id": "s1",
                    "expected_revision": 3
                }),
            )
            .expect("session proposal");
        assert_eq!(proposal.expected_revision, 3);
        assert!(matches!(
            proposal.kind,
            crate::ConfigProposalKind::SessionArchive { .. }
        ));
    }

    #[test]
    fn model_and_agent_updates_have_strict_partial_shapes_and_reject_credentials() {
        let schema: serde_json::Value =
            serde_json::from_str(&config_propose_schema()).expect("schema");
        for action in ["model.update", "agent.update"] {
            let valid = serde_json::json!({
                "action": action, "id":"existing", "expected_revision":2,
                "changes":{"display_name":"Renamed"}
            });
            crate::validate_schema(&schema, &valid).expect("partial update");
            for field in ["api_key", "password", "token", "ciphertext", "nonce"] {
                let invalid = serde_json::json!({
                    "action":action, "id":"existing", "expected_revision":2,
                    "changes":{field:"must-not-enter-proposal"}
                });
                assert!(
                    crate::validate_schema(&schema, &invalid).is_err(),
                    "{action}/{field}"
                );
            }
        }
        let invalid = serde_json::json!({
            "action":"model.create", "display_name":"Fixture", "provider":"not-a-protocol",
            "model":"fixture"
        });
        assert!(crate::validate_schema(&schema, &invalid).is_err());
    }

    struct NoopSink;

    impl crate::UiActionSink for NoopSink {
        fn submit(&self, _action: crate::UiAction) -> Result<(), crate::UiActionError> {
            Ok(())
        }
    }
}
