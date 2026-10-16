//! 真实回环 SSE → Provider → 用量宿主 → 主循环 → 会话回桥。
//! 用通道闸门控制尾帧，证明首片在 HTTP 响应结束前可见，不依赖固定 sleep。
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::Duration,
};

use fluxdb_app::*;
use fluxdb_core::{CancelFlag, ModelCapabilities, ToolRisk};
use fluxdb_storage::FileStorage;

const WAIT: Duration = Duration::from_secs(8);

struct Fixture {
    storage: FileStorage,
    job: AiConversationJob,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("fluxdb-live-stream-{}", uuid::Uuid::new_v4()));
        let storage = FileStorage::new(root);
        let mut app = AppController::new();
        app.set_ai_storage(storage.clone());
        let AppEvent::AiModelSaved { id, .. } =
            app.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
                display_name: "stream fixture".into(),
                provider: "openai".into(),
                model: "offline".into(),
                enabled: true,
                ..Default::default()
            }))
        else {
            panic!("fixture model");
        };
        let revision = app.state().ai_runtime_revision;
        app.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(id),
            default_decision_model_id: None,
            expected_revision: revision,
        });
        let target = TargetRef {
            connection_id: None,
            database: None,
            schema: None,
            object: None,
            redis_db: None,
        };
        let job = app
            .prepare_ai_conversation(
                "stream test",
                &TurnTargetPin {
                    target_json: target.to_json(),
                    target,
                    captured_tab_id: None,
                    captured_tab_revision: 0,
                },
            )
            .unwrap();
        Self { storage, job }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.storage.root_dir());
    }
}

fn read_request(stream: &mut TcpStream) {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
            let len = String::from_utf8_lossy(&bytes[..end])
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + len {
                return;
            }
        }
    }
}

fn frames(protocol: ChatProtocol) -> (&'static str, &'static str) {
    match protocol {
        ChatProtocol::OpenAiCompatible => (
            "data: {\"choices\":[{\"delta\":{\"content\":\"alpha\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" beta\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n",
        ),
        ChatProtocol::AnthropicMessages => (
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"alpha\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" beta\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
        ChatProtocol::OpenAiResponses => (
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"alpha\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" beta\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4,\"output_tokens\":3}}}\n\n",
        ),
    }
}

fn run_http_stream(protocol: ChatProtocol, incomplete: bool, cancel_midstream: bool) {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (release, gate) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        // 不接受标题或重试请求；它们会立即连接失败，而不是让测试等待。
        drop(listener);
        read_request(&mut socket);
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let (first, tail) = frames(protocol);
        socket.write_all(first.as_bytes()).unwrap();
        socket.flush().unwrap();
        gate.recv_timeout(WAIT).expect("必须先看见首片，再释放尾帧");
        if !incomplete {
            let _ = socket.write_all(tail.as_bytes());
        } else {
            // 完整工具调用参数但没有模型结束事实，也不得执行。
            socket.write_all(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"db_query\",\"arguments\":\"{}\"}}]}}]}\n\n").unwrap();
        }
    });
    let (tx, rx) = mpsc::sync_channel(64);
    let storage = fixture.storage.clone();
    let job = fixture.job.clone();
    let cancel = CancelFlag::new();
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        let provider = AiConversationProvider::for_test(
            protocol,
            endpoint,
            "offline".into(),
            ModelCapabilities {
                streaming: true,
                tool_calling: true,
                reasoning_summary: false,
            },
        );
        let mut tools = ToolCatalog::new(Vec::new());
        tools
            .add_tool(Box::new(RecordingTool::new("db.query", ToolRisk::Read)))
            .unwrap();
        run_ai_conversation_turn(
            &storage,
            &job,
            &worker_cancel,
            &tx,
            &provider,
            &tools,
            &KernelAuditHook,
            TurnPermissionContext::default(),
        );
    });
    let mut streamed = String::new();
    loop {
        let update = rx.recv_timeout(WAIT).unwrap();
        assert!(!update.terminal, "尾帧未释放，不能先收到终态");
        if let Some(delta) = update.delta {
            streamed.push_str(&delta.delta);
            if streamed == "alpha" {
                break;
            }
        }
    }
    assert!(!worker.is_finished(), "首片必须在请求返回前送达");
    if cancel_midstream {
        cancel.request();
    }
    release.send(()).unwrap();
    let terminal = loop {
        let update = rx.recv_timeout(WAIT).unwrap();
        if let Some(delta) = update.delta {
            streamed.push_str(&delta.delta);
        }
        if update.terminal {
            break update.view.unwrap();
        }
    };
    worker.join().unwrap();
    server.join().unwrap();
    let body = terminal
        .messages
        .iter()
        .find(|m| m.id == fixture.job.assistant_message_id)
        .unwrap()
        .parts
        .iter()
        .find(|p| p.kind == "text")
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&body.payload_json).unwrap();
    if incomplete || cancel_midstream {
        assert_eq!(payload["text"], "alpha");
        assert!(matches!(body.status.as_str(), "error" | "interrupted"));
    } else {
        assert_eq!(payload["text"], "alpha beta");
        assert_eq!(streamed, "alpha beta", "不能在返回后重放相同事件");
        assert_eq!(body.status, "complete");
    }
    fixture
        .storage
        .with_ai_connection(|conn| {
            if incomplete {
                let events = fluxdb_storage::ai_platform::run_events::list_for_turn(
                    conn,
                    &fixture.job.turn_id,
                )?;
                let diagnostic = events
                    .iter()
                    .find(|e| e.kind == KIND_TURN_DIAGNOSTICS)
                    .unwrap();
                let payload: serde_json::Value =
                    serde_json::from_str(&diagnostic.summary_json).unwrap();
                assert_eq!(payload["model_failures"], 1, "断流必须进入诊断失败计数");
            }
            let tool_calls: i64 = conn
                .query_row("SELECT COUNT(*) FROM ai_tool_calls", [], |r| r.get(0))
                .unwrap();
            assert_eq!(tool_calls, 0, "响应未完整结束不得执行工具");
            let main_requests: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM ai_usage_ledger WHERE purpose='main'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(main_requests, 1, "首片之后中断不能自动重试");
            let pending: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM ai_usage_ledger WHERE measurement='pending'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(pending, 0, "中断/取消也必须结算用量");
            Ok(())
        })
        .unwrap();
}

#[test]
fn three_protocols_deliver_before_http_completion_without_replay() {
    for protocol in [
        ChatProtocol::OpenAiCompatible,
        ChatProtocol::AnthropicMessages,
        ChatProtocol::OpenAiResponses,
    ] {
        run_http_stream(protocol, false, false);
    }
}

#[test]
fn eof_keeps_partial_response_without_retry_or_tool_execution() {
    run_http_stream(ChatProtocol::OpenAiCompatible, true, false);
}

#[test]
fn cancel_keeps_partial_response_and_settles_usage() {
    run_http_stream(ChatProtocol::OpenAiCompatible, false, true);
}

#[test]
fn cancelling_suspended_provider_does_not_leave_pending_usage() {
    struct Suspended;
    impl fluxdb_core::GenerationProvider for Suspended {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calling: false,
                reasoning_summary: false,
            }
        }
        fn stream(
            &self,
            _: fluxdb_core::ModelRequest,
            _: CancelFlag,
        ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
            unreachable!("主循环必须走增量接口")
        }
        fn stream_events<'a>(
            &'a self,
            _: fluxdb_core::ModelRequest,
            _: CancelFlag,
            emit: &'a mut (dyn FnMut(&fluxdb_core::ModelEvent) + Send),
        ) -> fluxdb_core::AiFuture<'a, ()> {
            Box::pin(async move {
                emit(&fluxdb_core::ModelEvent::TextDelta("before suspend".into()));
                std::future::pending().await
            })
        }
    }
    let fixture = Fixture::new();
    let storage = fixture.storage.clone();
    let job = fixture.job.clone();
    let cancel = CancelFlag::new();
    let flag = cancel.clone();
    let (tx, rx) = mpsc::sync_channel(16);
    let worker = std::thread::spawn(move || {
        run_ai_conversation_turn(
            &storage,
            &job,
            &flag,
            &tx,
            &Suspended,
            &ToolCatalog::new(Vec::new()),
            &KernelAuditHook,
            TurnPermissionContext::default(),
        );
    });
    loop {
        if rx.recv_timeout(WAIT).unwrap().delta.is_some() {
            break;
        }
    }
    cancel.request();
    loop {
        if rx.recv_timeout(WAIT).unwrap().terminal {
            break;
        }
    }
    worker.join().unwrap();
    fixture
        .storage
        .with_ai_connection(|conn| {
            let state: String = conn
                .query_row(
                    "SELECT measurement FROM ai_usage_ledger WHERE purpose='main'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, "unknown");
            Ok(())
        })
        .unwrap();
}
