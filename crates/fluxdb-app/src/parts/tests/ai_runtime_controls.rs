// 任务调度的单元断言；会话/工具/事件断言由 ai_runtime_conversation 集成测试覆盖。
use std::sync::mpsc as runtime_mpsc;

#[test]
fn ai_runtime_executes_accepted_tasks_serially_on_owned_thread() {
    let mut runtime = AiRuntimeHandle::new(2).unwrap();
    let (started, ready) = runtime_mpsc::channel();
    let (release, gate) = runtime_mpsc::channel();
    let (finished, results) = runtime_mpsc::channel();
    let first_finished = finished.clone();
    runtime
        .submit(
            "first".into(),
            CancelFlag::new(),
            Box::new(move |_| {
                started
                    .send(std::thread::current().name().unwrap().to_owned())
                    .unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
                first_finished.send("first").unwrap();
            }),
        )
        .unwrap();
    assert_eq!(
        ready.recv_timeout(Duration::from_secs(2)).unwrap(),
        "fluxdb-ai-runtime"
    );
    runtime
        .submit(
            "second".into(),
            CancelFlag::new(),
            Box::new(move |_| {
                finished.send("second").unwrap();
            }),
        )
        .unwrap();
    assert!(
        results.try_recv().is_err(),
        "second task cannot overtake an active task"
    );
    release.send(()).unwrap();
    assert_eq!(
        results.recv_timeout(Duration::from_secs(2)).unwrap(),
        "first"
    );
    assert_eq!(
        results.recv_timeout(Duration::from_secs(2)).unwrap(),
        "second"
    );
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn ai_runtime_full_task_queue_rejects_immediately() {
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let (started, ready) = runtime_mpsc::channel();
    let (release, gate) = runtime_mpsc::channel();
    runtime
        .submit(
            "active".into(),
            CancelFlag::new(),
            Box::new(move |_| {
                started.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
            }),
        )
        .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime
        .submit("queued".into(), CancelFlag::new(), Box::new(|_| {}))
        .unwrap();
    let started = Instant::now();
    assert_eq!(
        runtime.submit("overflow".into(), CancelFlag::new(), Box::new(|_| {})),
        Err(RuntimeError::ControlQueueFull)
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    release.send(()).unwrap();
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn ai_runtime_shutdown_cancels_active_and_full_queue_without_waiting_for_delivery() {
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let (started, ready) = runtime_mpsc::channel();
    runtime
        .submit(
            "active".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                started.send(()).unwrap();
                while !turn.cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }),
        )
        .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let (finished, result) = runtime_mpsc::channel();
    runtime
        .submit(
            "queued".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                finished.send(turn.cancel.is_cancelled()).unwrap();
            }),
        )
        .unwrap();
    assert!(runtime.shutdown(Duration::from_secs(2)));
    assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
    assert_eq!(
        runtime.submit("closed".into(), CancelFlag::new(), Box::new(|_| {})),
        Err(RuntimeError::Closed)
    );
    assert_eq!(runtime.cancel("closed"), Err(RuntimeError::Closed));
}

#[test]
fn ai_runtime_drop_requests_cancel_without_blocking_caller() {
    let runtime = AiRuntimeHandle::new(1).unwrap();
    let (started, ready) = runtime_mpsc::channel();
    let (finished, done) = runtime_mpsc::channel();
    runtime
        .submit(
            "active".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                started.send(()).unwrap();
                while !turn.cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                finished.send(()).unwrap();
            }),
        )
        .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    drop(runtime);
    assert!(started.elapsed() < Duration::from_millis(200));
    done.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn ai_runtime_shutdown_timeout_keeps_join_until_external_call_returns() {
    let mut runtime = AiRuntimeHandle::new(1).unwrap();
    let (started, ready) = runtime_mpsc::channel();
    let (release, gate) = runtime_mpsc::channel();
    runtime
        .submit(
            "blocked".into(),
            CancelFlag::new(),
            Box::new(move |_| {
                started.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
            }),
        )
        .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!runtime.shutdown(Duration::from_millis(20)));
    assert!(runtime.join.is_some());
    release.send(()).unwrap();
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn ai_runtime_steering_is_scoped_bounded_and_preserves_accepted_input() {
    let mut runtime = AiRuntimeHandle::new(2).unwrap();
    runtime.steering("first", "accepted").unwrap();
    runtime.steering("second", "other").unwrap();
    assert_eq!(
        runtime.steering("first", "overflow"),
        Err(RuntimeError::ControlQueueFull)
    );
    let (finished, result) = runtime_mpsc::channel();
    runtime
        .submit(
            "first".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                finished.send(turn.take_steering()).unwrap();
            }),
        )
        .unwrap();
    assert_eq!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        vec!["accepted"]
    );
    assert_eq!(
        runtime.steering("first", "x".repeat(64 * 1024 + 1)),
        Err(RuntimeError::ControlQueueFull)
    );
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn ai_runtime_cancel_before_submission_is_consumed_by_matching_turn_only() {
    let mut runtime = AiRuntimeHandle::new(2).unwrap();
    runtime.cancel("cancelled").unwrap();
    runtime.cancel("cancelled").unwrap();
    let (finished, result) = runtime_mpsc::channel();
    let first = finished.clone();
    runtime
        .submit(
            "other".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                first.send(turn.cancel.is_cancelled()).unwrap();
            }),
        )
        .unwrap();
    runtime
        .submit(
            "cancelled".into(),
            CancelFlag::new(),
            Box::new(move |turn| {
                finished.send(turn.cancel.is_cancelled()).unwrap();
            }),
        )
        .unwrap();
    assert!(!result.recv_timeout(Duration::from_secs(2)).unwrap());
    assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
    assert!(runtime.shutdown(Duration::from_secs(2)));
}

#[test]
fn ai_conversation_terminal_is_visible_while_sender_is_still_alive() {
    let (sender, receiver) = conversation_event_channel();
    let job = AiConversationJob {
        session_id: "session".into(),
        turn_id: "turn".into(),
        assistant_message_id: "message".into(),
        model_profile_id: "model".into(),
    };
    sender
        .publish_terminal(AiConversationUpdate::view(
            job,
            AiConversationView::default(),
            true,
        ))
        .unwrap();
    assert!(receiver.try_recv().unwrap().terminal);
    assert!(matches!(
        receiver.try_recv(),
        Err(runtime_mpsc::TryRecvError::Empty)
    ));
}
