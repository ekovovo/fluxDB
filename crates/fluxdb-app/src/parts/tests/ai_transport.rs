// 流式传输/SseParser 边界测试（设计 §6/§17，T003）。
//
// 本文件只使用内存 Reader 和本地 mock server；不访问外部网络、不使用真实凭据。

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fluxdb_core::{
    CancelFlag as TransportCancelFlag, FinishReason as TransportFinishReason,
    ModelEvent as TransportModelEvent, ToolCallRequest as TransportToolCallRequest,
};

fn transport_limits(max_frame_bytes: usize, max_total_bytes: usize) -> TransportLimits {
    TransportLimits {
        max_frame_bytes,
        max_total_bytes,
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_secs(2),
        response_deadline: Duration::from_secs(2),
    }
}

fn transport_sse(frames: impl IntoIterator<Item = serde_json::Value>) -> String {
    frames
        .into_iter()
        .map(|payload| format!("data: {payload}\n\n"))
        .collect()
}

#[test]
fn transport_sse_parser_handles_frames_and_drops_residual() {
    let mut parser = SseParser::new(256);
    let body = b": comment\r\nevent: message\r\nid: 7\r\ndata: first\r\n\r\ndata: second\n\n";
    let first_frame_end = body
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("第一帧 CRLF 结束符")
        + 4;
    let mut payloads = Vec::new();

    for (index, byte) in body.iter().enumerate() {
        let parsed = parser.push(std::slice::from_ref(byte)).expect("逐字节解析");
        if index + 1 < first_frame_end {
            assert!(parsed.is_empty(), "帧结束前不得提前产出 data 载荷");
        }
        payloads.extend(parsed);
    }

    assert_eq!(payloads, vec!["first".to_string(), "second".to_string()]);

    let mut unfinished = SseParser::new(256);
    assert!(
        unfinished
            .push(b"data: dangling\n")
            .expect("不完整帧可暂存")
            .is_empty()
    );
    unfinished.finish();
}

#[test]
fn transport_single_frame_limit_reports_limit() {
    let mut parser = SseParser::new(8);
    let error = parser
        .push(b"data: 123456789")
        .expect_err("未结束残余帧必须报错");
    assert_eq!(error, TransportError::FrameTooLarge { limit: 8 });
    assert!(error.to_string().contains('8'));
}

#[test]
fn transport_total_body_limit_reports_limit() {
    let input = b"data: a\n\ndata: b\n\n";
    let mut reader = Cursor::new(input);
    let error = stream_sse_reader(
        &mut reader,
        transport_limits(64, 12),
        ChatProtocol::OpenAiCompatible,
        &TransportCancelFlag::new(),
    )
    .expect_err("整包超过上限必须报错");
    assert_eq!(error, TransportError::BodyTooLarge { limit: 12 });
}

#[test]
fn transport_openai_fragmented_events_accumulate_tool_arguments() {
    let stream = transport_sse([
        serde_json::json!({"choices":[{"delta":{"content":"你"},"finish_reason":null}]}),
        serde_json::json!({"choices":[{"delta":{"content":"好"},"finish_reason":null}]}),
        serde_json::json!({
            "choices":[{"delta":{"tool_calls":[{
                "index":0,
                "id":"call-1",
                "function":{"name":"db.query","arguments":"{\"sql\":"}
            }]},"finish_reason":null}]
        }),
        serde_json::json!({
            "choices":[{"delta":{"tool_calls":[{
                "index":0,
                "function":{"arguments":"\"select 1\"}"}
            }]},"finish_reason":null}]
        }),
        serde_json::json!({
            "usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":2}},
            "choices":[]
        }),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ]);

    let mut reader = Cursor::new(stream.as_bytes());
    let events = stream_sse_reader(
        &mut reader,
        transport_limits(4096, 64 * 1024),
        ChatProtocol::OpenAiCompatible,
        &TransportCancelFlag::new(),
    )
    .expect("OpenAI SSE 应可解析");

    assert_eq!(
        events,
        vec![
            TransportModelEvent::TextDelta("你".into()),
            TransportModelEvent::TextDelta("好".into()),
            TransportModelEvent::Usage(fluxdb_core::Usage {
                input_tokens: 10,
                output_tokens: 5,
                cached_input_tokens: 2,
            }),
            TransportModelEvent::ToolCallRequested(TransportToolCallRequest {
                provider_call_id: "call-1".into(),
                tool_id: "db.query".into(),
                arguments_json: r#"{"sql":"select 1"}"#.into(),
            }),
            TransportModelEvent::Finished(TransportFinishReason::ToolCalls),
        ]
    );
}

#[test]
fn transport_openai_finish_reason_maps_length_and_stop() {
    for (reason, expected) in [
        ("length", TransportFinishReason::Length),
        ("stop", TransportFinishReason::Stop),
    ] {
        let stream = transport_sse([serde_json::json!({
            "choices":[{"delta":{},"finish_reason":reason}]
        })]);
        let mut reader = Cursor::new(stream.as_bytes());
        let events = stream_sse_reader(
            &mut reader,
            transport_limits(1024, 4096),
            ChatProtocol::OpenAiCompatible,
            &TransportCancelFlag::new(),
        )
        .expect("finish_reason 应可解析");
        assert_eq!(events, vec![TransportModelEvent::Finished(expected)]);
    }
}

#[test]
fn transport_anthropic_fragmented_events_accumulate_tool_arguments() {
    let stream = transport_sse([
        serde_json::json!({
            "type":"content_block_start",
            "index":0,
            "content_block":{"type":"tool_use","id":"toolu-1","name":"db.query"}
        }),
        serde_json::json!({
            "type":"content_block_delta",
            "index":0,
            "delta":{"type":"input_json_delta","partial_json":"{\"sql\":"}
        }),
        serde_json::json!({
            "type":"content_block_delta",
            "index":0,
            "delta":{"type":"input_json_delta","partial_json":"\"select 1\"}"}
        }),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({
            "type":"content_block_delta",
            "index":1,
            "delta":{"type":"text_delta","text":"ok"}
        }),
        serde_json::json!({
            "type":"message_delta",
            "usage":{"input_tokens":10,"output_tokens":4,"cache_read_input_tokens":1}
        }),
        serde_json::json!({"type":"message_stop"}),
    ]);

    let mut reader = Cursor::new(stream.as_bytes());
    let events = stream_sse_reader(
        &mut reader,
        transport_limits(4096, 64 * 1024),
        ChatProtocol::AnthropicMessages,
        &TransportCancelFlag::new(),
    )
    .expect("Anthropic SSE 应可解析");

    assert_eq!(
        events,
        vec![
            TransportModelEvent::ToolCallRequested(TransportToolCallRequest {
                provider_call_id: "toolu-1".into(),
                tool_id: "db.query".into(),
                arguments_json: r#"{"sql":"select 1"}"#.into(),
            }),
            TransportModelEvent::TextDelta("ok".into()),
            TransportModelEvent::Usage(fluxdb_core::Usage {
                input_tokens: 11,
                output_tokens: 4,
                cached_input_tokens: 1,
            }),
            TransportModelEvent::Finished(TransportFinishReason::Stop),
        ]
    );
}

#[test]
fn transport_cancel_before_read_returns_only_cancelled() {
    let stream = transport_sse([serde_json::json!({
        "choices":[{"delta":{"content":"不应读取"},"finish_reason":null}]
    })]);
    let mut reader = Cursor::new(stream.as_bytes());
    let cancel = TransportCancelFlag::new();
    cancel.request();
    let events = stream_sse_reader(
        &mut reader,
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &cancel,
    )
    .expect("取消不是传输错误");
    assert_eq!(
        events,
        vec![TransportModelEvent::Finished(
            TransportFinishReason::Cancelled
        )]
    );
}

struct TransportCancelAfterFirstRead {
    inner: Cursor<Vec<u8>>,
    cancel: TransportCancelFlag,
    reads: Arc<AtomicUsize>,
}

impl Read for TransportCancelAfterFirstRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        if read > 0 {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.cancel.request();
        }
        Ok(read)
    }
}

#[test]
fn transport_cancel_after_first_read_stops_after_one_read() {
    let stream = transport_sse([serde_json::json!({
        "choices":[{"delta":{"content":"已解析内容"},"finish_reason":null}]
    })]);
    let reads = Arc::new(AtomicUsize::new(0));
    let cancel = TransportCancelFlag::new();
    let mut reader = TransportCancelAfterFirstRead {
        inner: Cursor::new(stream.into_bytes()),
        cancel: cancel.clone(),
        reads: reads.clone(),
    };

    let events = stream_sse_reader(
        &mut reader,
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &cancel,
    )
    .expect("取消不是传输错误");

    assert_eq!(reads.load(Ordering::SeqCst), 1);
    // 取消必须保留本次调用已经解析出的增量（UI 要显示已流出的内容），只在末尾补 Cancelled。
    assert_eq!(
        events,
        vec![
            TransportModelEvent::TextDelta("已解析内容".into()),
            TransportModelEvent::Finished(TransportFinishReason::Cancelled),
        ]
    );
}

#[test]
fn transport_done_stops_before_later_frames() {
    let stream = format!(
        "{}data: [DONE]\n\n{}",
        transport_sse([serde_json::json!({
            "choices":[{"delta":{"content":"before"},"finish_reason":null}]
        })]),
        transport_sse([serde_json::json!({
            "choices":[{"delta":{"content":"after"},"finish_reason":null}]
        })])
    );
    let mut reader = Cursor::new(stream.as_bytes());
    let events = stream_sse_reader(
        &mut reader,
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &TransportCancelFlag::new(),
    )
    .expect("解析 [DONE]");
    assert_eq!(
        events,
        vec![TransportModelEvent::TextDelta("before".into())]
    );
}

#[test]
fn transport_malformed_json_error_does_not_echo_payload() {
    let raw_payload = r#"{"broken": "#;
    let stream = format!("data: {raw_payload}\n\n");
    let mut reader = Cursor::new(stream.as_bytes());
    let error = stream_sse_reader(
        &mut reader,
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &TransportCancelFlag::new(),
    )
    .expect_err("非法 JSON 必须报 Malformed");
    assert!(matches!(error, TransportError::Malformed(_)));
    assert!(!error.to_string().contains(raw_payload));
}

#[test]
fn transport_protocol_structural_mismatch_is_malformed() {
    let stream = transport_sse([serde_json::json!({"choices":"not-an-array"})]);
    let mut reader = Cursor::new(stream.as_bytes());
    let error = stream_sse_reader(
        &mut reader,
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &TransportCancelFlag::new(),
    )
    .expect_err("不符合 OpenAI SSE 结构应报 Malformed");
    assert!(matches!(error, TransportError::Malformed(_)));
}

#[test]
fn transport_auth_debug_redacts_values() {
    let bearer = TransportAuth::Bearer("sk-e2e-secret".into());
    let header = TransportAuth::Header {
        name: "x-api-key",
        value: "sk-header-secret".into(),
    };
    assert!(!format!("{bearer:?}").contains("sk-e2e-secret"));
    assert!(!format!("{header:?}").contains("sk-header-secret"));
}

// 本地 mock server 不调用外部网络；若沙箱禁止绑定端口，本用例会明确失败并记录为受阻。
fn transport_mock_server(body: &'static str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("设置非阻塞");
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("未收到 mock 请求: {other:?}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
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
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("写 mock 响应");
        request
    });
    (url, handle)
}

#[test]
fn transport_stream_chat_local_mock_server_round_trip() {
    let body = transport_sse([
        serde_json::json!({
            "choices":[{"delta":{"content":"mock"},"finish_reason":null}]
        }),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ]);
    let body: &'static str = Box::leak(body.into_boxed_str());
    let (url, handle) = transport_mock_server(body);

    let events = stream_chat(
        transport_limits(1024, 4096),
        ChatProtocol::OpenAiCompatible,
        &url,
        &TransportAuth::None,
        "{}",
        &[],
        &TransportCancelFlag::new(),
    )
    .expect("本地 mock stream_chat 应成功");

    assert_eq!(
        events,
        vec![
            TransportModelEvent::TextDelta("mock".into()),
            TransportModelEvent::Finished(TransportFinishReason::Stop),
        ]
    );
    let request = handle.join().expect("mock server 线程");
    let request = String::from_utf8_lossy(&request);
    assert!(request.starts_with("POST /v1/chat/completions "));
}

// ---- OpenAI Responses（openai-responses）SSE 投影 ----

#[test]
fn ai_transport_responses_projects_text_tool_and_completion() {
    let mut state = StreamState::default();
    let payloads = [
        r#"{"type":"response.created","response":{"id":"r1"}}"#,
        r#"{"type":"response.output_text.delta","delta":"就"}"#,
        r#"{"type":"response.output_text.delta","delta":"绪"}"#,
        r#"{"type":"response.output_item.added","item":{"id":"fc1","type":"function_call","call_id":"call_1","name":"db_query","arguments":""}}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc1","delta":"{\"sql\":\"SELECT 1\"}"}"#,
        r#"{"type":"response.output_item.done","item":{"id":"fc1","type":"function_call","call_id":"call_1","name":"db_query","arguments":"{\"sql\":\"SELECT 1\"}"}}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":18,"output_tokens":68,"input_tokens_details":{"cached_tokens":4}}}}"#,
    ];
    let mut events = Vec::new();
    for payload in payloads {
        events.extend(
            parse_sse_payload(ChatProtocol::OpenAiResponses, payload, &mut state).expect("解析"),
        );
    }
    assert!(matches!(&events[0], fluxdb_core::ModelEvent::TextDelta(text) if text == "就"));
    assert!(matches!(&events[1], fluxdb_core::ModelEvent::TextDelta(text) if text == "绪"));
    let call = events
        .iter()
        .find_map(|event| match event {
            fluxdb_core::ModelEvent::ToolCallRequested(call) => Some(call.clone()),
            _ => None,
        })
        .expect("工具调用");
    assert_eq!(call.provider_call_id, "call_1");
    assert_eq!(call.tool_id, "db_query");
    assert_eq!(call.arguments_json, r#"{"sql":"SELECT 1"}"#);
    let usage = events
        .iter()
        .find_map(|event| match event {
            fluxdb_core::ModelEvent::Usage(usage) => Some(*usage),
            _ => None,
        })
        .expect("usage");
    assert_eq!(usage.input_tokens, 18);
    assert_eq!(usage.output_tokens, 68);
    assert_eq!(usage.cached_input_tokens, 4);
    assert_eq!(
        events.last(),
        Some(&fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop))
    );
}

#[test]
fn ai_transport_responses_maps_incomplete_and_failed_without_leaking_reasoning() {
    // 私有推理增量（reasoning_text.*）必须被忽略：不能进事件、不能落库。
    let mut state = StreamState::default();
    let reasoning = parse_sse_payload(
        ChatProtocol::OpenAiResponses,
        r#"{"type":"response.reasoning_text.delta","delta":"private chain of thought"}"#,
        &mut state,
    )
    .expect("解析");
    assert!(reasoning.is_empty(), "私有推理不得投影成事件");

    // 公开摘要增量仍然映射。
    let summary = parse_sse_payload(
        ChatProtocol::OpenAiResponses,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"先看表结构"}"#,
        &mut state,
    )
    .expect("解析");
    assert!(matches!(
        summary.as_slice(),
        [fluxdb_core::ModelEvent::ReasoningSummaryDelta(text)] if text == "先看表结构"
    ));

    // 长度截断 / 失败 / 连接中途断开都要有明确终态，不能静默"空回复"。
    for (payload, expected) in [
        (
            r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
            fluxdb_core::FinishReason::Length,
        ),
        (r#"{"type":"response.failed"}"#, fluxdb_core::FinishReason::Error),
        (r#"{"type":"error","message":"boom"}"#, fluxdb_core::FinishReason::Error),
    ] {
        let mut state = StreamState::default();
        let events = parse_sse_payload(ChatProtocol::OpenAiResponses, payload, &mut state)
            .expect("解析");
        assert_eq!(
            events,
            vec![fluxdb_core::ModelEvent::Finished(expected)],
            "payload={payload}"
        );
    }
}

#[test]
fn transport_anthropic_combines_start_input_cache_and_final_output_once() {
    let mut state = StreamState::default();
    let mut events = Vec::new();
    for payload in [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}}}"#,
        r#"{"type":"message_delta","usage":{"output_tokens":2}}"#,
        r#"{"type":"message_delta","usage":{"output_tokens":4}}"#,
        r#"{"type":"message_stop"}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        events.extend(parse_sse_payload(ChatProtocol::AnthropicMessages, payload, &mut state).unwrap());
    }
    let usage: Vec<_> = events.into_iter().filter_map(|event| {
        if let TransportModelEvent::Usage(usage) = event {Some(usage)} else {None}
    }).collect();
    assert_eq!(usage, vec![fluxdb_core::Usage { input_tokens:15,output_tokens:4,cached_input_tokens:2 }]);
}

#[test]
fn transport_missing_token_fields_never_emit_fake_zero_usage() {
    for (protocol, payloads) in [
        (ChatProtocol::OpenAiCompatible, vec![r#"{"usage":{"prompt_tokens":10},"choices":[]}"#]),
        (ChatProtocol::OpenAiResponses, vec![r#"{"type":"response.completed","response":{"usage":{"input_tokens":10}}}"#]),
        (ChatProtocol::AnthropicMessages, vec![r#"{"type":"message_delta","usage":{"output_tokens":4}}"#,r#"{"type":"message_stop"}"#]),
        (ChatProtocol::AnthropicMessages, vec![r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}"#,r#"{"type":"message_stop"}"#]),
    ] {
        let mut state = StreamState::default();
        for payload in payloads {
            let events = parse_sse_payload(protocol, payload, &mut state).unwrap();
            assert!(!events.iter().any(|event| matches!(event, TransportModelEvent::Usage(_))));
        }
    }
}

#[test]
fn transport_invalid_or_overflowing_token_counts_are_rejected() {
    for (protocol, payloads) in [
        (ChatProtocol::OpenAiCompatible, vec![r#"{"usage":{"prompt_tokens":-1,"completion_tokens":4},"choices":[]}"#]),
        (ChatProtocol::OpenAiResponses, vec![r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"input_tokens_details":{"cached_tokens":2}}}}"#]),
        (ChatProtocol::AnthropicMessages, vec![r#"{"type":"message_start","message":{"usage":{"input_tokens":9223372036854775807,"cache_creation_input_tokens":1}}}"#,r#"{"type":"message_delta","usage":{"output_tokens":4}}"#,r#"{"type":"message_stop"}"#]),
    ] {
        let mut state = StreamState::default();
        let mut failed = false;
        for payload in payloads {
            if parse_sse_payload(protocol, payload, &mut state).is_err() {failed=true;break;}
        }
        assert!(failed);
    }
}

#[test]
fn transport_defaults_share_one_model_request_timeout() {
    let limits = TransportLimits::default();
    // 连接、等响应头（首响应）、正文读取共用同一个超时常量，避免各处各写一份。
    let unified = std::time::Duration::from_millis(MODEL_REQUEST_TIMEOUT_MS);
    assert_eq!(
        limits.response_deadline,
        unified,
        "等模型开口的预算应取统一的 MODEL_REQUEST_TIMEOUT_MS"
    );
    assert_eq!(limits.connect_timeout, unified, "连接预算应取同一常量");
    assert_eq!(limits.read_timeout, unified, "正文读取预算应取同一常量");
}

/// 分片慢速但持续产出的回答不能被"整包总预算"误杀：这里让每个 SSE 帧之间
/// 间隔超过单次读预算的一半，总时长明显超过读预算，但如果按空闲间隔判定就应成功。
///
/// 该用例只在允许绑定回环端口时执行（沙箱内会按真实失败报告为受阻）。
#[test]
fn transport_idle_body_timeout_keeps_slow_but_progressing_stream() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("设置非阻塞");
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("未收到 mock 请求: {other:?}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
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
        // 分块传输：每帧之间停顿，让整包总时长超过单次读预算。
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .expect("写 mock 响应头");
        for frame in [
            r#"{"choices":[{"delta":{"content":"慢"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"但"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"在"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ] {
            std::thread::sleep(Duration::from_millis(250));
            let payload = format!("data: {frame}\n\n");
            write!(stream, "{:x}\r\n{}\r\n", payload.len(), payload).expect("写分块");
        }
        write!(stream, "0\r\n\r\n").expect("写结束分块");
    });

    // 单次读预算 400ms，整包总时长约 1s+：空闲判定应通过，总预算判定会失败。
    let limits = TransportLimits {
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_millis(400),
        response_deadline: Duration::from_secs(2),
        ..transport_limits(1024, 4096)
    };
    let events = stream_chat(
        limits,
        ChatProtocol::OpenAiCompatible,
        &url,
        &TransportAuth::None,
        "{}",
        &[],
        &TransportCancelFlag::new(),
    )
    .expect("持续产出但间隔小于空闲预算的流不应被超时截断");

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            TransportModelEvent::TextDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "慢但在", "分片内容应完整保留");
    handle.join().expect("mock server 线程");
}

/// 反向用例：开始吐字后长时间不再有数据，必须按空闲预算超时，
/// 证明空闲超时没有被实现成"无限等待"。
#[test]
fn transport_idle_body_timeout_still_trips_on_stalled_stream() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("设置非阻塞");
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => panic!("未收到 mock 请求: {other:?}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
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
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .expect("写 mock 响应头");
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"开始\"},\"finish_reason\":null}]}\n\n";
        write!(stream, "{:x}\r\n{}\r\n", payload.len(), payload).expect("写首帧");
        stream.flush().expect("刷新首帧");
        // 之后长时间不再发送任何字节：应触发空闲超时。
        std::thread::sleep(Duration::from_secs(2));
    });

    let limits = TransportLimits {
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_millis(300),
        response_deadline: Duration::from_secs(2),
        ..transport_limits(1024, 4096)
    };
    let error = stream_chat(
        limits,
        ChatProtocol::OpenAiCompatible,
        &url,
        &TransportAuth::None,
        "{}",
        &[],
        &TransportCancelFlag::new(),
    )
    .expect_err("长时间无数据的流必须超时");
    assert!(
        matches!(error, TransportError::Timeout(_)),
        "空闲超时应映射为 Timeout，实际：{error:?}"
    );
    handle.join().expect("mock server 线程");
}
