// 使用本地 HTTP 服务验证真实请求路径、鉴权和响应，不消耗远端额度。
fn ai_mock_server(
    responses: Vec<(u16, &'static str)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    other => panic!("未收到测试请求: {other:?}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => panic!("测试连接提前关闭"),
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "读取测试请求超时");
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => panic!("读取测试请求失败: {e}"),
                }
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]);
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (base, handle)
}

#[test]
fn ai_model_network_routes_and_endpoint_validation() {
    assert_eq!(
        ai_request_url("https://example.com/api/v1/", "openai-compatible", false).unwrap(),
        "https://example.com/api/v1/chat/completions"
    );
    assert_eq!(
        ai_request_url("https://example.com/api/v4", "anthropic-messages", false).unwrap(),
        "https://example.com/api/v4/messages"
    );
    assert_eq!(
        ai_request_url("https://example.com", "openai-compatible", false).unwrap(),
        "https://example.com/v1/chat/completions"
    );
    assert_eq!(
        ai_request_url("https://open.bigmodel.cn/api/v1", "openai-responses", false).unwrap(),
        "https://open.bigmodel.cn/api/v1/responses"
    );
    assert_eq!(
        ai_request_url("https://example.com/custom", "openai-responses", true).unwrap(),
        "https://example.com/custom"
    );
    assert_eq!(
        ai_request_url("https://example.com/custom", "system-one", true).unwrap(),
        "https://example.com/custom"
    );
    assert_eq!(
        ai_request_url("https://example.com/api", "system-one", false).unwrap(),
        "https://example.com/api/systemone"
    );
    let candidates = ai_models_url_candidates("https://example.com/api/v1").unwrap();
    assert_eq!(candidates[0], "https://example.com/api/v1/models");
    for invalid in [
        "file:///tmp/a",
        "https://user:secret@example.com",
        "https://example.com?key=secret",
        "https://example.com#fragment",
        "not a url",
    ] {
        assert!(ai_endpoint_url(invalid).is_err());
    }
}

#[test]
fn ai_model_network_fetch_fallback_and_slug() {
    let (base, handle) = ai_mock_server(vec![
        (404, "{}"),
        (
            200,
            r#"{"models":[{"slug":"z"},{"id":"a"},{"id":"a"},{"id":""}]}"#,
        ),
    ]);
    assert_eq!(
        fetch_ai_models("openai", Some(&base), "test-key").unwrap(),
        ["a", "z"]
    );
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/models "));
    assert!(requests[1].starts_with("GET /models "));
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer test-key")
    );
}

#[test]
fn ai_model_network_fetch_does_not_retry_auth_errors() {
    let (base, handle) = ai_mock_server(vec![(401, "{}")]);
    assert!(
        fetch_ai_models("openai", Some(&base), "test-key")
            .unwrap_err()
            .message
            .contains("401")
    );
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[test]
fn ai_model_network_full_url_round_trip_preserves_options_and_cas() {
    let (mut controller, dir) = controller_with_ai_storage("full-url");
    let mut input = sample_model_input(Some("test-key"));
    input.endpoint = Some("https://example.com/custom".into());
    input.endpoint_is_full_url = true;
    let AppEvent::AiModelSaved { id, revision } =
        controller.dispatch(AppCommand::SaveAiModel(input.clone()))
    else {
        panic!("保存失败")
    };
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let conn = storage.open_ai_connection().unwrap();
    conn.execute("UPDATE ai_model_profiles SET provider_options_json=json_set(provider_options_json, '$.other', 123) WHERE id=?1", [&id]).unwrap();
    controller.dispatch(AppCommand::RefreshAiSettings);
    assert!(controller.state().ai_models[0].endpoint_is_full_url);
    input.id = Some(id.clone());
    input.expected_revision = Some(revision);
    input.api_key = None;
    input.endpoint_is_full_url = false;
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiModel(input.clone())),
        AppEvent::AiModelSaved { .. }
    ));
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiModel(input)),
        AppEvent::AiSettingsError(_)
    ));
    let options: String = conn
        .query_row(
            "SELECT provider_options_json FROM ai_model_profiles WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    let options: serde_json::Value = serde_json::from_str(&options).unwrap();
    assert_eq!(options["other"], 123);
    assert_eq!(options["endpoint_is_full_url"], false);
    assert!(matches!(
        controller.dispatch(AppCommand::FetchAiModels {
            provider: "openai".into(),
            endpoint: Some("https://example.invalid/custom".into()),
            endpoint_is_full_url: true,
            api_key: "test-key".into()
        }),
        AppEvent::AiModelsFetchFailed { .. }
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ai_model_network_real_protocol_requests_and_response_validation() {
    for (provider, full, route, response, expected) in [
        (
            "openai",
            false,
            "/v1/chat/completions",
            r#"{"choices":[{"message":{"content":"你好"}}]}"#,
            "ok",
        ),
        (
            "openai",
            true,
            "/custom",
            r#"{"choices":[{"message":{"content":"你好"}}]}"#,
            "ok",
        ),
        (
            "openai-responses",
            false,
            "/v1/responses",
            r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"你好"}]}]}"#,
            "ok",
        ),
        (
            "openai-responses",
            true,
            "/custom",
            r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"你好"}]}]}"#,
            "ok",
        ),
        (
            "anthropic",
            false,
            "/v1/messages",
            r#"{"type":"message","content":[{"type":"text","text":"你好"}]}"#,
            "ok",
        ),
        (
            "decisions",
            false,
            "/v1/systemone",
            r#"{"answers":{"connection":{"type":"choice","choice":"success","probabilities":{"success":0.9,"failure":0.1},"confidence":0.9}}}"#,
            "ok",
        ),
        (
            "decisions",
            true,
            "/custom",
            r#"{"answers":{"connection":{"type":"choice","choice":"unknown","probabilities":{},"confidence":0.9}}}"#,
            "failed",
        ),
        ("openai", false, "/v1/chat/completions", "{}", "failed"),
        (
            "openai-responses",
            false,
            "/v1/responses",
            r#"{"status":"completed","output":[]}"#,
            "failed",
        ),
    ] {
        let (base, handle) = ai_mock_server(vec![(200, response)]);
        let (mut controller, dir) = controller_with_ai_storage("real-protocol");
        let mut input = sample_model_input(Some("test-key"));
        input.provider = provider.into();
        input.endpoint = Some(format!("{base}{}", if full { "/custom" } else { "/v1" }));
        input.endpoint_is_full_url = full;
        let AppEvent::AiModelSaved { id, revision } =
            controller.dispatch(AppCommand::SaveAiModel(input))
        else {
            panic!("创建失败")
        };
        let AppEvent::AiModelTested { status, .. } =
            controller.dispatch(AppCommand::TestAiModel { id: id.clone() })
        else {
            panic!("测试失败")
        };
        assert!(status.starts_with(expected), "{provider}: {status}");
        let requests = handle.join().unwrap();
        assert!(requests[0].starts_with(&format!("POST {route} ")));
        let body: serde_json::Value =
            serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "main-model");
        if provider == "decisions" {
            assert_eq!(body["questions"]["connection"]["type"], "choice");
            assert!(body.get("messages").is_none());
        } else if provider == "openai-responses" {
            assert_eq!(body["input"], "只回复 ok");
            assert_eq!(body["max_output_tokens"], 128);
            assert!(body.get("messages").is_none());
        }
        controller.dispatch(AppCommand::RefreshAiSettings);
        let item = &controller.state().ai_models[0];
        assert_eq!(item.connection_status, expected);
        assert_eq!(item.revision, revision);
        assert!(item.connection_checked_at_ms.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
