impl NavicatMain {
    fn mcp_binary_path(&self) -> String {
        self.ai_mcp_binary_override
            .clone()
            .or_else(|| fluxdb_app::bundled_mcp_binary().map(|p| p.display().to_string()))
            .unwrap_or_default()
    }

    fn refresh_mcp_install_status(&mut self, cx: &mut Context<Self>) {
        self.run_mcp_clients_command(
            AppCommand::RefreshMcpClients {
                binary_path: self.mcp_binary_path(),
            },
            "refresh".into(),
            None,
            cx,
        );
    }

    fn sync_mcp_clients(&mut self, cx: &mut Context<Self>) {
        self.run_mcp_clients_command(
            AppCommand::SyncMcpClients {
                binary_path: self.mcp_binary_path(),
            },
            "sync".into(),
            None,
            cx,
        );
    }

    fn set_mcp_client(&mut self, key: String, registered: bool, cx: &mut Context<Self>) {
        self.run_mcp_clients_command(
            AppCommand::SetMcpClientRegistration {
                client_key: key.clone(),
                registered,
                binary_path: self.mcp_binary_path(),
            },
            key,
            Some(if registered {
                "已写入客户端配置；请在客户端刷新或重启 MCP"
            } else {
                "已从客户端卸载"
            }),
            cx,
        );
    }

    /// 文件读写在后台串行执行；只回传配置状态，不提取或回传客户端凭据。
    fn run_mcp_clients_command(
        &mut self,
        command: AppCommand,
        operation: String,
        success: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        if self.ai_mcp_clients_busy.is_some() {
            return;
        }
        self.ai_mcp_clients_busy = Some(operation);
        self.ai_mcp_clients_error = None;
        let binary_path = self.mcp_binary_path();
        let mut controller = self.controller.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let (event, snapshot) = cx
                .background_spawn(async move {
                    let event = controller.dispatch(command);
                    let snapshot = if matches!(event, AppEvent::AiMcpClientsLoaded(_)) {
                        event.clone()
                    } else {
                        controller.dispatch(AppCommand::RefreshMcpClients { binary_path })
                    };
                    (event, snapshot)
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.ai_mcp_clients_busy = None;
                match snapshot {
                    AppEvent::AiMcpClientsLoaded(snapshot) => {
                        this.ai_mcp_install_status = snapshot.clients;
                        this.ai_mcp_connection_details = snapshot.connection;
                        this.ai_mcp_connection_error = snapshot.connection_error;
                    }
                    AppEvent::AiSettingsError(error) => {
                        this.ai_mcp_clients_error = Some(error);
                    }
                    _ => {}
                }
                if let AppEvent::AiSettingsError(message) = event {
                    this.ai_mcp_clients_error = Some(message.clone());
                    this.show_message(message, AppMessageKind::Error, cx);
                } else if let Some(message) = success {
                    this.show_message(message, AppMessageKind::Success, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn ai_mcp_clients_panel(
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> GroupBox {
    let busy = panel.mcp_clients_busy.is_some();
    let owner = cx.entity().downgrade();
    let statuses = panel.mcp_install_status.clone();
    let can_configure = panel.mcp_connection_details.is_some();
    let add = Button::new("ai-mcp-client-add")
        .small()
        .cursor_pointer()
        .disabled(busy || !can_configure)
        .icon(gpui_component::Icon::default().path(app_icon_path(AppIcon::Plus)))
        .child("添加客户端")
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |mut menu, _, _| {
            for client in fluxdb_app::McpClientKind::all() {
                // 已有同名条目的客户端走行内安装/重新安装，菜单只提供尚未接入的客户端。
                if statuses
                    .iter()
                    .any(|s| s.key == client.key() && s.registered)
                {
                    continue;
                }
                let owner = owner.clone();
                let key = client.key().to_string();
                let id = format!("ai-mcp-client-add:{key}");
                let label = client.label();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id(SharedString::from(id.clone()))
                            .child(label)
                    })
                    .on_click(move |_, _, cx| {
                        let _ =
                            owner.update(cx, |this, cx| this.set_mcp_client(key.clone(), true, cx));
                    }),
                );
            }
            menu
        });
    let header = div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div().min_w_0().flex().items_center().gap_2().child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child("客户端接入"),
            ),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new("ai-mcp-client-refresh")
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .accessibility_label("刷新客户端配置")
                        .tooltip("刷新客户端配置")
                        .disabled(busy)
                        .loading(panel.mcp_clients_busy.as_deref() == Some("refresh"))
                        .icon(gpui_component::Icon::default().path(app_icon_path(AppIcon::Refresh)))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.refresh_mcp_install_status(cx)),
                        ),
                )
                .child(add),
        );
    let mut group = settings_panel_group_el(header).footer(
        div()
            .text_size(px(11.))
            .text_color(colors.muted)
            .child("「已配置」只是写入了客户端配置，不代表已连接；「卸载」删除该配置，关闭服务也会保留它。"),
    );
    if busy {
        group = group.child(
            div()
                .id("ai-mcp-clients-loading")
                .px_3()
                .py_2()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child("正在更新配置…")
        );
    }
    if let Some(error) = &panel.mcp_clients_error {
        group = group.child(
            div()
                .id("ai-mcp-clients-error")
                .px_3()
                .py_2()
                .text_size(px(12.))
                .text_color(colors.danger)
                .child(error.clone())
        );
    }
    let mut count = 0;
    for status in &panel.mcp_install_status {
        if !status.config_exists && !status.managed && !status.registered {
            continue;
        }
        count += 1;
        group = group.child(client_registration_row(
            status,
            busy,
            can_configure,
            colors,
            cx,
        ));
    }
    if count == 0 && !busy {
        group = group.child(
            div()
                .px_3()
                .py_3()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("暂无客户端，点击右上角添加，或使用下方手动接入。"),
        );
    }
    group.child(ai_mcp_manual_connection(panel, colors, cx))
}

fn ai_mcp_manual_connection(
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let busy = panel.mcp_clients_busy.is_some();
    div().px_3().py_2().bg(colors.panel_alt.alpha(0.45)).flex().flex_col().gap_2()
        .child(div().flex().flex_wrap().items_center().justify_between().gap_2()
            .child(div().flex().items_center().gap_2()
                .child(div().text_size(px(12.)).font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text).child("其他客户端"))
                .child(div().text_size(px(11.)).text_color(colors.muted).child("MCP · stdio")))
            .when_some(panel.mcp_connection_details.clone(), |this, details| {
                let command = details.command_line;
                let json = details.json_example;
                this.child(div().flex().flex_wrap().items_center().gap_1()
                    .child(Button::new("ai-mcp-copy-command").small().ghost().cursor_pointer()
                        .disabled(busy).child("复制命令")
                        .tooltip(if cfg!(windows) { "复制 PowerShell 启动命令" } else { "复制启动命令" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
                            this.show_message("启动命令已复制", AppMessageKind::Success, cx);
                        })))
                    .child(Button::new("ai-mcp-copy-config").small().ghost().cursor_pointer()
                        .disabled(busy).child("复制 JSON")
                        .tooltip("复制 JSON 配置示例，请按客户端格式调整")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(json.clone()));
                            this.show_message("JSON 配置示例已复制，请按客户端格式调整", AppMessageKind::Success, cx);
                        })))
                    .child(Button::new("ai-mcp-config-details").small().ghost().cursor_pointer()
                        .child(if panel.mcp_details_open { "收起详情" } else { "配置详情" })
                        .icon(gpui_component::Icon::default().path(app_icon_path(
                            if panel.mcp_details_open { AppIcon::ChevronUp } else { AppIcon::ChevronDown }
                        )))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_mcp_details_open = !this.ai_mcp_details_open;
                            cx.notify();
                        }))))
            }))
        .when_some(panel.mcp_connection_error.clone(), |this, error| {
            this.child(div().text_size(px(12.)).text_color(colors.danger).child(error))
        })
        .when_some(panel.mcp_connection_details.clone(), |this, details| {
            this.child(gpui_component::collapsible::Collapsible::new().open(panel.mcp_details_open)
                .content(div().flex().flex_col().gap_2().pt_1().pb_1()
                    .child(div().text_size(px(11.)).text_color(colors.muted)
                        .child("支持 MCP stdio 的客户端可手动接入；部分工具需插件或桥接。JSON 仅为示例，格式和位置以客户端文档为准；不会写入审批规则。"))
                    .child(div().id("ai-mcp-config-preview").max_h(px(180.)).overflow_scroll()
                        .rounded_md().border_1().border_color(colors.border_soft).bg(colors.input_bg)
                        .p_3().font_family(PLATFORM_MONOSPACE_FONT).text_size(px(11.))
                        .text_color(colors.text).child(details.json_example))
                    .child(div().id("ai-mcp-connection-command").min_w_0().text_ellipsis()
                        .text_size(px(11.)).text_color(colors.muted).child(details.command_line)
                        )))
        })
}

fn client_registration_row(
    status: &fluxdb_app::McpInstallClientStatus,
    busy: bool,
    can_configure: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let key = status.key.clone();
    // 同名条目与本应用写入的一致才算已接入：此时提供卸载，否则提供安装/重新安装。
    let remove = status.up_to_date;
    let (label, color, icon) = if status.error.is_some() {
        ("配置异常", colors.danger, AppIcon::TriangleAlert)
    } else if status.registered {
        ("已配置", colors.success, AppIcon::Check)
    } else {
        ("未配置", colors.muted, AppIcon::CircleSlash)
    };
    let path = status.config_path.clone();
    let file_name = std::path::Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    div()
        .id(SharedString::from(format!("ai-mcp-client:{}", status.key)))
        .border_b_1()
        .border_color(colors.border_soft)
        .px_3()
        .py_2()
        .min_h(px(48.))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(28.))
                .flex_shrink_0()
                .rounded_md()
                .bg(colors.panel_alt)
                .flex()
                .items_center()
                .justify_center()
                .child(app_icon(AppIcon::Plug, 14., colors.muted)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .min_w_0()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(colors.text)
                                .child(status.label.clone()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "ai-mcp-client-path:{}",
                                    status.key
                                )))
                                .min_w_0()
                                .text_ellipsis()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(file_name)
                                .tooltip(move |window, cx| {
                                    Tooltip::new(path.clone()).build(window, cx)
                                }),
                        ),
                )
                .when_some(status.error.clone(), |this, error| {
                    this.child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.danger)
                            .child(error),
                    )
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(11.))
                .text_color(if status.error.is_some() {
                    color
                } else {
                    colors.muted
                })
                .child(app_icon(icon, 12., color))
                .child(label),
        )
        .child(
            Button::new(SharedString::from(format!(
                "ai-mcp-client-{}:{}",
                if remove { "remove" } else { "connect" },
                status.key
            )))
            .small()
            .ghost()
            .cursor_pointer()
            .text_color(colors.muted)
            .disabled(busy || (!remove && !can_configure))
            .child(if remove {
                "卸载"
            } else if status.registered {
                "重新安装"
            } else {
                "安装"
            })
            .on_click(
                cx.listener(move |this, _, _, cx| this.set_mcp_client(key.clone(), !remove, cx)),
            ),
        )
        .into_any_element()
}
