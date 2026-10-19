/// 真实 Provider：把既有 transport 包成 `GenerationProvider`（设计 §17/T017）。
///
/// 只负责"模型事件"；工具 schema 校验、审计、授权、执行与审批全部由 `TurnRunner`/运行时负责。
/// 凭据只以运行时明文存在于本结构内：不实现 `Debug`，不进日志/事件/持久化。
pub struct AiConversationProvider {
    protocol: ChatProtocol,
    endpoint: String,
    auth: TransportAuth,
    model: String,
    max_output_tokens: Option<i64>,
    capabilities: fluxdb_core::ModelCapabilities,
}

impl AiConversationProvider {
    /// 从落盘模型配置构造；凭据在本函数内 AES 解密后直接交给 transport，不返回给调用方。
    pub fn from_storage(
        storage: &fluxdb_storage::FileStorage,
        model_profile_id: &str,
    ) -> fluxdb_core::Result<Self> {
        let (model, sealed) = with_ai_storage(storage, |conn| {
            let model = fluxdb_storage::ai_platform::models::load_model(conn, model_profile_id)?;
            let sealed =
                fluxdb_storage::ai_platform::models::load_encrypted_secret(conn, &model.id)?;
            Ok((model, sealed))
        })?;
        let protocol = match model.api_protocol.as_str() {
            "openai-compatible" => ChatProtocol::OpenAiCompatible,
            "anthropic-messages" => ChatProtocol::AnthropicMessages,
            "openai-responses" => ChatProtocol::OpenAiResponses,
            _ => {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "该 Provider 暂不支持对话",
                ));
            }
        };
        let auth = if model.auth_mode == fluxdb_storage::ai_platform::models::AuthMode::None {
            TransportAuth::None
        } else {
            let sealed = sealed.ok_or_else(|| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型缺少 API Key")
            })?;
            let (cipher, _) = fluxdb_storage::ai_platform::load_ai_cipher(storage.root_dir())?;
            let key = fluxdb_storage::ai_platform::model_secret::open(&cipher, &model.id, &sealed)
                .map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型凭据不可用")
                })?;
            match protocol {
                ChatProtocol::OpenAiCompatible | ChatProtocol::OpenAiResponses => {
                    TransportAuth::Bearer(key)
                }
                ChatProtocol::AnthropicMessages => TransportAuth::Header {
                    name: "x-api-key",
                    value: key,
                },
            }
        };
        let base =
            model
                .endpoint
                .as_deref()
                .unwrap_or(if protocol == ChatProtocol::OpenAiCompatible {
                    "https://api.openai.com/v1"
                } else {
                    "https://api.anthropic.com/v1"
                });
        let endpoint = ai_request_url(base, &model.api_protocol, model.endpoint_is_full_url)?;
        Ok(Self {
            protocol,
            endpoint,
            auth,
            model: model.model.clone(),
            max_output_tokens: resolve_output_token_limit(
                &model.api_protocol,
                model.context_window_tokens,
                model.max_output_tokens,
            ),
            capabilities: fluxdb_core::ModelCapabilities {
                streaming: true,
                // 工具交给模型前仍要经过内核授权；这里只声明协议能力。
                tool_calling: true,
                reasoning_summary: false,
            },
        })
    }
}

impl fluxdb_core::GenerationProvider for AiConversationProvider {
    fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
        self.capabilities
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
            let mapper = ToolNameMapper::new(&request.tools)?;
            let body = build_chat_body(
                self.protocol, &self.model, &request, &mapper,
                self.capabilities, self.max_output_tokens,
            );
            let headers: &[(&str, &str)] = if self.protocol == ChatProtocol::AnthropicMessages {
                &[("anthropic-version", "2023-06-01")]
            } else {
                &[]
            };
            let mut mapping_error = None;
            let result = stream_chat_events(
                TransportLimits { max_total_bytes: 16 * 1024 * 1024, ..Default::default() },
                self.protocol, &self.endpoint, &self.auth, &body.to_string(), headers, &cancel,
                &mut |event| {
                    if mapping_error.is_none() {
                        mapping_error = emit_host_model_event(event, &mapper, self.capabilities.tool_calling, emit).err();
                    }
                },
            ).map_err(TransportError::to_core_error);
            if let Some(error) = mapping_error { return Err(error); }
            result
        })
    }
}
