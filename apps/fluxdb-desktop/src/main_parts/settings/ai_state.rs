/// 模型新建/编辑弹框的输入实体（T013）。弹框打开时按需创建，不常驻内存。
///
/// Key 输入用 `mask_toggle()`：默认掩码，眼睛按钮由组件内置，不自绘输入框。
#[derive(Clone)]
struct AiModelDialogInputs {
    provider: Entity<SelectState<SearchableVec<String>>>,
    display_name: Entity<InputState>,
    model_key: Entity<InputState>,
    endpoint: Entity<InputState>,
    api_key: Entity<InputState>,
    context_window: Entity<InputState>,
    max_output: Entity<InputState>,
    price_input: Entity<InputState>,
    price_output: Entity<InputState>,
    price_cache: Entity<InputState>,
    context_unit: Entity<SelectState<SearchableVec<String>>>,
    output_unit: Entity<SelectState<SearchableVec<String>>>,
}

impl AiModelDialogInputs {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        fn input_state(
            placeholder: &'static str,
            window: &mut Window,
            cx: &mut Context<NavicatMain>,
        ) -> Entity<InputState> {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        }
        let provider_labels: Vec<String> = fluxdb_app::ModelProvider::ALL
            .iter()
            .map(|p| p.label().to_string())
            .collect();
        let provider_items = SearchableVec::new(provider_labels);
        Self {
            provider: cx.new(|cx| {
                SelectState::new(provider_items, Some(IndexPath::new(0)), window, cx)
                    .searchable(false)
            }),
            display_name: input_state("例如 主模型 · 高能力", window, cx),
            model_key: input_state("Provider 模型标识", window, cx),
            endpoint: input_state("https://…/v1", window, cx),
            // Key 默认掩码；眼睛切换由组件内置，不自绘输入框。
            api_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("输入 API Key")
                    .masked(true)
            }),
            context_unit: cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(vec!["K".into(), "M".into()]),
                    Some(IndexPath::new(0)),
                    window,
                    cx,
                )
                .searchable(false)
            }),
            output_unit: cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(vec!["K".into(), "M".into()]),
                    Some(IndexPath::new(0)),
                    window,
                    cx,
                )
                .searchable(false)
            }),
            context_window: input_state("例如 128", window, cx),
            max_output: input_state("例如 8", window, cx),
            price_input: input_state("输入计价 $/1M Token", window, cx),
            price_output: input_state("输出计价 $/1M Token", window, cx),
            price_cache: input_state("缓存读取 $/1M Token", window, cx),
        }
    }
}

impl AiModelDialog {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        Self {
            model_id: None,
            expected_revision: 0,
            inputs: AiModelDialogInputs::new(window, cx),
            enabled: true,
            endpoint_is_full_url: false,
            fetching_models: false,
            fetch_generation: 0,
            catalog_source_fingerprint: None,
            model_options: Vec::new(),
            model_options_open: false,
            original_api_key: None,
            model_capabilities: Vec::new(),
            capacity_units: [AiTokenUnit::K; 2],
            capacity_autofill: [None, None],
            subscriptions: std::rc::Rc::new(Vec::new()),
            validation_error: None,
        }
    }
}

/// Agent 编辑分区页签。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AiAgentTab {
    Basic,
    Model,
    Tools,
}

/// Agent 编辑器输入实体（列表选中或新建时按需创建，不常驻）。
#[derive(Clone)]
struct AiAgentEditorInputs {
    display_name: Entity<InputState>,
    description: Entity<InputState>,
    system_prompt: Entity<gpui_component::input::TextareaState>,
    compression_percent: Entity<InputState>,
}

impl AiAgentEditorInputs {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        let input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<NavicatMain>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };
        Self {
            display_name: input("例如 通用数据库 Agent", window, cx),
            description: input("职责描述（用于路由与用户识别）", window, cx),
            system_prompt: cx.new(|cx| {
                gpui_component::input::TextareaState::new(window, cx)
                    .placeholder("直接输入此 Agent 的系统提示词")
                    .rows(7)
            }),
            compression_percent: input("80", window, cx),
        }
    }
}

/// Agent 编辑器状态：`agent_id=None` 表示新建草稿。
#[derive(Clone)]
struct AiAgentEditorState {
    deleting: bool,
    agent_id: Option<String>,
    expected_revision: i64,
    enabled: bool,
    tab: AiAgentTab,
    selected_model_id: Option<String>,
    model_menu_open: bool,
    /// 允许的工具白名单（勾选集合）；等于「全部」时保存为空 = 不限制。
    selected_tools: std::collections::HashSet<String>,
    /// 校验/保存失败原因（有值时编辑器保留草稿并内联提示）。
    error: Option<String>,
    inputs: AiAgentEditorInputs,
}

/// 模型弹框的非输入状态。
#[derive(Clone)]
struct AiModelDialog {
    /// `None` 表示新建。
    model_id: Option<String>,
    expected_revision: i64,
    inputs: AiModelDialogInputs,
    enabled: bool,
    endpoint_is_full_url: bool,
    /// 正在拉取 Provider 模型列表（「获取模型列表」按钮的 loading/disabled）。
    fetching_models: bool,
    fetch_generation: u64,
    catalog_source_fingerprint: Option<u64>,
    /// 拉取到的模型 ID 候选；点击填入「模型 ID」输入框。
    model_options: Vec<String>,
    /// 候选列表是否展开。
    model_options_open: bool,
    /// 编辑态预填的 API Key 明文（仅存活于本次弹框内）：与输入相同表示 Keep。
    original_api_key: Option<String>,
    model_capabilities: Vec<fluxdb_app::AiDiscoveredModel>,
    capacity_units: [AiTokenUnit; 2],
    capacity_autofill: [Option<String>; 2],
    subscriptions: std::rc::Rc<Vec<Subscription>>,
    /// 弹框内联校验/保存失败原因（有值时保持弹框打开并保留草稿）。
    validation_error: Option<String>,
}
