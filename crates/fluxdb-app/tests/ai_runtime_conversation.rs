//! 统一 actor 的生产入口：隔离存储、受控 HTTP Provider、真实 SQLite 工具与审批。
use fluxdb_app::*;
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason, ModelEvent,
};
use fluxdb_storage::{FileStorage, ai_platform::sessions};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    storage: FileStorage,
    controller: AppController,
}

impl Fixture {
    fn new(endpoint: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fluxdb-runtime-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let storage = FileStorage::new(&root);
        let mut controller = AppController::new();
        controller.set_ai_storage(storage.clone());
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "Local model".into(),
            provider: "openai".into(),
            model: "fixture".into(),
            endpoint: Some(endpoint.into()),
            enabled: true,
            api_key: Some("local-fixture-key".into()),
            context_window_tokens: Some(8192),
            max_output_tokens: Some(1024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            _ => panic!("model setup failed"),
        };
        assert!(matches!(
            controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                display_name: "Runtime agent".into(),
                model_profile_id: Some(model),
                system_prompt: "Use the fixed target".into(),
                enabled: true,
                ..Default::default()
            })),
            AppEvent::AiAgentSaved { .. }
        ));
        Self {
            root,
            storage,
            controller,
        }
    }

    fn job(&mut self) -> AiConversationJob {
        let target = TargetRef {
            connection_id: Some(1),
            database: Some("main".into()),
            ..Default::default()
        };
        self.controller
            .prepare_ai_conversation(
                "test task",
                &TurnTargetPin {
                    target_json: target.to_json(),
                    target,
                    captured_tab_id: None,
                    captured_tab_revision: 0,
                },
            )
            .unwrap()
    }

    fn connection(&self) -> ConnectionConfig {
        let path = self.root.join("target.sqlite");
        rusqlite::Connection::open(&path).unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS counter(value INTEGER); DELETE FROM counter; INSERT INTO counter VALUES(0);"
        ).unwrap();
        ConnectionConfig {
            id: ConnectionId(1),
            name: "Isolated SQLite".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path,
                read_only: false,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }
    }

    fn status(&self, job: &AiConversationJob) -> sessions::TurnStatus {
        self.storage
            .with_ai_connection(|conn| Ok(sessions::load_turn(conn, &job.turn_id)?.status))
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn terminal(receiver: &ConversationEventReceiver) -> AiConversationUpdate {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match receiver.try_recv() {
            Ok(update) if update.terminal => return update,
            Ok(_) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("actor ended without terminal view"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "terminal view timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn model_server(
    steps: Vec<serde_json::Value>,
) -> (String, std::thread::JoinHandle<Vec<serde_json::Value>>) {
    model_server_gated(steps, None)
}

fn model_server_gated(
    steps: Vec<serde_json::Value>,
    first_reply_gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> (String, std::thread::JoinHandle<Vec<serde_json::Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (index, event) in steps.into_iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("accept: {error}"),
                }
                assert!(Instant::now() < deadline, "model request timed out");
                std::thread::sleep(Duration::from_millis(5));
            };
            // macOS 的 accept 可能继承监听 socket 的非阻塞标志；请求读取使用明确的超时阻塞模式。
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let (body_start, body_length) = loop {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&buffer[..read]);
                assert!(bytes.len() < 1024 * 1024);
                if let Some(start) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..start]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (start + 4, length);
                }
            };
            while bytes.len() < body_start + body_length {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&buffer[..read]);
                assert!(bytes.len() < 1024 * 1024);
            }
            requests.push(
                serde_json::from_slice(&bytes[body_start..body_start + body_length]).unwrap(),
            );
            if index == 0 {
                if let Some((seen, release)) = &first_reply_gate {
                    seen.send(()).unwrap();
                    release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            }
            let body = format!("data: {event}\n\ndata: [DONE]\n\n");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
        requests
    });
    (format!("http://{address}/v1"), server)
}

fn text_event(text: &str) -> serde_json::Value {
    serde_json::json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}]})
}

fn tool_event(name: &str, sql: &str) -> serde_json::Value {
    let args = serde_json::json!({"target":"conn:1","sql":sql}).to_string();
    serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":name,"arguments":args}}]},"finish_reason":"tool_calls"}]})
}

#[test]
fn production_submission_executes_real_sqlite_read_and_projects_receipt() {
    let (endpoint, server) = model_server(vec![
        tool_event("db_query", "SELECT value FROM counter"),
        text_event("Read complete"),
    ]);
    let mut fixture = Fixture::new(&endpoint);
    let config = fixture.connection();
    let job = fixture.job();
    let mut runtime = AiRuntimeHandle::new(4).unwrap();
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            job.clone(),
            vec![config],
            None,
            CancelFlag::new(),
        )
        .unwrap();
    let update = terminal(&receiver);
    assert!(
        update
            .view
            .unwrap()
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| part.kind == "tool_result_ref")
    );
    assert_eq!(fixture.status(&job), sessions::TurnStatus::Completed);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool")
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            let status: String = conn
                .query_row(
                    "SELECT status FROM ai_tool_calls WHERE turn_id=?1",
                    [&job.turn_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "succeeded");
            Ok(())
        })
        .unwrap();
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn running_turn_absorbs_steering_at_next_model_request() {
    let (seen, first_request) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (endpoint, server) = model_server_gated(
        vec![
            tool_event("db_query", "SELECT value FROM counter"),
            text_event("After reading"),
        ],
        Some((seen, gate)),
    );
    let mut fixture = Fixture::new(&endpoint);
    let config = fixture.connection();
    let job = fixture.job();
    let mut runtime = AiRuntimeHandle::new(4).unwrap();
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            job.clone(),
            vec![config],
            None,
            CancelFlag::new(),
        )
        .unwrap();
    first_request.recv_timeout(Duration::from_secs(5)).unwrap();
    runtime.steering(&job.turn_id, "补充：请核对数量").unwrap();
    release.send(()).unwrap();
    assert!(terminal(&receiver).view.unwrap().error.is_none());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    let first = requests[0]["messages"].as_array().unwrap();
    let second = requests[1]["messages"].as_array().unwrap();
    assert!(
        !first
            .iter()
            .any(|message| message["content"].to_string().contains("核对数量"))
    );
    assert!(
        second
            .iter()
            .any(|message| message["content"].to_string().contains("核对数量"))
    );
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn approval_resume_uses_same_actor_and_writes_exactly_once() {
    let (endpoint, server) = model_server(vec![
        tool_event("db_execute", "UPDATE counter SET value=value+1"),
        text_event("Write complete"),
    ]);
    let mut fixture = Fixture::new(&endpoint);
    let config = fixture.connection();
    let job = fixture.job();
    let mut runtime = AiRuntimeHandle::new(4).unwrap();
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            job.clone(),
            vec![config.clone()],
            None,
            CancelFlag::new(),
        )
        .unwrap();
    let view = terminal(&receiver).view.unwrap();
    assert_eq!(fixture.status(&job), sessions::TurnStatus::WaitingApproval);
    let approval = view
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find_map(|part| {
            let payload: serde_json::Value = serde_json::from_str(&part.payload_json).ok()?;
            Some((
                payload["approval_id"].as_str()?.to_owned(),
                payload["approval_revision"].as_i64()?,
            ))
        })
        .unwrap();
    let target = fixture.root.join("target.sqlite");
    let value = || {
        rusqlite::Connection::open(&target)
            .unwrap()
            .query_row("SELECT value FROM counter", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(value(), 0, "no side effect before approval");
    assert!(!matches!(
        fixture.controller.dispatch(AppCommand::AiConversation(
            AiConversationCommand::DecideApproval {
                approval_id: approval.0.clone(),
                approved: true,
                expected_revision: approval.1,
            }
        )),
        AppEvent::AiSettingsError(_)
    ));
    let resumed = fixture
        .controller
        .prepare_ai_resume_job(&approval.0)
        .unwrap();
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            resumed,
            vec![config.clone()],
            Some(approval.0.clone()),
            CancelFlag::new(),
        )
        .unwrap();
    terminal(&receiver);
    assert_eq!(value(), 1);
    assert_eq!(fixture.status(&job), sessions::TurnStatus::Completed);
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            job,
            vec![config],
            Some(approval.0),
            CancelFlag::new(),
        )
        .unwrap();
    terminal(&receiver);
    assert_eq!(value(), 1, "duplicate resume must not repeat a write");
    assert_eq!(server.join().unwrap().len(), 2);
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn full_event_queue_does_not_block_shutdown_or_terminal_persistence() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1");
    let job = fixture.job();
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let (ready, gate) = mpsc::channel();
    let receiver = runtime
        .submit_conversation_source(
            fixture.storage.clone(),
            job.clone(),
            CancelFlag::new(),
            move |cancel, emit| {
                for _ in 0..10 {
                    emit(&ModelEvent::TextDelta("fragment".repeat(100).into()));
                    std::thread::sleep(Duration::from_millis(20));
                }
                ready
                    .send(std::thread::current().name().unwrap().to_owned())
                    .unwrap();
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                emit(&ModelEvent::Finished(FinishReason::Cancelled));
                Ok(Vec::new())
            },
        )
        .unwrap();
    assert_eq!(
        gate.recv_timeout(Duration::from_secs(2)).unwrap(),
        "fluxdb-ai-runtime"
    );
    assert!(
        runtime.shutdown(Duration::from_secs(2)),
        "UI has not consumed the full queue"
    );
    assert_eq!(fixture.status(&job), sessions::TurnStatus::Cancelled);
    assert!(terminal(&receiver).view.unwrap().error.is_none());
    fixture
        .storage
        .with_ai_connection(|conn| {
            assert_eq!(
                sessions::list_parts(conn, &job.assistant_message_id)?.len(),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn shutdown_finalizes_queued_conversation_without_calling_source() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1");
    let first = fixture.job();
    fixture
        .controller
        .dispatch(AppCommand::AiConversation(AiConversationCommand::New));
    let second = fixture.job();
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let (started, gate) = mpsc::channel();
    let receiver1 = runtime
        .submit_conversation_source(
            fixture.storage.clone(),
            first.clone(),
            CancelFlag::new(),
            move |cancel, emit| {
                started.send(()).unwrap();
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                emit(&ModelEvent::Finished(FinishReason::Cancelled));
                Ok(Vec::new())
            },
        )
        .unwrap();
    gate.recv_timeout(Duration::from_secs(2)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let source_calls = calls.clone();
    let queued_cancel = CancelFlag::new();
    let receiver2 = runtime
        .submit_conversation_source(
            fixture.storage.clone(),
            second.clone(),
            queued_cancel.clone(),
            move |_, _| {
                source_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            },
        )
        .unwrap();
    runtime.cancel(&second.turn_id).unwrap();
    runtime.cancel(&second.turn_id).unwrap();
    assert!(
        queued_cancel.is_cancelled(),
        "queued cancellation is immediate and idempotent"
    );
    assert!(runtime.shutdown(Duration::from_secs(2)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    terminal(&receiver1);
    terminal(&receiver2);
    assert_eq!(fixture.status(&first), sessions::TurnStatus::Cancelled);
    assert_eq!(fixture.status(&second), sessions::TurnStatus::Cancelled);
}

#[test]
fn provider_setup_failure_is_persisted_and_visible_through_runtime_bridge() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1");
    let job = fixture.job();
    assert!(matches!(
        fixture
            .controller
            .dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
                id: Some(job.model_profile_id.clone()),
                display_name: "Local model".into(),
                provider: "openai".into(),
                model: "fixture".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                enabled: true,
                clear_key: true,
                expected_revision: Some(1),
                ..Default::default()
            })),
        AppEvent::AiModelSaved { .. }
    ));
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let receiver = runtime
        .submit_conversation(
            fixture.storage.clone(),
            job.clone(),
            Vec::new(),
            None,
            CancelFlag::new(),
        )
        .unwrap();
    let update = terminal(&receiver);
    assert!(update.view.unwrap().error.is_some());
    assert_eq!(fixture.status(&job), sessions::TurnStatus::Failed);
    assert!(runtime.shutdown(Duration::from_secs(2)));
}
