// 主模型 Provider 适配与秘密 transport 边界（设计 §8/§17，T017）。
//
// 分层职责：
// - `CredentialLease`：**请求前**才向凭据服务申请一次性明文；`SecretLease` 不实现 Serialize/Display，
//   Debug 只输出 `[REDACTED]`，且只在 `stream()` 内短暂存在（请求头构造完即随作用域结束）。
// - `ToolNameMapper`：宿主稳定 ID（`config.model.create`）与 Provider 安全名称**可逆**映射，
//   注册时检测冲突；回程把 Provider 名称还原成宿主 ID，运行时只认宿主 ID。
// - `ChatTransport`：真正的 HTTP 调用抽象。生产实现走 `stream_chat`（ureq，不跟随重定向）；
//   测试注入假实现即可在无网络下断言请求体/认证与事件映射。
// - `HttpChatProvider`：`fluxdb_core::GenerationProvider` 实现，负责能力声明、请求体构造、工具名映射与回程还原。

/// 一次性的明文凭据租约。
///
/// 刻意不实现 `Clone`/`Serialize`/`Display`；`Debug` 只显示占位符。
pub struct SecretLease(String);

impl SecretLease {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 只在构造请求头的边界内借用明文。
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretLease([REDACTED])")
    }
}

/// 凭据服务边界：按模型档案申请一次明文租约。
pub trait CredentialLease: Send + Sync {
    fn lease(&self, model_profile_id: &str) -> fluxdb_core::Result<SecretLease>;
}

/// 认证注入方式（决定 header 形态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>` + `anthropic-version`
    AnthropicApiKey,
}

/// Provider 能力与协议配置（不含任何凭据）。
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub model_profile_id: String,
    pub model: String,
    pub endpoint: String,
    pub protocol: crate::ChatProtocol,
    pub auth_style: AuthStyle,
    pub capabilities: fluxdb_core::ModelCapabilities,
    pub limits: crate::TransportLimits,
    pub max_output_tokens: Option<i64>,
}

/// 假/真传输的统一入口（测试可注入）。
pub trait ChatTransport: Send + Sync {
    fn call(
        &self,
        config: &ProviderConfig,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &fluxdb_core::CancelFlag,
    ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>;
    /// 网络实现从解析点即时回调；简单测试传输可沿用收集型接口。
    fn call_events(
        &self,
        config: &ProviderConfig,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &fluxdb_core::CancelFlag,
        emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
    ) -> fluxdb_core::Result<()> {
        for event in self.call(config, auth, body_json, cancel)? {
            emit(&event);
        }
        Ok(())
    }

}

/// 生产实现：`stream_chat`（ureq，禁重定向，有界可取消）。
pub struct UreqChatTransport;

impl ChatTransport for UreqChatTransport {
    fn call(
        &self,
        config: &ProviderConfig,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &fluxdb_core::CancelFlag,
    ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>> {
        let mut events = Vec::new();
        self.call_events(config, auth, body_json, cancel, &mut |event| events.push(event.clone()))?;
        Ok(events)
    }

    fn call_events(
        &self,
        config: &ProviderConfig,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &fluxdb_core::CancelFlag,
        emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
    ) -> fluxdb_core::Result<()> {
        let extra: &[(&str, &str)] = match config.auth_style {
            AuthStyle::Bearer => &[],
            AuthStyle::AnthropicApiKey => &[("anthropic-version", "2023-06-01")],
        };
        crate::stream_chat_events(config.limits, config.protocol, &config.endpoint,
            auth, body_json, extra, cancel, emit).map_err(crate::TransportError::to_core_error)
    }
}

/// 只在工具调用事件上分配映射后的 ID；正文增量直接借用，避免逐 token 复制。
fn emit_host_model_event(
    event: &fluxdb_core::ModelEvent,
    mapper: &ToolNameMapper,
    tool_calling: bool,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> fluxdb_core::Result<()> {
    if let fluxdb_core::ModelEvent::ToolCallRequested(call) = event {
        if !tool_calling {
            return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,
                "模型未声明工具能力却返回工具调用"));
        }
        let host_id = mapper.to_host(&call.tool_id).ok_or_else(||
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型返回了未注册的工具"))?;
        emit(&fluxdb_core::ModelEvent::ToolCallRequested(fluxdb_core::ToolCallRequest {
            provider_call_id: call.provider_call_id.clone(),
            tool_id: host_id.into(),
            arguments_json: call.arguments_json.clone(),
        }));
    } else {
        emit(event);
    }
    Ok(())
}

/// 宿主稳定工具 ID ↔ Provider 安全名称的可逆映射。
pub struct ToolNameMapper {
    pairs: Vec<(String, String)>,
}

impl ToolNameMapper {
    /// 按工具描述注册；Provider 名称冲突时返回错误（不静默覆盖）。
    pub fn new(descriptors: &[fluxdb_core::ToolDescriptor]) -> fluxdb_core::Result<Self> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for descriptor in descriptors {
            let provider_name = sanitize_tool_name(&descriptor.id);
            if let Some((existing_host, _)) = pairs
                .iter()
                .find(|(_, provider)| provider == &provider_name)
            {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("工具名映射冲突: {} 与 {existing_host}", descriptor.id),
                ));
            }
            pairs.push((descriptor.id.clone(), provider_name));
        }
        Ok(Self { pairs })
    }

    /// 宿主 ID → Provider 名称。
    pub fn to_provider(&self, host_id: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(host, _)| host == host_id)
            .map(|(_, provider)| provider.as_str())
    }

    /// Provider 名称 → 宿主 ID（回程只认宿主 ID）。
    pub fn to_host(&self, provider_name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(_, provider)| provider == provider_name)
            .map(|(host, _)| host.as_str())
    }
}

/// 把宿主工具 ID 转成 Provider 可接受的名称（保留字母/数字/`_`/`-`，其余替换为 `_`）。
fn sanitize_tool_name(host_id: &str) -> String {
    host_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 构造聊天请求体（**不含任何凭据**）。
pub fn build_chat_body(
    protocol: crate::ChatProtocol,
    model: &str,
    request: &fluxdb_core::ModelRequest,
    mapper: &ToolNameMapper,
    capabilities: fluxdb_core::ModelCapabilities,
    max_output_tokens: Option<i64>,
) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|m| {
            let mut item = serde_json::json!({
                "role": match m.role {
                    fluxdb_core::ModelRole::System => "system",
                    fluxdb_core::ModelRole::User => "user",
                    fluxdb_core::ModelRole::Assistant => "assistant",
                    fluxdb_core::ModelRole::Tool => "tool",
                },
                "content": m.text,
            });
            // 工具配对：assistant 要回带 tool_calls，工具结果要带 tool_call_id，
            // 否则真实模型在第二轮会收到"无来源的工具输出"。
            match m.role {
                fluxdb_core::ModelRole::Assistant if !m.tool_calls.is_empty() => {
                    if m.text.is_empty() {
                        item["content"] = serde_json::Value::Null;
                    }
                    item["tool_calls"] = serde_json::json!(
                        m.tool_calls
                            .iter()
                            .map(|call| serde_json::json!({
                                "id": call.provider_call_id,
                                "type": "function",
                                "function": {
                                    "name": mapper
                                        .to_provider(&call.tool_id)
                                        .unwrap_or(call.tool_id.as_str()),
                                    "arguments": call.arguments_json,
                                },
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                fluxdb_core::ModelRole::Tool => {
                    if let Some(call_id) = &m.tool_call_id {
                        item["tool_call_id"] = serde_json::json!(call_id);
                    }
                }
                _ => {}
            }
            item
        })
        .collect();
    let max_tokens = request.max_output_tokens.or(max_output_tokens);
    match protocol {
        crate::ChatProtocol::OpenAiCompatible => {
            let mut body = serde_json::json!({
                "model": model,
                "messages": messages,
                "stream": true,
            });
            if let Some(max) = max_tokens {
                body["max_tokens"] = serde_json::json!(max);
            }
            // 只有声明了 tool_calling 能力才把工具发给模型。
            if capabilities.tool_calling && !request.tools.is_empty() {
                let tools: Vec<serde_json::Value> = request
                    .tools
                    .iter()
                    .filter_map(|tool| {
                        mapper.to_provider(&tool.id).map(|name| {
                            serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "description": tool.description,
                                    "parameters": serde_json::from_str::<serde_json::Value>(
                                        &tool.input_schema_json
                                    ).unwrap_or_else(|_| serde_json::json!({"type":"object"})),
                                }
                            })
                        })
                    })
                    .collect();
                body["tools"] = serde_json::json!(tools);
            }
            body
        }
        crate::ChatProtocol::OpenAiResponses => {
            // Responses API：system 走顶层 `instructions`；function_call / function_call_output 必须成对重放。
            let instructions: Vec<String> = request
                .messages
                .iter()
                .filter(|message| message.role == fluxdb_core::ModelRole::System)
                .map(|message| message.text.clone())
                .collect();
            let mut input: Vec<serde_json::Value> = Vec::new();
            for message in &request.messages {
                match message.role {
                    fluxdb_core::ModelRole::System => {}
                    fluxdb_core::ModelRole::User => input.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": message.text }],
                    })),
                    fluxdb_core::ModelRole::Assistant => {
                        if !message.text.is_empty() {
                            input.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "output_text", "text": message.text }],
                            }));
                        }
                        for call in &message.tool_calls {
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": call.provider_call_id,
                                "name": mapper.to_provider(&call.tool_id).unwrap_or(call.tool_id.as_str()),
                                "arguments": call.arguments_json,
                            }));
                        }
                    }
                    fluxdb_core::ModelRole::Tool => match &message.tool_call_id {
                        Some(call_id) => input.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": message.text,
                        })),
                        None => input.push(serde_json::json!({
                            "type": "message",
                            "role": "user",
                            "content": [{ "type": "input_text", "text": message.text }],
                        })),
                    },
                }
            }
            let mut body = serde_json::json!({
                "model": model,
                "input": input,
                "stream": true,
            });
            if !instructions.is_empty() {
                body["instructions"] = serde_json::json!(instructions.join("\n"));
            }
            if let Some(max) = max_tokens {
                body["max_output_tokens"] = serde_json::json!(max);
            }
            if capabilities.tool_calling && !request.tools.is_empty() {
                let tools: Vec<serde_json::Value> = request
                    .tools
                    .iter()
                    .filter_map(|tool| {
                        mapper.to_provider(&tool.id).map(|name| {
                            serde_json::json!({
                                "type": "function",
                                "name": name,
                                "description": tool.description,
                                "parameters": serde_json::from_str::<serde_json::Value>(
                                    &tool.input_schema_json
                                ).unwrap_or_else(|_| serde_json::json!({"type":"object"})),
                            })
                        })
                    })
                    .collect();
                body["tools"] = serde_json::json!(tools);
            }
            body
        }
        crate::ChatProtocol::AnthropicMessages => {
            // Anthropic 的 system 是顶层字段，消息里只放 user/assistant（tool 结果用 user 承载）。
            let system: Vec<String> = request
                .messages
                .iter()
                .filter(|m| m.role == fluxdb_core::ModelRole::System)
                .map(|m| m.text.clone())
                .collect();
            let msgs: Vec<serde_json::Value> = request
                .messages
                .iter()
                .filter(|m| m.role != fluxdb_core::ModelRole::System)
                .map(|m| {
                    serde_json::json!({
                        "role": match m.role {
                            fluxdb_core::ModelRole::Assistant => "assistant",
                            _ => "user",
                        },
                        "content": m.text,
                    })
                })
                .collect();
            let mut body = serde_json::json!({
                "model": model,
                "messages": msgs,
                "max_tokens": max_tokens.unwrap_or(4096),
                "stream": true,
            });
            if !system.is_empty() {
                body["system"] = serde_json::json!(system.join("\n"));
            }
            if capabilities.tool_calling && !request.tools.is_empty() {
                let tools: Vec<serde_json::Value> = request
                    .tools
                    .iter()
                    .filter_map(|tool| {
                        mapper.to_provider(&tool.id).map(|name| {
                            serde_json::json!({
                                "name": name,
                                "description": tool.description,
                                "input_schema": serde_json::from_str::<serde_json::Value>(
                                    &tool.input_schema_json
                                ).unwrap_or_else(|_| serde_json::json!({"type":"object"})),
                            })
                        })
                    })
                    .collect();
                body["tools"] = serde_json::json!(tools);
            }
            body
        }
    }
}

/// 真实 Provider：请求前申请凭据租约，回程把工具名还原成宿主 ID。
pub struct HttpChatProvider {
    config: ProviderConfig,
    lease: Box<dyn CredentialLease>,
    transport: Box<dyn ChatTransport>,
}

impl HttpChatProvider {
    pub fn new(
        config: ProviderConfig,
        lease: Box<dyn CredentialLease>,
        transport: Box<dyn ChatTransport>,
    ) -> Self {
        Self {
            config,
            lease,
            transport,
        }
    }
}

impl std::fmt::Debug for HttpChatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpChatProvider")
            .field("model_profile_id", &self.config.model_profile_id)
            .field("model", &self.config.model)
            .field("endpoint", &self.config.endpoint)
            .field("protocol", &self.config.protocol)
            .finish()
    }
}

impl fluxdb_core::GenerationProvider for HttpChatProvider {
    fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
        self.config.capabilities
    }

    fn stream(
        &self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            let mut events = Vec::new();
            self.stream_events(request, cancel, &mut |event| events.push(event.clone())).await?;
            Ok(events)
        })
    }

    fn stream_events<'a>(
        &'a self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
        emit: &'a mut (dyn FnMut(&fluxdb_core::ModelEvent) + Send),
    ) -> fluxdb_core::AiFuture<'a, ()> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                emit(&fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Cancelled));
                return Ok(());
            }
            let mapper = ToolNameMapper::new(&request.tools)?;
            let body = build_chat_body(
                self.config.protocol,
                &self.config.model,
                &request,
                &mapper,
                self.config.capabilities,
                self.config.max_output_tokens,
            );
            let body_json = serde_json::to_string(&body)
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, format!("请求序列化失败: {e}")))?;

            // 凭据只在这一次请求内存在：租约 → 构造认证头 → 请求 → 作用域结束即丢弃。
            let lease = self.lease.lease(&self.config.model_profile_id)?;
            let auth = match self.config.auth_style {
                AuthStyle::Bearer => crate::TransportAuth::Bearer(lease.expose().to_string()),
                AuthStyle::AnthropicApiKey => crate::TransportAuth::Header {
                    name: "x-api-key",
                    value: lease.expose().to_string(),
                },
            };

            let mut mapping_error = None;
            let result = self.transport.call_events(&self.config, &auth, &body_json, &cancel,
                &mut |event| {
                    if mapping_error.is_none() {
                        mapping_error = emit_host_model_event(event, &mapper, self.config.capabilities.tool_calling, emit).err();
                    }
                });
            drop(auth);
            drop(lease);
            if let Some(error) = mapping_error { return Err(error); }
            result
        })
    }
}
