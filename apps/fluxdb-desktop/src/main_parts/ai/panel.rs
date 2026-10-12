use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

// T028 原生面板，唯一内容树共享 Input 实体，切换形式不创建运行时或重复订阅。
// 已核对 0.6.0：Sheet.resizable 未消费该属性；复用 ResizablePanelGroup 实现硬比例范围和 resize 回调。
fn ai_chat_workspace(
    workspace: gpui::AnyElement,
    this: &mut NavicatMain,
    window: &mut Window,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    if !this.ai_chat.open || this.ai_chat.mode == fluxdb_storage::ai_platform::settings::PanelMode::Tab {
        return workspace;
    }
    let viewport = f32::from(window.viewport_size().width) as f64;
    let mode = this.ai_chat.mode;
    let width = fluxdb_storage::ai_platform::settings::resolve_panel_width(
        mode,
        this.ai_chat.desired_width(),
        viewport,
    ) as f32;
    if width <= 0. {
        return workspace;
    }
    let max = (viewport * mode.max_ratio()).floor() as f32;
    let min = 320_f32.min(max);
    let weak = cx.entity().downgrade();
    let pane = ai_chat_panel(this, window, colors, cx).into_any_element();
    let group = gpui_component::h_resizable(
        if mode == fluxdb_storage::ai_platform::settings::PanelMode::Embedded {
            "ai-chat-embedded"
        } else {
            "ai-chat-drawer"
        },
    )
    .on_resize(move |state, window, cx| {
        if let Some(size) = state.read(cx).sizes().last().copied() {
            let _ = weak.update(cx, |this, cx| {
                let w = fluxdb_storage::ai_platform::settings::resolve_panel_width(
                    this.ai_chat.mode,
                    f32::from(size) as f64,
                    f32::from(window.viewport_size().width) as f64,
                );
                if this.ai_chat.mode == fluxdb_storage::ai_platform::settings::PanelMode::Drawer {
                    this.ai_chat.drawer = w
                } else {
                    this.ai_chat.embedded = w;
                }
                this.save_ai_chat_ui(window, cx);
                cx.notify();
            });
        }
    });
    if mode == fluxdb_storage::ai_platform::settings::PanelMode::Embedded {
        group
            .child(gpui_component::resizable_panel().min_h(px(0.)).child(workspace))
            .child(
                gpui_component::resizable_panel()
                    .min_h(px(0.))
                    .size(px(width))
                    .size_range(px(min)..px(max))
                    .child(pane),
            )
            .into_any_element()
    } else {
        let overlay = div()
            .size_full()
            .bg(colors.app_bg.alpha(0.14))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_ai_chat(window, cx);
                    cx.stop_propagation();
                }),
            );
        div()
            .relative()
            .size_full()
            // 与正常工作区保持相同的纵向 flex 约束；抽屉只覆盖，不参与底层布局分配。
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(workspace)
            .child(
                div().absolute().inset_0().child(
                    group
                        .child(gpui_component::resizable_panel().min_h(px(0.)).child(overlay))
                        .child(
                            gpui_component::resizable_panel()
                                .min_h(px(0.))
                                .size(px(width))
                                .size_range(px(min)..px(max))
                                .child(pane),
                        ),
                ),
            )
            .into_any_element()
    }
}
fn ai_chat_small_button(
    id: &'static str,
    icon: AppIcon,
    title: &'static str,
    colors: UiColors,
) -> gpui_component::button::Button {
    Button::new(id)
        .small()
        .ghost()
        .cursor_pointer()
        .tooltip(title)
        .child(app_icon(icon, 14., colors.muted))
}
fn ai_chat_panel(
    this: &mut NavicatMain,
    _window: &mut Window,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    this.sync_ai_chat_models(_window, cx);
    this.sync_ai_chat_agents(_window, cx);
    let mut chat = this.controller.state().ai_conversation.clone();
    if let Some(message_id) = this.ai_chat.stream.message_id() {
        // 首个分片即可挂载显示实体；临时消息仅用于渲染，不写 AppState 或数据库。
        let index = chat.messages.iter().position(|message| message.id == message_id)
            .unwrap_or_else(|| {
                chat.messages.push(fluxdb_app::AiConversationMessage {
                    id: message_id.to_owned(), role: "assistant".into(), parts: Vec::new(),
                });
                chat.messages.len() - 1
            });
        let message = &mut chat.messages[index];
        if !message.parts.iter().any(|part| part.kind == "text") {
            message.parts.push(fluxdb_app::AiConversationPart {
                id: format!("{message_id}-text"), kind: "text".into(),
                status: "streaming".into(), payload_json: r#"{"text":""}"#.into(),
            });
        }
    }
    let target = chat
        .target_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    let target_connection_id = target
        .as_ref()
        .and_then(|value| value.get("connection_id"))
        .and_then(serde_json::Value::as_i64);
    let target_connection = target_connection_id.and_then(|connection_id| {
        this.controller
            .state()
            .connections
            .iter()
            .find(|connection| connection.config.id.0 as i64 == connection_id)
    });
    let scope_primary = target_connection
        .map(|connection| connection.config.name.clone())
        .or_else(|| {
            target_connection_id
                .map(|connection_id| format!("连接 {connection_id}"))
        })
        .or_else(|| {
            target
                .as_ref()
                .and_then(|value| value.get("database"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    let scope_permission = if target.is_some() {
        let read_only = target_connection
            .map(|connection| fluxdb_app::connection_is_read_only(&connection.config))
            .unwrap_or(true);
        if read_only {
            "只读"
        } else {
            "修改需审批"
        }
    } else {
        "可解释与规划"
    };
    let scope_permission_color = if scope_permission == "修改需审批" {
        colors.warning
    } else {
        colors.success
    };
    let loading = this.ai_chat.loading;
    let history = this.ai_chat.history;
    let running = this.ai_chat.session_running(chat.session_id.as_deref());
    let error = this.ai_chat.error.clone().or(chat.error.clone());
    let ready = !loading
        && !this.ai_chat.recovery_pending
        && !this.ai_chat.recovery_failed
        && this.ai_chat._io.is_none();
    let scroll = this.ai_chat.scroll.clone();
    if this.ai_chat.follow_reply && !history && !loading {
        // Markdown 异步排版会改变内容高度；跟随模式在布局后持续锚定底部。
        scroll.scroll_to_bottom();
    }
    let mut timeline = div()
        .id("ai-chat-timeline")
        .debug_selector(|| "ai-chat-timeline".into())
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .on_scroll_wheel(cx.listener(|this, _, _, cx| {
            this.ai_chat.follow_reply = false;
            cx.notify();
        }))
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_5();
    // 活跃流式助手消息：其正文由平滑揭示实体渲染，其余消息按 checkpoint 正文渲染。
    let stream_message_id = this.ai_chat.stream.message_id().map(str::to_owned);
    let stream_view = this.ai_chat.stream_view.clone();
    if loading {
        timeline = timeline.child(
            div()
                .text_color(colors.muted)
                .text_size(px(12.))
                .child("正在加载会话…"),
        );
    } else if history {
        let agent_name = |agent_id: &str| -> String {
            this.controller
                .state()
                .ai_agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .map(|agent| agent.display_name.clone())
                .unwrap_or_else(|| agent_id.to_string())
        };
        // 列表标题：数量 + 交互说明，替代原先只有一串纯文字标题的空白列表。
        timeline = timeline.child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child(format!("{} 个会话", chat.sessions.len())),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .child("按最近更新排序 · 点击恢复该会话"),
                ),
        );
        if chat.sessions.is_empty() {
            timeline = timeline.child(
                div()
                    .py_4()
                    .text_color(colors.muted)
                    .text_size(px(12.))
                    .child("暂无会话历史"),
            );
        }
        for session in &chat.sessions {
            let id = session.id.clone();
            let current = chat.session_id.as_deref() == Some(session.id.as_str());
            let (status_label, status_color) =
                ai_session_status_label(session.last_turn_status.as_deref().unwrap_or_default(), colors);
            let meta = format!(
                "{} · {} 条消息 · {}",
                agent_name(&session.agent_id),
                session.message_count,
                ai_session_timestamp(session.updated_at_ms),
            );
            // 会话卡片：整卡就是一个 Button，视觉是卡片、交互语义是按钮，避免叠一个透明浮层。
            let id_for_debug = id.clone();
            timeline = timeline.child(
                Button::new(SharedString::from(format!("ai-session:{id}")))
                    .debug_selector(move || format!("ai-session:{id_for_debug}"))
                    .ghost()
                    .cursor_pointer()
                    .w_full()
                    .h(px(56.))
                    .px_2()
                    .justify_start()
                    .rounded(colors.radius)
                    .border_1()
                    .border_color(if current { colors.info } else { colors.border_soft })
                    .when(current, |this| this.bg(colors.hover))
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .text_size(px(12.))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(colors.text)
                                            .truncate()
                                            .child(session.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .when(current, |this| {
                                                this.child(ai_pill("当前", colors.info, colors))
                                            })
                                            .child(ai_pill(status_label, status_color, colors)),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .text_size(px(10.))
                                    .text_color(colors.muted)
                                    .truncate()
                                    .child(meta),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.save_ai_chat_ui(window, cx);
                        this.ai_chat.epoch += 1;
                        this.ai_chat.history = false;
                        this.ai_chat_request(
                            fluxdb_app::AiConversationCommand::Load {
                                session_id: Some(id.clone()),
                            },
                            window,
                            cx,
                        );
                    })),
            );
        }
    } else if chat.messages.is_empty() && error.is_none() {
        timeline = timeline
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .py_6()
                    .child(app_icon(AppIcon::Bot, 28., colors.muted))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("有什么可以帮你？"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("解释 SQL · 数据分析 · Redis 排障"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                Button::new("ai-suggestion-schema")
                                    .debug_selector(|| "ai-suggestion-schema".into())
                                    .small()
                                    .outline()
                                    .cursor_pointer()
                                    .child("解释当前表结构")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ai_chat.input.update(cx, |input, cx| {
                                            input.set_value("解释当前表结构和关键字段。", window, cx);
                                        });
                                        this.ai_chat.dirty = true;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("ai-suggestion-analysis")
                                    .debug_selector(|| "ai-suggestion-analysis".into())
                                    .small()
                                    .outline()
                                    .cursor_pointer()
                                    .child("分析这组数据")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ai_chat.input.update(cx, |input, cx| {
                                            input.set_value("分析当前数据并总结关键结论。", window, cx);
                                        });
                                        this.ai_chat.dirty = true;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1());
    } else {
        for message in &chat.messages {
            let user = message.role == "user";
            timeline = timeline.child(
                div()
                    .id(SharedString::from(format!("ai-message:{}", message.id)))
                    .debug_selector({
                        let id = message.id.clone();
                        move || format!("ai-message:{id}")
                    })
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .gap_1p5()
                    .when(user, |row| {
                        row.child(
                            div()
                                .self_end()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.muted)
                                .child(app_icon(AppIcon::Users, 13., colors.muted))
                                .child("你"),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("ai-message-content:{}", message.id)))
                            .debug_selector({
                                let id = message.id.clone();
                                move || format!("ai-message-content:{id}")
                            })
                            .test_support()
                            .when(user, |row| {
                                row.self_end()
                                    .min_w(px(0.))
                                    .max_w(gpui::relative(0.82))
                                    .px_3()
                                    .py_2()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(colors.border_soft)
                                    .bg(colors.panel_alt)
                                    .flex()
                                    .flex_col()
                                    .gap_1p5()
                            })
                            .when(!user, |row| {
                                row.w_full()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .text_size(px(11.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(colors.text)
                                            .child(app_icon(AppIcon::Bot, 13., colors.success))
                                            .child("fluxDB Agent"),
                                    )
                            })
                            .children(if user {
                                let mut children = message.parts.iter().filter(|p| p.kind == "text").map(|p| {
                                    ai_chat_text_only(p, colors).into_any_element()
                                }).collect::<Vec<_>>();
                                children.extend(message.parts.iter().filter(|part| part.kind == "context_ref").filter_map(|part| {
                                    let payload: serde_json::Value = serde_json::from_str(&part.payload_json).ok()?;
                                    let tag = ai_reference_tag(payload.get("label")?.as_str()?, payload.get("kind")?.as_str()?, colors);
                                    let id = part.id.clone();
                                    Some(div().debug_selector(move || format!("ai-message-reference:{id}")).child(tag).into_any_element())
                                }));
                                children
                            } else {
                                let active_stream =
                                    stream_message_id.as_deref() == Some(message.id.as_str());
                                ai_chat_message_parts(
                                    message,
                                    &chat,
                                    &ai_approval_context(
                                        chat.target_json.as_deref(),
                                        &this.controller.state().connections,
                                        this.ai_chat.deciding_approval,
                                    ),
                                    stream_view.as_ref().filter(|_| active_stream),
                                    active_stream.then(|| this.ai_chat.stream.revealed_text()),
                                    colors,
                                    cx,
                                )
                            }),
                    ),
            );
        }
        if running {
            timeline = timeline.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(if this.ai_chat.stream.is_finished() { "正在显示回复…" } else { "正在生成…" }),
            );
        }
    }
    let token_label = if chat.usage.requests > 0 && chat.usage.unknown_requests == chat.usage.requests {
        "Token 用量未知".to_string()
    } else if chat.usage.unknown_requests > 0 {
        format!("{} tokens（部分未知）", chat.usage.total_tokens)
    } else {
        format!("{} tokens", chat.usage.total_tokens)
    };
    // 恢复记录已解释暂停原因时，不再叠加重复的全宽警告。
    let has_recovery_notice = chat.messages.iter().flat_map(|message| &message.parts).any(|part| {
        part.kind == "notice"
            && chat.active_turn_id.as_ref().is_some_and(|turn| part.id == format!("{turn}-restart-notice"))
            && serde_json::from_str::<serde_json::Value>(&part.payload_json).ok()
            .and_then(|payload| payload.get("recovery_status").and_then(|value| value.as_str()).map(str::to_owned))
            .is_some_and(|status| status != "resolved")
    });
    if !running && chat.active_turn_id.is_some() && !has_recovery_notice {
        timeline = timeline.child(
            div().id("ai-chat-unfinished").py_1().flex().items_center().gap_1p5()
                .text_size(px(10.)).text_color(colors.muted)
                .child(app_icon(AppIcon::CircleSlash, 12., colors.warning))
                .child("任务已暂停 · 请核对待审批项；不会自动重放工具"),
        );
    }
    if let Some(error) = error {
        timeline = timeline
            .child(gpui_component::alert::Alert::error("ai-chat-error", error).small())
            .child(
                Button::new("ai-chat-retry")
                    .debug_selector(|| "ai-chat-retry".into())
                    .small()
                    .ghost()
                    .child("重新加载会话")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ai_chat.loaded = false;
                        this.open_ai_chat(window, cx);
                    })),
            );
    }
    div()
        .id("ai-chat-panel")
        .debug_selector(|| "ai-chat-panel".into())
        .size_full()
        .min_w(px(0.))
        .min_h(px(0.))
        .overflow_hidden()
        .bg(colors.panel_bg)
        .border_l_1()
        .border_color(colors.border)
        .occlude()
        .flex()
        .flex_col()
        .on_prepaint({
            let owner = cx.entity().downgrade();
            move |bounds, _, cx| { let _ = owner.update(cx, |this, cx| {
                if this.ai_chat.panel_height != bounds.size.height { this.ai_chat.panel_height = bounds.size.height; cx.notify(); }
            }); }
        })
        .capture_action(cx.listener(|this, action: &gpui_component::input::Enter, window, cx| {
            if action.shift || action.secondary || !this.handle_ai_reference_key("enter", window, cx) { cx.propagate(); }
        }))
        .capture_action(cx.listener(|this, _: &gpui_component::input::MoveUp, window, cx| {
            if !this.handle_ai_reference_key("up", window, cx) { cx.propagate(); }
        }))
        .capture_action(cx.listener(|this, _: &gpui_component::input::MoveDown, window, cx| {
            if !this.handle_ai_reference_key("down", window, cx) { cx.propagate(); }
        }))
        .capture_action(cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
            if !this.handle_ai_reference_key("escape", window, cx)
                && !this.reject_pending_ai_chat_approval(window, cx) {
                cx.propagate();
            }
        }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                // `@` 引用下拉打开时，Esc 先关它，不关整个面板。
                if this.ai_chat.reference_open {
                    this.ai_chat.reference_epoch += 1;
                    this.ai_chat.reference_loading = false;
                    this.ai_chat.reference_open = false;
                    this.ai_chat.reference_query = None;
                    this.ai_chat.reference_options.clear();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                // 有弹出层（模型/引用下拉）打开时，Esc 只关下拉，不关整个面板（设计 §31.4）。
                // 下拉打开时焦点在下拉列表上，据此判定，让 Select 自己消费这次 Esc。
                let popup_open = this
                    .ai_chat
                    .model_select
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
;
                if popup_open || this.ai_chat.mode_menu_open {
                    return;
                }
                if this.reject_pending_ai_chat_approval(window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if this.ai_chat.mode == fluxdb_storage::ai_platform::settings::PanelMode::Tab {
                    cx.propagate();
                    return;
                }
                this.close_ai_chat(window, cx);
                cx.stop_propagation();
            }
        }))
        .child(
            div()
                .debug_selector(|| "ai-chat-header".into())
                .h(px(48.))
                .flex_none()
                .border_b_1()
                .border_color(colors.border_soft)
                .px_3()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .truncate()
                        .child(if history {
                            "会话历史".into()
                        } else {
                            chat.title.clone()
                        }),
                )
                .child(
                    div().id("ai-chat-tokens").debug_selector(|| "ai-chat-tokens".into())
                        .aria_label(token_label.clone()).test_support().flex_none()
                        .text_size(px(10.)).text_color(colors.muted)
                        .child(token_label),
                )
                .child(
                    ai_chat_small_button("ai-chat-new", AppIcon::Plus, "新建会话", colors)
                        .debug_selector(|| "ai-chat-new".into())
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.new_ai_chat(window, cx))),
                )
                .child(
                    this.ai_chat_mode_button(!ready, colors, cx),
                )
                .child(
                    ai_chat_small_button(
                        "ai-chat-history",
                        AppIcon::CalendarClock,
                        "会话历史",
                        colors,
                    )
                    .debug_selector(|| "ai-chat-history".into())
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ai_chat.history = !this.ai_chat.history;
                        cx.notify();
                    })),
                )
                .child(
                    ai_chat_small_button("ai-chat-close", AppIcon::Close, "关闭", colors)
                        .debug_selector(|| "ai-chat-close".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.close_ai_chat(window, cx)),
                        ),
                ),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(colors.border_soft)
                .flex()
                .items_center()
                .flex_wrap()
                .gap_1p5()
                .child(app_icon(AppIcon::Database, 12., colors.muted))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .child(if scope_primary.is_some() {
                            "本轮目标"
                        } else {
                            "无执行目标"
                        }),
                )
                .when_some(scope_primary, |row, scope_primary| {
                    row.child(ai_scope_tag(
                        AppIcon::Database,
                        &scope_primary,
                        colors.info,
                        colors,
                    ))
                })
                .child(ai_scope_tag(
                    if scope_permission == "修改需审批" {
                        AppIcon::Edit
                    } else {
                        AppIcon::Check
                    },
                    scope_permission,
                    scope_permission_color,
                    colors,
                ))
        )
        .child(timeline)
        .when(this.ai_chat.new_reply, |panel| {
            panel.child(
                Button::new("ai-chat-new-reply")
                    .small()
                    .ghost()
                    .child("查看新回复")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ai_chat.scroll.scroll_to_bottom();
                        this.ai_chat.follow_reply = true;
                        this.ai_chat.new_reply = false;
                        cx.notify();
                    })),
            )
        })
        .child(ai_chat_composer(this, _window, colors, cx))
}
