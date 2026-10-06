// AI 运行时 actor 边界测试（设计 §12，T016）。
//
// 本文件只使用 scripted/custom Provider 与内存 sink，不发网络、不用真实凭据，
// 也不读取生产 SQLite；重点是控制队列、取消旁路、事件顺序和关闭安全。

mod ai_runtime_test_support {
    use crate::{AiEvent, AiEventSink, ScriptedGenerationProvider};
    use fluxdb_core::{
        AiFuture, CancelFlag, Error, ErrorKind, FinishReason, GenerationProvider,
        ModelCapabilities, ModelEvent, ModelMessage, ModelRequest, ModelRole,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    pub fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn event_session_id(event: &AiEvent) -> &str {
        match event {
            AiEvent::TurnStarted { session_id, .. }
            | AiEvent::TextDelta { session_id, .. }
            | AiEvent::ReasoningSummaryDelta { session_id, .. }
            | AiEvent::ToolCallRequested { session_id, .. }
            | AiEvent::ToolStarted { session_id, .. }
            | AiEvent::ToolFinished { session_id, .. }
            | AiEvent::ApprovalRequired { session_id, .. }
            | AiEvent::Notice { session_id, .. }
            | AiEvent::Paused { session_id, .. }
            | AiEvent::Finished { session_id, .. }
            | AiEvent::Error { session_id, .. } => session_id,
        }
    }

    pub fn request(model: &str, user_text: &str) -> ModelRequest {
        ModelRequest {
            model: model.to_string(),
            messages: vec![ModelMessage::new(ModelRole::User, user_text)],
            tools: Vec::new(),
            max_output_tokens: Some(64),
        }
    }

    #[derive(Clone)]
    pub struct RecordingSink {
        events: Arc<Mutex<Vec<AiEvent>>>,
        thread_names: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl RecordingSink {
        pub fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                thread_names: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn events(&self) -> Vec<AiEvent> {
            self.events.lock().expect("事件锁").clone()
        }

        pub fn thread_names(&self) -> Vec<Option<String>> {
            self.thread_names.lock().expect("线程名锁").clone()
        }

        pub fn wait_for_turn_finished(&self, turn_id: &str, timeout: Duration) -> bool {
            let expected = turn_id.to_string();
            wait_until(
                || {
                    self.events.lock().expect("事件锁").iter().any(|event| {
                        event.turn_id() == expected && matches!(event, AiEvent::Finished { .. })
                    })
                },
                timeout,
            )
        }
    }

    impl AiEventSink for RecordingSink {
        fn emit(&self, event: AiEvent) {
            self.thread_names
                .lock()
                .expect("线程名锁")
                .push(std::thread::current().name().map(str::to_string));
            self.events.lock().expect("事件锁").push(event);
        }
    }

    pub struct SharedProvider<T: ?Sized>(pub Arc<T>);

    impl<T: GenerationProvider + ?Sized> GenerationProvider for SharedProvider<T> {
        fn capabilities(&self) -> ModelCapabilities {
            self.0.capabilities()
        }

        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            self.0.stream(request, cancel)
        }
    }

    pub struct SharedScriptedProvider {
        pub inner: Arc<ScriptedGenerationProvider>,
    }

    impl GenerationProvider for SharedScriptedProvider {
        fn capabilities(&self) -> ModelCapabilities {
            self.inner.capabilities()
        }

        fn stream(
            &self,
            request: ModelRequest,
            cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            self.inner.stream(request, cancel)
        }
    }

    pub struct FailingProvider;

    impl GenerationProvider for FailingProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            Box::pin(async {
                Err(Error::new(
                    ErrorKind::Internal,
                    "模拟 Provider 在流开始前失败",
                ))
            })
        }
    }

    pub struct GateProvider {
        calls: AtomicUsize,
        started: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl GateProvider {
        pub fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                started: Arc::new((Mutex::new(false), Condvar::new())),
                release: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        pub fn wait_started(&self, timeout: Duration) -> bool {
            let (lock, cv) = &*self.started;
            let mut started = lock.lock().expect("启动锁");
            let deadline = Instant::now() + timeout;
            while !*started {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (guard, result) = cv.wait_timeout(started, remaining).expect("启动等待");
                started = guard;
                if result.timed_out() && !*started {
                    return false;
                }
            }
            true
        }

        pub fn release(&self) {
            let (lock, cv) = &*self.release;
            *lock.lock().expect("释放锁") = true;
            cv.notify_all();
        }
    }

    impl GenerationProvider for GateProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calling: false,
                reasoning_summary: false,
            }
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                let (lock, cv) = &*self.started;
                *lock.lock().expect("启动锁") = true;
                cv.notify_all();

                let (lock, cv) = &*self.release;
                let mut released = lock.lock().expect("释放锁");
                while !*released {
                    released = cv.wait(released).expect("释放等待");
                }
            }

            let events = if call == 0 {
                vec![
                    ModelEvent::TextDelta("first".into()),
                    ModelEvent::Finished(FinishReason::Stop),
                ]
            } else {
                vec![
                    ModelEvent::TextDelta("second".into()),
                    ModelEvent::Finished(FinishReason::Stop),
                ]
            };
            Box::pin(async move { Ok(events) })
        }
    }

    pub struct CancelAwareBlockingProvider {
        started: Arc<(Mutex<bool>, Condvar)>,
    }

    impl CancelAwareBlockingProvider {
        pub fn new() -> Self {
            Self {
                started: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        pub fn wait_started(&self, timeout: Duration) -> bool {
            let (lock, cv) = &*self.started;
            let mut started = lock.lock().expect("启动锁");
            let deadline = Instant::now() + timeout;
            while !*started {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (guard, result) = cv.wait_timeout(started, remaining).expect("启动等待");
                started = guard;
                if result.timed_out() && !*started {
                    return false;
                }
            }
            true
        }
    }

    impl GenerationProvider for CancelAwareBlockingProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calling: false,
                reasoning_summary: false,
            }
        }

        fn stream(
            &self,
            _request: ModelRequest,
            cancel: CancelFlag,
        ) -> AiFuture<'_, Vec<ModelEvent>> {
            let (lock, cv) = &*self.started;
            *lock.lock().expect("启动锁") = true;
            cv.notify_all();

            for _ in 0..500 {
                if cancel.is_cancelled() {
                    return Box::pin(async {
                        Ok(vec![ModelEvent::Finished(FinishReason::Cancelled)])
                    });
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Box::pin(async { Ok(vec![ModelEvent::Finished(FinishReason::Stop)]) })
        }
    }

    #[derive(Clone)]
    pub struct CancelGatedSink {
        events: Arc<Mutex<Vec<AiEvent>>>,
        first_text_seen: Arc<(Mutex<bool>, Condvar)>,
        first_text_release: Arc<(Mutex<bool>, Condvar)>,
        first_text_blocked: Arc<Mutex<bool>>,
    }

    impl CancelGatedSink {
        pub fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                first_text_seen: Arc::new((Mutex::new(false), Condvar::new())),
                first_text_release: Arc::new((Mutex::new(false), Condvar::new())),
                first_text_blocked: Arc::new(Mutex::new(false)),
            }
        }

        pub fn events(&self) -> Vec<AiEvent> {
            self.events.lock().expect("事件锁").clone()
        }

        pub fn wait_for_first_text(&self, timeout: Duration) -> bool {
            let (lock, _) = &*self.first_text_seen;
            let mut seen = lock.lock().expect("首文本锁");
            let deadline = Instant::now() + timeout;
            while !*seen {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                let (guard, result) = self
                    .first_text_seen
                    .1
                    .wait_timeout(seen, remaining)
                    .expect("首文本等待");
                seen = guard;
                if result.timed_out() && !*seen {
                    return false;
                }
            }
            true
        }

        pub fn release_first_text(&self) {
            let (lock, cv) = &*self.first_text_release;
            *lock.lock().expect("首文本释放锁") = true;
            cv.notify_all();
        }

        pub fn wait_for_turn_finished(&self, turn_id: &str, timeout: Duration) -> bool {
            let expected = turn_id.to_string();
            wait_until(
                || {
                    self.events.lock().expect("事件锁").iter().any(|event| {
                        event.turn_id() == expected && matches!(event, AiEvent::Finished { .. })
                    })
                },
                timeout,
            )
        }
    }

    impl AiEventSink for CancelGatedSink {
        fn emit(&self, event: AiEvent) {
            let is_text = matches!(&event, AiEvent::TextDelta { .. });
            self.events.lock().expect("事件锁").push(event);

            if !is_text {
                return;
            }

            let mut blocked = self.first_text_blocked.lock().expect("首文本阻塞锁");
            if *blocked {
                return;
            }
            *blocked = true;
            drop(blocked);

            let (seen_lock, seen_cv) = &*self.first_text_seen;
            *seen_lock.lock().expect("首文本锁") = true;
            seen_cv.notify_all();

            let (release_lock, release_cv) = &*self.first_text_release;
            let mut released = release_lock.lock().expect("首文本释放锁");
            while !*released {
                let (guard, result) = release_cv
                    .wait_timeout(released, Duration::from_secs(2))
                    .expect("首文本释放等待");
                released = guard;
                if result.timed_out() {
                    break;
                }
            }
        }
    }
}

use ai_runtime_test_support as rt;

#[test]
fn ai_runtime_event_order_and_seq_are_monotonic() {
    let provider = std::sync::Arc::new(ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: false,
            reasoning_summary: false,
        },
        vec![vec![
            fluxdb_core::ModelEvent::TextDelta("第一段".into()),
            fluxdb_core::ModelEvent::Usage(fluxdb_core::Usage {
                input_tokens: 7,
                output_tokens: 2,
                cached_input_tokens: 1,
            }),
            fluxdb_core::ModelEvent::TextDelta("第二段".into()),
            fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
        ]],
    ));
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedScriptedProvider { inner: provider }),
        Box::new(sink.clone()),
        8,
    );

    handle
        .start_turn("session-1", "turn-1", rt::request("model", "hello"))
        .expect("启动 Turn");
    assert!(sink.wait_for_turn_finished("turn-1", std::time::Duration::from_secs(2)));
    let events = sink.events();

    assert_eq!(events.len(), 4, "Usage 不应转成 UI 事件: {events:?}");
    let seqs: Vec<u64> = events.iter().map(AiEvent::seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    assert!(matches!(
        events.first(),
        Some(AiEvent::TurnStarted {
            session_id,
            turn_id,
            seq: 1,
        }) if session_id == "session-1" && turn_id == "turn-1"
    ));
    assert!(events.iter().all(|event| event.turn_id() == "turn-1"));
    assert!(
        events
            .iter()
            .all(|event| rt::event_session_id(event) == "session-1")
    );
    assert!(matches!(
        events.last(),
        Some(AiEvent::Finished {
            reason: fluxdb_core::FinishReason::Stop,
            ..
        })
    ));

    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_provider_error_is_visible_before_error_finish() {
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(Box::new(rt::FailingProvider), Box::new(sink.clone()), 8);

    handle
        .start_turn("session-err", "turn-err", rt::request("model", "hello"))
        .expect("启动失败场景 Turn");
    assert!(sink.wait_for_turn_finished("turn-err", std::time::Duration::from_secs(2)));
    let events = sink.events();

    let error_index = events
        .iter()
        .position(|event| matches!(event, AiEvent::Error { .. }))
        .expect("必须有 Error 事件");
    let message = match &events[error_index] {
        AiEvent::Error { message, .. } => message,
        other => panic!("期望 Error，实际 {other:?}"),
    };
    assert!(!message.is_empty());
    assert!(!message.to_lowercase().contains("panic"));
    assert_eq!(
        error_index + 1,
        events.len() - 1,
        "Error 之后应紧接 Finished(Error)"
    );
    assert!(matches!(
        events.last(),
        Some(AiEvent::Finished {
            reason: fluxdb_core::FinishReason::Error,
            ..
        })
    ));
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_cancel_is_not_starved_by_large_text_stream() {
    const DELTA_COUNT: usize = 200;
    let mut events = Vec::with_capacity(DELTA_COUNT);
    for index in 0..DELTA_COUNT {
        events.push(fluxdb_core::ModelEvent::TextDelta(format!(
            "delta-{index}-{}",
            "x".repeat(4096)
        )));
    }
    let provider = std::sync::Arc::new(ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: false,
            reasoning_summary: false,
        },
        vec![events],
    ));
    let sink = rt::CancelGatedSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedScriptedProvider { inner: provider }),
        Box::new(sink.clone()),
        8,
    );

    handle
        .start_turn(
            "session-cancel",
            "turn-cancel",
            rt::request("model", "hello"),
        )
        .expect("启动取消场景 Turn");
    assert!(sink.wait_for_first_text(std::time::Duration::from_secs(2)));

    let cancel_started = std::time::Instant::now();
    let cancel_result = handle.cancel("turn-cancel");
    sink.release_first_text();
    assert!(
        cancel_result.is_ok(),
        "取消应通过旁路成功: {cancel_result:?}"
    );
    assert!(sink.wait_for_turn_finished("turn-cancel", std::time::Duration::from_secs(2)));

    let ui_events = sink.events();
    let text_count = ui_events
        .iter()
        .filter(|event| matches!(event, AiEvent::TextDelta { .. }))
        .count();
    assert!(text_count <= 6, "取消后仍发送了过多文本增量: {text_count}");
    assert!(matches!(
        ui_events.last(),
        Some(AiEvent::Finished {
            reason: fluxdb_core::FinishReason::Cancelled,
            ..
        })
    ));
    assert!(cancel_started.elapsed() < std::time::Duration::from_secs(2));
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_cancel_of_queued_turn_finishes_immediately_when_started() {
    let provider = std::sync::Arc::new(rt::GateProvider::new());
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        8,
    );

    handle
        .start_turn("session-first", "turn-first", rt::request("model", "first"))
        .expect("启动第一个 Turn");
    assert!(provider.wait_started(std::time::Duration::from_secs(2)));

    handle
        .cancel("turn-second")
        .expect("排队取消不能阻塞或失败");
    handle
        .start_turn(
            "session-second",
            "turn-second",
            rt::request("model", "second"),
        )
        .expect("排队第二个 Turn");
    provider.release();

    assert!(sink.wait_for_turn_finished("turn-second", std::time::Duration::from_secs(2)));
    let second_events: Vec<_> = sink
        .events()
        .into_iter()
        .filter(|event| event.turn_id() == "turn-second")
        .collect();
    assert_eq!(second_events.len(), 2, "取消的排队 Turn 应只发开始和结束");
    assert!(matches!(
        second_events.first(),
        Some(AiEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        second_events.last(),
        Some(AiEvent::Finished {
            reason: fluxdb_core::FinishReason::Cancelled,
            ..
        })
    ));
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_steering_is_absorbed_into_next_turn_request() {
    let provider = std::sync::Arc::new(ScriptedGenerationProvider::text_only("回答"));
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedScriptedProvider {
            inner: provider.clone(),
        }),
        Box::new(sink.clone()),
        8,
    );

    handle
        .steering("next-turn", "补充要求")
        .expect("发送 steering");
    handle
        .start_turn(
            "session-steer",
            "next-turn",
            rt::request("model", "原始问题"),
        )
        .expect("启动下一 Turn");
    assert!(sink.wait_for_turn_finished("next-turn", std::time::Duration::from_secs(2)));

    let recorded = provider.request_at(0).expect("应记录第一次请求");
    assert!(recorded.messages.iter().any(|message| {
        message.role == fluxdb_core::ModelRole::User && message.text == "补充要求"
    }));
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_control_queue_full_returns_immediately() {
    let provider = std::sync::Arc::new(rt::GateProvider::new());
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        1,
    );

    handle
        .start_turn("session-busy", "turn-busy", rt::request("model", "busy"))
        .expect("启动占住 actor 的 Turn");
    assert!(provider.wait_started(std::time::Duration::from_secs(2)));

    handle
        .steering("turn-busy", "占满控制通道")
        .expect("容量 1 的空队列应接纳一条 steering");
    let started = std::time::Instant::now();
    let result = handle.start_turn(
        "session-overflow",
        "turn-overflow",
        rt::request("model", "overflow"),
    );
    let elapsed = started.elapsed();
    assert_eq!(result, Err(RuntimeError::ControlQueueFull));
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "满队列必须立即返回，实际 {elapsed:?}"
    );

    provider.release();
    assert!(sink.wait_for_turn_finished("turn-busy", std::time::Duration::from_secs(2)));
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

#[test]
fn ai_runtime_shutdown_safely_ends_active_turn() {
    let provider = std::sync::Arc::new(rt::CancelAwareBlockingProvider::new());
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        4,
    );

    handle
        .start_turn(
            "session-shutdown",
            "turn-shutdown",
            rt::request("model", "slow"),
        )
        .expect("启动长 Turn");
    assert!(provider.wait_started(std::time::Duration::from_secs(2)));

    let started = std::time::Instant::now();
    let shutdown_ok = handle.shutdown(std::time::Duration::from_secs(2));
    assert!(shutdown_ok, "shutdown 应在超时前安全结束 actor");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(
        handle.start_turn(
            "session-closed",
            "turn-closed",
            rt::request("model", "after shutdown")
        ),
        Err(RuntimeError::Closed)
    );
}

#[test]
fn ai_runtime_drop_does_not_hang_after_finished_turn() {
    let started = std::time::Instant::now();
    {
        let provider = std::sync::Arc::new(ScriptedGenerationProvider::text_only("done"));
        let sink = rt::RecordingSink::new();
        let handle = spawn_ai_runtime(
            Box::new(rt::SharedScriptedProvider { inner: provider }),
            Box::new(sink.clone()),
            4,
        );
        handle
            .start_turn("session-drop", "turn-drop", rt::request("model", "hello"))
            .expect("启动 Drop 场景 Turn");
        assert!(sink.wait_for_turn_finished("turn-drop", std::time::Duration::from_secs(2)));
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "Drop 不应挂死，实际 {:?}",
        started.elapsed()
    );
}

#[test]
fn ai_runtime_sink_events_are_emitted_on_runtime_thread() {
    let provider = std::sync::Arc::new(ScriptedGenerationProvider::text_only("thread"));
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedScriptedProvider { inner: provider }),
        Box::new(sink.clone()),
        4,
    );

    handle
        .start_turn(
            "session-thread",
            "turn-thread",
            rt::request("model", "hello"),
        )
        .expect("启动线程名场景 Turn");
    assert!(sink.wait_for_turn_finished("turn-thread", std::time::Duration::from_secs(2)));

    let names = sink.thread_names();
    assert!(!names.is_empty());
    assert!(
        names
            .iter()
            .all(|name| name.as_deref() == Some("fluxdb-ai-runtime")),
        "事件应全部来自 actor 线程: {names:?}"
    );
    assert!(handle.shutdown(std::time::Duration::from_secs(1)));
}

/// 回归：控制队列满时也必须能可靠关闭（A 报出的缺陷 —— `try_send(Shutdown)` 在 Full 时失败，
/// actor 会一直阻塞在 `recv()`；修复后关闭走旁路标志 + `recv_timeout` 轮询）。
#[test]
fn ai_runtime_shutdown_works_when_control_queue_is_full() {
    let provider = std::sync::Arc::new(rt::CancelAwareBlockingProvider::new());
    let sink = rt::RecordingSink::new();
    let mut handle = spawn_ai_runtime(
        Box::new(rt::SharedProvider(provider.clone())),
        Box::new(sink.clone()),
        1,
    );

    // actor 被长 Turn 占住；此时队列容量 1，先塞一条命令占满，再确认后续投递返回 Full。
    handle
        .start_turn("session-full", "turn-full", rt::request("model", "slow"))
        .expect("启动长 Turn");
    assert!(provider.wait_started(std::time::Duration::from_secs(2)));
    handle.steering("turn-full", "占位").expect("占满控制队列");
    assert_eq!(
        handle.steering("turn-full", "再占一条"),
        Err(RuntimeError::ControlQueueFull),
        "队列应已满"
    );

    // 队列满的情况下关闭：必须在超时前成功。
    let started = std::time::Instant::now();
    let ok = handle.shutdown(std::time::Duration::from_secs(2));
    assert!(ok, "控制队列满时 shutdown 也必须成功");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "shutdown 不应等到超时"
    );
}
