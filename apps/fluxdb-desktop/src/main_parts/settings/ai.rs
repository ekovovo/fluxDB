// 设置页「AI」分区：大模型与工具/MCP（T013/T014）。
//
// 交互以 docs/prototype/ai-agent-ui.html 为基准，控件全部复用 gpui-component；
// UI 只读 AppState 中的公开列表并派发 AppCommand，不接触 SQLite 或任何凭据。

/// 行容器：既有 `settings_row_container` 需要 `&'static str`，AI 行 ID 来自数据库，
/// 因此这里按同样的视觉规范用运行时 ID 复刻一份，不改动既有 helper 签名。
fn ai_row_container(id: &str, colors: UiColors) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("settings-ai-row:{id}")))
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
}

/// AI 设置页的运行时 UI 状态：渲染期间不能自读 `NavicatMain`，由调用方注入。
#[derive(Clone)]
struct AiSettingsPanelState {
    filter: String,
    filter_input: Entity<InputState>,
    testing_id: Option<String>,
    default_model_menu_open: bool,
    default_decision_menu_open: bool,
    tools_tab: AiToolsTab,
    disabled_tools: std::collections::HashSet<String>,
    agent_editor: Option<AiAgentEditorState>,
}

/// 通用小胶囊：浅色底 + 彩色文字。
fn ai_pill(label: &str, color: gpui::Rgba, colors: UiColors) -> impl IntoElement {
    let _ = colors;
    div()
        .px_2()
        .py(px(1.))
        .rounded(px(4.))
        .bg(color.alpha(0.14))
        .text_size(px(11.))
        .text_color(color)
        .child(label.to_string())
}

/// 凭据状态标签：四态用不同颜色区分，避免「4 态里 3 态同色」。
fn ai_credential_tag(status: &str, colors: UiColors) -> impl IntoElement {
    let color = match status {
        "已配置" => colors.success,
        "缺少凭据" => colors.warning,
        "无需凭据" => colors.muted,
        "不可用" => colors.danger,
        _ => colors.muted,
    };
    ai_pill(status, color, colors)
}

/// 生成 / 决策 能力标签。
fn ai_capability_tag(label: &str, decision: bool, colors: UiColors) -> impl IntoElement {
    let color = if decision { colors.info } else { colors.muted };
    ai_pill(label, color, colors)
}

/// 连接状态标签：连接成功(绿) / 连接失败(红) / 未连接(灰)。
fn ai_connection_tag(status: &str, colors: UiColors) -> impl IntoElement {
    let (label, color) = match status {
        "ok" => ("连接成功", colors.success),
        "failed" => ("连接失败", colors.danger),
        _ => ("未连接", colors.muted),
    };
    ai_pill(label, color, colors)
}

/// 空态行。
fn ai_empty_state(text: &str, colors: UiColors) -> gpui::AnyElement {
    div()
        .px_4()
        .py_6()
        .text_size(px(12.))
        .text_color(colors.muted)
        .child(text.to_string())
        .into_any_element()
}

/// 表头：模型 / 能力与上下文 / 计价 / 凭据 / 启用 / 操作（对齐六列宽度）。
fn ai_model_table_header(colors: UiColors) -> impl IntoElement {
    let head = move |label: &str, w: f32| {
        div()
            .w(px(w))
            .text_size(px(11.))
            .text_color(colors.muted)
            .child(label.to_string())
    };
    div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .child(head("模型", 240.))
        .child(head("能力与上下文", 200.))
        .child(head("计价（输入 / 输出）", 170.))
        .child(head("凭据", 96.))
        .child(head("启用", 60.))
        .child(div().flex_1())
}

/// 大模型列表行：对照交互稿六列（模型 / 能力与上下文 / 计价 / 凭据 / 启用 / 操作）。
fn ai_model_row(
    model: &fluxdb_app::AiModelItem,
    testing: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let this = cx.entity().downgrade();
    let id = model.id.clone();
    let edit_id = model.id.clone();
    let test_id = model.id.clone();
    let enabled = model.enabled;
    let revision = model.revision;
    let decision = model.kind == "决策";

    let tokens = |v: Option<i64>| match v {
        Some(n) if n >= 1000 => format!("{}k", n / 1000),
        Some(n) => n.to_string(),
        None => "—".into(),
    };
    let money = |micros: Option<i64>| match micros {
        Some(v) => format!("${:.2}", v as f64 / 1_000_000.0),
        None => "—".into(),
    };

    let capability_note = if decision {
        "Choice / Score · 内置 Jev".to_string()
    } else {
        format!(
            "{} ctx / {} out",
            tokens(model.context_window_tokens),
            tokens(model.max_output_tokens)
        )
    };
    let price_line = format!(
        "{} / {}",
        money(model.input_price_micros),
        money(model.output_price_micros)
    );
    let cache_line = format!("缓存读 {}", money(model.cached_input_price_micros));

    div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_2()
        .min_h(px(60.))
        .flex()
        .items_center()
        .gap_3()
        .child(
            // 列 1：模型
            div()
                .w(px(240.))
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(0.))
                .child(app_icon(
                    if decision { AppIcon::Workflow } else { AppIcon::Bot },
                    16.,
                    colors.muted,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.text)
                                .child(model.display_name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(format!("{} · {}", model.provider, model.model)),
                        ),
                ),
        )
        .child(
            // 列 2：能力与上下文
            div()
                .w(px(200.))
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .child(ai_capability_tag(&model.kind, decision, colors))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(capability_note),
                ),
        )
        .child(
            // 列 3：计价
            div()
                .w(px(170.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text)
                        .child(price_line),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(cache_line),
                ),
        )
        .child(
            // 列 4：凭据
            div()
                .w(px(96.))
                .flex()
                .items_start()
                .child(ai_credential_tag(&model.credential_status, colors)),
        )
        .child(
            // 列 5：启用
            div().w(px(60.)).child(
                Switch::new(SharedString::from(format!("ai-model:{id}")))
                    .checked(enabled)
                    .on_click(move |checked, _, cx| {
                        let _ = this.update(cx, |this, cx| {
                            let _ = this.controller.dispatch(AppCommand::SetAiModelEnabled {
                                id: id.clone(),
                                enabled: *checked,
                                expected_revision: revision,
                            });
                            cx.notify();
                        });
                    }),
            ),
        )
        .child(
            // 列 6：操作（常驻连接状态 + 测试连接 + 编辑）
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(ai_connection_tag(&model.connection_status, colors))
                .child(
                    Button::new(SharedString::from(format!("ai-model-test:{test_id}")))
                        .small()
                        .ghost()
                        .disabled(testing)
                        .child(if testing { "测试中…" } else { "测试连接" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_ai_model_test(test_id.clone(), cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ai-model-edit:{edit_id}")))
                        .small()
                        .ghost()
                        .child("编辑")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_ai_model_dialog(Some(edit_id.clone()), window, cx);
                        })),
                ),
        )
}

/// 默认模型 / 默认决策模型下拉触发条。
fn ai_default_selector_trigger(
    id: &'static str,
    current: &str,
    open: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("ai-default-trigger:{id}")))
        .h(px(34.))
        .px_3()
        .rounded(colors.radius)
        .border_1()
        .border_color(if open { colors.info } else { colors.border })
        .bg(colors.input_bg)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .cursor_pointer()
        .hover(move |style| style.bg(colors.hover))
        .child(
            div()
                .text_size(px(12.))
                .text_color(colors.text)
                .child(current.to_string()),
        )
        .child(app_icon(
            if open {
                AppIcon::ChevronUp
            } else {
                AppIcon::ChevronDown
            },
            14.,
            colors.muted,
        ))
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
            this.toggle_ai_default_menu(id);
            cx.stop_propagation();
        }))
}

/// 默认模型候选列表（内联展开，不用浮层以免定位复杂）。
fn ai_default_options(colors: UiColors) -> Div {
    div()
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border)
        .bg(colors.panel_bg)
        .overflow_hidden()
}

/// 「大模型」分区。
fn settings_ai_models_panel(
    state: &AppState,
    panel: AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let filter = panel.filter.trim().to_lowercase();
    let filtered: Vec<&fluxdb_app::AiModelItem> = state
        .ai_models
        .iter()
        .filter(|m| {
            filter.is_empty()
                || m.display_name.to_lowercase().contains(&filter)
                || m.provider.to_lowercase().contains(&filter)
                || m.model.to_lowercase().contains(&filter)
                || m.kind.to_lowercase().contains(&filter)
        })
        .collect();

    let rows: Vec<gpui::AnyElement> = if state.ai_models.is_empty() {
        vec![ai_empty_state(
            "尚未配置模型。点击右上角「新建模型」创建第一个模型档案。",
            colors,
        )]
    } else if filtered.is_empty() {
        vec![ai_empty_state("没有匹配的模型。", colors)]
    } else {
        filtered
            .iter()
            .map(|m| {
                let testing = panel.testing_id.as_deref() == Some(m.id.as_str());
                ai_model_row(m, testing, colors, cx).into_any_element()
            })
            .collect()
    };

    let model_count = state.ai_models.len();
    let model_total_note = format!("{model_count} 个模型 · 密钥不回显");

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group("模型档案", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .child("管理生成模型与决策模型档案"),
                        )
                        .child(
                            Button::new("ai-model-new")
                                .small()
                                .primary()
                                .child("新建模型")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_ai_model_dialog(None, window, cx);
                                })),
                        ),
                )
                // 工具栏：搜索框 + 「N 个模型 · 密钥不回显」。
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(ai_model_search_field(&panel.filter_input, colors, cx))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(model_total_note),
                        ),
                )
                .child(ai_model_table_header(colors))
                .child(div().children(rows)),
        )
        .child(
            settings_panel_group("默认模型与决策", colors)
                .child(ai_default_model_section(state, &panel, colors, cx))
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_3()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("Jev 是内置可选能力；未配置决策模型时也可运行主 Agent。"),
                ),
        )
}

/// 大模型搜索框（对照交互稿的圆角搜索条）。
fn ai_model_search_field(
    input: &Entity<InputState>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let input = input.clone();
    let has_value = !input.read(cx).value().is_empty();
    let input_for_clear = input.clone();
    div()
        .w(px(300.))
        .h(px(32.))
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border)
        .bg(colors.input_bg)
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .text_color(colors.muted)
        .child(app_icon(AppIcon::Search, 15., colors.muted))
        .child(
            Input::new(&input)
                .appearance(false)
                .focus_bordered(false)
                .w_full()
                .h_full()
                .text_size(px(12.)),
        )
        .when(has_value, |this| {
            this.child(
                div()
                    .size(px(18.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |style| style.bg(colors.hover))
                    .child(app_icon(AppIcon::Close, 12., colors.muted))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            input_for_clear.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                            this.ai_model_filter = String::new();
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            )
        })
}

/// 「默认模型与决策」：两个可展开的候选列表。
fn ai_default_model_section(
    state: &AppState,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let generation: Vec<(String, String)> = state
        .ai_models
        .iter()
        .filter(|m| m.kind == "生成")
        .map(|m| (m.id.clone(), m.display_name.clone()))
        .collect();
    let decision: Vec<(String, String)> = state
        .ai_models
        .iter()
        .filter(|m| m.kind == "决策")
        .map(|m| (m.id.clone(), m.display_name.clone()))
        .collect();

    let label_for = |options: &[(String, String)], id: &Option<String>| -> Option<String> {
        id.as_ref()
            .and_then(|id| options.iter().find(|(i, _)| i == id))
            .map(|(_, n)| n.clone())
    };

    let current_main = label_for(&generation, &state.ai_default_model_id)
        .unwrap_or_else(|| "未设置（跟随 Agent）".to_string());
    let current_decision = label_for(&decision, &state.ai_default_decision_model_id)
        .unwrap_or_else(|| "自动选择唯一合格模型".to_string());

    let revision = state.ai_runtime_revision;
    let cur_main = state.ai_default_model_id.clone();
    let cur_decision = state.ai_default_decision_model_id.clone();

    let left = {
        let mut col = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .child("默认主模型"),
            )
            .child(ai_default_selector_trigger(
                "main",
                &current_main,
                panel.default_model_menu_open,
                colors,
                cx,
            ));
        if panel.default_model_menu_open {
            let mut menu = ai_default_options(colors);
            for (id, name) in &generation {
                menu = menu.child(default_option_row(
                    "main",
                    id,
                    name,
                    cur_main.as_deref() == Some(id.as_str()),
                    colors,
                    cur_decision.clone(),
                    revision,
                    cx,
                ));
            }
            col = col.child(menu);
        }
        col.child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child("用户在会话中固定模型时，不被自动路由覆盖。"),
        )
    };

    let right = {
        let mut col = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .child("默认决策模型"),
            )
            .child(ai_default_selector_trigger(
                "decision",
                &current_decision,
                panel.default_decision_menu_open,
                colors,
                cx,
            ));
        if panel.default_decision_menu_open {
            let mut menu = ai_default_options(colors);
            for (id, name) in &decision {
                menu = menu.child(default_option_row(
                    "decision",
                    id,
                    name,
                    cur_decision.as_deref() == Some(id.as_str()),
                    colors,
                    cur_main.clone(),
                    revision,
                    cx,
                ));
            }
            col = col.child(menu);
        }
        col.child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child("Jev 为内置可选能力，未配置也可运行主 Agent。"),
        )
    };

    div()
        .px_4()
        .py_3()
        .flex()
        .gap_4()
        .child(left)
        .child(right)
}

/// 默认模型下拉里的一个候选行；选中后派发保存命令。
#[allow(clippy::too_many_arguments)]
fn default_option_row(
    which: &'static str,
    id: &str,
    name: &str,
    selected: bool,
    colors: UiColors,
    other_value: Option<String>,
    expected_revision: i64,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    let id_owned = id.to_string();
    div()
        .id(SharedString::from(format!("ai-default-opt:{which}:{id}")))
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .cursor_pointer()
        .bg(if selected { colors.hover } else { colors.panel_bg })
        .hover(move |style| style.bg(colors.hover))
        .child(
            div()
                .text_size(px(12.))
                .text_color(colors.text)
                .child(name.to_string()),
        )
        .when(selected, |this| {
            this.child(app_icon(AppIcon::Check, 14., colors.info))
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                let (default_model_id, default_decision_model_id) = if which == "main" {
                    (Some(id_owned.clone()), other_value.clone())
                } else {
                    (other_value.clone(), Some(id_owned.clone()))
                };
                let _ = this.controller.dispatch(AppCommand::SaveAiDefaultModels {
                    default_model_id,
                    default_decision_model_id,
                    expected_revision,
                });
                this.ai_default_model_menu_open = false;
                this.ai_default_decision_menu_open = false;
                cx.notify();
                cx.stop_propagation();
            }),
        )
}

/// 工具与 MCP 页签。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AiToolsTab {
    Tools,
    Mcp,
    Plugins,
    Audit,
}

/// 13 个标准工具目录（设计 §20.5 / 交互稿）。
const AI_TOOL_CATALOG: &[(&str, &str, &str, bool)] = &[
    ("connections.list", "公共", "列出已授权连接资源", false),
    ("db.search_objects", "SQL", "渐进搜索表、视图与字段", false),
    ("db.describe_object", "SQL", "读取真实列、索引和关系", false),
    ("db.query", "SQL", "单条有界只读查询，驱动绑定参数", false),
    ("db.explain", "SQL", "不执行式查询计划", false),
    ("db.execute", "SQL", "修改专用入口，范围授权与审批", true),
    ("redis.scan", "Redis", "渐进扫描，不全量 KEYS", false),
    ("redis.inspect", "Redis", "按类型有界预览值与 TTL", false),
    ("redis.execute", "Redis", "受控命令，检查所有 Key 与风险", true),
    ("redis.server_info", "Redis", "受限版本与诊断摘要", false),
    ("result.read_page", "公共", "读取已取得的结果快照", false),
    ("operation.status", "公共", "查询当前 caller 的操作状态", false),
    ("operation.cancel", "公共", "取消自身操作，不伪造服务端撤销", false),
];

/// 页签按钮：选中态下边框高亮。
fn ai_tools_tab_button(
    label: &'static str,
    value: AiToolsTab,
    active: AiToolsTab,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    let is_active = value == active;
    div()
        .id(SharedString::from(format!("ai-tools-tab:{label}")))
        .px_3()
        .pt_1()
        .pb_2()
        .cursor_pointer()
        .border_b_2()
        .border_color(if is_active {
            colors.info
        } else {
            colors.info.alpha(0.0)
        })
        .text_size(px(12.))
        .text_color(if is_active { colors.text } else { colors.muted })
        .font_weight(if is_active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .hover(move |style| style.text_color(colors.text))
        .child(label.to_string())
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
            this.ai_tools_tab = value;
            cx.notify();
            cx.stop_propagation();
        }))
}

/// 页签条。
fn ai_tools_tab_bar(active: AiToolsTab, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    div()
        .border_b_1()
        .border_color(colors.border_soft)
        .flex()
        .items_center()
        .gap_1()
        .child(ai_tools_tab_button("工具", AiToolsTab::Tools, active, colors, cx))
        .child(ai_tools_tab_button("本地 MCP", AiToolsTab::Mcp, active, colors, cx))
        .child(ai_tools_tab_button("内置插件", AiToolsTab::Plugins, active, colors, cx))
        .child(ai_tools_tab_button("安全审计", AiToolsTab::Audit, active, colors, cx))
}

/// 表头单元（固定宽）。
fn ai_table_head(label: &str, w: f32, colors: UiColors) -> Div {
    div()
        .w(px(w))
        .text_size(px(11.))
        .text_color(colors.muted)
        .child(label.to_string())
}

/// 「工具」页签：13 个标准工具表。
fn ai_tools_catalog_panel(
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let header = div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .child(ai_table_head("工具", 210., colors))
        .child(ai_table_head("职责", 320., colors))
        .child(ai_table_head("风险", 120., colors))
        .child(ai_table_head("分组", 80., colors))
        .child(ai_table_head("启用", 60., colors))
        .child(div().flex_1());
    let mut rows = div().flex().flex_col();
    for (id, group, desc, write) in AI_TOOL_CATALOG {
        let enabled = !panel.disabled_tools.contains(*id);
        rows = rows.child(ai_tool_row(id, group, desc, *write, enabled, colors, cx));
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group("工具目录", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .child("内部 Agent 与本地 MCP 共用工具、授权和不可变执行回执。"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(format!("{} 个标准工具", AI_TOOL_CATALOG.len())),
                        ),
                )
                .child(header)
                .child(rows),
        )
        .child(
            settings_panel_group("启用说明", colors).child(
                div()
                    .px_4()
                    .py_3()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("启用工具只是让它可被发现。每次调用仍重新检查目标、参数、连接权限、安全审计与审批范围。"),
            ),
        )
}

/// 单个工具行。
#[allow(clippy::too_many_arguments)]
fn ai_tool_row(
    id: &str,
    group: &str,
    desc: &str,
    write: bool,
    enabled: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let id_for_switch = id.to_string();
    div()
        .border_t_1()
        .border_color(colors.border_soft)
        .py_2()
        .min_h(px(46.))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(210.))
                .text_size(px(12.))
                .text_color(colors.text)
                .child(id.to_string()),
        )
        .child(
            div()
                .w(px(320.))
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(desc.to_string()),
        )
        .child(
            div().w(px(120.)).flex().items_start().child(if write {
                ai_pill("修改 / 授权", colors.warning, colors).into_any_element()
            } else {
                ai_pill("读取 / 有界", colors.success, colors).into_any_element()
            }),
        )
        .child(
            div()
                .w(px(80.))
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(group.to_string()),
        )
        .child(
            div().w(px(60.)).child(
                Switch::new(SharedString::from(format!("ai-tool:{id}")))
                    .checked(enabled)
                    .on_click(cx.listener(move |this, checked, _, cx| {
                        if *checked {
                            this.ai_disabled_tools.remove(&id_for_switch);
                        } else {
                            this.ai_disabled_tools.insert(id_for_switch.clone());
                        }
                        cx.notify();
                    })),
            ),
        )
        .child(div().flex_1())
}

/// 「内置插件」页签：内置能力行 + 内核约束标签。
fn ai_plugins_panel(state: &AppState, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    let body: Vec<gpui::AnyElement> = if state.ai_plugin_instances.is_empty() {
        vec![ai_empty_state(
            "暂无内置插件实例。Jev 等内置能力随应用注册，无需安装包。",
            colors,
        )]
    } else {
        state
            .ai_plugin_instances
            .iter()
            .map(|p| {
                let this = cx.entity().downgrade();
                let id = p.id.clone();
                let enabled = p.enabled;
                let revision = p.revision;
                let mandatory = p.mandatory;
                ai_row_container(&p.id, colors)
                    .min_h(px(52.))
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(colors.text)
                                    .child(p.display_name.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(colors.muted)
                                            .child(format!("{} · 内置", p.package_key)),
                                    )
                                    .when(mandatory, |this| {
                                        this.child(ai_pill("内核约束", colors.info, colors))
                                    }),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("ai-plugin:{id}")))
                            .checked(enabled)
                            .disabled(mandatory)
                            .on_click(move |checked, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    let _ = this.controller.dispatch(
                                        AppCommand::SetAiPluginInstanceEnabled {
                                            id: id.clone(),
                                            enabled: *checked,
                                            expected_revision: revision,
                                        },
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element()
            })
            .collect()
    };
    div().flex().flex_col().gap_3().child(
        settings_panel_group("内置能力", colors)
            .child(div().children(body))
            .child(
                div()
                    .border_t_1()
                    .border_color(colors.border_soft)
                    .px_4()
                    .py_3()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("安全审计与内核执行保护不可关闭；第三方插件本期仅预留接口，未实现安装。"),
            ),
    )
}

/// 「安全审计」页签：只读记录，不能在此执行任何操作。
fn ai_audit_panel(colors: UiColors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group("安全审计", colors).child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("安全审计始终开启（只读）"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("参数或目标变化会使旧审计与审批失效；审计无法执行或保存时，操作不会继续。本页只查看内核判定，规则调整在「内置插件 / 安全审计」。"),
                    ),
            ),
        )
        .child(
            settings_panel_group("审计记录", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(ai_table_head("调用 / 来源", 260., colors))
                        .child(ai_table_head("目标", 220., colors))
                        .child(ai_table_head("判断", 100., colors))
                        .child(ai_table_head("匹配规则", 220., colors))
                        .child(div().flex_1()),
                )
                .child(ai_empty_state(
                    "暂无审计记录。真实工具调用在运行时阶段（M2）写入后在此按判定筛选查看。",
                    colors,
                )),
        )
}

/// 从连接配置里取一个用于展示的库/Schema 标签（Tcp 用 database，SQLite 用文件名）。
fn connection_database_label(config: &fluxdb_core::ConnectionConfig) -> String {
    match &config.endpoint {
        fluxdb_core::Endpoint::Tcp { database, .. } => database.clone().unwrap_or_default(),
        fluxdb_core::Endpoint::SqliteFile { path, .. } => {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        }
        fluxdb_core::Endpoint::Uri { .. } => String::new(),
    }
}

/// 「本地 MCP」页签：fluxdb-local Server 卡 + 授权范围 + 客户端验证示例。
fn ai_mcp_panel(state: &AppState, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    let _ = cx;
    let connection_rows: Vec<gpui::AnyElement> = if state.connections.is_empty() {
        vec![ai_empty_state("尚未配置连接。", colors)]
    } else {
        state
            .connections
            .iter()
            .map(|c| {
                div()
                    .border_t_1()
                    .border_color(colors.border_soft)
                    .px_4()
                    .py_2()
                    .min_h(px(48.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(colors.text)
                                    .child(c.config.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(format!(
                                        "{} · {}",
                                        database_kind_name(c.config.kind),
                                        connection_database_label(&c.config)
                                    )),
                            ),
                    )
                    .child(ai_pill("只读", colors.success, colors))
                    .into_any_element()
            })
            .collect()
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group("fluxdb-local", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(colors.text)
                                        .child("本地 Server · stdio"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(colors.muted)
                                        .child("不需要配置内部主模型或 Jev；外部 Agent（Claude Code 等）可调用。"),
                                ),
                        )
                        .child(ai_pill("13 个候选工具", colors.info, colors)),
                )
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_3()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("默认只读 · 不暴露修改工具。客户端允许调用工具不等于 fluxDB 的写入授权；Server 本体在 M2-MCP 阶段交付。"),
                ),
        )
        .child(
            settings_panel_group("已配置连接的授权范围", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(ai_table_head("连接 / 范围", 320., colors))
                        .child(ai_table_head("读写边界", 120., colors))
                        .child(div().flex_1()),
                )
                .child(div().children(connection_rows)),
        )
        .child(
            settings_panel_group("客户端验证示例", colors).child(
                div()
                    .border_t_1()
                    .border_color(colors.border_soft)
                    .px_4()
                    .py_3()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("claude -p --mcp-config ./fluxdb-local.json --strict-mcp-config --tools \"\" --allowedTools mcp__fluxdb_local__fluxdb_query \"使用授权测试库查询订单总额\""),
            ),
        )
}

/// 「工具与 MCP」分区：四页签。
fn settings_ai_tools_mcp_panel(
    state: &AppState,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let body = match panel.tools_tab {
        AiToolsTab::Tools => ai_tools_catalog_panel(panel, colors, cx),
        AiToolsTab::Mcp => ai_mcp_panel(state, colors, cx),
        AiToolsTab::Plugins => ai_plugins_panel(state, colors, cx),
        AiToolsTab::Audit => ai_audit_panel(colors),
    };
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(ai_tools_tab_bar(panel.tools_tab, colors, cx))
        .child(body)
}

// ---- 模型新建/编辑弹框 ----

/// 读取输入框文本（空串视为缺省）。
fn ai_input_text(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// 解析可选正整数；空串返回 None，非法值返回 Err。
fn ai_input_optional_i64(input: &Entity<InputState>, cx: &App) -> Result<Option<i64>, String> {
    let raw = ai_input_text(input, cx);
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<i64>()
        .map(Some)
        .map_err(|_| format!("`{raw}` 不是有效数字"))
}

/// 把「美元 / 百万 Token」换算成 micro-USD（1 USD = 1_000_000 micro）。
fn ai_input_price_micros(input: &Entity<InputState>, cx: &App) -> Result<Option<i64>, String> {
    let raw = ai_input_text(input, cx);
    if raw.is_empty() {
        return Ok(None);
    }
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("`{raw}` 不是有效计价"))?;
    if !value.is_finite() || value < 0.0 {
        return Err("计价不能为负".into());
    }
    Ok(Some((value * 1_000_000.0).round() as i64))
}

/// 模型表单的一个「标签 + 输入」行（无说明文字，视觉以交互稿为准）。
fn ai_field(label: &str, input: &Entity<InputState>, colors: UiColors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.text)
                .child(label.to_string()),
        )
        .child(Input::new(input).text_size(px(13.)))
}

/// API Key 字段：默认掩码 + 组件内置眼睛按钮。
fn ai_secret_field(label: &str, input: &Entity<InputState>, colors: UiColors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.text)
                .child(label.to_string()),
        )
        // mask_toggle 必须加在渲染的 Input 上（组件级开关），只设 state 不会出现眼睛。
        .child(Input::new(input).mask_toggle().text_size(px(13.)))
}

/// Provider 下拉字段。
fn ai_select_field(
    label: &str,
    select: &Entity<SelectState<SearchableVec<String>>>,
    colors: UiColors,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.text)
                .child(label.to_string()),
        )
        .child(Select::new(select).w_full().h(px(32.)))
}

use fluxdb_app::AiModelSaveInput;

/// 「模型 ID」字段：可输入，也可点「获取模型列表」拉取后从浮动面板选择。
///
/// 拉取需要 API Key（因此本字段排在 API Key 之后）。候选是输入框下方的卡片：
/// 顶部搜索、按模型 ID 前缀分组、悬停高亮、限高滚动（对照交互稿的候选样式）。
/// 放在弹框滚动容器内联展开而不是绝对浮层，避免被滚动视口裁切。
fn ai_model_id_field(
    dialog: &AiModelDialog,
    filter_input: &Entity<InputState>,
    filter_text: &str,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let fetching = dialog.fetching_models;
    let mut col = div()
        .relative()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(colors.text)
                        .child("模型 ID"),
                )
                .child(
                    Button::new("ai-model-fetch")
                        .small()
                        .ghost()
                        .disabled(fetching)
                        .child(if fetching { "拉取中…" } else { "获取模型列表" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.fetch_ai_model_list(cx);
                        })),
                ),
        )
        .child(Input::new(&dialog.inputs.model_key).text_size(px(13.)));
    if dialog.model_options_open && !dialog.model_options.is_empty() {
        col = col.child(ai_model_options_panel(
            dialog,
            filter_input,
            filter_text,
            colors,
            cx,
        ));
    }
    col
}

/// 模型候选浮层：搜索框 + 分组列表。
fn ai_model_options_panel(
    dialog: &AiModelDialog,
    filter_input: &Entity<InputState>,
    filter_text: &str,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let filter = filter_text.trim().to_lowercase();
    // 按「模型 ID 前缀」分组：`inclusionai/ling-3.1-flash` → inclusionai。
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for model in &dialog.model_options {
        if !filter.is_empty() && !model.to_lowercase().contains(&filter) {
            continue;
        }
        let group = model
            .split_once('/')
            .map(|(prefix, _)| prefix.to_string())
            .unwrap_or_default();
        match groups.iter_mut().find(|(key, _)| key == &group) {
            Some((_, items)) => items.push(model.clone()),
            None => groups.push((group, vec![model.clone()])),
        }
    }

    let mut list = div().flex().flex_col().py_1();
    if groups.is_empty() {
        list = list.child(
            div()
                .px_3()
                .py_2()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("没有匹配的模型"),
        );
    }
    // 只有存在带前缀分组时才给无前缀项加「其他」标题，避免平铺列表多一个多余标题。
    let show_plain_header = groups.iter().any(|(key, _)| !key.is_empty());
    for (group, items) in &groups {
        if !group.is_empty() || show_plain_header {
            list = list.child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(if group.is_empty() {
                        "其他".to_string()
                    } else {
                        group.clone()
                    }),
            );
        }
        for model in items {
            let model_owned = model.clone();
            let selected = dialog
                .inputs
                .model_key
                .read(cx)
                .value()
                .trim()
                == model_owned.as_str();
            list = list.child(
                div()
                    .id(SharedString::from(format!("ai-model-opt:{model}")))
                    .px_3()
                    .py_2()
                    .mx_1()
                    .rounded(colors.radius)
                    .cursor_pointer()
                    .bg(if selected {
                        colors.hover
                    } else {
                        colors.panel_bg.alpha(0.0)
                    })
                    .hover(move |style| style.bg(colors.hover))
                    .text_size(px(12.5))
                    .text_color(colors.text)
                    .child(model.clone())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if let Some(dialog) = this.ai_model_dialog.as_mut() {
                                dialog.inputs.model_key.update(cx, |state, cx| {
                                    state.set_value(model_owned.clone(), window, cx);
                                });
                                dialog.model_options_open = false;
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            );
        }
    }

    div()
        .mt_1()
        .w_full()
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.border)
        .bg(colors.panel_bg)
        // 面板内点击（含搜索框）不外传给弹框根，避免误关浮层。
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .shadow(vec![box_shadow(
            px(0.),
            px(8.),
            px(24.),
            px(0.),
            hsla(0., 0., 0., if colors.is_dark { 0.34 } else { 0.14 }),
        )])
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .text_color(colors.muted)
                .child(app_icon(AppIcon::Search, 14., colors.muted))
                .child(
                    Input::new(filter_input)
                        .appearance(false)
                        .focus_bordered(false)
                        .w_full()
                        .text_size(px(12.5)),
                ),
        )
        .child(
            div()
                .border_t_1()
                .border_color(colors.border_soft)
                .max_h(px(168.))
                .overflow_y_scrollbar()
                .child(list),
        )
}

/// 模型新建/编辑弹框；未打开时不渲染。/// 模型新建/编辑弹框；未打开时不渲染。
fn ai_model_dialog(
    dialog: Option<AiModelDialog>,
    filter_input: Entity<InputState>,
    filter_text: String,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Option<gpui::AnyElement> {
    let dialog = dialog.as_ref()?;
    let inputs = &dialog.inputs;
    let is_new = dialog.model_id.is_none();
    let this = cx.entity().downgrade();

    let mask = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .bg(if colors.is_dark {
            hsla(220. / 360., 0.12, 0.08, 0.54)
        } else {
            hsla(210. / 360., 0.20, 0.20, 0.16)
        })
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
            this.ai_model_dialog = None;
            cx.notify();
            cx.stop_propagation();
        }));

    let panel = div()
        .w(px(560.))
        .max_h(px(720.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.border)
        .bg(colors.panel_bg)
        .shadow(vec![box_shadow(px(0.), px(18.), px(48.), px(0.), hsla(220. / 360., 0.3, 0.1, 0.28))])
        .flex()
        .flex_col()
        .overflow_hidden()
        // 内部点击不穿透到遮罩；同时关闭模型候选浮层（点空白处收起）。
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
            if let Some(dialog) = this.ai_model_dialog.as_mut() {
                dialog.model_options_open = false;
            }
            cx.stop_propagation();
        }))
        .on_action(cx.listener(|this, _: &CancelDialog, _, cx| {
            // Esc 等同取消，不写入任何数据。
            this.ai_model_dialog = None;
            cx.notify();
            cx.stop_propagation();
        }))
        .child(
            div()
                .px_5()
                .pt_4()
                .pb_3()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if is_new { "新建模型" } else { "编辑模型" }),
                )
                .child(
                    Button::new("ai-model-close")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_model_dialog = None;
                            cx.notify();
                        })),
                ),
        )
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_5()
                .pb_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(ai_field("显示名称", &inputs.display_name, colors))
                .child(ai_select_field("Provider", &inputs.provider, colors))
                .child(ai_field("Endpoint", &inputs.endpoint, colors))
                // 模型 ID 依赖 API Key 才能拉取，因此排在 API Key 之后。
                .child(ai_secret_field("API Key", &inputs.api_key, colors))
                .child(ai_model_id_field(
                    dialog,
                    &filter_input,
                    &filter_text,
                    colors,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(ai_field(
                            "上下文窗口（Token）",
                            &inputs.context_window,
                            colors,
                        )))
                        .child(div().flex_1().child(ai_field(
                            "最大输出（Token）",
                            &inputs.max_output,
                            colors,
                        ))),
                )
                // 计价三项（输入 / 输出 / 缓存读取）固定同一行，避免换行破坏栅格。
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().min_w(px(0.)).child(ai_field(
                            "计价 · 输入 $/1M",
                            &inputs.price_input,
                            colors,
                        )))
                        .child(div().flex_1().min_w(px(0.)).child(ai_field(
                            "计价 · 输出 $/1M",
                            &inputs.price_output,
                            colors,
                        )))
                        .child(div().flex_1().min_w(px(0.)).child(ai_field(
                            "计价 · 缓存 $/1M",
                            &inputs.price_cache,
                            colors,
                        ))),
                ),
        );

    let footer = div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_5()
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("ai-model-cancel")
                        .small()
                        .ghost()
                        .child("取消")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_model_dialog = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-model-save")
                        .small()
                        .primary()
                        .child("保存模型")
                        .on_click(move |_, _, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.submit_ai_model_dialog(cx);
                            });
                        }),
                ),
        );

    Some(mask.child(panel.child(footer)).into_any_element())
}

impl NavicatMain {
    /// 切换「默认主模型 / 默认决策模型」候选列表的展开状态。
    fn toggle_ai_default_menu(&mut self, which: &str) {
        if which == "main" {
            self.ai_default_model_menu_open = !self.ai_default_model_menu_open;
            self.ai_default_decision_menu_open = false;
        } else {
            self.ai_default_decision_menu_open = !self.ai_default_decision_menu_open;
            self.ai_default_model_menu_open = false;
        }
    }

    /// 测试模型连接：真实网络 I/O 放后台线程；失败走 Alert，不写进列表。
    fn start_ai_model_test(&mut self, id: String, cx: &mut Context<Self>) {
        if self.ai_model_testing_id.is_some() {
            return;
        }
        self.ai_model_testing_id = Some(id.clone());
        cx.notify();
        let id_for_task = id.clone();
        let mut controller = self.controller.clone();
        cx.spawn(async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch(AppCommand::TestAiModel { id: id_for_task })
                })
                .await;
            let _ = cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this.ai_model_testing_id = None;
                    match event {
                        AppEvent::AiModelTested { status, .. } => {
                            if let Some(message) = status.strip_prefix("failed:") {
                                // 失败详情只在 Alert 里出现，不落在模型行上。
                                this.show_message(
                                    format!("连接失败：{message}"),
                                    AppMessageKind::Error,
                                    cx,
                                );
                            } else {
                                match status.as_str() {
                                    "ok" => this.show_message(
                                        "连接成功",
                                        AppMessageKind::Success,
                                        cx,
                                    ),
                                    "missing_key" => this.show_message(
                                        "缺少 API Key，无法测试连接",
                                        AppMessageKind::Warning,
                                        cx,
                                    ),
                                    "unsupported" => this.show_message(
                                        "该 Provider 不支持测试连接",
                                        AppMessageKind::Warning,
                                        cx,
                                    ),
                                    _ => {}
                                }
                            }
                            // 连接状态已落库，重新加载以刷新行内状态标签。
                            let _ = this.controller.dispatch(AppCommand::RefreshAiSettings);
                        }
                        AppEvent::AiSettingsError(message) => {
                            this.show_message(message, AppMessageKind::Error, cx);
                        }
                        _ => {}
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 拉取 Provider 的模型列表：真实网络 I/O 走后台线程，结果填入候选下拉。
    ///
    /// API Key 只从当前弹框内存读取并随命令一次性使用，不落库、不写日志。
    fn fetch_ai_model_list(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_model_dialog.as_ref() else {
            return;
        };
        let provider = dialog
            .inputs
            .provider
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_else(|| "openai".to_string());
        let endpoint = dialog.inputs.endpoint.read(cx).value().trim().to_string();
        let api_key = dialog.inputs.api_key.read(cx).value().to_string();
        if api_key.trim().is_empty() {
            self.show_message(
                "请先填写 API Key 再拉取模型列表",
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        if let Some(dialog) = self.ai_model_dialog.as_mut() {
            dialog.fetching_models = true;
            dialog.model_options_open = false;
        }
        cx.notify();
        let mut controller = self.controller.clone();
        cx.spawn(async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch(AppCommand::FetchAiModels {
                        provider,
                        endpoint: (!endpoint.is_empty()).then_some(endpoint),
                        api_key,
                    })
                })
                .await;
            let _ = cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    let mut error = None;
                    if let Some(dialog) = this.ai_model_dialog.as_mut() {
                        dialog.fetching_models = false;
                        match &event {
                            AppEvent::AiModelsFetched { models } => {
                                dialog.model_options = models.clone();
                                dialog.model_options_open = true;
                            }
                            AppEvent::AiModelsFetchFailed { message } => {
                                error = Some(message.clone());
                            }
                            _ => {}
                        }
                    }
                    if let Some(message) = error {
                        this.show_message(&message, AppMessageKind::Error, cx);
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 打开 Agent 编辑器：`Some(id)` 选中已有 Agent，`None` 新建草稿。
    fn open_ai_agent_editor(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = id.as_ref().and_then(|id| {
            self.controller
                .state()
                .ai_agents
                .iter()
                .find(|a| &a.id == id)
                .cloned()
        });
        if id.is_some() && existing.is_none() {
            self.show_message("Agent 不存在，可能已被删除", AppMessageKind::Warning, cx);
            return;
        }
        let inputs = AiAgentEditorInputs::new(window, cx);
        let mut editor = AiAgentEditorState {
            agent_id: None,
            expected_revision: 0,
            enabled: true,
            tab: AiAgentTab::Basic,
            selected_model_id: None,
            model_menu_open: false,
            inputs,
        };
        if let Some(agent) = &existing {
            editor.agent_id = Some(agent.id.clone());
            editor.expected_revision = agent.revision;
            editor.enabled = agent.enabled;
            editor.selected_model_id = agent.model_profile_id.clone();
            let set = |input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<Self>| {
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            };
            set(&editor.inputs.display_name, agent.display_name.clone(), window, cx);
            set(&editor.inputs.description, agent.description.clone(), window, cx);
            set(&editor.inputs.system_prompt, agent.system_prompt.clone(), window, cx);
        }
        self.ai_agent_editor = Some(editor);
        cx.notify();
    }

    /// 提交 Agent 编辑器：先做 UI 校验，再派发保存命令。
    fn submit_ai_agent_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.ai_agent_editor.clone() else {
            return;
        };
        let display_name = ai_input_text(&editor.inputs.display_name, cx);
        if display_name.is_empty() {
            self.show_message("名称不能为空", AppMessageKind::Warning, cx);
            return;
        }
        let input = fluxdb_app::AiAgentSaveInput {
            id: editor.agent_id.clone(),
            display_name,
            description: ai_input_text(&editor.inputs.description, cx),
            system_prompt: ai_input_text(&editor.inputs.system_prompt, cx),
            model_profile_id: editor.selected_model_id.clone(),
            enabled: editor.enabled,
            expected_revision: editor.expected_revision,
        };
        match self.controller.dispatch(AppCommand::SaveAiAgent(input)) {
            AppEvent::AiAgentSaved { id, revision } => {
                if let Some(state) = self.ai_agent_editor.as_mut() {
                    state.agent_id = Some(id);
                    state.expected_revision = revision;
                }
                self.show_message("Agent 已保存", AppMessageKind::Success, cx);
            }
            AppEvent::AiSettingsError(message) => {
                self.show_message(message, AppMessageKind::Error, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// 打开模型新建/编辑弹框：`None` 表示新建。
    fn open_ai_model_dialog(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = id.as_ref().and_then(|id| {
            self.controller
                .state()
                .ai_models
                .iter()
                .find(|m| &m.id == id)
                .cloned()
        });
        if id.is_some() && existing.is_none() {
            self.show_message("模型不存在，可能已被删除", AppMessageKind::Warning, cx);
            return;
        }
        let mut dialog = AiModelDialog::new(window, cx);
        // 每次打开都清掉上一次的候选列表与搜索词，避免串到别的模型。
        dialog.model_options.clear();
        dialog.model_options_open = false;
        self.ai_model_dialog_filter = String::new();
        self.ai_model_dialog_filter_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        if let Some(model) = &existing {
            dialog.model_id = Some(model.id.clone());
            dialog.expected_revision = model.revision;
            dialog.enabled = model.enabled;
            fill_ai_model_inputs(&dialog.inputs, model, window, cx);
            // 编辑态把已存 Key 预填进掩码输入框（显示黑点，点眼睛看真实值）。
            // 明文只活在这个 InputState 里：不进事件、不进快照、不落日志。
            if model.credential_status == "已配置" {
                if let Ok(secret) = self.controller.reveal_ai_model_secret(&model.id) {
                    dialog.original_api_key = Some(secret.clone());
                    dialog.inputs.api_key.update(cx, |state, cx| {
                        state.set_value(secret, window, cx);
                    });
                }
            }
        }
        // 调试用（仅 debug）：预置候选，便于截图核对浮层样式。
        if cfg!(debug_assertions) {
            if let Ok(raw) = std::env::var("FLUXDB_DEBUG_AI_MODEL_OPTIONS") {
                if raw == "1" {
                    dialog.model_options = vec![
                        "Space Bunny Alpha".into(),
                        "deepseek-v4.1-flash".into(),
                        "deepseek-v4.1-flash:free".into(),
                        "glm-5.3-flash".into(),
                        "qwen3.8-flash".into(),
                        "inclusionai/ling-3.1-flash".into(),
                        "deepseek/deepseek-v4.1-flash-fast".into(),
                    ];
                    dialog.model_options_open = true;
                }
            }
        }
        self.ai_model_dialog = Some(dialog);
        cx.notify();
    }

    /// 提交模型弹框：先在 UI 侧做校验，再派发专用保存命令。
    ///
    /// 明文 Key 只进这一条命令；成功后弹框立即关闭。
    fn submit_ai_model_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_model_dialog.clone() else {
            return;
        };
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let display_name = read(&dialog.inputs.display_name);
        let model = read(&dialog.inputs.model_key);
        if display_name.is_empty() || model.is_empty() {
            self.show_message("请填写显示名称与模型标识", AppMessageKind::Warning, cx);
            return;
        }
        let context_window = match ai_input_optional_i64(&dialog.inputs.context_window, cx) {
            Ok(v) => v,
            Err(msg) => return self.show_message(&msg, AppMessageKind::Error, cx),
        };
        let max_output = match ai_input_optional_i64(&dialog.inputs.max_output, cx) {
            Ok(v) => v,
            Err(msg) => return self.show_message(&msg, AppMessageKind::Error, cx),
        };
        let (price_in, price_out, price_cache) = match (
            ai_input_price_micros(&dialog.inputs.price_input, cx),
            ai_input_price_micros(&dialog.inputs.price_output, cx),
            ai_input_price_micros(&dialog.inputs.price_cache, cx),
        ) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            (Err(msg), _, _) | (_, Err(msg), _) | (_, _, Err(msg)) => {
                return self.show_message(&msg, AppMessageKind::Error, cx);
            }
        };
        let endpoint = read(&dialog.inputs.endpoint);
        // Key 输入是掩码字段：用户没有实际输入时为空，表示保留原凭据。
        let api_key_value = read(&dialog.inputs.api_key);
        // 与预填值相同 → 视为 Keep（不递增 secret_revision）；只有真正改过才 Set。
        let keep_existing_key = match &dialog.original_api_key {
            Some(original) => original == &api_key_value,
            None => api_key_value.is_empty(),
        };
        let api_key = (!keep_existing_key && !api_key_value.is_empty()).then_some(api_key_value.clone());
        let input = AiModelSaveInput {
            id: dialog.model_id.clone(),
            display_name,
            // Provider 是模型类型与协议的唯一来源（不再单独选「模型用途」）。
            provider: dialog
                .inputs
                .provider
                .read(cx)
                .selected_value()
                .cloned()
                .unwrap_or_else(|| fluxdb_app::ModelProvider::OpenAi.label().to_string()),
            model,
            endpoint: (!endpoint.is_empty()).then_some(endpoint),
            enabled: dialog.enabled,
            context_window_tokens: context_window,
            max_output_tokens: max_output,
            input_price_micros: price_in,
            output_price_micros: price_out,
            cached_input_price_micros: price_cache,
            api_key,
            clear_key: false,
            expected_revision: dialog.model_id.as_ref().map(|_| dialog.expected_revision),
        };
        self.ai_model_dialog = None;
        if let AppEvent::AiSettingsError(message) = self.controller.dispatch(AppCommand::SaveAiModel(input)) {
            self.show_message(&message, AppMessageKind::Error, cx);
        }
        cx.notify();
    }
}

/// 用已有模型档案回填弹框输入。
///
/// 只回填非敏感字段；API Key 不回显（掩码框留空表示保留原凭据）。
fn fill_ai_model_inputs(
    inputs: &AiModelDialogInputs,
    model: &fluxdb_app::AiModelItem,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) {
    // 用普通函数而不是闭包：闭包会同时可变借用 window，无法重复调用。
    fn set(
        input: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }
    // Provider 下拉回填：编辑时必须显示已存 Provider，否则保存会把类型改掉。
    let expected = fluxdb_app::ModelProvider::from_stored(&model.provider).label();
    let current = inputs
        .provider
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_default();
    if current != expected {
        inputs.provider.update(cx, |select, cx| {
            select.set_selected_value(&expected.to_string(), window, cx);
        });
    }
    set(&inputs.display_name, model.display_name.clone(), window, cx);
    set(&inputs.model_key, model.model.clone(), window, cx);
    // Endpoint 必须回填：否则「保存了再打开就没了」（曾漏掉这一行）。
    set(
        &inputs.endpoint,
        model.endpoint.clone().unwrap_or_default(),
        window,
        cx,
    );
    set(
        &inputs.context_window,
        model
            .context_window_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        window,
        cx,
    );
    set(
        &inputs.max_output,
        model
            .max_output_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        window,
        cx,
    );
    let price = |micros: Option<i64>| {
        micros
            .map(|v| format!("{}", v as f64 / 1_000_000.0))
            .unwrap_or_default()
    };
    set(
        &inputs.price_input,
        price(model.input_price_micros),
        window,
        cx,
    );
    set(
        &inputs.price_output,
        price(model.output_price_micros),
        window,
        cx,
    );
    set(
        &inputs.price_cache,
        price(model.cached_input_price_micros),
        window,
        cx,
    );
}

// ---- Agent 配置 ----

/// Agent 编辑页签按钮。
fn ai_agent_tab_button(
    label: &'static str,
    value: AiAgentTab,
    active: AiAgentTab,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    let is_active = value == active;
    div()
        .id(SharedString::from(format!("ai-agent-tab:{label}")))
        .px_3()
        .pt_1()
        .pb_2()
        .cursor_pointer()
        .border_b_2()
        .border_color(if is_active {
            colors.info
        } else {
            colors.info.alpha(0.0)
        })
        .text_size(px(12.))
        .text_color(if is_active { colors.text } else { colors.muted })
        .font_weight(if is_active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .hover(move |style| style.text_color(colors.text))
        .child(label.to_string())
        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
            if let Some(editor) = this.ai_agent_editor.as_mut() {
                editor.tab = value;
            }
            cx.notify();
            cx.stop_propagation();
        }))
}

/// 多行提示词输入（保留换行）。
fn ai_prompt_field(label: &str, input: &Entity<InputState>, colors: UiColors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.text)
                .child(label.to_string()),
        )
        .child(Input::new(input))
}

/// Agent 编辑器：页签 + 表单 + 底部保存栏。
fn ai_agent_editor_form(
    state: &AppState,
    editor: &AiAgentEditorState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let tabs = div()
        .border_b_1()
        .border_color(colors.border_soft)
        .flex()
        .items_center()
        .gap_1()
        .child(ai_agent_tab_button("基本", AiAgentTab::Basic, editor.tab, colors, cx))
        .child(ai_agent_tab_button("模型与提示词", AiAgentTab::Model, editor.tab, colors, cx))
        .child(ai_agent_tab_button("工具与内置能力", AiAgentTab::Tools, editor.tab, colors, cx));

    let body: gpui::AnyElement = match editor.tab {
        AiAgentTab::Basic => {
            let enabled = editor.enabled;
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(ai_field("名称", &editor.inputs.display_name, colors))
                .child(ai_field("职责描述", &editor.inputs.description, colors))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("描述用于路由与用户识别，不是硬流程约束。"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child("启用 Agent"),
                        )
                        .child(
                            Switch::new("ai-agent-enabled").checked(enabled).on_click(
                                cx.listener(move |this, checked, _, cx| {
                                    if let Some(editor) = this.ai_agent_editor.as_mut() {
                                        editor.enabled = *checked;
                                    }
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("执行目标自动跟随当前界面（SQL 编辑器、表数据、Redis Key）；切换标签页不改变已启动任务的目标。"),
                )
                .into_any_element()
        }
        AiAgentTab::Model => {
            let generation: Vec<(String, String)> = state
                .ai_models
                .iter()
                .filter(|m| m.kind == "生成")
                .map(|m| (m.id.clone(), m.display_name.clone()))
                .collect();
            let current_label = editor
                .selected_model_id
                .as_ref()
                .and_then(|id| generation.iter().find(|(i, _)| i == id))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| "跟随默认主模型".to_string());

            let mut col = div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.text)
                                .child("默认生成模型"),
                        )
                        .child(ai_default_selector_trigger(
                            "agent-model",
                            &current_label,
                            editor.model_menu_open,
                            colors,
                            cx,
                        )),
                );
            if editor.model_menu_open {
                let mut menu = ai_default_options(colors);
                for (id, name) in &generation {
                    let id_owned = id.clone();
                    let selected = editor.selected_model_id.as_deref() == Some(id.as_str());
                    menu = menu.child(
                        div()
                            .id(SharedString::from(format!("ai-agent-model-opt:{id}")))
                            .h(px(30.))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .cursor_pointer()
                            .bg(if selected { colors.hover } else { colors.panel_bg })
                            .hover(move |style| style.bg(colors.hover))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(colors.text)
                                    .child(name.clone()),
                            )
                            .when(selected, |this| {
                                this.child(app_icon(AppIcon::Check, 14., colors.info))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(editor) = this.ai_agent_editor.as_mut() {
                                        editor.selected_model_id = Some(id_owned.clone());
                                        editor.model_menu_open = false;
                                    }
                                    cx.notify();
                                    cx.stop_propagation();
                                }),
                            ),
                    );
                }
                col = col.child(menu);
            }
            col.child(ai_prompt_field(
                "提示词 · 直接输入",
                &editor.inputs.system_prompt,
                colors,
            ))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("保存在此 Agent 中，没有模板/多版本。编辑只影响下一个 Turn；权限、预算与工具校验仍由内核保证。"),
            )
            .into_any_element()
        }
        AiAgentTab::Tools => {
            let mut list = div().flex().flex_col().gap_1();
            for (id, group, desc, write) in AI_TOOL_CATALOG {
                list = list.child(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(200.))
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child(id.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(desc.to_string()),
                        )
                        .child(ai_pill(
                            if *write { "写入" } else { "读取" },
                            if *write { colors.warning } else { colors.success },
                            colors,
                        ))
                        .child(
                            div()
                                .w(px(60.))
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(group.to_string()),
                        ),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child("此 Agent 可用的工具与内置能力"),
                )
                .child(list)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("按 Agent 收窄工具/内置能力与精确绑定在运行时阶段（M2）接入；当前显示内核默认全集。"),
                )
                .into_any_element()
        }
    };

    let is_new = editor.agent_id.is_none();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(tabs)
        .child(body)
        .child(
            div()
                .border_t_1()
                .border_color(colors.border_soft)
                .pt_3()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("ai-agent-cancel")
                        .small()
                        .ghost()
                        .child("取消")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_agent_editor = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-agent-save")
                        .small()
                        .primary()
                        .child(if is_new { "创建 Agent" } else { "保存 Agent" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_ai_agent_editor(cx);
                        })),
                ),
        )
}

/// 「Agent 配置」分区：左侧列表 + 右侧编辑器。
fn settings_ai_agents_panel(
    state: &AppState,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected_id = panel
        .agent_editor
        .as_ref()
        .and_then(|e| e.agent_id.clone());

    let list_rows: Vec<gpui::AnyElement> = if state.ai_agents.is_empty() {
        vec![ai_empty_state("暂无 Agent。点击「新建」创建。", colors)]
    } else {
        state
            .ai_agents
            .iter()
            .map(|a| {
                let id = a.id.clone();
                let selected = selected_id.as_deref() == Some(a.id.as_str());
                div()
                    .id(SharedString::from(format!("ai-agent-item:{}", a.id)))
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .bg(if selected { colors.hover } else { colors.panel_bg })
                    .hover(move |style| style.bg(colors.hover))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(a.display_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(if a.enabled {
                                "已启用".to_string()
                            } else {
                                "未启用".to_string()
                            }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.open_ai_agent_editor(Some(id.clone()), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    };

    let editor_body: gpui::AnyElement = match panel.agent_editor.as_ref() {
        None => ai_empty_state("从左侧选择一个 Agent，或点击「新建」。", colors),
        Some(editor) => ai_agent_editor_form(state, editor, colors, cx).into_any_element(),
    };

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.text)
                                .child("Agent"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child("每个 Agent 直接配置提示词、模型角色与插件能力。"),
                        ),
                )
                .child(
                    Button::new("ai-agent-new")
                        .small()
                        .primary()
                        .child("新建")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_ai_agent_editor(None, window, cx);
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .gap_4()
                .items_start()
                .child(
                    div()
                        .w(px(240.))
                        .flex_none()
                        .rounded(colors.radius)
                        .border_1()
                        .border_color(colors.border)
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .children(list_rows),
                )
                .child(div().flex_1().min_w(px(0.)).child(editor_body)),
        )
}

/// 「会话历史」分区：M1 只提供入口与说明（真实会话在运行时阶段写入）。
fn settings_ai_sessions_panel(colors: UiColors) -> Div {
    div().flex().flex_col().gap_3().child(
        settings_panel_group("会话历史", colors)
            .child(
                div()
                    .border_t_1()
                    .border_color(colors.border_soft)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(ai_table_head("会话", 300., colors))
                    .child(ai_table_head("Agent / 目标", 240., colors))
                    .child(ai_table_head("状态", 120., colors))
                    .child(div().flex_1()),
            )
            .child(ai_empty_state(
                "暂无会话记录。普通打开恢复上次查看的会话，新建才开始新的草稿；运行时阶段（M2）写入后在此列出。",
                colors,
            )),
    )
}
