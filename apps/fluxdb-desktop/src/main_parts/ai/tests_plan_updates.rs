// 真实 SSE 工具调用 → 生产执行链 → 同一张计划卡；闸门确保先观察到中间状态。
#[gpui_kit::test]
async fn plan_updates_keep_one_card_at_its_original_position(cx: &mut TestAppContext) {
    use std::io::{Read, Write};
    use std::sync::mpsc;
    cx.executor().allow_parking();
    for theme in [ThemeMode::Light, ThemeMode::Dark] {
        let fixture = Fixture::seeded();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        fixture
            .storage
            .with_ai_connection(|conn| {
                conn.execute("UPDATE ai_model_profiles SET endpoint=?1", [&endpoint])
                    .unwrap();
                Ok(())
            })
            .unwrap();
        let (release, gate) = mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            for request_index in 0..3 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(8)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = socket.read(&mut buf).unwrap();
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
                            break;
                        }
                    }
                }
                if request_index == 1 {
                    gate.recv_timeout(std::time::Duration::from_secs(8))
                        .expect("页面须先显示中间计划");
                }
                let payload = if request_index < 2 {
                    let args = serde_json::json!({
                        "title": "分析数据",
                        "steps": [
                            {"title": "查看结构", "status": "done"},
                            {"title": "汇总结果", "status": if request_index == 0 { "in_progress" } else { "done" }}
                        ]
                    }).to_string();
                    serde_json::json!({"choices":[{
                        "delta":{"tool_calls":[{"index":0,"id":format!("plan-{request_index}"),
                            "function":{"name":"plan_update","arguments":args}}]},
                        "finish_reason":"tool_calls"
                    }]})
                } else {
                    serde_json::json!({"choices":[{"delta":{"content":"分析完成"},"finish_reason":"stop"}]})
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {payload}\n\ndata: [DONE]\n\n").unwrap();
                socket.flush().unwrap();
            }
        });
        let (view, visual) = open_main(cx, &fixture, fixture.controller(), theme);
        open_panel(&view, visual);
        replace_input(&view, visual, "先制定计划，再更新完成状态");
        click_selector(visual, "ai-chat-send");
        wait_idle(&view, visual);
        wait_source(&view, visual, "first public plan", |main| {
            let Some(job) = &main.ai_chat.running else {
                return false;
            };
            main.controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .find(|message| message.id == job.assistant_message_id)
                .is_some_and(|message| message.parts.iter().any(|part| part.kind == "plan"))
        })
        .await;
        let job = view.read_with(visual, |main, _| main.ai_chat.running.clone().unwrap());
        let (card_id, position) = view.read_with(visual, |main, _| {
            let message = main
                .controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .find(|message| message.id == job.assistant_message_id)
                .unwrap();
            let position = message
                .parts
                .iter()
                .position(|part| part.kind == "plan")
                .unwrap();
            let payload: serde_json::Value =
                serde_json::from_str(&message.parts[position].payload_json).unwrap();
            assert_eq!(payload["steps"][1]["status"], "in_progress");
            (message.parts[position].id.clone(), position)
        });
        draw(visual);
        assert!(
            visual
                .debug_bounds(Box::leak(format!("ai-plan:{card_id}").into_boxed_str()))
                .is_some()
        );
        release.send(()).unwrap();
        wait_source(&view, visual, "plan replaced in place", |main| {
            main.ai_chat.running.is_none()
        })
        .await;
        server.join().unwrap();
        draw(visual);
        view.read_with(visual, |main, _| {
            let message = main
                .controller
                .state()
                .ai_conversation
                .messages
                .iter()
                .find(|message| message.id == job.assistant_message_id)
                .unwrap();
            let plans: Vec<_> = message
                .parts
                .iter()
                .enumerate()
                .filter(|(_, part)| part.kind == "plan")
                .collect();
            assert_eq!(plans.len(), 1, "更新不能追加第二张计划卡");
            assert_eq!(plans[0].0, position, "不把原计划挪到最后一次更新的位置");
            assert_eq!(plans[0].1.id, card_id, "保持同一组件身份");
            let payload: serde_json::Value =
                serde_json::from_str(&plans[0].1.payload_json).unwrap();
            assert_eq!(payload["steps"][1]["status"], "done");
        });
        assert!(
            visual
                .debug_bounds(Box::leak(format!("ai-plan:{card_id}").into_boxed_str()))
                .is_some()
        );
    }
}
