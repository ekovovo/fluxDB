// 复用隔离 Fixture；只替换远端 HTTP 服务，不替换 Provider、主循环或 UI 回桥。
#[gpui_kit::test]
async fn real_sse_is_visible_in_production_view_before_response_finishes(cx: &mut TestAppContext) {
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
            let (mut socket, _) = listener.accept().unwrap();
            drop(listener);
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(8)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let read = socket.read(&mut buf).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buf[..read]);
                if let Some(end) = request.windows(4).position(|x| x == b"\r\n\r\n") {
                    let len = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
            socket.write_all("data: {\"choices\":[{\"delta\":{\"content\":\"## 真流式\\n\\n首片已到达\"}}]}\n\n".as_bytes()).unwrap();
            socket.flush().unwrap();
            gate.recv_timeout(std::time::Duration::from_secs(8))
                .expect("页面必须在尾帧前显示首片");
            socket.write_all("data: {\"choices\":[{\"delta\":{\"content\":\"，尾片已完成。\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".as_bytes()).unwrap();
        });
        let (view, visual) = open_main(cx, &fixture, fixture.controller(), theme);
        open_panel(&view, visual);
        replace_input(&view, visual, "use isolated HTTP fixture");
        click_selector(visual, "ai-chat-send");
        wait_idle(&view, visual);
        wait_source(&view, visual, "real SSE rendered before tail", |main| {
            main.ai_chat.running.is_some()
                && main.ai_chat.stream.revealed_text().contains("首片已到达")
        })
        .await;
        draw(visual);
        assert!(visual.debug_bounds("ai-chat-stream-text").is_some());
        assert!(!server.is_finished(), "远端仍被闸门挂起，页面已经显示首片");
        assert!(visual.debug_bounds("ai-chat-stop").is_some());
        assert!(visual.debug_bounds("ai-chat-send").is_none(), "生成中不能重复发送");
        let job = view.read_with(visual, |main, _| main.ai_chat.running.clone().unwrap());
        // 输入草稿不得因流式刷新被覆盖；暗色主题同时覆盖真实停止点击。
        replace_input(&view, visual, "keep my unsent draft");
        let stopped = matches!(theme, ThemeMode::Dark);
        if stopped {
            click_selector(visual, "ai-chat-stop");
        }
        release.send(()).unwrap();
        wait_source(&view, visual, "real SSE terminal", |main| {
            main.ai_chat.running.is_none()
        })
        .await;
        server.join().unwrap();
        view.read_with(visual, |main, cx| {
            assert_eq!(
                main.ai_chat.input.read(cx).value().as_str(),
                "keep my unsent draft"
            );
            assert!(main.ai_chat.stream.message_id().is_none());
        });
        let saved = fixture
            .storage
            .with_ai_connection(|conn| Ok(conversation::load(conn, Some(&job.session_id))?))
            .unwrap();
        let text = saved
            .parts
            .iter()
            .find(|p| {
                p.message_id == job.assistant_message_id && p.kind == sessions::PartKind::Text
            })
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text.payload_json).unwrap();
        assert_eq!(payload["text"], if stopped { "## 真流式\n\n首片已到达" }
            else { "## 真流式\n\n首片已到达，尾片已完成。" });
        assert_eq!(text.status, if stopped { sessions::PartStatus::Interrupted }
            else { sessions::PartStatus::Complete });
    }
}

#[test]
fn streaming_render_batches_utf8_and_reduces_large_markdown_frequency() {
    let mut stream = AiChatStream::default();
    stream.begin("message", "");
    assert_eq!(stream.render_interval().as_millis(), 50);
    let mut offset = 0;
    for _ in 0..10_000 {
        stream.push(offset, "你🙂");
        offset += "你🙂".len();
    }
    assert_eq!(stream.render_interval().as_millis(), 250);
    assert_eq!(stream.reveal_next().unwrap(), "你🙂".repeat(10_000));
    assert!(
        !stream.has_pending(),
        "一次渲染合并整个突发，不按 token 重复解析 Markdown"
    );
    assert!(stream.reveal_next().is_none());
}
