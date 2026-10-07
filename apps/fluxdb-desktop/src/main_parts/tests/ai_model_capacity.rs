// 在 main.rs 与 ai_agent_defaults.rs 同级 include；复用 B 自有隔离 fixture。
// K/M 与本地 HTTP 元数据均经生产控件交互；异步竞争批量派发真实平台事件。
#[cfg(test)]
mod ai_model_capacity_tests {
    use super::ai_agent_defaults_tests::{Fixture, click, open, replace, wait_for};
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};

    fn seeded_model(fixture: &Fixture, context: i64, output: i64) -> (AppController, String) {
        let mut controller = fixture.controller();
        let event = controller.dispatch(AppCommand::SaveAiModel(fluxdb_app::AiModelSaveInput {
            id: None,
            display_name: "B 容量无损换算模型".into(),
            provider: "openai".into(),
            model: "b-capacity-fixture".into(),
            endpoint: Some("https://capacity.example.invalid/v1".into()),
            endpoint_is_full_url: false,
            enabled: true,
            context_window_tokens: Some(context),
            max_output_tokens: Some(output),
            input_price_micros: None,
            output_price_micros: None,
            cached_input_price_micros: None,
            api_key: None,
            clear_key: false,
            expected_revision: None,
        }));
        let AppEvent::AiModelSaved { id, .. } = event else {
            panic!("容量 fixture 应经生产保存成功：{event:?}");
        };
        (controller, id)
    }

    fn capacities(view: &Entity<NavicatMain>, cx: &VisualTestContext) -> (String, String) {
        view.read_with(cx, |main, app| {
            let inputs = &main
                .ai_model_dialog
                .as_ref()
                .expect("容量弹框应打开")
                .inputs;
            (
                inputs.context_window.read(app).value().to_string(),
                inputs.max_output.read(app).value().to_string(),
            )
        })
    }

    fn units(view: &Entity<NavicatMain>, cx: &VisualTestContext) -> (String, String) {
        view.read_with(cx, |main, app| {
            let inputs = &main
                .ai_model_dialog
                .as_ref()
                .expect("模型弹框已打开")
                .inputs;
            (
                inputs
                    .context_unit
                    .read(app)
                    .selected_value()
                    .cloned()
                    .expect("上下文单位应已选中"),
                inputs
                    .output_unit
                    .read(app)
                    .selected_value()
                    .cloned()
                    .expect("输出单位应已选中"),
            )
        })
    }

    fn select_unit(cx: &mut VisualTestContext, selector: &'static str, direction: &'static str) {
        click(cx, selector);
        // gpui-component 0.6.0 List 绑定 up/down/enter，打开 Select 后焦点交给真实列表。
        cx.simulate_keystrokes(direction);
        cx.simulate_keystrokes("enter");
    }

    fn assert_saved(
        fixture: &Fixture,
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        id: &str,
        context: i64,
        output: i64,
    ) {
        let stored = fixture
            .storage
            .with_ai_connection(|conn| fluxdb_storage::ai_platform::models::load_model(conn, id))
            .expect("独立读取已存模型");
        assert_eq!(stored.context_window_tokens, Some(context));
        assert_eq!(stored.max_output_tokens, Some(output));
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_models.len(), 1, "编辑不能新增模型行");
        assert_eq!(snapshot.ai_models[0].id, id);
        assert_eq!(
            snapshot.ai_models[0].context_window_tokens,
            stored.context_window_tokens
        );
        assert_eq!(
            snapshot.ai_models[0].max_output_tokens,
            stored.max_output_tokens
        );
        assert_eq!(snapshot.ai_models[0].revision, stored.revision);
        // 即时重新打开真实编辑控件，确认保存后无需手动刷新即得到正确 K 展示。
        click(cx, "ai-model-edit");
        assert!(view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        assert_eq!(
            units(view, cx),
            ("K".into(), "K".into()),
            "重开两个单位均默认 K"
        );
        println!(
            "capacity evidence: id={id}, context={context}, output={output}, revision={}, snapshot=1",
            stored.revision
        );
    }

    #[gpui::test]
    fn ai_ui_model_capacity_saved_tokens_default_k_roundtrip_is_lossless(cx: &mut TestAppContext) {
        let fixture = Fixture::new("capacity-roundtrip");
        let (controller, id) = seeded_model(&fixture, 128_001, 8_192);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        click(cx, "ai-model-edit");
        assert_eq!(
            capacities(&view, cx),
            ("128.001".into(), "8.192".into()),
            "默认 K 必须按十进制 1000，无损展示已有整数 tokens"
        );
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        assert_saved(&fixture, &view, cx, &id, 128_001, 8_192);
        assert_eq!(capacities(&view, cx), ("128.001".into(), "8.192".into()));
    }

    #[gpui::test]
    fn ai_ui_model_capacity_default_k_input_saves_integer_tokens(cx: &mut TestAppContext) {
        let fixture = Fixture::new("capacity-k-input");
        let (controller, id) = seeded_model(&fixture, 128_000, 8_000);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        click(cx, "ai-model-edit");
        // 换算可以表达 1 token，但保存不削减原 DDL 的窗口 1024 / 输出 256 下限。
        replace(cx, "ai-field:上下文窗口（Token）", "1.025");
        replace(cx, "ai-field:最大输出（Token）", "0.257");
        click(cx, "ai-model-save");
        assert!(
            !view.read_with(cx, |main, _| main.ai_model_dialog.is_some()),
            "整 token 的 K 小数必须可以保存；实际错误={:?}，输入={:?}",
            view.read_with(cx, |main, _| main
                .ai_model_dialog
                .as_ref()
                .and_then(|dialog| dialog.validation_error.clone())),
            capacities(&view, cx)
        );
        assert_saved(&fixture, &view, cx, &id, 1_025, 257);
        assert_eq!(capacities(&view, cx), ("1.025".into(), "0.257".into()));
    }

    #[gpui::test]
    fn ai_ui_model_capacity_subtoken_and_overflow_reject_without_data_loss(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("capacity-invalid");
        let (controller, id) = seeded_model(&fixture, 128_001, 8_192);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        let before = fixture
            .storage
            .with_ai_connection(|conn| fluxdb_storage::ai_platform::models::load_model(conn, &id))
            .unwrap();
        click(cx, "ai-model-edit");
        for (invalid, output) in [
            ("0.0001", "8.192"),
            ("9223372036854775.808", "8.192"),
            ("-1", "8.192"),
            ("NaN", "8.192"),
            ("inf", "8.192"),
            ("1.023", "0.256"),     // 小于现有窗口下限 1024，另一个字段合法。
            ("128.001", "0.255"),   // 小于现有输出下限 256。
            ("128.001", "128.002"), // 超过已知窗口，不能保存。
        ] {
            replace(cx, "ai-field:上下文窗口（Token）", invalid);
            replace(cx, "ai-field:最大输出（Token）", output);
            click(cx, "ai-model-save");
            let (error, draft) = view.read_with(cx, |main, app| {
                let dialog = main.ai_model_dialog.as_ref().expect("无效值必须保留弹框");
                (
                    dialog.validation_error.clone(),
                    dialog.inputs.context_window.read(app).value().to_string(),
                )
            });
            assert!(
                error.as_deref().is_some_and(|message| !message.is_empty()),
                "必须给出可见校验反馈：{invalid}"
            );
            assert_eq!(draft, invalid, "失败必须保留用户输入");
            assert_eq!(capacities(&view, cx).1, output, "拒绝后也须保留输出草稿");
            let stored = fixture
                .storage
                .with_ai_connection(|conn| {
                    fluxdb_storage::ai_platform::models::load_model(conn, &id)
                })
                .unwrap();
            assert_eq!(stored.context_window_tokens, before.context_window_tokens);
            assert_eq!(stored.max_output_tokens, before.max_output_tokens);
            assert_eq!(stored.revision, before.revision, "无效值不得触发持久化更新");
            let snapshot =
                view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
            assert_eq!(snapshot.ai_models.len(), 1);
            assert_eq!(
                snapshot.ai_models[0].context_window_tokens,
                before.context_window_tokens
            );
            assert_eq!(snapshot.ai_models[0].revision, before.revision);
        }
    }

    #[gpui::test]
    fn ai_ui_model_capacity_new_empty_values_remain_optional(cx: &mut TestAppContext) {
        let fixture = Fixture::new("capacity-empty-new");
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        // 与既有 ai_ui_ 用例同一 1920x1080 主视口的实际「新建模型」命中点。
        cx.simulate_click(point(px(1840.), px(180.)), Modifiers::default());
        assert_eq!(capacities(&view, cx), (String::new(), String::new()));
        replace(cx, "ai-field:显示名称", "B 空容量模型");
        replace(cx, "ai-model-id-input", "b-empty-capacity");
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_models.len(), 1);
        let model = &snapshot.ai_models[0];
        assert_eq!(model.context_window_tokens, None);
        assert_eq!(model.max_output_tokens, None);
        let stored = fixture
            .storage
            .with_ai_connection(|conn| {
                fluxdb_storage::ai_platform::models::load_model(conn, &model.id)
            })
            .unwrap();
        assert_eq!(stored.context_window_tokens, None);
        assert_eq!(stored.max_output_tokens, None);
    }

    #[gpui::test]
    fn ai_ui_model_capacity_saved_k_to_m_save_reopen_k_is_lossless(cx: &mut TestAppContext) {
        let fixture = Fixture::new("capacity-unit-roundtrip");
        let (controller, id) = seeded_model(&fixture, 128_001, 8_192);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        click(cx, "ai-model-edit");
        assert_eq!(units(&view, cx), ("K".into(), "K".into()));
        assert_eq!(capacities(&view, cx), ("128.001".into(), "8.192".into()));
        select_unit(cx, "ai-capacity-unit:上下文窗口（Token）", "down");
        assert_eq!(units(&view, cx), ("M".into(), "K".into()));
        assert_eq!(
            capacities(&view, cx),
            ("0.128001".into(), "8.192".into()),
            "上下文切换不能串改输出字段"
        );
        select_unit(cx, "ai-capacity-unit:最大输出（Token）", "down");
        assert_eq!(units(&view, cx), ("M".into(), "M".into()));
        assert_eq!(
            capacities(&view, cx),
            ("0.128001".into(), "0.008192".into())
        );
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        assert_saved(&fixture, &view, cx, &id, 128_001, 8_192);
        assert_eq!(capacities(&view, cx), ("128.001".into(), "8.192".into()));
    }

    #[gpui::test]
    fn ai_ui_model_capacity_manual_k_m_switches_and_m_input_save_losslessly(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("capacity-manual-unit");
        let (controller, id) = seeded_model(&fixture, 128_000, 8_000);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        click(cx, "ai-model-edit");
        replace(cx, "ai-field:上下文窗口（Token）", "1.001");
        replace(cx, "ai-field:最大输出（Token）", "0.001");
        for _ in 0..2 {
            select_unit(cx, "ai-capacity-unit:上下文窗口（Token）", "down");
            select_unit(cx, "ai-capacity-unit:最大输出（Token）", "down");
            assert_eq!(units(&view, cx), ("M".into(), "M".into()));
            assert_eq!(
                capacities(&view, cx),
                ("0.001001".into(), "0.000001".into()),
                "手填值不能丢失到 0 或按 1024 换算"
            );
            select_unit(cx, "ai-capacity-unit:上下文窗口（Token）", "up");
            select_unit(cx, "ai-capacity-unit:最大输出（Token）", "up");
            assert_eq!(units(&view, cx), ("K".into(), "K".into()));
            assert_eq!(capacities(&view, cx), ("1.001".into(), "0.001".into()));
        }
        select_unit(cx, "ai-capacity-unit:上下文窗口（Token）", "down");
        select_unit(cx, "ai-capacity-unit:最大输出（Token）", "down");
        replace(cx, "ai-field:上下文窗口（Token）", "1.000001");
        replace(cx, "ai-field:最大输出（Token）", "0.008193");
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        assert_saved(&fixture, &view, cx, &id, 1_000_001, 8_193);
        assert_eq!(capacities(&view, cx), ("1000.001".into(), "8.193".into()));
    }

    #[gpui::test]
    fn ai_ui_model_capacity_invalid_k_unit_change_preserves_value_and_previous_unit(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("capacity-invalid-switch");
        let (controller, id) = seeded_model(&fixture, 128_001, 8_192);
        let (view, cx) = open(cx, &fixture, controller, SettingsPanelSection::AiModels);
        click(cx, "ai-model-edit");
        replace(cx, "ai-field:上下文窗口（Token）", "0.0001");
        select_unit(cx, "ai-capacity-unit:上下文窗口（Token）", "down");
        assert_eq!(
            units(&view, cx),
            ("K".into(), "K".into()),
            "非法草稿不得错误地变成新单位数值"
        );
        assert_eq!(capacities(&view, cx), ("0.0001".into(), "8.192".into()));
        let error = view.read_with(cx, |main, _| {
            main.ai_model_dialog
                .as_ref()
                .unwrap()
                .validation_error
                .clone()
        });
        assert!(error.as_deref().is_some_and(|message| !message.is_empty()));
        let stored = fixture
            .storage
            .with_ai_connection(|conn| fluxdb_storage::ai_platform::models::load_model(conn, &id))
            .unwrap();
        assert_eq!(stored.context_window_tokens, Some(128_001));
        assert_eq!(stored.max_output_tokens, Some(8_192));
    }

    struct CatalogServer {
        endpoint: String,
        result: std::sync::mpsc::Receiver<Result<String, String>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl CatalogServer {
        fn new(body: &'static str) -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::Ordering;
            let listener =
                std::net::TcpListener::bind(("127.0.0.1", 0)).expect("隔离本地 HTTP fixture");
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopped = stop.clone();
            let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let request_started = armed.clone();
            let (tx, result) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                let response = (|| -> Result<String, String> {
                    // deadline 从实际 fetch 前开始，不能把前置手填/渲染耗时算作网络超时。
                    let mut deadline = None;
                    let mut stream = loop {
                        if request_started.load(Ordering::Relaxed) && deadline.is_none() {
                            deadline =
                                Some(std::time::Instant::now() + std::time::Duration::from_secs(20));
                        }
                        if stopped.load(Ordering::Relaxed) {
                            return Err("fixture 已关闭".into());
                        }
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::TimedOut
                                        | std::io::ErrorKind::Interrupted
                                ) && deadline
                                    .is_none_or(|limit| std::time::Instant::now() < limit) =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(5))
                            }
                            Err(error) => return Err(format!("接受模型目录请求失败：{error}")),
                        }
                    };
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .map_err(|e| e.to_string())?;
                    stream
                        .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                        .map_err(|e| e.to_string())?;
                    // 并行测试负载下客户端可能晚于 1s 才写请求头：读超时重试到总 deadline。
                    let mut request = Vec::new();
                    let mut chunk = [0; 2048];
                    let read_deadline =
                        std::time::Instant::now() + std::time::Duration::from_secs(20);
                    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) => return Err("请求头为空".into()),
                            Ok(n) => {
                                if request.len() + n > 16 * 1024 {
                                    return Err("请求头超过 fixture 上限".into());
                                }
                                request.extend_from_slice(&chunk[..n]);
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::TimedOut
                                        | std::io::ErrorKind::Interrupted
                                ) =>
                            {
                                if std::time::Instant::now() >= read_deadline {
                                    return Err(format!("读取模型目录请求超时：{error}"));
                                }
                            }
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                    // 证据只保留请求行，不保留 Authorization 或任何凭据载荷。
                    let first_line = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    println!("metadata HTTP received: {first_line}, endpoint=isolated-loopback");
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|e| e.to_string())?;
                    Ok(first_line)
                })();
                if let Err(error) = &response {
                    eprintln!("metadata HTTP fixture error: {error}");
                }
                let _ = tx.send(response);
            });
            Self {
                endpoint,
                result,
                stop,
                armed,
                thread: Some(thread),
            }
        }

        fn assert_request(&self) {
            let line = self
                .result
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("生产 FetchAiModels 必须实际请求本地 fixture")
                .expect("本地 HTTP 请求成功");
            assert_eq!(line, "GET /v1/models HTTP/1.1");
            println!("metadata HTTP evidence: {line}, endpoint=isolated-loopback");
        }
    }

    impl Drop for CatalogServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("HTTP fixture 线程正常退出");
            }
        }
    }

    const CATALOG_BODY: &str = r#"{"data":[{"id":"b-catalog-model","context_window_tokens":128001,"max_output_tokens":8192}]}"#;

    fn open_catalog_dialog(
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        server: &CatalogServer,
    ) {
        cx.simulate_click(point(px(1840.), px(180.)), Modifiers::default());
        replace(cx, "ai-field:显示名称", "B 目录响应模型");
        replace(cx, "ai-model-endpoint-input", &server.endpoint);
        // 精确命中真实掩码 Input，不点眼睛/标签，也不绕过交互给 state 塞凭据。
        replace(cx, "ai-secret-input", "sk-b-local-catalog-not-real");
        assert_eq!(
            view.read_with(cx, |main, app| main
                .ai_model_dialog
                .as_ref()
                .unwrap()
                .inputs
                .api_key
                .read(app)
                .value()
                .to_string()),
            "sk-b-local-catalog-not-real",
            "必须实际命中 API Key 输入，不能给孤立字段塞值"
        );
    }

    fn choose_catalog_model(view: &Entity<NavicatMain>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
        let bounds = cx
            .debug_bounds("ai-model-options-overlay")
            .expect("真实模型候选浮层已打开");
        // fixture 仅一条无分组模型：面板下部是唯一可点击选项，避开顶部搜索框。
        cx.simulate_click(
            point(bounds.center().x, bounds.bottom() - px(18.)),
            Modifiers::default(),
        );
        assert_eq!(
            view.read_with(cx, |main, app| main
                .ai_model_dialog
                .as_ref()
                .unwrap()
                .inputs
                .model_key
                .read(app)
                .value()
                .to_string()),
            "b-catalog-model"
        );
        assert!(!view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().model_options_open
        }));
    }

    // simulate_click/input 会自动 drain executor，无法制造“响应前切换”的竞态。
    // 这里派发完全相同的官方平台鼠标/键盘事件，但批量结束前不 drain；不调用 UI 处理函数。
    fn pending_click_at(cx: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
        cx.update(|window, app| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    position,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                }),
                app,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                    position,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 1,
                }),
                app,
            );
        });
    }

    fn pending_click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, app| {
            let _ = window.draw(app).clear(app);
        });
        let position = cx
            .debug_bounds(selector)
            .expect("批量交互目标真实可见")
            .center();
        pending_click_at(cx, position);
    }

    fn pending_keys(cx: &mut VisualTestContext, keys: &str) {
        cx.update(|window, app| {
            for key in keys.split(' ') {
                window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app);
            }
        });
    }

    fn pending_replace(cx: &mut VisualTestContext, selector: &'static str, text: &str) {
        pending_click(cx, selector);
        pending_keys(
            cx,
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
        );
        cx.update(|window, app| {
            for key in text.split("") {
                window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app);
            }
        });
    }

    fn trace_catalog(view: &Entity<NavicatMain>, cx: &VisualTestContext, phase: &str) {
        view.read_with(cx, |main, app| {
            if let Some(dialog) = &main.ai_model_dialog {
                println!("catalog {phase}: generation={}, fetching={}, provider={:?}, endpoint={:?}, options={}, capabilities={}",
                    dialog.fetch_generation, dialog.fetching_models,
                    dialog.inputs.provider.read(app).selected_value(),
                    dialog.inputs.endpoint.read(app).value(),
                    dialog.model_options.len(), dialog.model_capabilities.len());
            } else { println!("catalog {phase}: dialog=closed"); }
        });
    }

    fn begin_catalog_fetch(
        view: &Entity<NavicatMain>,
        cx: &mut VisualTestContext,
        server: &CatalogServer,
    ) {
        trace_catalog(view, cx, "before-click");
        server
            .armed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        pending_click(cx, "ai-model-fetch");
        trace_catalog(view, cx, "immediate-after-click");
        assert!(
            view.read_with(cx, |main, _| main
                .ai_model_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.fetching_models)),
            "必须经实际 fetch 按钮点击启动 loading，不能以 condition 超时冒充按钮命中证据"
        );
    }

    #[gpui::test]
    async fn ai_ui_model_capacity_http_fetch_selection_fills_empty_and_saves(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("catalog-filled");
        let server = CatalogServer::new(CATALOG_BODY);
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        open_catalog_dialog(&view, cx, &server);
        begin_catalog_fetch(&view, cx, &server);
        cx.run_until_parked();
        trace_catalog(&view, cx, "after-drain");
        wait_for(&view, cx, |main| {
            main.ai_model_dialog
                .as_ref()
                .is_some_and(|dialog| !dialog.fetching_models && !dialog.model_options.is_empty())
        })
        .await;
        server.assert_request();
        assert_eq!(
            capacities(&view, cx),
            (String::new(), String::new()),
            "未选模型不能随便套用候选容量"
        );
        choose_catalog_model(&view, cx);
        assert_eq!(capacities(&view, cx), ("128.001".into(), "8.192".into()));
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_eq!(snapshot.ai_models.len(), 1);
        assert_eq!(snapshot.ai_models[0].model, "b-catalog-model");
        assert_saved(
            &fixture,
            &view,
            cx,
            &snapshot.ai_models[0].id,
            128_001,
            8_192,
        );
        assert_eq!(capacities(&view, cx), ("128.001".into(), "8.192".into()));
    }

    #[gpui::test]
    async fn ai_ui_model_capacity_http_missing_output_preserves_handwritten_fields(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("catalog-missing-field");
        let server = CatalogServer::new(
            r#"{"data":[{"id":"b-catalog-model","context_window_tokens":128001}]}"#,
        );
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        open_catalog_dialog(&view, cx, &server);
        replace(cx, "ai-field:上下文窗口（Token）", "256.001");
        replace(cx, "ai-field:最大输出（Token）", "4.097");
        begin_catalog_fetch(&view, cx, &server);
        cx.run_until_parked();
        trace_catalog(&view, cx, "after-drain");
        wait_for(&view, cx, |main| {
            main.ai_model_dialog
                .as_ref()
                .is_some_and(|dialog| !dialog.fetching_models && !dialog.model_options.is_empty())
        })
        .await;
        server.assert_request();
        choose_catalog_model(&view, cx);
        assert_eq!(
            capacities(&view, cx),
            ("256.001".into(), "4.097".into()),
            "已手填上下文优先，缺失输出元数据不清空手填"
        );
        click(cx, "ai-model-save");
        assert!(!view.read_with(cx, |main, _| main.ai_model_dialog.is_some()));
        let snapshot = view.read_with(cx, |main, _| render_state_snapshot(main.controller.state()));
        assert_saved(
            &fixture,
            &view,
            cx,
            &snapshot.ai_models[0].id,
            256_001,
            4_097,
        );
    }

    #[gpui::test]
    async fn ai_ui_model_capacity_http_scheduled_result_does_not_overwrite_new_input(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("catalog-changed-input");
        let server = CatalogServer::new(CATALOG_BODY);
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        open_catalog_dialog(&view, cx, &server);
        replace(cx, "ai-model-id-input", "b-catalog-model");
        begin_catalog_fetch(&view, cx, &server);
        assert!(view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().fetching_models
        }));
        pending_replace(cx, "ai-field:上下文窗口（Token）", "777.777");
        pending_replace(cx, "ai-field:最大输出（Token）", "2.001");
        assert_eq!(capacities(&view, cx), ("777.777".into(), "2.001".into()));
        cx.run_until_parked();
        wait_for(&view, cx, |main| {
            main.ai_model_dialog.as_ref().is_some_and(|dialog| {
                !dialog.fetching_models && !dialog.model_capabilities.is_empty()
            })
        })
        .await;
        server.assert_request();
        assert_eq!(
            capacities(&view, cx),
            ("777.777".into(), "2.001".into()),
            "响应到达前的实际输入修改必须优先"
        );
    }

    #[gpui::test]
    fn ai_ui_model_capacity_http_scheduled_result_cannot_fill_reopened_dialog(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("catalog-new-dialog");
        let server = CatalogServer::new(CATALOG_BODY);
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        open_catalog_dialog(&view, cx, &server);
        replace(cx, "ai-model-id-input", "b-catalog-model");
        let old_instance = view.read_with(cx, |main, _| {
            main.ai_model_dialog
                .as_ref()
                .unwrap()
                .inputs
                .model_key
                .entity_id()
        });
        begin_catalog_fetch(&view, cx, &server);
        assert!(view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().fetching_models
        }));
        // 精确命中生产取消按钮；批量交互结束前不 drain，旧响应仍晚于新弹框。
        pending_click(cx, "ai-model-cancel");
        assert!(view.read_with(cx, |main, _| main.ai_model_dialog.is_none()));
        cx.update(|window, app| {
            let _ = window.draw(app).clear(app);
        });
        pending_click_at(cx, point(px(1840.), px(180.)));
        pending_replace(cx, "ai-model-id-input", "b-new-dialog-model");
        pending_replace(cx, "ai-field:上下文窗口（Token）", "512.003");
        pending_replace(cx, "ai-field:最大输出（Token）", "16.001");
        assert_ne!(
            view.read_with(cx, |main, _| main
                .ai_model_dialog
                .as_ref()
                .unwrap()
                .inputs
                .model_key
                .entity_id()),
            old_instance
        );
        cx.run_until_parked();
        server.assert_request();
        assert_eq!(capacities(&view, cx), ("512.003".into(), "16.001".into()));
        view.read_with(cx, |main, app| {
            let dialog = main.ai_model_dialog.as_ref().unwrap();
            assert_eq!(
                dialog.inputs.model_key.read(app).value(),
                "b-new-dialog-model"
            );
            assert!(dialog.model_options.is_empty());
            assert!(dialog.model_capabilities.is_empty());
            assert!(!dialog.model_options_open && !dialog.fetching_models);
        });
    }

    #[gpui::test]
    fn ai_ui_model_capacity_http_scheduled_result_cannot_fill_changed_provider(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new("catalog-new-provider");
        let server = CatalogServer::new(CATALOG_BODY);
        let (view, cx) = open(
            cx,
            &fixture,
            fixture.controller(),
            SettingsPanelSection::AiModels,
        );
        open_catalog_dialog(&view, cx, &server);
        replace(cx, "ai-model-id-input", "b-catalog-model");
        begin_catalog_fetch(&view, cx, &server);
        let old_generation = view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().fetch_generation
        });
        assert!(view.read_with(cx, |main, _| {
            main.ai_model_dialog.as_ref().unwrap().fetching_models
        }));
        cx.update(|window, app| {
            let _ = window.draw(app).clear(app);
        });
        let name = cx.debug_bounds("ai-field:显示名称").unwrap();
        // Provider 没有 debug_selector，按相邻名称输入的真实布局定位 Select。
        pending_click_at(cx, point(name.center().x, name.bottom() + px(50.)));
        pending_keys(cx, "down enter");
        pending_replace(cx, "ai-field:上下文窗口（Token）", "256.001");
        view.read_with(cx, |main, app| {
            let dialog = main.ai_model_dialog.as_ref().unwrap();
            assert_eq!(
                dialog
                    .inputs
                    .provider
                    .read(app)
                    .selected_value()
                    .map(String::as_str),
                Some("OpenAI Responses")
            );
            assert!(
                dialog.fetch_generation > old_generation,
                "真实 Provider 修改必须使旧请求失效"
            );
        });
        cx.run_until_parked();
        server.assert_request();
        assert_eq!(capacities(&view, cx), ("256.001".into(), String::new()));
        view.read_with(cx, |main, _| {
            let dialog = main.ai_model_dialog.as_ref().unwrap();
            assert!(dialog.model_options.is_empty() && dialog.model_capabilities.is_empty());
            assert!(!dialog.fetching_models);
        });
    }
}
