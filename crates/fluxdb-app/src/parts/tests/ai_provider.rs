// Provider 适配与秘密边界测试（设计 §8/§17，T017）。
//
// 本文件只用假 lease / 假 transport 做无网络断言；最后两个本地 mock server
// 用例只在沙箱允许绑定回环端口时执行，不访问外部网络、不使用真实凭据。

use std::collections::VecDeque;

fn ai_provider_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(future)
}

fn ai_provider_tool(id: &str) -> fluxdb_core::ToolDescriptor {
    fluxdb_core::ToolDescriptor {
        id: id.to_string(),
        version: 1,
        description: format!("tool {id}"),
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json:
            r#"{"type":"object","properties":{"x":{"type":"string"}},"required":["x"]}"#.to_string(),
    }
}

fn ai_provider_request(tools: Vec<fluxdb_core::ToolDescriptor>) -> fluxdb_core::ModelRequest {
    fluxdb_core::ModelRequest {
        model: "request-model".to_string(),
        messages: vec![
            fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::System, "system prompt"),
            fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::User, "hello"),
        ],
        tools,
        max_output_tokens: Some(128),
    }
}

fn ai_provider_capabilities(tool_calling: bool) -> fluxdb_core::ModelCapabilities {
    fluxdb_core::ModelCapabilities {
        streaming: true,
        tool_calling,
        reasoning_summary: false,
    }
}

fn ai_provider_config(
    protocol: crate::ChatProtocol,
    capabilities: fluxdb_core::ModelCapabilities,
) -> crate::ProviderConfig {
    crate::ProviderConfig {
        model_profile_id: "test-profile".to_string(),
        model: "test-model".to_string(),
        endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
        protocol,
        auth_style: crate::AuthStyle::Bearer,
        capabilities,
        limits: crate::TransportLimits::default(),
        max_output_tokens: Some(64),
    }
}

#[derive(Clone, Debug)]
struct AiProviderCapturedCall {
    config: crate::ProviderConfig,
    auth: crate::TransportAuth,
    body_json: String,
}

struct AiProviderFakeState {
    calls: Vec<AiProviderCapturedCall>,
    responses: VecDeque<fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>>,
}

#[derive(Clone)]
struct AiProviderFakeTransport {
    state: Arc<std::sync::Mutex<AiProviderFakeState>>,
}

impl AiProviderFakeTransport {
    fn new(responses: Vec<fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>>) -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(AiProviderFakeState {
                calls: Vec::new(),
                responses: responses.into(),
            })),
        }
    }

    fn calls(&self) -> Vec<AiProviderCapturedCall> {
        self.state
            .lock()
            .expect("fake transport state")
            .calls
            .clone()
    }
}

impl crate::ChatTransport for AiProviderFakeTransport {
    fn call(
        &self,
        config: &crate::ProviderConfig,
        auth: &crate::TransportAuth,
        body_json: &str,
        _cancel: &fluxdb_core::CancelFlag,
    ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>> {
        let mut state = self.state.lock().expect("fake transport state");
        state.calls.push(AiProviderCapturedCall {
            config: config.clone(),
            auth: auth.clone(),
            body_json: body_json.to_string(),
        });
        state
            .responses
            .pop_front()
            .expect("fake transport 收到未预置的调用")
    }
}

struct AiProviderCountingLease {
    calls: Arc<AtomicUsize>,
    key: String,
}

impl crate::CredentialLease for AiProviderCountingLease {
    fn lease(&self, _model_profile_id: &str) -> fluxdb_core::Result<crate::SecretLease> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::SecretLease::new(self.key.clone()))
    }
}

#[test]
fn ai_provider_tool_name_mapper_is_reversible_and_rejects_collisions() {
    let mapper = crate::ToolNameMapper::new(&[
        ai_provider_tool("db.query"),
        ai_provider_tool("redis.execute"),
    ])
    .expect("无冲突工具名应注册成功");

    assert_eq!(mapper.to_provider("db.query"), Some("db_query"));
    assert_eq!(mapper.to_provider("redis.execute"), Some("redis_execute"));
    assert_eq!(mapper.to_host("db_query"), Some("db.query"));
    assert_eq!(mapper.to_host("redis_execute"), Some("redis.execute"));

    let error = match crate::ToolNameMapper::new(&[
        ai_provider_tool("db.query"),
        ai_provider_tool("db/query"),
    ]) {
        Ok(_) => panic!("sanitize 后同名必须在注册时拒绝"),
        Err(error) => error,
    };
    assert!(error.message.contains("冲突"), "实际错误：{error}");
}

#[test]
fn ai_provider_chat_bodies_exclude_credentials_and_map_tool_names() {
    let request = ai_provider_request(vec![
        ai_provider_tool("db.query"),
        ai_provider_tool("redis.execute"),
    ]);
    let mapper = crate::ToolNameMapper::new(&request.tools).expect("工具名注册");
    let capabilities = ai_provider_capabilities(true);

    let openai = crate::build_chat_body(
        crate::ChatProtocol::OpenAiCompatible,
        "body-model",
        &request,
        &mapper,
        capabilities,
        Some(256),
    );
    let openai_text = openai.to_string().to_ascii_lowercase();
    for forbidden in ["api_key", "apikey", "authorization", "secret", "ciphertext"] {
        assert!(
            !openai_text.contains(forbidden),
            "OpenAI body 泄漏敏感词 {forbidden}"
        );
    }
    assert_eq!(
        openai["tools"][0]["function"]["name"],
        serde_json::json!("db_query")
    );
    assert!(openai["tools"][0]["function"]["parameters"].is_object());
    assert!(
        openai["tools"]
            .as_array()
            .expect("tools 数组")
            .iter()
            .all(|tool| tool["function"]["name"] != serde_json::json!("db.query"))
    );

    let anthropic = crate::build_chat_body(
        crate::ChatProtocol::AnthropicMessages,
        "body-model",
        &request,
        &mapper,
        capabilities,
        Some(256),
    );
    let anthropic_text = anthropic.to_string().to_ascii_lowercase();
    for forbidden in ["api_key", "apikey", "authorization", "secret", "ciphertext"] {
        assert!(
            !anthropic_text.contains(forbidden),
            "Anthropic body 泄漏敏感词 {forbidden}"
        );
    }
    assert_eq!(anthropic["system"], serde_json::json!("system prompt"));
    assert_eq!(anthropic["tools"][0]["name"], serde_json::json!("db_query"));
    assert!(anthropic["tools"][0]["input_schema"].is_object());
    assert!(
        anthropic["messages"]
            .as_array()
            .expect("messages 数组")
            .iter()
            .all(|message| message["role"] != serde_json::json!("system")),
        "Anthropic 顶层 system 不应重复出现在 messages"
    );
}

#[test]
fn ai_provider_capability_gate_omits_tools_and_rejects_tool_calls() {
    let request = ai_provider_request(vec![ai_provider_tool("db.query")]);
    let mapper = crate::ToolNameMapper::new(&request.tools).expect("工具名注册");
    let capabilities = ai_provider_capabilities(false);

    for protocol in [
        crate::ChatProtocol::OpenAiCompatible,
        crate::ChatProtocol::AnthropicMessages,
    ] {
        let body = crate::build_chat_body(
            protocol,
            "body-model",
            &request,
            &mapper,
            capabilities,
            None,
        );
        assert!(
            body.get("tools").is_none(),
            "未声明 tool_calling 时不得发送 tools"
        );
    }

    let transport =
        AiProviderFakeTransport::new(vec![Ok(vec![fluxdb_core::ModelEvent::ToolCallRequested(
            fluxdb_core::ToolCallRequest {
                provider_call_id: "call-1".to_string(),
                tool_id: "db_query".to_string(),
                arguments_json: "{}".to_string(),
            },
        )])]);
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(crate::ChatProtocol::OpenAiCompatible, capabilities),
        Box::new(AiProviderCountingLease {
            calls: Arc::new(AtomicUsize::new(0)),
            key: "sk-provider-test-secret".to_string(),
        }),
        Box::new(transport),
    );

    let error = ai_provider_block_on(provider.stream(
        ai_provider_request(vec![ai_provider_tool("db.query")]),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("能力关闭时收到工具调用必须是协议错误");
    assert!(
        error.message.contains("未声明工具能力"),
        "实际错误：{error}"
    );
}

#[test]
fn ai_provider_maps_returned_provider_tool_names_to_host_ids() {
    let transport =
        AiProviderFakeTransport::new(vec![Ok(vec![fluxdb_core::ModelEvent::ToolCallRequested(
            fluxdb_core::ToolCallRequest {
                provider_call_id: "call-1".to_string(),
                tool_id: "db_query".to_string(),
                arguments_json: r#"{"x":"1"}"#.to_string(),
            },
        )])]);
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(
            crate::ChatProtocol::OpenAiCompatible,
            ai_provider_capabilities(true),
        ),
        Box::new(AiProviderCountingLease {
            calls: Arc::new(AtomicUsize::new(0)),
            key: "sk-provider-test-secret".to_string(),
        }),
        Box::new(transport),
    );

    let events = ai_provider_block_on(provider.stream(
        ai_provider_request(vec![ai_provider_tool("db.query")]),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("已知工具名应回程映射");

    assert_eq!(
        events,
        vec![fluxdb_core::ModelEvent::ToolCallRequested(
            fluxdb_core::ToolCallRequest {
                provider_call_id: "call-1".to_string(),
                tool_id: "db.query".to_string(),
                arguments_json: r#"{"x":"1"}"#.to_string(),
            }
        )]
    );
}

#[test]
fn ai_provider_rejects_unknown_returned_tool_names() {
    let transport =
        AiProviderFakeTransport::new(vec![Ok(vec![fluxdb_core::ModelEvent::ToolCallRequested(
            fluxdb_core::ToolCallRequest {
                provider_call_id: "call-1".to_string(),
                tool_id: "not_registered".to_string(),
                arguments_json: "{}".to_string(),
            },
        )])]);
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(
            crate::ChatProtocol::OpenAiCompatible,
            ai_provider_capabilities(true),
        ),
        Box::new(AiProviderCountingLease {
            calls: Arc::new(AtomicUsize::new(0)),
            key: "sk-provider-test-secret".to_string(),
        }),
        Box::new(transport),
    );

    let error = ai_provider_block_on(provider.stream(
        ai_provider_request(vec![ai_provider_tool("db.query")]),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("未注册的 Provider 工具名必须拒绝");
    assert!(error.message.contains("未注册的工具"), "实际错误：{error}");
}

#[test]
fn ai_provider_lease_is_called_once_and_debug_never_leaks_plaintext() {
    let lease_calls = Arc::new(AtomicUsize::new(0));
    let key = "sk-provider-test-secret";
    let transport =
        AiProviderFakeTransport::new(vec![Ok(vec![fluxdb_core::ModelEvent::Finished(
            fluxdb_core::FinishReason::Stop,
        )])]);
    let transport_handle = transport.clone();
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(
            crate::ChatProtocol::OpenAiCompatible,
            ai_provider_capabilities(true),
        ),
        Box::new(AiProviderCountingLease {
            calls: lease_calls.clone(),
            key: key.to_string(),
        }),
        Box::new(transport),
    );

    let events = ai_provider_block_on(provider.stream(
        ai_provider_request(Vec::new()),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("正常 stream");
    assert_eq!(events.len(), 1);
    assert_eq!(
        lease_calls.load(Ordering::SeqCst),
        1,
        "每次 stream 只租一次凭据"
    );

    let calls = transport_handle.calls();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        &calls[0].auth,
        crate::TransportAuth::Bearer(value) if value == key
    ));
    assert!(!calls[0].body_json.contains(key));
    assert_eq!(calls[0].config.model, "test-model");

    let debug = format!("{provider:?}");
    assert!(!debug.contains(key), "Provider Debug 不得含明文：{debug}");
    let lease_debug = format!("{:?}", crate::SecretLease::new(key));
    assert!(!lease_debug.contains(key));
    assert!(lease_debug.contains("REDACTED"));
}

#[test]
fn ai_provider_pre_canceled_stream_does_not_touch_credentials_or_transport() {
    let lease_calls = Arc::new(AtomicUsize::new(0));
    let transport = AiProviderFakeTransport::new(Vec::new());
    let transport_handle = transport.clone();
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(
            crate::ChatProtocol::OpenAiCompatible,
            ai_provider_capabilities(true),
        ),
        Box::new(AiProviderCountingLease {
            calls: lease_calls.clone(),
            key: "sk-provider-test-secret".to_string(),
        }),
        Box::new(transport),
    );
    let cancel = fluxdb_core::CancelFlag::new();
    cancel.request();

    let events = ai_provider_block_on(provider.stream(ai_provider_request(Vec::new()), cancel))
        .expect("预取消不是传输错误");
    assert_eq!(
        events,
        vec![fluxdb_core::ModelEvent::Finished(
            fluxdb_core::FinishReason::Cancelled
        )]
    );
    assert_eq!(lease_calls.load(Ordering::SeqCst), 0);
    assert!(transport_handle.calls().is_empty());
}

#[test]
fn ai_provider_transport_errors_are_preserved_without_credentials() {
    let key = "sk-provider-test-secret";
    let transport = AiProviderFakeTransport::new(vec![Err(fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        "transport down",
    ))]);
    let provider = crate::HttpChatProvider::new(
        ai_provider_config(
            crate::ChatProtocol::OpenAiCompatible,
            ai_provider_capabilities(false),
        ),
        Box::new(AiProviderCountingLease {
            calls: Arc::new(AtomicUsize::new(0)),
            key: key.to_string(),
        }),
        Box::new(transport),
    );

    let error = ai_provider_block_on(provider.stream(
        ai_provider_request(Vec::new()),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("transport 错误必须透传");
    assert_eq!(error.kind, fluxdb_core::ErrorKind::Internal);
    assert!(error.message.contains("transport down"));
    assert!(!error.message.contains(key));
}

// 本地回环 mock：仅用于 UreqChatTransport 的状态码与截断 SSE 边界。
// 某些沙箱禁止 bind，此时该用例会按真实失败报告为受阻，而不是伪造成功。
fn ai_provider_mock_server(
    status: &'static str,
    body: Vec<u8>,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定回环 mock 端口");
    listener.set_nonblocking(true).expect("设置非阻塞");
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().expect("mock 地址")
    );
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => panic!("未收到 mock 请求: {other:?}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .expect("设置读取超时");
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
                Err(error) => panic!("读取 mock 请求失败: {error}"),
            }
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]);
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("写 mock 响应头");
        stream.write_all(&body).expect("写 mock 响应体");
    });
    (url, handle)
}

#[test]
fn ai_provider_ureq_transport_preserves_http_429_and_drops_truncated_sse() {
    let (url, handle) = ai_provider_mock_server("429 Too Many Requests", Vec::new());
    let mut config = ai_provider_config(
        crate::ChatProtocol::OpenAiCompatible,
        ai_provider_capabilities(false),
    );
    config.endpoint = url;
    let error = crate::UreqChatTransport
        .call(
            &config,
            &crate::TransportAuth::Bearer("sk-provider-test-secret".to_string()),
            "{}",
            &fluxdb_core::CancelFlag::new(),
        )
        .expect_err("429 必须映射为 HTTP 状态错误");
    assert_eq!(error.kind, fluxdb_core::ErrorKind::Internal);
    assert!(error.message.contains("HTTP 429"), "实际错误：{error}");
    handle.join().expect("429 mock server");

    let truncated =
        b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n"
            .to_vec();
    let (url, handle) = ai_provider_mock_server("200 OK", truncated);
    config.endpoint = url;
    let events = crate::UreqChatTransport
        .call(
            &config,
            &crate::TransportAuth::Bearer("sk-provider-test-secret".to_string()),
            "{}",
            &fluxdb_core::CancelFlag::new(),
        )
        .expect("截断帧不应 panic，残余按 SSE 语义丢弃");
    assert!(events.is_empty(), "未以空行结束的残余帧不得产出事件");
    handle.join().expect("truncated mock server");
}

/// T017 缺口补测：1) 3xx 不跟随重定向（`max_redirects(0)`）；2) Anthropic 形态带 `anthropic-version` 头。
///
/// 沙箱内禁止绑定回环端口，本用例需在沙箱外运行（与其它 mock server 用例一致）。
#[test]
fn ai_provider_does_not_follow_redirects_and_sends_anthropic_version() {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定回环 mock 端口");
    listener.set_nonblocking(true).expect("非阻塞");
    let addr = listener.local_addr().expect("mock 地址");
    let url = format!("http://{addr}/v1/messages");

    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => panic!("未收到请求: {other:?}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .expect("读取超时");
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&request).to_string();
        // 3xx + 指向同主机的另一个路径：如果实现跟随重定向，就会产生第二次连接。
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        drop(stream);

        // 短暂窗口内是否出现第二次连接（出现即说明跟随了重定向）。
        let followed = {
            let start = std::time::Instant::now();
            let mut seen = false;
            while start.elapsed() < std::time::Duration::from_millis(300) {
                match listener.accept() {
                    Ok(_) => {
                        seen = true;
                        break;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
            seen
        };
        (head, followed)
    });

    let mut config = ai_provider_config(
        crate::ChatProtocol::AnthropicMessages,
        ai_provider_capabilities(false),
    );
    config.endpoint = url;
    config.auth_style = crate::AuthStyle::AnthropicApiKey;

    let error = crate::UreqChatTransport
        .call(
            &config,
            &crate::TransportAuth::Header {
                name: "x-api-key",
                value: "sk-provider-test-secret".to_string(),
            },
            "{}",
            &fluxdb_core::CancelFlag::new(),
        )
        .expect_err("3xx 必须按 HTTP 状态返回，而不是跟随重定向");
    assert!(error.message.contains("HTTP 302"), "实际错误：{error}");

    let (head, followed) = server.join().expect("redirect mock server");
    assert!(!followed, "不得跟随重定向（max_redirects(0)）");
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains("anthropic-version: 2023-06-01"),
        "Anthropic 形态必须带 anthropic-version 头，实际请求头:\n{head}"
    );
}

#[test]
fn ai_provider_responses_body_pairs_tool_calls_and_outputs() {
    use fluxdb_core::{ModelMessage, ModelRole, ModelToolCall};
    let mapper = ToolNameMapper::new(&[fluxdb_core::ToolDescriptor {
        id: "db.query".into(),
        version: 1,
        description: "查询".into(),
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json: r#"{"type":"object"}"#.into(),
    }])
    .expect("映射");
    let request = fluxdb_core::ModelRequest {
        model: "m".into(),
        messages: vec![
            ModelMessage::new(ModelRole::System, "你是数据库助手"),
            ModelMessage::new(ModelRole::User, "查一下"),
            ModelMessage::assistant_with_tool_calls(
                "",
                vec![ModelToolCall {
                    provider_call_id: "call_1".into(),
                    tool_id: "db.query".into(),
                    arguments_json: r#"{"sql":"select 1"}"#.into(),
                }],
            ),
            ModelMessage::tool_result("call_1", "db.query", r#"{"tool":"db.query"}"#),
        ],
        tools: vec![fluxdb_core::ToolDescriptor {
            id: "db.query".into(),
            version: 1,
            description: "查询".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: r#"{"type":"object"}"#.into(),
        }],
        max_output_tokens: Some(64),
    };
    let body = build_chat_body(
        ChatProtocol::OpenAiResponses,
        "m",
        &request,
        &mapper,
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        Some(64),
    );
    // system 进顶层 instructions，不进 input。
    assert_eq!(body["instructions"], serde_json::json!("你是数据库助手"));
    assert_eq!(body["stream"], serde_json::json!(true));
    assert_eq!(body["max_output_tokens"], serde_json::json!(64));
    // 工具名必须用 Provider 安全名（点号会被替换）。
    assert_eq!(body["tools"][0]["name"], serde_json::json!("db_query"));
    assert_eq!(body["tools"][0]["type"], serde_json::json!("function"));
    let input = body["input"].as_array().expect("input 数组");
    let kinds: Vec<&str> = input
        .iter()
        .map(|item| item["type"].as_str().unwrap_or("message"))
        .collect();
    assert_eq!(kinds, vec!["message", "function_call", "function_call_output"]);
    // assistant 的 function_call 必须带 call_id + Provider 名；结果必须回到同一 call_id。
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("function_call");
    assert_eq!(call["call_id"], serde_json::json!("call_1"));
    assert_eq!(call["name"], serde_json::json!("db_query"));
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function_call_output");
    assert_eq!(output["call_id"], serde_json::json!("call_1"));
    assert_eq!(output["output"], serde_json::json!(r#"{"tool":"db.query"}"#));
}

#[test]
fn ai_provider_chat_completions_body_pairs_tool_calls_and_outputs() {
    use fluxdb_core::{ModelMessage, ModelRole, ModelToolCall};
    let mapper = ToolNameMapper::new(&[fluxdb_core::ToolDescriptor {
        id: "db.query".into(),
        version: 1,
        description: "查询".into(),
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json: r#"{"type":"object"}"#.into(),
    }])
    .expect("映射");
    let request = fluxdb_core::ModelRequest {
        model: "m".into(),
        messages: vec![
            ModelMessage::new(ModelRole::User, "查一下"),
            ModelMessage::assistant_with_tool_calls(
                "",
                vec![ModelToolCall {
                    provider_call_id: "call_9".into(),
                    tool_id: "db.query".into(),
                    arguments_json: r#"{"sql":"select 1"}"#.into(),
                }],
            ),
            ModelMessage::tool_result("call_9", "db.query", r#"{"status":"succeeded"}"#),
        ],
        tools: Vec::new(),
        max_output_tokens: Some(32),
    };
    let body = build_chat_body(
        ChatProtocol::OpenAiCompatible,
        "m",
        &request,
        &mapper,
        fluxdb_core::ModelCapabilities::default(),
        Some(32),
    );
    let messages = body["messages"].as_array().expect("messages");
    let assistant = messages
        .iter()
        .find(|m| m["role"] == serde_json::json!("assistant"))
        .expect("assistant 消息");
    // 只有工具调用没有正文时 content 必须是 null，且 tool_calls 带 id 与 Provider 安全名。
    assert!(assistant["content"].is_null());
    assert_eq!(assistant["tool_calls"][0]["id"], serde_json::json!("call_9"));
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        serde_json::json!("db_query")
    );
    let tool = messages
        .iter()
        .find(|m| m["role"] == serde_json::json!("tool"))
        .expect("tool 消息");
    assert_eq!(tool["tool_call_id"], serde_json::json!("call_9"));
}
