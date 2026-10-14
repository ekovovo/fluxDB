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
            egress: true,
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
    max_iterations: Entity<InputState>,
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
            max_iterations: input("100", window, cx),
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

/// 会话重命名弹框的待提交对象。
#[derive(Clone)]
struct AiSessionRename {
    id: String,
    title: String,
    expected_revision: i64,
}

/// MCP Server Profile 编辑器（T060）：左侧列表选中或新建时创建，不常驻内存。
#[derive(Clone)]
struct AiMcpServerProfileEditorInputs {
    display_name: Entity<InputState>,
    max_inflight: Entity<InputState>,
    max_request_ms: Entity<InputState>,
    max_request_bytes: Entity<InputState>,
    max_response_bytes: Entity<InputState>,
}

impl AiMcpServerProfileEditorInputs {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        let input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<NavicatMain>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };
        Self {
            display_name: input("例如 Claude Code 本地服务", window, cx),
            max_inflight: input("4", window, cx),
            max_request_ms: input("30000", window, cx),
            max_request_bytes: input("262144", window, cx),
            max_response_bytes: input("131072", window, cx),
        }
    }
}

/// MCP Server Profile 编辑状态：`profile_id=None` 表示新建草稿。
#[derive(Clone)]
struct AiMcpServerProfileEditor {
    profile_id: Option<String>,
    expected_revision: i64,
    enabled: bool,
    /// `disabled` / `scoped` / `desktop_approval`。
    write_mode: String,
    write_mode_menu_open: bool,
    /// 允许对外公开的 MCP 工具名勾选集合；空表示不开放任何工具。
    selected_tools: std::collections::HashSet<String>,
    /// 工具白名单展开态：默认收起，只显示摘要，授权与活动区保持在首屏可见。
    tools_expanded: bool,
    /// 保存失败原因；保留草稿并内联显示。
    error: Option<String>,
    inputs: AiMcpServerProfileEditorInputs,
}

/// MCP 资源授权编辑状态（T060）：与 Profile 编辑器独立，保存时不覆盖 Profile 草稿。
#[derive(Clone)]
struct AiMcpServerGrantEditorInputs {
    database_name: Entity<InputState>,
    namespace: Entity<InputState>,
    /// 关系型：`all_objects` / `table_allowlist`（逗号分隔）。
    table_allowlist: Entity<InputState>,
    /// Redis：`key_prefixes`（逗号分隔）。
    key_prefixes: Entity<InputState>,
    /// 受限写规则 JSON（留空 = `{}`，只读授权不需要）。
    write_policy: Entity<gpui_component::input::TextareaState>,
    max_rows: Entity<InputState>,
}

impl AiMcpServerGrantEditorInputs {
    fn new(window: &mut Window, cx: &mut Context<NavicatMain>) -> Self {
        let input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<NavicatMain>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };
        Self {
            database_name: input("库名；Redis 填 DB 序号", window, cx),
            namespace: input("Schema；不用可留空", window, cx),
            table_allowlist: input("限定表，逗号分隔", window, cx),
            key_prefixes: input("Key 前缀，逗号分隔", window, cx),
            write_policy: cx.new(|cx| {
                gpui_component::input::TextareaState::new(window, cx)
                    .placeholder("{\"allowed_operations\":[\"update\"],…}")
                    .rows(3)
            }),
            max_rows: input("200", window, cx),
        }
    }
}

/// MCP 资源授权编辑状态：`grant_id=None` 表示新建草稿。
#[derive(Clone)]
struct AiMcpServerGrantEditor {
    grant_id: Option<String>,
    expected_revision: i64,
    profile_id: String,
    connection_id: i64,
    connection_label: String,
    /// `sql` / `redis`，由连接类型推导。
    resource_kind: String,
    connection_menu_open: bool,
    enabled: bool,
    allow_read: bool,
    allow_write: bool,
    /// 整库/整 DB 通行；与 `table_allowlist`/`key_prefixes` 互斥。
    all_scope: bool,
    error: Option<String>,
    inputs: AiMcpServerGrantEditorInputs,
}

/// 模型弹框的非输入状态。
#[derive(Clone)]
struct AiModelDialog {
    /// `None` 表示新建。
    model_id: Option<String>,
    expected_revision: i64,
    inputs: AiModelDialogInputs,
    enabled: bool,
    /// 数据出站许可（设计 §29/§31.1）：关闭后该模型不再接收会话数据。
    egress: bool,
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
