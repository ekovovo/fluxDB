/// 测试模型连接：按协议发送最小请求并校验返回结构，结果落库但不改配置 revision。
///
/// 返回 `(id, status)`，status ∈ `ok` / `missing_key` / `not_required` /
/// `unsupported` / `failed:<一句话中文原因>`；任何分支都不会把 API Key 写入结果。
/// 2xx 且返回符合协议 → 落库 `ok`；协议、HTTP 或网络错误 → 落库 `failed`；
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
    if !matches!(
        protocol,
        "openai-compatible" | "openai-responses" | "anthropic-messages" | "system-one"
    ) {
        // 未知协议不可用聊天请求冒充。
        return Ok((id.to_string(), "unsupported".to_string()));
    }
    if view.credential_status == CredentialStatus::NotRequired && protocol != "system-one" {
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
    if protocol == "system-one" && view.endpoint.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Ok((id.to_string(), "failed:请配置决策模型请求地址".into()));
    }
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

    let url = ai_request_url(&base, protocol, view.endpoint_is_full_url)?;

    // 测试数据不包含用户上下文；决策协议使用真实 Choice 请求。
    let agent = ai_agent(std::time::Duration::from_secs(10));

    // ureq 未启用 json feature，这里自行序列化并显式带 Content-Type。
    let body = if protocol == "system-one" {
        serde_json::json!({
            "model": view.model,
            "state": {"text": "The connection test succeeded."},
            "questions": {"connection": {
                "type": "choice",
                "instructions": "Classify the connection test result",
                "criteria": {"success": "Succeeded", "failure": "Failed"}
            }}
        })
    } else if protocol == "openai-responses" {
        serde_json::json!({
            "model": view.model,
            "input": "只回复 ok",
            "max_output_tokens": 128,
            "stream": false,
        })
    } else if protocol == "anthropic-messages" {
        serde_json::json!({
            "model": view.model,
            "max_tokens": 8,
            "messages": [{ "role": "user", "content": "你好" }],
        })
    } else {
        serde_json::json!({
            "model": view.model,
            "messages": [{ "role": "user", "content": "你好" }],
            "max_tokens": 8,
            "stream": false,
        })
    };
    let body = serde_json::to_string(&body).map_err(|e| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("测试请求序列化失败: {e}"),
        )
    })?;
    let request = agent.post(&url).header("Content-Type", "application/json");
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
        Ok(mut response) => {
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                let valid = response
                    .body_mut()
                    .with_config()
                    .limit(1024 * 1024)
                    .read_to_string()
                    .ok()
                    .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                    .is_some_and(|body| ai_test_response_valid(protocol, &body));
                if !valid {
                    write_connection_status(storage, id, "failed")?;
                    return Ok((id.to_string(), "failed:响应不符合模型协议".into()));
                }
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
    fetch_ai_model_capabilities(provider, endpoint, api_key)
        .map(|items| items.into_iter().map(|m| m.id).collect())
}

pub fn fetch_ai_model_capabilities(
    provider: &str,
    endpoint: Option<&str>,
    api_key: &str,
) -> fluxdb_core::Result<Vec<AiDiscoveredModel>> {
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
    let candidates = ai_models_url_candidates(base)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    for (index, url) in candidates.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "获取模型能力超时",
            ));
        }
        let agent = ai_agent(remaining);
        let request = agent.get(url.as_str());
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
        if matches!(status, 404 | 405) && index + 1 < candidates.len() {
            tracing::debug!(
                status,
                candidate = index,
                "模型列表路由不支持，尝试下一候选"
            );
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("拉取模型列表失败：HTTP {status}"),
            ));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .map_err(|_| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "读取响应失败")
            })?;
        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "响应不是合法 JSON")
        })?;
        let models = parse_ai_model_capabilities(&parsed)?;
        return Ok(models);
    }
    Err(fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        "未找到可用模型列表路由",
    ))
}

/// 系统 DNS 可能返回不可用的 IPv6 地址，而 IPv4 正常。此解析器保留全部地址，
/// 但把 IPv4 排到前面，连接仍会自动回退到 IPv6，不牺牲 IPv6-only 网络。
#[derive(Debug)]
struct PrefersIpv4Resolver;

impl ureq::unversioned::resolver::Resolver for PrefersIpv4Resolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let resolved = ureq::unversioned::resolver::DefaultResolver::default()
            .resolve(uri, config, timeout)?;
        let mut ordered = ureq::unversioned::resolver::DefaultResolver::default().empty();
        for address in resolved.iter().filter(|address| address.is_ipv4()) {
            ordered.push(*address);
        }
        for address in resolved.iter().filter(|address| address.is_ipv6()) {
            ordered.push(*address);
        }
        Ok(ordered)
    }
}

fn ai_agent(timeout: std::time::Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .build();
    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        PrefersIpv4Resolver,
    )
}

/// 把 ureq 错误映射为不含敏感信息的中文原因。
fn ai_net_error_reason(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => match code {
            401 | 403 => "鉴权失败".into(),
            404 => "接口不存在".into(),
            _ => "服务返回错误".into(),
        },
        ureq::Error::Timeout(_) => "连接超时".into(),
        ureq::Error::HostNotFound => "域名解析失败".into(),
        ureq::Error::ConnectionFailed => "无法连接服务器".into(),
        // 只暴露稳定的错误类别，不透出底层文本、请求地址或凭据。
        ureq::Error::Io(error) => format!("网络读写失败（{:?}）", error.kind()),
        ureq::Error::BadUri(_) => "请求地址无效".into(),
        _ => "网络请求失败".into(),
    }
}

fn ai_endpoint_url(endpoint: &str) -> fluxdb_core::Result<url::Url> {
    let url = url::Url::parse(endpoint.trim()).map_err(|_| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "请求地址必须是合法 HTTP/HTTPS URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "请求地址不能包含凭据、查询串或片段，且必须使用 HTTP/HTTPS",
        ));
    }
    Ok(url)
}

fn ai_version_segment(path: &str) -> bool {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .strip_prefix('v')
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

fn ai_request_url(endpoint: &str, protocol: &str, full: bool) -> fluxdb_core::Result<String> {
    let mut url = ai_endpoint_url(endpoint)?;
    if full {
        return Ok(url.into());
    }
    let path = url.path().trim_end_matches('/');
    let route = match protocol {
        "system-one" => "systemone",
        "anthropic-messages" => "messages",
        "openai-responses" => "responses",
        _ => "chat/completions",
    };
    // 已含版本的 Base URL 不重复加 /v1；System One 不走聊天协议。
    let prefix = if protocol == "system-one" || ai_version_segment(path) {
        ""
    } else {
        "/v1"
    };
    url.set_path(&format!("{path}{prefix}/{route}"));
    Ok(url.into())
}

fn ai_models_url_candidates(endpoint: &str) -> fluxdb_core::Result<Vec<String>> {
    let url = ai_endpoint_url(endpoint)?;
    let path = url.path().trim_end_matches('/');
    let mut paths = if let Some(root) = path.strip_suffix("/v1") {
        vec![format!("{path}/models"), format!("{root}/models")]
    } else if ai_version_segment(path) {
        vec![format!("{path}/models"), format!("{path}/v1/models")]
    } else {
        vec![format!("{path}/v1/models"), format!("{path}/models")]
    };
    // 只剥离已知的协议兼容后缀，不猜测任意路径或切换主机。
    for suffix in [
        "/api/claudecode",
        "/api/anthropic",
        "/apps/anthropic",
        "/claudecode",
        "/anthropic",
        "/claude",
    ] {
        if let Some(root) = path.strip_suffix(suffix) {
            paths.extend([format!("{root}/v1/models"), format!("{root}/models")]);
            break;
        }
    }
    let mut candidates = Vec::new();
    for path in paths {
        let mut candidate = url.clone();
        candidate.set_path(&path);
        let candidate: String = candidate.into();
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn ai_test_response_valid(protocol: &str, body: &serde_json::Value) -> bool {
    if body.get("error").is_some_and(|e| !e.is_null()) {
        return false;
    }
    match protocol {
        "system-one" => {
            let answer = &body["answers"]["connection"];
            let number = |v: &serde_json::Value| {
                v.as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
            };
            answer["type"] == "choice"
                && matches!(answer["choice"].as_str(), Some("success" | "failure"))
                && number(&answer["confidence"])
                && answer["probabilities"].as_object().is_some_and(|p| {
                    p.contains_key("success") && p.contains_key("failure") && p.values().all(number)
                })
        }
        "anthropic-messages" => {
            body["type"] == "message"
                && body["content"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "text"
                            && item["text"].as_str().is_some_and(|s| !s.is_empty())
                    })
                })
        }
        "openai-responses" => {
            let completed = body
                .get("status")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s == "completed");
            completed
                && body["output"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["content"].as_array().is_some_and(|parts| {
                            parts.iter().any(|part| {
                                part["type"] == "output_text"
                                    && part["text"].as_str().is_some_and(|s| !s.is_empty())
                            })
                        })
                    })
                })
        }
        _ => body["choices"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["message"]["content"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    || item["message"]["reasoning_content"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
            })
        }),
    }
}

/// 有界、严格读取 Provider 容量字段；未知容量为 None，不能当成已探测。
pub fn parse_ai_model_capabilities(
    body: &serde_json::Value,
) -> fluxdb_core::Result<Vec<AiDiscoveredModel>> {
    let items = body
        .get("data")
        .or_else(|| body.get("models"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "未返回模型列表")
        })?;
    if items.len() > 4096 {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "模型列表超过上限",
        ));
    }
    let positive =
        |value: &serde_json::Value| value.as_i64().filter(|n| (1..=1_000_000_000).contains(n));
    let mut models = Vec::new();
    for item in items {
        let Some(id) = item
            .get("id")
            .or_else(|| item.get("slug"))
            .and_then(|v| v.as_str())
            .filter(|id| {
                !id.trim().is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
            })
        else {
            continue;
        };
        let window = [
            &item["top_provider"]["context_length"],
            &item["context_window_tokens"],
            &item["max_input_tokens"],
            &item["context_length"],
            &item["context_window"],
        ]
        .into_iter()
        .find_map(positive);
        let output = [
            &item["top_provider"]["max_completion_tokens"],
            &item["max_output_tokens"],
            &item["max_tokens"],
            &item["max_completion_tokens"],
        ]
        .into_iter()
        .find_map(positive)
        .filter(|n| window.is_none_or(|w| *n <= w));
        models.push(AiDiscoveredModel {
            id: id.into(),
            context_window_tokens: window,
            max_output_tokens: output,
        });
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    // 同 ID 容量冲突时不猜，丢弃冲突容量并保持可手工输入。
    let mut unique: Vec<AiDiscoveredModel> = Vec::new();
    for model in models {
        if let Some(last) = unique.last_mut().filter(|last| last.id == model.id) {
            if last.context_window_tokens != model.context_window_tokens {
                last.context_window_tokens = None;
            }
            if last.max_output_tokens != model.max_output_tokens {
                last.max_output_tokens = None;
            }
        } else {
            unique.push(model);
        }
    }
    if unique.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "未返回任何模型 ID，请检查 Endpoint 与 API Key",
        ));
    }
    Ok(unique)
}
