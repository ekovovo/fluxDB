// AI 设置职责拆分；由 ai.rs include! 引入，保持原有作用域。

/// 表头：模型 / 能力与上下文 / 计价 / 凭据 / 启用 / 操作（对齐六列宽度）。
fn ai_model_table_header(colors: UiColors) -> impl IntoElement {
    let head = move |label: &str, w: f32| {
        div()
            .w(px(w))
            .flex_shrink_0()
            .debug_selector(|| format!("ai-head:{label}"))
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
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(11.))
                .text_color(colors.muted)
                .child("模型"),
        )
        .child(head("能力与上下文", 150.))
        .child(head("计价（输入 / 输出）", 140.))
        .child(head("凭据", 80.))
        .child(head("启用", 48.))
        .child(head("状态 / 操作", 148.))
}

/// 大模型列表行：对照交互稿六列（模型 / 能力与上下文 / 计价 / 凭据 / 启用 / 操作）。
fn ai_model_row(
    model: &fluxdb_app::AiModelItem,
    testing: bool,
    deleting: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let this = cx.entity().downgrade();
    let id = model.id.clone();
    let edit_id = model.id.clone();
    let test_id = model.id.clone();
    let delete_id = model.id.clone();
    let delete_name = model.display_name.clone();
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
                .flex_1()
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(0.))
                .child(app_icon(
                    if decision {
                        AppIcon::Workflow
                    } else {
                        AppIcon::Bot
                    },
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
                                .truncate()
                                .child(model.display_name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .truncate()
                                .child(format!(
                            "{} · {}",
                            fluxdb_app::ModelProvider::from_stored(&model.provider).label(),
                            model.model
                        )),
                        ),
                ),
        )
        .child(
            // 列 2：能力与上下文
            div()
                .w(px(150.))
                .flex_shrink_0()
                .debug_selector(|| "ai-model-capability".into())
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
                .w(px(140.))
                .flex_shrink_0()
                .debug_selector(|| "ai-model-price".into())
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
                .w(px(80.))
                .flex_shrink_0()
                .debug_selector(|| "ai-model-credential".into())
                .flex()
                .items_start()
                .child(ai_credential_tag(&model.credential_status, colors)),
        )
        .child(
            // 列 5：启用
            div()
                .w(px(48.))
                .flex_shrink_0()
                .debug_selector(|| "ai-model-enable".into())
                .child(
                    Switch::new(SharedString::from(format!("ai-model:{id}")))
                        .disabled(deleting)
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
            // 列 6：状态和图标操作，与表头使用同一宽度。
            div()
                .w(px(148.))
                .flex_shrink_0()
                .debug_selector(|| "ai-model-actions".into())
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(ai_connection_tag(&model.connection_status, colors))
                .child(
                    Button::new(SharedString::from(format!("ai-model-test:{test_id}")))
                        .small()
                        .ghost()
                        .disabled(testing || deleting)
                        .loading(testing)
                        .cursor_pointer()
                        .icon(
                            gpui_component::Icon::default().path(app_icon_path(AppIcon::Activity)),
                        )
                        .tooltip(if testing {
                            "测试中…"
                        } else {
                            "测试连接"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_ai_model_test(test_id.clone(), cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ai-model-edit:{edit_id}")))
                        .debug_selector(|| "ai-model-edit".into())
                        .disabled(deleting)
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .icon(gpui_component::Icon::default().path(app_icon_path(AppIcon::Edit)))
                        .tooltip("编辑")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_ai_model_dialog(Some(edit_id.clone()), window, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ai-model-delete:{delete_id}")))
                        .debug_selector(|| "ai-model-delete".into())
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .disabled(testing || deleting)
                        .loading(deleting)
                        .icon(
                            gpui_component::Icon::default()
                                .path(app_icon_path(AppIcon::Trash))
                                .text_color(colors.danger),
                        )
                        .tooltip("删除模型")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_ai_model_delete(
                                delete_id.clone(),
                                delete_name.clone(),
                                revision,
                                window,
                                cx,
                            );
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
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.toggle_ai_default_menu(id);
                cx.stop_propagation();
            }),
        )
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

    let rows: Vec<gpui::AnyElement> = if panel.settings_loading {
        vec![ai_empty_state("正在加载模型列表…", colors)]
    } else if let Some(message) = panel.settings_error.as_ref() {
        // 错误态必须与「空库」区分：显示原因 + 重试，而不是伪装成空列表（设计 §39.5）。
        vec![ai_settings_error_block(message, colors, cx)]
    } else if state.ai_models.is_empty() {
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
                ai_model_row(
                    m,
                    testing,
                    panel.deleting_id.as_deref() == Some(m.id.as_str()),
                    colors,
                    cx,
                )
                .into_any_element()
            })
            .collect()
    };

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
                // 工具栏：搜索框 + 刷新。
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
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Button::new("ai-refresh-models")
                                        .small()
                                        .ghost()
                                        .disabled(panel.settings_loading)
                                        .child(if panel.settings_loading {
                                            "刷新中…"
                                        } else {
                                            "刷新"
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.reload_ai_settings(cx);
                                        })),
                                ),
                        ),
                )
                // 窄窗口：六列保持最小宽度并横向滚动，而不是把列压扁/裁切（T013 窄窗口验收）。
                .child(
                    div().w_full().overflow_x_scrollbar().child(
                        div()
                            .min_w(px(880.))
                            .flex()
                            .flex_col()
                            .child(ai_model_table_header(colors))
                            .child(div().children(rows)),
                    ),
                ),
        )
        .child(
            settings_panel_group("默认模型与决策", colors)
                .child(ai_default_model_section(state, &panel, colors, cx)),
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
        col
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
        col
    };

    div().px_4().py_3().flex().gap_4().child(left).child(right)
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
        .bg(if selected {
            colors.hover
        } else {
            colors.panel_bg
        })
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
