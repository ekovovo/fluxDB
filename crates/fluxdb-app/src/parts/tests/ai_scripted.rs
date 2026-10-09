// Scripted Provider / 工具回执闭环测试（设计 §37，T004）。
//
// 目标：证明"模型要工具 → 工具真实执行 → 回执回到模型"这条链路可复现，
// 且脚本不能绕过工具直接伪造结果（副作用计数与回执必须一一对应）。

use fluxdb_core::{
    AiTool, CancelFlag, DecisionProvider, DecisionRequest, DecisionResponse, FinishReason,
    GenerationProvider, ModelCapabilities, ModelEvent, ModelMessage, ModelRequest, ModelRole,
    ToolCallRequest, ToolExecutionContext, ToolRisk,
};

/// 用一个最小运行时跑异步 Provider/Tool（不引入真实网络）。
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(future)
}

fn request_with(messages: Vec<ModelMessage>) -> ModelRequest {
    ModelRequest {
        model: "scripted-model".into(),
        messages,
        tools: Vec::new(),
        max_output_tokens: Some(64),
    }
}

#[test]
fn scripted_round_trip_executes_real_tool_and_feeds_receipt_back() {
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            // 第 1 次推理：要求调用工具。
            vec![ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: "call-1".into(),
                tool_id: "db.query".into(),
                arguments_json: r#"{"sql":"select 1"}"#.into(),
            })],
            // 第 2 次推理：基于回执给出结论。
            vec![
                ModelEvent::TextDelta("查询完成".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ],
    );
    let tool = RecordingTool::new("db.query", ToolRisk::Read).with_result(r#"{"rows":1}"#);
    let cancel = CancelFlag::new();

    // 第 1 次：模型请求工具。
    let first = block_on(provider.stream(request_with(Vec::new()), cancel.clone())).expect("第一次推理");
    let ToolCallRequest { provider_call_id, tool_id, arguments_json } = match first.as_slice() {
        [ModelEvent::ToolCallRequested(call)] => call.clone(),
        other => panic!("第一次推理应只请求工具，实际 {other:?}"),
    };

    // 真实执行工具（脚本不参与），拿到不可变回执。
    let receipt = block_on(tool.execute(
        ToolExecutionContext {
            session_id: "s-1".into(),
            turn_id: "t-1".into(),
            iteration: 1,
            call_id: provider_call_id.clone(),
            progress: None,
        cancel: fluxdb_core::CancelFlag::new(),
        },
        arguments_json.clone(),
    ))
    .expect("工具执行");
    assert!(receipt.is_success());
    assert_eq!(receipt.call_id, provider_call_id);
    assert_eq!(tool.call_count(), 1, "工具必须被真实执行一次");
    assert_eq!(
        tool.calls(),
        vec![(provider_call_id.clone(), arguments_json.clone())]
    );

    // 把回执作为 tool 消息回填给模型，第 2 次推理。
    let second = block_on(provider.stream(
        request_with(vec![
            ModelMessage::new(ModelRole::User, "统计订单"),
            ModelMessage::new(ModelRole::Assistant, format!("调用 {tool_id}")),
            ModelMessage::new(ModelRole::Tool, receipt.result_json.clone()),
        ]),
        cancel,
    ))
    .expect("第二次推理");
    assert!(matches!(second.last(), Some(ModelEvent::Finished(FinishReason::Stop))));

    // 第 2 次请求里必须带真实回执载荷（而不是脚本自己编的结果）。
    let second_request = provider.request_at(1).expect("第二次请求");
    let last = second_request.messages.last().expect("回执消息");
    assert_eq!(last.role, ModelRole::Tool);
    assert_eq!(last.text, receipt.result_json);
    assert_eq!(provider.call_count(), 2);
}

#[test]
fn scripted_provider_returns_cancelled_when_flag_pre_set() {
    let provider = ScriptedGenerationProvider::text_only("不应出现");
    let cancel = CancelFlag::new();
    cancel.request();
    let events = block_on(provider.stream(request_with(Vec::new()), cancel)).expect("取消路径");
    assert_eq!(events, vec![ModelEvent::Finished(FinishReason::Cancelled)]);
}

#[test]
fn scripted_decision_provider_records_request_and_returns_fixed_choice() {
    let provider = ScriptedDecisionProvider::new(DecisionResponse {
        chosen: Some("model-b".into()),
        scores: vec![("model-a".into(), 0.2), ("model-b".into(), 0.8)],
        reason: Some("更便宜且满足能力".into()),
    });
    let response = block_on(provider.decide(
        DecisionRequest {
            model: "jev".into(),
            candidates: vec!["model-a".into(), "model-b".into()],
            prompt: "选择模型".into(),
        },
        CancelFlag::new(),
    ))
    .expect("决策");
    assert_eq!(response.chosen.as_deref(), Some("model-b"));
    assert_eq!(provider.call_count(), 1);
}
