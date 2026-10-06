// AI 设置职责拆分；由 ai.rs include! 引入，保持原有作用域。

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
        .debug_selector(|| format!("ai-agent-tab:{label}"))
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
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if let Some(editor) = this.ai_agent_editor.as_mut() {
                    editor.tab = value;
                }
                cx.notify();
                cx.stop_propagation();
            }),
        )
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
        .child(
            div()
                .debug_selector(|| "ai-agent-prompt".into())
                .child(Input::new(input)),
        )
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
        .child(ai_agent_tab_button(
            "基本",
            AiAgentTab::Basic,
            editor.tab,
            colors,
            cx,
        ))
        .child(ai_agent_tab_button(
            "模型与提示词",
            AiAgentTab::Model,
            editor.tab,
            colors,
            cx,
        ))
        .child(ai_agent_tab_button(
            "工具与内置能力",
            AiAgentTab::Tools,
            editor.tab,
            colors,
            cx,
        ));

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

            let mut col = div().flex().flex_col().gap_3().child(
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
            .into_any_element()
        }
        AiAgentTab::Tools => {
            let mut list = div().flex().flex_col().gap_1();
            for (id, group, desc, write) in AI_TOOL_CATALOG {
                let id_owned = id.to_string();
                let checked = editor.selected_tools.contains(*id);
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
                            if *write {
                                colors.warning
                            } else {
                                colors.success
                            },
                            colors,
                        ))
                        .child(
                            div()
                                .w(px(60.))
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(group.to_string()),
                        )
                        .child(
                            Switch::new(SharedString::from(format!("ai-agent-tool:{id}")))
                                .checked(checked)
                                .on_click(cx.listener(move |this, checked, _, cx| {
                                    if let Some(editor) = this.ai_agent_editor.as_mut() {
                                        if *checked {
                                            editor.selected_tools.insert(id_owned.clone());
                                        } else {
                                            editor.selected_tools.remove(&id_owned);
                                        }
                                    }
                                    cx.notify();
                                })),
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
                .into_any_element()
        }
    };

    let is_new = editor.agent_id.is_none();
    // 校验/保存失败（含 CAS 冲突）：内联红条，编辑器保持打开、草稿不丢（T014 验收）。
    let error_banner = div().when_some(editor.error.clone(), |this, message| {
        this.rounded(colors.radius)
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(colors.danger.alpha(0.12))
            .child(app_icon(AppIcon::CircleSlash, 14., colors.danger))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.danger)
                    .child(message),
            )
    });
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(tabs)
        .child(body)
        .child(error_banner)
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
                        .debug_selector(|| "ai-agent-save".into())
                        .child(if is_new {
                            "创建 Agent"
                        } else {
                            "保存 Agent"
                        })
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
    let selected_id = panel.agent_editor.as_ref().and_then(|e| e.agent_id.clone());

    let list_rows: Vec<gpui::AnyElement> =
        if state.ai_agents.is_empty() {
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
                        .bg(if selected {
                            colors.hover
                        } else {
                            colors.panel_bg
                        })
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
                        .child(div().text_size(px(11.)).text_color(colors.muted).child(
                            if a.enabled {
                                "已启用".to_string()
                            } else {
                                "未启用".to_string()
                            },
                        ))
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
                "暂无会话记录。",
                colors,
            )),
    )
}
