// AI 设置职责拆分；由 ai.rs include! 引入，保持原有作用域。

/// 工具与 MCP 页签。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AiToolsTab {
    Tools,
    Mcp,
    Plugins,
    Audit,
}

/// 标准工具目录（设计 §20.5 / 交互稿 + 设计 §22 的 Redis 有界订阅）。
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
    (
        "redis.subscribe",
        "Redis",
        "有界订阅，限时/限条数/限字节可停",
        false,
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
        .debug_selector(|| format!("ai-tools-tab:{label}"))
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
                // 切到「本地 MCP」时刷新客户端注册状态（读配置，不写）。
                if matches!(value, AiToolsTab::Mcp) {
                    this.refresh_mcp_install_status();
                }
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

/// 「工具」页签：标准工具表。
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
        // 没有行 = 从未被显式关闭，按默认启用显示。
        let setting = panel.tool_settings.iter().find(|t| t.tool_id == *id);
        let enabled = setting.map(|t| t.enabled).unwrap_or(true);
        let revision = setting.map(|t| t.revision).unwrap_or(0);
        rows = rows.child(ai_tool_row(
            id,
            group,
            desc,
            *write,
            enabled,
            revision,
            colors,
            cx,
        ));
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
    revision: i64,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let tool_id_for_switch = id.to_string();
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
            div()
                .w(px(60.))
                .child(div().debug_selector(|| format!("ai-tool:{id}")).child(
                    Switch::new(SharedString::from(format!("ai-tool:{id}")))
                        .checked(enabled)
                        .on_click(cx.listener(move |this, checked, _, cx| {
                            // 全局启停经命令落库；关闭会级联收窄各 Agent 白名单。
                            this.set_ai_tool_enabled(
                                tool_id_for_switch.clone(),
                                *checked,
                                revision,
                                cx,
                            );
                        })),
                )),
        )
        .child(div().flex_1())
}

/// 内置能力的图标：按 package_key 归类，未知实例用通用插件图标。
fn ai_builtin_icon(package_key: &str) -> AppIcon {
    match package_key {
        "fluxdb.context.ui" => AppIcon::Text,
        "fluxdb.tools.database" => AppIcon::Database,
        "fluxdb.context.files" => AppIcon::FileSearch,
        "fluxdb.model.router" => AppIcon::Workflow,
        "fluxdb.decision.jev" => AppIcon::Activity,
        "fluxdb.context.compaction" => AppIcon::Filter,
        "fluxdb.security.audit" => AppIcon::Check,
        "fluxdb.usage.budget" => AppIcon::CalendarClock,
        "fluxdb.knowledge.skills" => AppIcon::Folder,
        "fluxdb.output.bi" => AppIcon::Table,
        "fluxdb.progress.plan" => AppIcon::List,
        _ => AppIcon::Plug,
    }
}

/// 按内置目录顺序（设计 §23.2）展示能力；未知实例排末尾，不隐藏。
///
/// 数据库按名称排序，中文名会把"BI 与报表输出"排到最前，与目录语义不符，
/// 因此展示顺序由这里统一决定，不改存储层排序。
fn ai_plugins_in_catalog_order(
    items: &[fluxdb_app::AiPluginItem],
) -> Vec<&fluxdb_app::AiPluginItem> {
    let mut rows: Vec<&fluxdb_app::AiPluginItem> = items.iter().collect();
    rows.sort_by_key(|p| {
        (
            fluxdb_app::builtin_catalog_order(&p.package_key).unwrap_or(usize::MAX),
            p.id.clone(),
        )
    });
    rows
}

/// 「内置插件」页签：内置能力行（说明 + 状态 + 启停）+ 内核约束标签。
fn ai_plugins_panel(state: &AppState, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    let body: Vec<gpui::AnyElement> = if state.ai_plugin_instances.is_empty() {
        vec![ai_empty_state(
            "暂无内置插件实例。",
            colors,
        )]
    } else {
        ai_plugins_in_catalog_order(&state.ai_plugin_instances)
            .into_iter()
            .map(|p| {
                let this = cx.entity().downgrade();
                let id = p.id.clone();
                let enabled = p.enabled;
                let revision = p.revision;
                let mandatory = p.mandatory;
                // Jev 有独立的装配/熔断状态，不复用通用启停标签。
                let is_jev = p.package_key == "fluxdb.decision.jev";
                let jev_status = state.ai_jev_status.clone();
                ai_row_container(&p.id, colors)
                    .min_h(px(52.))
                    .py_2()
                    .child(app_icon_box(
                        ai_builtin_icon(&p.package_key),
                        28.,
                        15.,
                        colors.muted,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
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
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(p.summary.clone()),
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
                                    })
                                    .when(!mandatory, |this| {
                                        this.child(ai_status_pill(enabled, colors))
                                    })
                                    .when(is_jev, |this| {
                                        this.child(ai_jev_status_pill(&jev_status, colors))
                                    }),
                            ),
                    )
                    .child(div().debug_selector(|| format!("ai-plugin:{id}")).child(
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
                    ))
                    .into_any_element()
            })
            .collect()
    };
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group_el(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("内置能力"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(format!(
                                "{} 项 · 第三方仅预留接口",
                                state.ai_plugin_instances.len()
                            )),
                    ),
            )
            .child(div().children(body)),
        )
        .child(ai_jev_status_note(&state.ai_jev_status, colors))
        .child(
            div()
                .px_4()
                .py_3()
                .rounded(px(6.))
                .bg(colors.border_soft)
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(
                    "第三方插件安装、市场与沙箱当前不实现。未来通过相同的能力契约扩展，\
                     不直接访问数据库、密钥或界面控件。",
                ),
        )
}

/// Jev 状态说明块：把"为什么不可用"讲清楚，但不弹错、不阻断基础 Agent。
fn ai_jev_status_note(status: &fluxdb_app::JevStatus, colors: UiColors) -> impl IntoElement {
    use fluxdb_app::JevStatusKind as K;
    let detail = match (status.kind, status.detail.clone(), status.circuit_until_ms) {
        (K::Available, _, _) => {
            "Jev 已自动装配：保存并启用一个合格的决策模型即可直接使用，无需安装插件或手工建绑定。"
                .to_string()
        }
        (K::NeedsChoice, Some(reason), _) | (K::Unavailable, Some(reason), _) => reason,
        (K::NeedsChoice, None, _) => {
            "存在多个合格决策模型，请在大模型设置中选择默认决策模型后再自动装配。"
                .to_string()
        }
        (K::Unavailable, None, Some(until)) => {
            format!("Jev 连续失败已熔断，将于 {until} 之后重新尝试。")
        }
        (K::Unavailable, None, None) => "Jev 当前不可用，Agent 继续按基础策略运行。".to_string(),
        (K::NotConfigured, _, _) => {
            "尚未配置可用的决策模型。Jev 只是可选优化，未配置时主 Agent 照常运行。".to_string()
        }
        (K::Disabled, _, _) => {
            "Jev 已关闭：不发起任何决策请求，主 Agent 与上下文压缩照常工作。".to_string()
        }
    };
    div()
        .debug_selector(|| "ai-jev-status-note".to_string())
        .px_4()
        .py_3()
        .rounded(px(6.))
        .bg(colors.border_soft)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colors.text)
                .child(format!("Jev：{}", status.label())),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(detail),
        )
}

/// 启停状态标签：可选能力显示真实启停，不显示成"一排错误"。
fn ai_status_pill(enabled: bool, colors: UiColors) -> impl IntoElement {
    if enabled {
        ai_pill("已启用", colors.success, colors)
    } else {
        ai_pill("已关闭", colors.muted, colors)
    }
}

/// Jev 状态标签（设计 §18 的状态表）：状态而不是错误条。
///
/// Jev 是内置可选能力，永远不是内核约束，开关始终可用。
fn ai_jev_status_pill(status: &fluxdb_app::JevStatus, colors: UiColors) -> impl IntoElement {
    use fluxdb_app::JevStatusKind as K;
    let (label, color) = match status.kind {
        K::Available => ("可用", colors.success),
        K::NeedsChoice => ("待选择模型", colors.warning),
        K::Unavailable => ("暂不可用", colors.warning),
        K::NotConfigured => ("未配置", colors.muted),
        K::Disabled => ("已禁用", colors.muted),
    };
    let text = match (status.detail.clone(), status.circuit_until_ms) {
        (Some(reason), _) => format!("{label}：{reason}"),
        (None, Some(until)) => format!("{label}：熔断至 {until}"),
        (None, None) => label.to_string(),
    };
    div().id("ai-jev-status").child(ai_pill(label, color, colors)).tooltip(move |window, cx| {
        Tooltip::new(text.clone()).build(window, cx)
    })
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
fn ai_mcp_panel(
    state: &AppState,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
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

    let client_rows: Vec<gpui::AnyElement> = if panel.mcp_install_status.is_empty() {
        vec![ai_empty_state("正在读取客户端配置…", colors)]
    } else {
        panel
            .mcp_install_status
            .iter()
            .map(|status| client_registration_row(status, panel.mcp_install_busy.as_deref(), colors, cx))
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
            settings_panel_group("注册到 CLI 客户端", colors)
                .child(
                    div()
                        .border_t_1()
                        .border_color(colors.border_soft)
                        .px_4()
                        .py_2()
                        .child(div().text_size(px(11.)).text_color(colors.muted).child(
                            "启动时会自动把本地只读 MCP Server 写入已安装客户端的用户级配置（幂等，不影响其它配置）；卸载后会关闭自动注册。下方按钮用于手动注册或卸载。",
                        )),
                )
                .child(div().children(client_rows)),
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

/// 单个客户端注册行：名称 + 配置文件路径 + 状态胶囊 + 注册/卸载按钮。
fn client_registration_row(
    status: &fluxdb_app::McpInstallClientStatus,
    busy: Option<&str>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let key = status.key.clone();
    let is_busy = busy == Some(status.key.as_str());
    let registered = status.registered;
    // 已安装但未注册 vs 客户端不存在：文案要区分，避免让用户以为按钮坏了。
    let status_pill = if registered {
        ai_pill("已注册", colors.success, colors)
    } else if status.config_exists {
        ai_pill("未注册", colors.muted, colors)
    } else {
        ai_pill("未检测到客户端", colors.muted, colors)
    };
    let action_key = status.key.clone();
    let action_registered = registered;
    let row_selector = format!("ai-mcp-client:{}", status.key);
    let button_selector = format!("ai-mcp-install:{}", status.key);
    div()
        .id(SharedString::from(row_selector.clone()))
        .debug_selector(move || row_selector.clone())
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_2()
        .min_h(px(52.))
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
                        .child(status.label.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(status.config_path.clone()),
                ),
        )
        .child(status_pill)
        .child({
            let label = if is_busy {
                "处理中…"
            } else if action_registered {
                "卸载"
            } else {
                "一键注册"
            };
            let mut button = Button::new(SharedString::from(format!("ai-mcp-install:{}", key)))
                .small()
                .debug_selector({
                    let selector = button_selector.clone();
                    move || selector.clone()
                })
                .child(label);
            if is_busy {
                button = button.disabled(true);
            }
            button.on_click(cx.listener(move |this, _, _, cx| {
                if this.ai_mcp_install_busy.is_some() {
                    return;
                }
                this.ai_mcp_install_busy = Some(action_key.clone());
                cx.notify();
                let key = action_key.clone();
                let uninstall = action_registered;
                let this_override_or_bundled = this.ai_mcp_binary_override.clone();
                let mut controller = this.controller.clone();
                let _ = cx.spawn(async move |view, cx| {
                    let event = cx
                        .background_spawn(async move {
                            if uninstall {
                                controller.dispatch(AppCommand::UninstallMcpClient { client: key })
                            } else {
                                // 一键注册：自动创建/复用默认只读 Profile，写客户端用户级配置。
                                let binary = this_override_or_bundled
                                    .clone()
                                    .or_else(|| {
                                        fluxdb_app::bundled_mcp_binary()
                                            .map(|p| p.display().to_string())
                                    })
                                    .unwrap_or_default();
                                if binary.is_empty() {
                                    return AppEvent::AiSettingsError(
                                        "未找到 fluxdb-mcp 二进制（应与 fluxdb-desktop 同目录）".into(),
                                    );
                                }
                                controller.dispatch(AppCommand::InstallMcpClient {
                                    client: key,
                                    profile_id: fluxdb_app::DEFAULT_MCP_PROFILE_ID.into(),
                                    binary_path: binary,
                                    config_root: None,
                                    preapprove_claude_tools: true,
                                })
                            }
                        })
                        .await;
                    let _ = cx.update(|cx| {
                        let Some(view) = view.upgrade() else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.ai_mcp_install_busy = None;
                            match event {
                                AppEvent::AiMcpClientRegistration { .. } => {
                                    this.refresh_mcp_install_status();
                                    this.show_message(
                                        if uninstall {
                                            "已从客户端移除本地 MCP Server"
                                        } else {
                                            "已注册到客户端，重启该客户端后生效"
                                        },
                                        AppMessageKind::Success,
                                        cx,
                                    );
                                }
                                AppEvent::AiSettingsError(message) => {
                                    this.show_message(
                                        format!("操作失败：{message}"),
                                        AppMessageKind::Error,
                                        cx,
                                    );
                                }
                                _ => {}
                            }
                            cx.notify();
                        });
                    });
                });
            }))
        })
        .into_any_element()
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
        AiToolsTab::Mcp => ai_mcp_panel(state, panel, colors, cx),
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

#[cfg(test)]
mod ai_tools_catalog_tests {
    use super::AI_TOOL_CATALOG;

    /// 设置页目录必须与实际下发给模型的工具定义一致：漏一个会让用户看到的工具清单
    /// 与真实能力不符（新增工具时本用例失败即提醒同步）。
    #[test]
    fn catalog_lists_every_registered_redis_tool() {
        let registered = fluxdb_app::standard_redis_tool_specs()
            .expect("Redis 工具定义")
            .into_iter()
            .map(|spec| spec.id)
            .collect::<std::collections::BTreeSet<_>>();
        let listed = AI_TOOL_CATALOG
            .iter()
            .map(|(id, ..)| (*id).to_string())
            .collect::<std::collections::BTreeSet<_>>();
        for id in &registered {
            assert!(listed.contains(id), "设置页工具目录缺少已注册的 {id}");
        }
    }

    /// 目录内不得重复列出同一个工具 ID。
    #[test]
    fn catalog_has_no_duplicate_ids() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, ..) in AI_TOOL_CATALOG {
            assert!(seen.insert((*id).to_string()), "工具目录重复: {id}");
        }
    }
}
