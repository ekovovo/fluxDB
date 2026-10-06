// AI 领域 DTO / Provider / 工具契约测试（设计 §8、§20、§37）。
//
// 本文件只验证无生产依赖的内存契约：不读 FileStorage/AppController/config，
// 不发真实网络请求，也不接触真实凭据。

fn ai_contract_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(future)
}

fn ai_contract_request(messages: Vec<fluxdb_core::ModelMessage>) -> fluxdb_core::ModelRequest {
    fluxdb_core::ModelRequest {
        model: "contract-model".into(),
        messages,
        tools: Vec::new(),
        max_output_tokens: Some(128),
    }
}

fn assert_no_credential_keys(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let normalized = key.to_ascii_lowercase();
                for forbidden in ["api_key", "apikey", "secret", "credential", "ciphertext"] {
                    assert!(
                        !normalized.contains(forbidden),
                        "序列化白名单不应出现凭据字段 {forbidden:?}: {key}"
                    );
                }
                assert_no_credential_keys(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                assert_no_credential_keys(item);
            }
        }
        _ => {}
    }
}

#[test]
fn ai_contract_dto_serde_round_trip_and_whitelist() {
    let descriptor = fluxdb_core::ToolDescriptor {
        id: "db.query".into(),
        version: 3,
        description: "执行只读查询".into(),
        risk: fluxdb_core::ToolRisk::Read,
        input_schema_json: r#"{"type":"object","properties":{"sql":{"type":"string"}}}"#.into(),
    };
    let descriptor_json =
        serde_json::to_value(&descriptor).expect("fluxdb_core::ToolDescriptor 序列化");
    assert_no_credential_keys(&descriptor_json);
    let descriptor_back: fluxdb_core::ToolDescriptor =
        serde_json::from_value(descriptor_json).expect("fluxdb_core::ToolDescriptor 反序列化");
    assert_eq!(descriptor_back, descriptor);

    let receipt = fluxdb_core::ToolReceipt {
        call_id: "call-1".into(),
        tool_id: "db.query".into(),
        status: fluxdb_core::ToolReceiptStatus::Failed,
        result_json: r#"{"ok":false}"#.into(),
        error_code: Some("query_failed".into()),
        affected_rows: Some(0),
        started_at_ms: 10,
        finished_at_ms: 20,
    };
    let receipt_json = serde_json::to_value(&receipt).expect("fluxdb_core::ToolReceipt 序列化");
    assert_no_credential_keys(&receipt_json);
    let receipt_back: fluxdb_core::ToolReceipt =
        serde_json::from_value(receipt_json).expect("fluxdb_core::ToolReceipt 反序列化");
    assert_eq!(receipt_back, receipt);

    for (role, expected) in [
        (fluxdb_core::ModelRole::System, "\"system\""),
        (fluxdb_core::ModelRole::User, "\"user\""),
        (fluxdb_core::ModelRole::Assistant, "\"assistant\""),
        (fluxdb_core::ModelRole::Tool, "\"tool\""),
    ] {
        let encoded = serde_json::to_string(&role).expect("fluxdb_core::ModelRole 序列化");
        assert_eq!(encoded, expected);
        let decoded: fluxdb_core::ModelRole =
            serde_json::from_str(&encoded).expect("fluxdb_core::ModelRole 反序列化");
        assert_eq!(decoded, role);
    }

    for (risk, expected) in [
        (fluxdb_core::ToolRisk::Read, "\"read\""),
        (fluxdb_core::ToolRisk::Write, "\"write\""),
        (fluxdb_core::ToolRisk::Admin, "\"admin\""),
    ] {
        let encoded = serde_json::to_string(&risk).expect("fluxdb_core::ToolRisk 序列化");
        assert_eq!(encoded, expected);
        let decoded: fluxdb_core::ToolRisk =
            serde_json::from_str(&encoded).expect("fluxdb_core::ToolRisk 反序列化");
        assert_eq!(decoded, risk);
    }

    for (status, expected) in [
        (fluxdb_core::ToolReceiptStatus::Succeeded, "\"succeeded\""),
        (fluxdb_core::ToolReceiptStatus::Failed, "\"failed\""),
        (fluxdb_core::ToolReceiptStatus::Denied, "\"denied\""),
        (fluxdb_core::ToolReceiptStatus::Cancelled, "\"cancelled\""),
        (fluxdb_core::ToolReceiptStatus::Unknown, "\"unknown\""),
    ] {
        let encoded =
            serde_json::to_string(&status).expect("fluxdb_core::ToolReceiptStatus 序列化");
        assert_eq!(encoded, expected);
        let decoded: fluxdb_core::ToolReceiptStatus =
            serde_json::from_str(&encoded).expect("fluxdb_core::ToolReceiptStatus 反序列化");
        assert_eq!(decoded, status);
    }

    for (reason, expected) in [
        (fluxdb_core::FinishReason::Stop, "\"stop\""),
        (fluxdb_core::FinishReason::Length, "\"length\""),
        (fluxdb_core::FinishReason::ToolCalls, "\"tool_calls\""),
        (fluxdb_core::FinishReason::Cancelled, "\"cancelled\""),
        (fluxdb_core::FinishReason::Error, "\"error\""),
    ] {
        let encoded = serde_json::to_string(&reason).expect("fluxdb_core::FinishReason 序列化");
        assert_eq!(encoded, expected);
        let decoded: fluxdb_core::FinishReason =
            serde_json::from_str(&encoded).expect("fluxdb_core::FinishReason 反序列化");
        assert_eq!(decoded, reason);
    }
}

#[test]
fn ai_contract_traits_are_object_safe_for_runtime_heterogeneity() {
    let generation: Box<dyn fluxdb_core::GenerationProvider> =
        Box::new(ScriptedGenerationProvider::text_only("contract ok"));
    assert!(generation.capabilities().streaming);
    let generation_events = ai_contract_block_on(generation.stream(
        ai_contract_request(Vec::new()),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("trait object 生成调用");
    assert_eq!(
        generation_events,
        vec![
            fluxdb_core::ModelEvent::TextDelta("contract ok".into()),
            fluxdb_core::ModelEvent::Usage(fluxdb_core::Usage {
                input_tokens: 10,
                output_tokens: 3,
                cached_input_tokens: 0,
            }),
            fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
        ]
    );

    let decision: Box<dyn fluxdb_core::DecisionProvider> = Box::new(ScriptedDecisionProvider::new(
        fluxdb_core::DecisionResponse {
            chosen: Some("candidate-a".into()),
            scores: vec![("candidate-a".into(), 1.0)],
            reason: None,
        },
    ));
    let decision_response = ai_contract_block_on(decision.decide(
        fluxdb_core::DecisionRequest {
            model: "decision-model".into(),
            candidates: vec!["candidate-a".into()],
            prompt: "选择候选".into(),
        },
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("trait object 决策调用");
    assert_eq!(decision_response.chosen.as_deref(), Some("candidate-a"));

    let tool: Box<dyn fluxdb_core::AiTool> =
        Box::new(RecordingTool::new("db.query", fluxdb_core::ToolRisk::Read));
    let tool_receipt = ai_contract_block_on(tool.execute(
        fluxdb_core::ToolExecutionContext {
            session_id: "s-contract".into(),
            turn_id: "t-contract".into(),
            iteration: 1,
            call_id: "call-contract".into(),
            cancel: fluxdb_core::CancelFlag::new(),
        },
        r#"{"sql":"select 1"}"#.into(),
    ))
    .expect("trait object 工具调用");
    assert_eq!(tool.descriptor().id, "db.query");
    assert_eq!(tool_receipt.tool_id, "db.query");
    assert!(tool_receipt.is_success());
}

#[test]
fn ai_contract_cancel_flag_is_idempotent_and_clone_shared() {
    let cancel = fluxdb_core::CancelFlag::new();
    assert!(!cancel.is_cancelled(), "默认不应取消");

    let clone = cancel.clone();
    clone.request();
    assert!(cancel.is_cancelled(), "clone 必须共享同一取消状态");

    cancel.request();
    assert!(cancel.is_cancelled(), "重复 request 必须幂等");
    let debug = format!("{cancel:?}");
    assert!(debug.contains("cancelled"), "Debug 应暴露取消语义: {debug}");
    assert!(!debug.contains("0x"), "Debug 不应泄漏内部指针地址: {debug}");
}

#[test]
fn ai_contract_scripted_generation_boundaries_and_request_recording() {
    let step = vec![
        fluxdb_core::ModelEvent::TextDelta("same step".into()),
        fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
    ];
    let provider = ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: false,
            reasoning_summary: false,
        },
        vec![step.clone()],
    );

    for index in 0..3 {
        let request = ai_contract_request(vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            format!("第 {index} 次调用"),
        )]);
        let events =
            ai_contract_block_on(provider.stream(request.clone(), fluxdb_core::CancelFlag::new()))
                .expect("单步脚本应重复返回最后一步");
        assert_eq!(events, step, "第 {index} 次调用应返回同一个脚本步骤");
        assert_eq!(provider.request_at(index), Some(request));
    }
    assert_eq!(provider.call_count(), 3);

    let empty =
        ScriptedGenerationProvider::new(fluxdb_core::ModelCapabilities::default(), Vec::new());
    let request = ai_contract_request(vec![fluxdb_core::ModelMessage::new(
        fluxdb_core::ModelRole::User,
        "空脚本",
    )]);
    let error = ai_contract_block_on(empty.stream(request.clone(), fluxdb_core::CancelFlag::new()))
        .expect_err("空脚本必须返回 Err 而不是 panic");
    assert!(
        error.to_string().contains("没有更多脚本步骤"),
        "空脚本错误应可读: {error}"
    );
    assert_eq!(empty.call_count(), 1, "空脚本调用也必须如实记录");
    assert_eq!(empty.request_at(0), Some(request));
}

#[test]
fn ai_contract_recording_tool_receipt_is_failed_and_not_skipped() {
    let tool = RecordingTool::new("db.write", fluxdb_core::ToolRisk::Write)
        .with_status(fluxdb_core::ToolReceiptStatus::Failed);
    let ctx = fluxdb_core::ToolExecutionContext {
        session_id: "s-tool".into(),
        turn_id: "t-tool".into(),
        iteration: 2,
        call_id: "call-tool-2".into(),
        cancel: fluxdb_core::CancelFlag::new(),
    };
    let arguments_json = r#"{"sql":"update t set n=n+1","expected_affected":1}"#.to_string();

    let receipt = ai_contract_block_on(tool.execute(ctx.clone(), arguments_json.clone()))
        .expect("工具执行本身应返回权威回执");
    assert!(!receipt.is_success(), "Failed 回执不能被视为成功");
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert_eq!(
        receipt.error_code, None,
        "RecordingTool 未设置错误码时应如是为 None"
    );
    assert_eq!(receipt.call_id, ctx.call_id);
    assert_eq!(receipt.tool_id, tool.descriptor().id);
    assert_eq!(receipt.result_json, r#"{"ok":true}"#);
    assert_eq!(tool.call_count(), 1, "脚本不能跳过真实工具执行");
    assert_eq!(tool.calls(), vec![(ctx.call_id, arguments_json)]);
}

#[test]
fn ai_contract_scripted_decision_records_requests_verbatim() {
    let fixed = fluxdb_core::DecisionResponse {
        chosen: Some("model-b".into()),
        scores: vec![("model-a".into(), 0.2), ("model-b".into(), 0.8)],
        reason: Some("满足能力且更便宜".into()),
    };
    let provider = ScriptedDecisionProvider::new(fixed.clone());
    let first = fluxdb_core::DecisionRequest {
        model: "jev-contract".into(),
        candidates: vec!["model-a".into(), "model-b".into()],
        prompt: "第一次选择".into(),
    };
    let second = fluxdb_core::DecisionRequest {
        model: "jev-contract".into(),
        candidates: vec!["model-c".into()],
        prompt: "第二次选择".into(),
    };

    let first_response =
        ai_contract_block_on(provider.decide(first.clone(), fluxdb_core::CancelFlag::new()))
            .expect("首次决策");
    let second_response =
        ai_contract_block_on(provider.decide(second.clone(), fluxdb_core::CancelFlag::new()))
            .expect("二次决策");
    assert_eq!(first_response, fixed);
    assert_eq!(second_response, fixed);
    assert_eq!(provider.call_count(), 2);

    let recorded = provider.requests.lock().expect("决策请求记录锁").clone();
    assert_eq!(
        recorded,
        vec![first, second],
        "请求 candidates/prompt 必须原样记录"
    );
}

#[test]
fn ai_contract_model_tool_receipt_rereasoning_loop_has_no_production_deps() {
    let provider = ScriptedGenerationProvider::new(
        fluxdb_core::ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            vec![fluxdb_core::ModelEvent::ToolCallRequested(
                fluxdb_core::ToolCallRequest {
                    provider_call_id: "provider-call-7".into(),
                    tool_id: "db.query".into(),
                    arguments_json: r#"{"sql":"select id from orders"}"#.into(),
                },
            )],
            vec![
                fluxdb_core::ModelEvent::TextDelta("已拿到订单 ID".into()),
                fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop),
            ],
        ],
    );
    let tool = RecordingTool::new("db.query", fluxdb_core::ToolRisk::Read)
        .with_result(r#"{"rows":[{"id":7}]}"#);

    let first_events = ai_contract_block_on(provider.stream(
        ai_contract_request(vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            "查询订单 7",
        )]),
        fluxdb_core::CancelFlag::new(),
    ))
    .expect("模型首次推理");
    let call = match first_events.as_slice() {
        [fluxdb_core::ModelEvent::ToolCallRequested(call)] => call.clone(),
        other => panic!("首次推理应请求工具，实际 {other:?}"),
    };

    let receipt = ai_contract_block_on(tool.execute(
        fluxdb_core::ToolExecutionContext {
            session_id: "s-loop".into(),
            turn_id: "t-loop".into(),
            iteration: 1,
            call_id: call.provider_call_id.clone(),
            cancel: fluxdb_core::CancelFlag::new(),
        },
        call.arguments_json.clone(),
    ))
    .expect("真实工具执行");
    assert_eq!(tool.call_count(), 1);

    let second_request = ai_contract_request(vec![
        fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::User, "查询订单 7"),
        fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::Assistant, "调用 db.query"),
        fluxdb_core::ModelMessage::new(fluxdb_core::ModelRole::Tool, receipt.result_json.clone()),
    ]);
    let second_events = ai_contract_block_on(
        provider.stream(second_request.clone(), fluxdb_core::CancelFlag::new()),
    )
    .expect("模型基于回执再次推理");
    assert!(matches!(
        second_events.last(),
        Some(fluxdb_core::ModelEvent::Finished(
            fluxdb_core::FinishReason::Stop
        ))
    ));

    let recorded_second = provider.request_at(1).expect("第二次请求");
    let last = recorded_second.messages.last().expect("回执消息");
    assert_eq!(last.role, fluxdb_core::ModelRole::Tool);
    assert_eq!(last.text, receipt.result_json);
    assert_eq!(recorded_second, second_request);
    assert_eq!(provider.call_count(), 2);
}
