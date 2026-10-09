// 面板控件复用 gpui-component Input/ResizablePanel。仅宿主 GUI 状态，业务在 app。
struct AiChatUi {
    open: bool,
    loading: bool,
    loaded: bool,
    tab_id: Option<TabId>,
    context_tab: Option<TabId>,
    mode_menu_open: bool,
    history: bool,
    dirty: bool,
    error: Option<String>,
    mode: fluxdb_storage::ai_platform::settings::PanelMode,
    drawer: f64,
    embedded: f64,
    input: Entity<TextareaState>,
    agent_select: Entity<SelectState<SearchableVec<AiChatModelOption>>>,
    agent_options: Vec<AiChatModelOption>,
    model_select: Entity<SelectState<SearchableVec<AiChatModelOption>>>,
    _model_subscription: Subscription,
    model_options: Vec<AiChatModelOption>,
    reference_open: bool,
    reference_index: usize,
    reference_scroll: gpui::ScrollHandle,
    reference_chips_scroll: gpui::ScrollHandle,
    composer_bounds: gpui::Bounds<gpui::Pixels>,
    panel_height: gpui::Pixels,
    reference_height: gpui::Pixels,
    input_max_rows: usize,
    reference_query: Option<String>,
    reference_options: Vec<fluxdb_app::AiContextReference>,
    reference_error: Option<String>,
    /// 本次 `@query` 没有匹配到任何候选（与"检索失败"区分）。
    reference_no_match: bool,
    _input_subscription: Subscription,
    scroll: gpui::ScrollHandle,
    /// 流式助手正文的 Markdown 实体：只持有"已揭示"文本，末条助手消息流式期由它渲染。
    /// 惰性创建——只有真正开始流式时才建，避免每个窗口都挂一个后台解析任务。
    stream_view: Option<Entity<gpui_component::text::TextViewState>>,
    /// 未揭示增量与揭示进度（内存态，不落盘）。
    stream: AiChatStream,
    /// 16ms 平滑揭示任务句柄；仅在有待揭示文本时存活。
    stream_reveal: Option<Task<()>>,
    pending: std::collections::VecDeque<fluxdb_app::AiConversationCommand>,
    _io: Option<Task<()>>,
    deciding_approval: bool,
    _pump: Option<Task<()>>,
    running: Option<fluxdb_app::AiConversationJob>,
    cancel: Option<fluxdb_core::CancelFlag>,
    runtime: Option<fluxdb_app::AiRuntimeHandle>,
    _runtime_quit: Subscription,
    recovery_pending: bool,
    recovery_failed: bool,
    epoch: u64,
    debug_open_pending: bool,
    follow_reply: bool,
    new_reply: bool,
    scripted_fixture: bool,
    scripted_stream_fixture: AiChatStreamFixture,
}
impl AiChatUi {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 16)
                .placeholder("随心输入")
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut NavicatMain, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.ai_chat.dirty = true;
                    // 输入变化即时重算 `@` 候选：只有存在未闭合的 `@query` 才打开下拉。
                    this.refresh_ai_reference_menu(window, cx);
                    if this.ai_chat.input.read(cx).value().rfind('@').is_none() {
                        this.ai_chat.reference_open = false;
                    }
                    cx.notify();
                }
            },
        );
        let model_select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<AiChatModelOption>::new()), None, window, cx));
        let model_subscription = cx.subscribe_in(&model_select, window, |this: &mut NavicatMain, _, event: &SelectEvent<SearchableVec<AiChatModelOption>>, window, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                let chat = &this.controller.state().ai_conversation;
                this.ai_chat_request(fluxdb_app::AiConversationCommand::SelectModel {
                    model_id: (!value.is_empty()).then(|| value.clone()),
                    expected_revision: chat.model_selection_revision,
                }, window, cx);
            }
        });
        Self {
            open: false,
            loading: false,
            loaded: false,
            tab_id: None,
            context_tab: None,
            mode_menu_open: false,
            history: false,
            dirty: false,
            error: None,
            mode: fluxdb_storage::ai_platform::settings::PanelMode::Drawer,
            drawer: 460.,
            embedded: 420.,
            input,
            agent_select: cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<AiChatModelOption>::new()), None, window, cx)),
            agent_options: Vec::new(),
            model_select,
            _model_subscription: model_subscription,
            model_options: Vec::new(),
            reference_open: false,
            reference_index: 0,
            reference_scroll: gpui::ScrollHandle::new(),
            reference_chips_scroll: gpui::ScrollHandle::new(),
            composer_bounds: gpui::Bounds::default(),
            panel_height: px(0.),
            reference_height: px(0.),
            input_max_rows: 16,
            reference_query: None,
            reference_options: Vec::new(),
            reference_error: None,
            reference_no_match: false,
            _input_subscription: sub,
            scroll: gpui::ScrollHandle::new(),
            stream_view: None,
            stream: AiChatStream::default(),
            stream_reveal: None,
            pending: std::collections::VecDeque::new(),
            _io: None,
            deciding_approval: false,
            _pump: None,
            running: None,
            cancel: None,
            runtime: None,
            _runtime_quit: cx.on_app_quit(|this, cx| {
                let runtime = this.ai_chat.runtime.take();
                if let Some(runtime) = &runtime { runtime.request_shutdown(); }
                cx.background_spawn(async move {
                    if let Some(mut runtime) = runtime {
                        if !runtime.shutdown(std::time::Duration::from_secs(2)) {
                            tracing::warn!("AI 任务未在退出期限内结束；重启时需恢复中断状态");
                        }
                    }
                })
            }),
            recovery_pending: true,
            recovery_failed: false,
            epoch: 0,
            follow_reply: true,
            new_reply: false,
            scripted_fixture: cfg!(debug_assertions)
                && std::env::var("FLUXDB_AI_E2E_SCRIPTED").is_ok_and(|v| v == "1"),
            scripted_stream_fixture: if cfg!(debug_assertions) {
                match std::env::var("FLUXDB_AI_E2E_STREAM").as_deref() {
                    Ok("fast") => AiChatStreamFixture::Fast,
                    Ok("burst") => AiChatStreamFixture::Burst,
                    Ok("error") => AiChatStreamFixture::Error,
                    _ => AiChatStreamFixture::Default,
                }
            } else { AiChatStreamFixture::Default },
            debug_open_pending: cfg!(debug_assertions)
                && std::env::var("FLUXDB_DEBUG_AI_CHAT").is_ok_and(|v| v == "1"),
        }
    }
    fn desired_width(&self) -> f64 {
        if self.mode == fluxdb_storage::ai_platform::settings::PanelMode::Drawer {
            self.drawer
        } else {
            self.embedded
        }
    }
}
