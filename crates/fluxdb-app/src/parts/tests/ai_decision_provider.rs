// System One / Jev 决策 Provider 测试（设计 §18，T036）。
//
// 全部离线：协议解析是纯函数；Provider 逻辑用假 transport + 假凭据租约，
// 不发网络、不读真实配置、不接触真实凭据。最后一个本地 mock server 用例只在
// 沙箱允许绑定回环端口时执行。

fn decision_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(future)
}

fn candidates() -> Vec<String> {
    vec!["model-a".to_string(), "model-b".to_string()]
}

#[test]
fn decision_parser_accepts_valid_choice_answer() {
    let body = serde_json::json!({
        "answers": {"decision": {
            "type": "choice",
            "choice": "model-b",
            "probabilities": {"model-a": 0.2, "model-b": 0.8},
            "confidence": 0.8
        }}
    });
    let response = crate::parse_systemone_answers(&body["answers"], &candidates(), None)
        .expect("合法 choice 应解析");
    assert_eq!(response.chosen.as_deref(), Some("model-b"));
    assert_eq!(response.confidence, Some(0.8));
    assert_eq!(response.usage, None);
    assert_eq!(
        response.scores,
        vec![("model-a".to_string(), 0.2), ("model-b".to_string(), 0.8)]
    );
}

#[test]
fn decision_parser_accepts_valid_score_answer() {
    let body = serde_json::json!({
        "answers": {"decision": {"type": "score", "score": 3.5}}
    });
    let response = crate::parse_systemone_answers(&body["answers"], &candidates(), None)
        .expect("合法 score 应解析");
    assert_eq!(response.chosen, None, "score 不产生稳定 ID");
    assert_eq!(response.confidence, None, "缺 confidence 不得伪造 0");
    assert_eq!(
        response.scores,
        vec![("model-a".to_string(), 3.5), ("model-b".to_string(), 3.5)]
    );
}

#[test]
fn decision_parser_rejects_choice_outside_candidates() {
    let body = serde_json::json!({
        "answers": {"decision": {
            "type": "choice", "choice": "model-x",
            "probabilities": {"model-a": 0.1}, "confidence": 0.5
        }}
    });
    let error = crate::parse_systemone_answers(&body["answers"], &candidates(), None)
        .expect_err("候选外的 choice 必须拒绝");
    assert!(matches!(error, crate::DecisionProtocolError::Malformed(_)));
}

#[test]
fn decision_parser_rejects_invalid_probabilities() {
    for (label, probabilities) in [
        ("负数", serde_json::json!({"model-a": -0.1, "model-b": 1.1})),
        ("超过 1", serde_json::json!({"model-a": 0.2, "model-b": 1.5})),
        ("字符串数字", serde_json::json!({"model-a": "0.2", "model-b": "0.8"})),
        ("非对象", serde_json::json!([0.2, 0.8])),
    ] {
        let body = serde_json::json!({
            "answers": {"decision": {
                "type": "choice", "choice": "model-b",
                "probabilities": probabilities, "confidence": 0.5
            }}
        });
        let error = crate::parse_systemone_answers(&body["answers"], &candidates(), None)
            .unwrap_err();
        assert!(
            matches!(error, crate::DecisionProtocolError::Malformed(_)),
            "{label} 概率必须拒绝，实际: {error:?}"
        );
    }
}

#[test]
fn decision_parser_rejects_invalid_confidence_and_score() {
    for (label, answer) in [
        (
            "confidence 超界",
            serde_json::json!({"type": "choice", "choice": "model-a", "probabilities": {"model-a": 1.0}, "confidence": 1.2}),
        ),
        (
            "confidence 非数",
            serde_json::json!({"type": "choice", "choice": "model-a", "probabilities": {"model-a": 1.0}, "confidence": "high"}),
        ),
        (
            "score 非数",
            serde_json::json!({"type": "score", "score": "3"}),
        ),
        (
            "未知类型",
            serde_json::json!({"type": "noul", "noul": 0.9}),
        ),
    ] {
        let body = serde_json::json!({ "answers": {"decision": answer} });
        let error = crate::parse_systemone_answers(&body["answers"], &candidates(), None)
            .unwrap_err();
        assert!(
            matches!(error, crate::DecisionProtocolError::Malformed(_)),
            "{label} 必须拒绝，实际: {error:?}"
        );
    }
}

#[test]
fn decision_parser_rejects_missing_answer() {
    let body = serde_json::json!({ "answers": {} });
    let error =
        crate::parse_systemone_answers(&body["answers"], &candidates(), None).unwrap_err();
    assert!(matches!(error, crate::DecisionProtocolError::Malformed(_)));
}

#[test]
fn decision_request_body_is_choice_with_candidate_criteria() {
    let request = fluxdb_core::DecisionRequest {
        model: "jev-model".into(),
        candidates: candidates(),
        prompt: "选择最合适的候选".into(),
    };
    let body = crate::build_systemone_body("jev-model", &request);
    assert_eq!(body["model"], "jev-model");
    assert_eq!(body["state"]["text"], "选择最合适的候选");
    let question = &body["questions"]["decision"];
    assert_eq!(question["type"], "choice");
    assert_eq!(
        question["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        candidates()
    );
    // 请求体不得包含任何凭据字段。
    let serialized = body.to_string();
    assert!(!serialized.contains("Authorization") && !serialized.contains("api_key"));
}

/// 假 transport：记录请求并返回预置响应。克隆共享同一份记录状态。
#[derive(Clone)]
struct FakeDecisionTransport(std::sync::Arc<FakeDecisionTransportState>);

struct FakeDecisionTransportState {
    calls: std::sync::Mutex<Vec<(String, String)>>,
    response: std::sync::Mutex<Result<serde_json::Value, crate::DecisionProtocolError>>,
}

impl FakeDecisionTransport {
    fn new(response: Result<serde_json::Value, crate::DecisionProtocolError>) -> Self {
        Self(std::sync::Arc::new(FakeDecisionTransportState {
            calls: std::sync::Mutex::new(Vec::new()),
            response: std::sync::Mutex::new(response),
        }))
    }

    /// 记录到的 (endpoint, 脱敏 auth Debug) 调用列表。
    fn calls(&self) -> Vec<(String, String)> {
        self.0.calls.lock().expect("假 transport 状态锁").clone()
    }
}

impl crate::DecisionTransport for FakeDecisionTransport {
    fn call(
        &self,
        endpoint: &str,
        auth: &crate::TransportAuth,
        body_json: &str,
        _cancel: &fluxdb_core::CancelFlag,
        _timeout: std::time::Duration,
    ) -> Result<serde_json::Value, crate::DecisionProtocolError> {
        let auth_debug = format!("{auth:?}");
        self.0
            .calls
            .lock()
            .expect("假 transport 状态锁")
            .push((endpoint.to_string(), auth_debug));
        self.0
            .response
            .lock()
            .expect("假 transport 状态锁")
            .clone()
            .map(|mut value| {
                // 把请求体塞进返回体便于断言（测试专用）。
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "_body".to_string(),
                        serde_json::Value::String(body_json.to_string()),
                    );
                }
                value
            })
    }
}

struct CountingDecisionLease {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::DecisionCredentialLease for CountingDecisionLease {
    fn lease(&self, _model_profile_id: &str) -> fluxdb_core::Result<crate::SecretLease> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::SecretLease::new("sk-decision-test-secret"))
    }
}

fn decision_provider(
    transport: FakeDecisionTransport,
) -> (
    crate::SystemOneDecisionProvider,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let lease_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = crate::SystemOneDecisionProvider::new(
        "decision-profile",
        "jev-model",
        "https://decision.example.invalid/systemone",
        crate::AuthStyle::Bearer,
        std::sync::Arc::new(CountingDecisionLease {
            calls: lease_calls.clone(),
        }),
        std::sync::Arc::new(transport),
        std::time::Duration::from_millis(800),
    );
    (provider, lease_calls)
}

#[test]
fn decision_provider_maps_choice_and_records_usage() {
    let transport = FakeDecisionTransport::new(Ok(serde_json::json!({
        "answers": {"decision": {
            "type": "choice", "choice": "model-a",
            "probabilities": {"model-a": 0.9, "model-b": 0.1}, "confidence": 0.9
        }},
        "usage": {"input_tokens": 42, "output_tokens": 7}
    })));
    let recorded = transport.clone();
    let (provider, lease_calls) = decision_provider(transport);
    assert_eq!(lease_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "未调用前不得解密凭据");
    let response = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("合法决策响应");
    assert_eq!(response.chosen.as_deref(), Some("model-a"));
    assert_eq!(response.confidence, Some(0.9));
    assert_eq!(
        response.usage,
        Some(fluxdb_core::Usage { input_tokens: 42, output_tokens: 7, cached_input_tokens: 0 })
    );
    assert_eq!(lease_calls.load(std::sync::atomic::Ordering::SeqCst), 1, "请求前申请一次凭据");
    let calls = recorded.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "https://decision.example.invalid/systemone");
    assert!(calls[0].1.contains("[REDACTED]"), "认证 Debug 必须脱敏: {}", calls[0].1);
}

#[test]
fn decision_provider_missing_usage_is_not_fabricated() {
    let transport = FakeDecisionTransport::new(Ok(serde_json::json!({
        "answers": {"decision": {
            "type": "choice", "choice": "model-b",
            "probabilities": {"model-a": 0.1, "model-b": 0.9}, "confidence": 0.9
        }}
    })));
    let (provider, _) = decision_provider(transport);
    let response = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("缺 usage 仍应成功解析答案");
    assert_eq!(response.chosen.as_deref(), Some("model-b"));
    assert_eq!(response.usage, None, "缺失 usage 必须保留 None，不伪造零");
}

#[test]
fn decision_provider_rejects_malformed_response_without_leaking_body() {
    let transport = FakeDecisionTransport::new(Ok(serde_json::json!({
        "answers": {"decision": {"type": "choice", "choice": "model-x", "probabilities": {}, "confidence": 0.5}},
        "debug_secret": "sk-should-never-leak"
    })));
    let (provider, _) = decision_provider(transport);
    let error = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("候选外 choice 必须拒绝");
    assert!(!error.message.contains("sk-should-never-leak"), "错误不得外泄响应正文");
}

#[test]
fn decision_provider_rejects_http_status() {
    let transport = FakeDecisionTransport::new(Err(crate::DecisionProtocolError::HttpStatus(503)));
    let (provider, _) = decision_provider(transport);
    let error = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("HTTP 503 必须失败");
    assert!(error.message.contains("503"), "实际错误：{}", error.message);
}

#[test]
fn decision_provider_honors_pre_cancel_without_request() {
    let transport = FakeDecisionTransport::new(Ok(serde_json::json!({})));
    let (provider, lease_calls) = decision_provider(transport);
    let cancel = fluxdb_core::CancelFlag::new();
    cancel.request();
    let error = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        cancel,
    ))
    .expect_err("取消后不得发请求");
    assert_eq!(error.kind, fluxdb_core::ErrorKind::Cancelled);
    assert_eq!(
        lease_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "取消路径不得解密凭据"
    );
}

#[test]
fn decision_provider_requires_candidates() {
    let transport = FakeDecisionTransport::new(Ok(serde_json::json!({})));
    let (provider, lease_calls) = decision_provider(transport);
    let error = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: Vec::new(),
            prompt: "没有候选".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect_err("无候选必须拒绝");
    assert!(error.message.contains("候选"), "实际错误：{}", error.message);
    assert_eq!(lease_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn decision_auth_debug_never_reveals_secret() {
    let auth = crate::TransportAuth::Bearer("sk-decision-test-secret".into());
    assert!(!format!("{auth:?}").contains("sk-decision-test-secret"));
}

/// 真实 HTTP 往返：本地 mock 返回固定 Choice，验证请求路径、认证头与解析闭环。
/// 若沙箱禁止绑定回环端口，本用例明确失败并记录为受阻。
#[test]
fn decision_provider_real_http_round_trip() {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("设置非阻塞");
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
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
                Err(_) => break,
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
        let body = r#"{"answers":{"decision":{"type":"choice","choice":"model-a","probabilities":{"model-a":0.95,"model-b":0.05},"confidence":0.95}},"usage":{"input_tokens":11,"output_tokens":3}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("写 mock 响应");
        String::from_utf8_lossy(&request).into_owned()
    });

    let lease_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = crate::SystemOneDecisionProvider::new(
        "decision-profile",
        "jev-model",
        endpoint,
        crate::AuthStyle::Bearer,
        std::sync::Arc::new(CountingDecisionLease {
            calls: lease_calls.clone(),
        }),
        std::sync::Arc::new(crate::UreqDecisionTransport),
        std::time::Duration::from_millis(2_000),
    );
    let response = decision_block_on(fluxdb_core::DecisionProvider::decide(
        &provider,
        fluxdb_core::DecisionRequest {
            model: "jev-model".into(),
            candidates: candidates(),
            prompt: "选择".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("真实 HTTP 往返");
    assert_eq!(response.chosen.as_deref(), Some("model-a"));
    let request = handle.join().expect("mock 线程");
    assert!(request.starts_with("POST /systemone "), "实际请求行: {request:?}");
    assert!(
        request.to_lowercase().contains("authorization: bearer sk-decision-test-secret"),
        "必须带 Bearer 认证头"
    );
}
