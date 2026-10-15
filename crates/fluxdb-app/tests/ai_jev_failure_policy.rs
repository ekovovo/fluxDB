//! T037 Jev 故障策略：超时 / 非法返回 / 低置信度 / 无凭据 → 走基础路径，熔断生效。
//!
//! 这些是**受控注入**用例：用本地 mock System One 服务制造超时、非法返回与低置信度，
//! 断言基础 Agent 照常运行、不会每轮重复等待或反复报错。与真实成功链路的验收分开报告。

use std::{
    fs,
    io::{Read as _, Write as _},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use fluxdb_app::reconcile_jev_builtins;
use fluxdb_app::{RelevanceCandidate, advise_relevance};
use fluxdb_storage::FileStorage;
use fluxdb_storage::ai_platform::{agents, models, sessions, settings};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t037-fault-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        let fixture = Self { dir, storage };
        fixture
            .storage
            .with_ai_connection(|conn| {
                sessions::create_session(
                    conn,
                    1_000,
                    "t037-fault",
                    agents::BUILTIN_AGENT_ID,
                    "T037",
                )?;
                sessions::start_turn(conn, 1_000, "t037-fault", "t037-fault-turn", "{}", "{}")?;
                Ok(())
            })
            .unwrap();
        fixture
    }

    /// 写入一个指向本地 mock 的 System One 决策模型；`with_key=false` 模拟无凭据。
    fn decision_model(&self, id: &str, endpoint: &str, with_key: bool) {
        self.storage
            .with_ai_connection(|conn| {
                let (cipher, _) =
                    fluxdb_storage::ai_platform::load_ai_cipher(self.storage.root_dir())?;
                models::save_model(
                    conn,
                    &cipher,
                    "fixture-key",
                    2_000,
                    &models::ModelEdit {
                        id: id.into(),
                        display_name: id.into(),
                        provider: "decisions".into(),
                        api_protocol: "system-one".into(),
                        kind: models::ModelKind::Decision,
                        model: format!("model-{id}"),
                        endpoint: Some(endpoint.into()),
                        endpoint_is_full_url: true,
                        enabled: true,
                        auth_mode: models::AuthMode::ApiKey,
                        egress: models::EgressPermission::Allow,
                        context_window_tokens: Some(8_192),
                        max_output_tokens: Some(512),
                        input_price_micros_per_million: None,
                        output_price_micros_per_million: None,
                        cached_input_price_micros_per_million: None,
                    },
                    None,
                    if with_key {
                        models::ApiKeyChange::Set(
                            fluxdb_storage::ai_platform::model_secret::SecretInput::new(
                                "sk-fixture-not-real".to_string(),
                            )
                            .unwrap(),
                        )
                    } else {
                        models::ApiKeyChange::Keep
                    },
                )
            })
            .unwrap();
        // 决策模型必须由用户显式选择（「Jev 决策优化 · 管理」）；fixture 直接写槽位，
        // 再交给 reconcile 建立 system 绑定。
        self.storage
            .with_ai_connection(|conn| {
                let current = settings::load_runtime_settings(conn)?;
                let next = settings::RuntimeSettings {
                    default_decision_model_profile_id: Some(id.to_string()),
                    ..current.clone()
                };
                settings::save_runtime_settings(conn, 2_500, &next, current.revision)?;
                Ok(())
            })
            .unwrap();
        self.reconcile();
    }

    fn reconcile(&self) {
        self.storage
            .with_ai_connection(|conn| {
                reconcile_jev_builtins(conn, 3_000)?;
                Ok(())
            })
            .unwrap();
    }

    fn advice(&self, model_hint: &str) -> fluxdb_app::JevAdvice {
        advise_relevance(
            &self.storage,
            Some(agents::BUILTIN_AGENT_ID),
            "t037-fault",
            "t037-fault-turn",
            &[
                RelevanceCandidate {
                    id: "object:a".into(),
                    label: "orders".into(),
                    kind: "table".into(),
                },
                RelevanceCandidate {
                    id: "object:b".into(),
                    label: "customers".into(),
                    kind: "table".into(),
                },
                RelevanceCandidate {
                    id: format!("object:{model_hint}"),
                    label: "audit_log".into(),
                    kind: "table".into(),
                },
            ],
            &fluxdb_core::CancelFlag::new(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// 本地 mock System One 服务：`mode` 决定返回/行为，返回收到的请求数句柄。
/// 本地 mock System One 服务。
///
/// 每个连接单独起线程：挂起模式会占住连接 30s，串行 accept 会让重试的第二次请求
/// 排不上队，测出来的就不是重试行为而是 mock 的串行缺陷。
fn spawn_mock(mode: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("非阻塞");
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let counter = Arc::clone(&counter);
                    std::thread::spawn(move || handle_mock_connection(stream, mode, counter));
                }
                // 非阻塞 socket 在没有连接时返回 WouldBlock，必须继续等而不是退出。
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });
    (endpoint, hits)
}

fn handle_mock_connection(
    mut stream: std::net::TcpStream,
    mode: &'static str,
    counter: Arc<AtomicUsize>,
) {
    counter.fetch_add(1, Ordering::SeqCst);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(read) = stream.read(&mut chunk) {
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&request).to_string();
    let length = head
        .split("\r\n")
        .find_map(|line| {
            line.strip_prefix("Content-Length: ")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .unwrap_or(request.len());
    let mut body = request[header_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let ids: Vec<String> = parsed["questions"]["decision"]["criteria"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    let chosen = ids.last().cloned().unwrap_or_default();

    // (HTTP 状态, 响应体)；None 表示挂起不回应，制造 deadline 超时。
    let payload: Option<(u16, String)> = match mode {
        "hang" => None,
        // 协议不匹配（noul）：确定性失败，不应重试。
        "invalid" => Some((
            200,
            serde_json::json!({
                "answers": {"decision": {"type": "noul", "value": true}}
            })
            .to_string(),
        )),
        // 上游故障 5xx：瞬时故障，应重试。
        "server_error" => Some((
            503,
            serde_json::json!({"error": "upstream unavailable"}).to_string(),
        )),
        // 鉴权失败：确定性错误，不应重试。
        "unauthorized" => Some((
            401,
            serde_json::json!({"error": "invalid credential"}).to_string(),
        )),
        // 概率持平：协议合法但低置信度，由采用门槛拒绝（不重试）。
        "low_confidence" => Some((
            200,
            serde_json::json!({
                "answers": {"decision": {
                    "type": "choice",
                    "choice": chosen,
                    "probabilities": ids.iter()
                        .map(|id| (id.clone(), serde_json::json!(0.34)))
                        .collect::<serde_json::Map<_, _>>(),
                }}
            })
            .to_string(),
        )),
        _ => Some((
            200,
            serde_json::json!({
                "answers": {"decision": {
                    "type": "choice",
                    "choice": chosen,
                    "probabilities": ids.iter()
                        .map(|id| (
                            id.clone(),
                            serde_json::json!(if id == &chosen { 0.82 } else { 0.09 }),
                        ))
                        .collect::<serde_json::Map<_, _>>(),
                }},
                "usage": {"input_tokens": 12, "output_tokens": 2}
            })
            .to_string(),
        )),
    };
    let Some((status, body)) = payload else {
        // 挂起时长必须大于生产 deadline + 重试退避，否则不会触发超时；
        // 按统一常量推导，超时值再调整时这里不会悄悄变成"mock 先返回"。
        std::thread::sleep(Duration::from_millis(
            fluxdb_app::MODEL_REQUEST_TIMEOUT_MS + 5_000,
        ));
        return;
    };
    let body = body.into_bytes();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}
#[test]
fn no_credential_never_reaches_transport() {
    let fixture = Fixture::new();
    let (endpoint, hits) = spawn_mock("ok");
    fixture.decision_model("no-key", &endpoint, false);
    let advice = fixture.advice("c");
    assert!(
        matches!(advice, fluxdb_app::JevAdvice::NotAttempted(_)),
        "无凭据时不得发请求：{advice:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "无凭据时零传输");
}

#[test]
fn invalid_response_falls_back_to_base_strategy() {
    let fixture = Fixture::new();
    let (endpoint, _hits) = spawn_mock("invalid");
    fixture.decision_model("invalid", &endpoint, true);
    let advice = fixture.advice("c");
    assert!(
        advice.adopted_id().is_none(),
        "非法返回不得被采用：{advice:?}"
    );
}

#[test]
fn low_confidence_is_not_adopted() {
    let fixture = Fixture::new();
    let (endpoint, _hits) = spawn_mock("low_confidence");
    fixture.decision_model("low", &endpoint, true);
    let advice = fixture.advice("c");
    assert!(
        advice.adopted_id().is_none(),
        "低置信度必须放弃建议：{advice:?}"
    );
}

#[test]
fn timeout_falls_back_without_blocking_repeatedly() {
    let fixture = Fixture::new();
    // mock 只收请求不回应，触发受控 deadline（生产值 `MODEL_REQUEST_TIMEOUT_MS`）。
    let (endpoint, hits) = spawn_mock("hang");
    fixture.decision_model("slow", &endpoint, true);
    let started = std::time::Instant::now();
    let advice = fixture.advice("c");
    let elapsed = started.elapsed();
    assert!(
        advice.adopted_id().is_none(),
        "超时不得采用建议：{advice:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "超时按错误类别重试一次，共两次请求"
    );
    // 上限按生产常量推导：两次 deadline + 一次退避，再留 5s 余量给调度与抖动。
    // 不写死数字，超时常量调整时这条断言跟着走，而不是变成陈旧假设。
    let budget = Duration::from_millis(fluxdb_app::MODEL_REQUEST_TIMEOUT_MS * 2 + 5_000);
    assert!(
        elapsed < budget,
        "两次 deadline + 退避应受控返回，预算 {budget:?}，实际 {elapsed:?}"
    );
}

#[test]
fn valid_choice_is_adopted_when_model_is_available() {
    let fixture = Fixture::new();
    let (endpoint, _hits) = spawn_mock("ok");
    fixture.decision_model("good", &endpoint, true);
    let advice = fixture.advice("c");
    assert_eq!(
        advice.adopted_id(),
        Some("object:c"),
        "真实成功链路应采用建议：{advice:?}"
    );
}

#[test]
fn server_error_is_retried_but_unauthorized_is_not() {
    // 5xx 属瞬时故障：按错误类别应重试一次，共两次请求。
    let transient = Fixture::new();
    let (endpoint, hits) = spawn_mock("server_error");
    transient.decision_model("retry-5xx", &endpoint, true);
    let advice = transient.advice("c");
    assert!(advice.adopted_id().is_none(), "5xx 最终仍失败");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "5xx 应重试一次");

    // 401 是确定性失败：重试只会把已注定的等待再乘一遍。
    let permanent = Fixture::new();
    let (endpoint, hits) = spawn_mock("unauthorized");
    permanent.decision_model("no-retry-401", &endpoint, true);
    let advice = permanent.advice("c");
    assert!(advice.adopted_id().is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 1, "鉴权失败不应重试");
}

#[test]
fn invalid_protocol_response_is_not_retried() {
    let fixture = Fixture::new();
    let (endpoint, hits) = spawn_mock("invalid");
    fixture.decision_model("no-retry-malformed", &endpoint, true);
    let advice = fixture.advice("c");
    assert!(advice.adopted_id().is_none());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "协议不合法是确定性失败，不应重试"
    );
}

#[test]
fn repeated_failures_open_circuit_and_stop_further_requests() {
    let fixture = Fixture::new();
    let (endpoint, hits) = spawn_mock("invalid");
    fixture.decision_model("flaky", &endpoint, true);
    for _ in 0..3 {
        let advice = fixture.advice("c");
        assert!(advice.adopted_id().is_none());
    }
    assert!(hits.load(Ordering::SeqCst) >= 3, "前三次应真实发出请求");
    let after = hits.load(Ordering::SeqCst);
    // 熔断 60s 内不再发请求，也不再重复报错。
    for _ in 0..3 {
        let advice = fixture.advice("c");
        assert!(
            matches!(advice, fluxdb_app::JevAdvice::NotAttempted(_)),
            "熔断期内应跳过而不是重试：{advice:?}"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after,
        "熔断期间不得再触达传输层"
    );
}
