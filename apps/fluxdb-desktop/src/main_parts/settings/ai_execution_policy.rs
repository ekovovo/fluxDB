/// 内部工具默认策略选择器：只读 / 写审批 / 帮我审批 / 自动执行。
///
/// 这是内部 Agent 工具的审批方式，不作用于对外 MCP 数据源（数据源用写入策略）。
fn ai_tools_policy_button(
    state: &AppState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let selected = state.ai_execution_policy.tools_default;
    let owner = cx.entity().downgrade();
    Button::new("execution-policy-tools")
        .debug_selector(|| "execution-policy-tools".into())
        .small()
        .cursor_pointer()
        .min_w(px(150.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(selected.label())
                .child(app_icon(AppIcon::ChevronDown, 12., colors.muted)),
        )
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            // check_side 必须显式设为右侧：保持默认左侧时，组件库会为每个未选中项渲染一个
            // 空图标占位（svg 空路径），每帧都用空路径去查资源目录并报读取失败；勾选标记
            // 改由组件画在行尾。
            let mut menu = menu
                .check_side(gpui_component::Side::Right)
                .min_w(px(190.));
            for mode in fluxdb_core::ExecutionPolicy::ALL {
                let owner = owner.clone();
                let option_id = format!("execution-policy-option:tools:{mode:?}");
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id(SharedString::from(option_id.clone()))
                            .debug_selector({
                                let id = option_id.clone();
                                move || id.clone()
                            })
                            .child(mode.label())
                            .test_support()
                    })
                    .checked(selected == mode)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            this.change_tools_policy(mode, window, cx)
                        });
                    }),
                );
            }
            menu
        })
        .into_any_element()
}

fn ai_policy_row(title: &str, note: &str, control: gpui::AnyElement, colors: UiColors) -> Div {
    div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(note.to_string()),
                ),
        )
        .child(control)
}

fn ai_execution_policy_panel(
    state: &AppState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> GroupBox {
    settings_panel_group("安全策略", colors).child(ai_policy_row(
        "内部工具默认策略",
        "只读 / 写审批 / 帮我审批 / 自动执行；不绕过连接权限和数据授权",
        ai_tools_policy_button(state, colors, cx),
        colors,
    ))
}

impl NavicatMain {
    fn save_execution_policy(
        &mut self,
        settings: fluxdb_core::ExecutionPolicySettings,
        cx: &mut Context<Self>,
    ) {
        let expected_revision = self.controller.state().ai_runtime_revision;
        let event = self.controller.dispatch(AppCommand::SaveExecutionPolicy {
            settings,
            expected_revision,
        });
        match event {
            AppEvent::AiSettingsError(message) => {
                self.show_message(&message, AppMessageKind::Error, cx)
            }
            _ => self.show_message(
                "安全策略已保存；待审批计划已失效",
                AppMessageKind::Success,
                cx,
            ),
        }
        cx.notify();
    }

    fn change_tools_policy(
        &mut self,
        value: fluxdb_core::ExecutionPolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.controller.state().ai_execution_policy.clone();
        settings.tools_default = value;
        if value == fluxdb_core::ExecutionPolicy::Automatic {
            let owner = cx.entity().downgrade();
            window.open_dialog(cx, move |dialog, _, _| {
                let owner = owner.clone();
                let settings = settings.clone();
                dialog
                    .title("启用自动执行")
                    .close_button(true)
                    .overlay_closable(true)
                    .keyboard(true)
                    .child("授权范围内的写入将不再逐次确认，可能造成不可逆的数据变化。只读连接、工具开关、数据范围和硬性校验仍然生效。")
                    .footer(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("execution-policy-cancel")
                                    .cursor_pointer()
                                    .child("取消")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("execution-policy-confirm")
                                    .danger()
                                    .cursor_pointer()
                                    .child("确认自动执行")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(
                                            Box::new(gpui_component::dialog::Confirm {
                                                secondary: false,
                                            }),
                                            cx,
                                        )
                                    }),
                            ),
                    )
                    .on_ok(move |_, _, cx| {
                        let _ =
                            owner.update(cx, |this, cx| this.save_execution_policy(settings.clone(), cx));
                        true
                    })
            });
        } else {
            self.save_execution_policy(settings, cx);
        }
    }
}
