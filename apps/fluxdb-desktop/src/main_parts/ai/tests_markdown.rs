// 流式 Markdown：正文/用户消息以 Markdown 渲染，流式期间由平滑揭示实体增量更新。
// 与 tests.rs 同作用域，复用 Fixture / open_main / click_selector / wait_source 等 helpers。

#[gpui::test]
fn assistant_and_user_text_parts_render_as_markdown(cx: &mut TestAppContext) {
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    assert_session(&view, cx, "one");
    // 用户消息与助手正文都必须挂 Markdown 容器（稳定 part id + `:markdown` 后缀）。
    assert!(
        cx.debug_bounds("one-user-text:markdown").is_some(),
        "用户消息应以 Markdown 渲染"
    );
    assert!(
        cx.debug_bounds("one-assistant-text:markdown").is_some(),
        "助手正文应以 Markdown 渲染"
    );
    // 工具卡/引用等非正文片段不应被当成 Markdown 正文。
    assert!(
        cx.debug_bounds("one-assistant-text:markdown").is_some()
            && cx.debug_bounds("one-assistant-summary:markdown").is_none(),
        "公开思考摘要不得混进正文 Markdown 行"
    );
}

#[gpui::test]
async fn streaming_assistant_text_uses_reveal_entity_then_settles(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    let prompt = "render this as markdown while streaming";
    let job = start_scripted_send(&fixture, &view, cx, prompt);
    // 流式期：活跃消息正文由揭示实体渲染，命中固定选择器 `ai-chat-stream-text`。
    let streamed = wait_source(&view, cx, "markdown stream entity", |main| {
        main.ai_chat.running.is_some() && main.ai_chat.stream.message_id().is_some()
    })
    .await;
    assert_eq!(
        streamed.active_turn_id.as_deref(),
        Some(job.turn_id.as_str())
    );
    draw(cx);
    assert!(
        cx.debug_bounds("ai-chat-stream-text").is_some(),
        "流式正文应挂载揭示实体并按 Markdown 渲染"
    );
    // Markdown 按节拍合并已收到的文本，等待至少一次非空渲染。
    wait_source(&view, cx, "reveal produced visible text", |main| {
        main.ai_chat.running.is_none() || main.ai_chat.stream.revealed_len() > 0
    })
    .await;
    // 等待终态：清掉流缓冲，正文回到 checkpoint 投影（同一部分正文仍以 Markdown 渲染）。
    wait_source(&view, cx, "markdown terminal", |main| {
        main.ai_chat.running.is_none()
    })
    .await;
    draw(cx);
    assert!(
        view.read_with(cx, |main, _| main.ai_chat.stream.message_id().is_none()),
        "终态后应清掉流缓冲，正文由 checkpoint 渲染"
    );
    assert!(
        cx.debug_bounds("ai-chat-stream-text").is_none(),
        "终态后不再有活跃流式正文容器"
    );
    let complete = "已读取当前目标。\n以下是 SQL 解释示例，不表示已执行查询。\nSELECT COUNT(*) FROM orders;";
    let rendered = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .messages
            .iter()
            .find(|message| message.id == job.assistant_message_id)
            .and_then(|message| {
                message
                    .parts
                    .iter()
                    .find(|part| part.kind == "text")
                    .and_then(|part| serde_json::from_str::<serde_json::Value>(&part.payload_json).ok())
                    .and_then(|payload| payload.get("text").and_then(|v| v.as_str()).map(str::to_owned))
            })
            .unwrap_or_default()
    });
    assert_eq!(rendered, complete, "终态正文必须完整落盘并可见");
    cx.executor().forbid_parking();
}
