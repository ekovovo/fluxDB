// AI 设置职责拆分；由 ai.rs include! 引入，保持原有作用域。

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
    (
        "redis.execute",
        "Redis",
        "受控命令，检查所有 Key 与风险",
        true,
    ),
    ("redis.server_info", "Redis", "受限版本与诊断摘要", false),
    ("result.read_page", "公共", "读取已取得的结果快照", false),
    (
        "operation.status",
        "公共",
        "查询当前 caller 的操作状态",
        false,
    ),
    (
        "operation.cancel",
        "公共",
        "取消自身操作，不伪造服务端撤销",
        false,
    ),
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
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.ai_tools_tab = value;
                cx.notify();
                cx.stop_propagation();
            }),
        )
}

/// 页签条。
fn ai_tools_tab_bar(active: AiToolsTab, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    div()
        .border_b_1()
        .border_color(colors.border_soft)
        .flex()
        .items_center()
        .gap_1()
        .child(ai_tools_tab_button(
            "工具",
            AiToolsTab::Tools,
            active,
            colors,
            cx,
        ))
        .child(ai_tools_tab_button(
            "本地 MCP",
            AiToolsTab::Mcp,
            active,
            colors,
            cx,
        ))
        .child(ai_tools_tab_button(
            "内置插件",
            AiToolsTab::Plugins,
            active,
            colors,
            cx,
        ))
        .child(ai_tools_tab_button(
            "安全审计",
            AiToolsTab::Audit,
            active,
            colors,
            cx,
        ))
        .child(div().flex_1())
        .child(
            Button::new("ai-refresh-tools")
                .small()
                .ghost()
                .child("刷新")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reload_ai_settings(cx);
                })),
        )
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
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(format!("{} 个标准工具", AI_TOOL_CATALOG.len())),
                        ),
                )
                // 窄窗口：工具表保持最小宽度并横向滚动，避免列被压扁/裁切。
                .child(
                    div()
                        .w_full()
                        .overflow_x_scrollbar()
                        .child(
                            div()
                                .min_w(px(760.))
                                .flex()
                                .flex_col()
                                .child(header)
                                .child(rows),
                        ),
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
        .child(div().w(px(120.)).flex().items_start().child(if write {
            ai_pill("修改 / 授权", colors.warning, colors).into_any_element()
        } else {
            ai_pill("读取 / 有界", colors.success, colors).into_any_element()
        }))
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
            "暂无内置插件实例。",
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
        settings_panel_group("内置能力", colors).child(div().children(body)),
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
                    "暂无审计记录。",
                    colors,
                )),
        )
}

/// 从连接配置里取一个用于展示的库/Schema 标签（Tcp 用 database，SQLite 用文件名）。
fn connection_database_label(config: &fluxdb_core::ConnectionConfig) -> String {
    match &config.endpoint {
        fluxdb_core::Endpoint::Tcp { database, .. } => database.clone().unwrap_or_default(),
        fluxdb_core::Endpoint::SqliteFile { path, .. } => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
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
                            .child(div().text_size(px(11.)).text_color(colors.muted).child(
                                format!(
                                    "{} · {}",
                                    database_kind_name(c.config.kind),
                                    connection_database_label(&c.config)
                                ),
                            )),
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
                        )
                        .child(ai_pill("13 个候选工具", colors.info, colors)),
                )
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
