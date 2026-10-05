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
                input_tokens: 10,
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
