// AI 设置职责拆分；由 ai.rs include! 引入，保持原有作用域。

/// 工具与 MCP 页签。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AiToolsTab {
    Tools,
    Mcp,
    Plugins,
    Audit,
}

/// 内置 Agent 工具；数据库/Redis 工具同时供 MCP 使用，产物工具不对外暴露。
const AI_AGENT_TOOL_CATALOG: &[(&str, &str, &str, bool)] = &[
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
    ("artifact.create", "产物", "创建报告草稿；内置渲染需启用 BI 插件", true),
    ("artifact.add_block", "产物", "为报告追加图表、表格或说明，不修改数据库", true),
    ("artifact.finalize", "产物", "校验来源并生成报告快照，不修改数据库", true),
];

/// 仅 MCP 提供的工具仍受全局开关控制，不进入内置 Agent 的配置。
const AI_MCP_ONLY_TOOL_CATALOG: &[(&str, &str, &str, bool)] = &[
    (
        "operation.status",
        "公共",
        "查询当前 caller 的操作状态（含中断后的未知结果）",
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
) -> impl IntoElement {
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
                if matches!(value, AiToolsTab::Mcp) {
                    this.refresh_mcp_install_status(cx);
                    // 默认 Profile 缺失时就地补建，页面无需额外的加载入口。
                    this.ensure_mcp_profile(cx);
                }
                cx.notify();
                cx.stop_propagation();
            }),
        ).test_support()
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
                .debug_selector(|| "ai-refresh-tools".into())
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
    state: &AppState,
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
    for (id, group, desc, write) in AI_AGENT_TOOL_CATALOG.iter().chain(AI_MCP_ONLY_TOOL_CATALOG) {
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
            ai_execution_policy_panel(state,colors,cx),
        )
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
                                .child(format!(
                                    "{} 个标准工具",
                                    AI_AGENT_TOOL_CATALOG.len() + AI_MCP_ONLY_TOOL_CATALOG.len()
                                )),
                        ),
                )
                .child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(format!(
                            "产物工具独立启停；BI 与报表输出{}，关闭只停用内置生成能力，工具仍显示，已完成报告仍可查看和导出。",
                            if state.ai_plugin_instances.iter().any(|plugin| plugin.id == fluxdb_app::BI_INSTANCE_ID && plugin.enabled) {
                                "已启用"
                            } else {
                                "未启用"
                            }
                        )),
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
        .child(div().w(px(120.)).flex().items_start().child(if group == "产物" {
            ai_pill("产物 / 本地", colors.info, colors).into_any_element()
        } else if write {
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
        "fluxdb.context.files" => AppIcon::FileSearch,
        "fluxdb.model.router" => AppIcon::Workflow,
        "fluxdb.decision.jev" => AppIcon::Activity,
        "fluxdb.context.compaction" => AppIcon::Filter,
        "fluxdb.security.audit" => AppIcon::Check,
        "fluxdb.usage.budget" => AppIcon::CalendarClock,
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

/// 「内置插件」页签：能力说明、启停状态，以及常驻能力/待实现标签。
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
                let manage_id = p.id.clone();
                let enabled = p.enabled;
                let revision = p.revision;
                let mandatory = p.mandatory;
                let retired = p.retired;
                // 「管理」入口由插件自己的配置声明决定：没有可配置项就不显示，
                // 也不按插件 id 特判。
                let has_config = !p.config_fields.is_empty();
                let config_error = p.config_error.clone();
                // Jev 有独立的装配/熔断状态，不复用通用启停标签。
                let is_jev = p.package_key == "fluxdb.decision.jev";
                let jev_status = state.ai_jev_status.clone();
                // 管理入口与插件行共用同一个 owner，避免在 `when` 闭包里被行元素遮蔽。
                let manage_owner = this.clone();
                let manage_selector = format!("ai-plugin-manage:{manage_id}");
                let manage_click_id = manage_id.clone();
                let manage_button = Button::new(SharedString::from(manage_selector.clone()))
                    .debug_selector({
                        let selector = manage_selector.clone();
                        move || selector.clone()
                    })
                    .small()
                    .ghost()
                    .cursor_pointer()
                    .child("管理")
                    .on_click(move |_, window, cx| {
                        let _ = manage_owner.update(cx, |this, cx| {
                            this.open_ai_plugin_config_dialog(&manage_click_id, window, cx);
                        });
                    });
                ai_row_container(&p.id, colors)
                    .min_h(px(52.))
                    .py_2()
                    // 无实现的占位行整行置灰：只占目录位，不给出可操作的启停状态。
                    .when(retired, |this| this.opacity(0.45))
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
                                    .text_color(if retired { colors.muted } else { colors.text })
                                    .child(p.display_name.clone()),
                            )
                            .when(!p.summary.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(colors.muted)
                                        .child(p.summary.clone()),
                                )
                            })
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
                                    .when(retired, |this| {
                                        this.child(ai_pill("待实现", colors.muted, colors))
                                    })
                                    .when(mandatory, |this| {
                                        this.child(ai_pill("常驻能力", colors.info, colors))
                                    })
                                    .when(!mandatory && !retired, |this| {
                                        this.child(ai_status_pill(enabled, colors))
                                    })
                                    .when(is_jev, |this| {
                                        this.child(ai_jev_status_pill(&jev_status, colors))
                                    })
                                    .when_some(config_error.clone(), |this, message| {
                                        this.child(ai_pill("配置不可读", colors.warning, colors))
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(colors.warning)
                                                    .child(message),
                                            )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(has_config, |this| {
                                this.child(manage_button)
                            })
                            .child(div().debug_selector(|| format!("ai-plugin:{id}")).child(
                                Switch::new(SharedString::from(format!("ai-plugin:{id}")))
                                    .checked(enabled)
                                    .disabled(mandatory || retired)
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
                            )),
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
            "Jev 已使用「管理」里选定的决策模型；无需安装插件或手工建绑定。".to_string()
        }
        (K::NeedsChoice, Some(reason), _) | (K::Unavailable, Some(reason), _) => reason,
        (K::NeedsChoice, None, _) => {
            "已有合格决策模型但尚未选择：点该行的「管理」选择决策模型后 Jev 才参与运行。"
                .to_string()
        }
        (K::Unavailable, None, Some(until)) => {
            format!("Jev 连续失败已熔断，将于 {until} 之后重新尝试。")
        }
        (K::Unavailable, None, None) => "Jev 当前不可用，Agent 继续按基础策略运行。".to_string(),
        (K::NotConfigured, _, _) => {
            "尚未配置可用的决策模型：先在大模型页添加并启用一个 System One 决策模型。Jev 只是可选优化，未配置时主 Agent 照常运行。".to_string()
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
/// Jev 是内置可选能力，永远不是常驻能力，开关始终可用。
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

/// 「安全审计」页签：只读记录。
///
/// 结构与设置页其余页签一致：先给"审计始终开启"的不可变事实，再给统计筛选与列表。
/// 列表来自 `AppState.ai_audit_rows`（读库失败时为 `ai_audit_error`，与空态分开显示）。
/// 行可展开查看最终计划、命中规则、plan hash；本页不提供任何执行/修改操作。
fn ai_audit_panel(state: &AppState, panel: &AiSettingsPanelState, colors: UiColors, cx: &mut Context<NavicatMain>) -> Div {
    let rows = &state.ai_audit_rows;
    // 统计基于当前快照（不单独查库），与列表同源，避免数字与行数不一致。
    let counts = fluxdb_app::AiAuditCounts {
        all: rows.len(),
        allow: rows.iter().filter(|r| r.decision == fluxdb_app::AiAuditDecision::Allow).count(),
        approval: rows.iter().filter(|r| r.decision == fluxdb_app::AiAuditDecision::RequireApproval).count(),
        deny: rows.iter().filter(|r| r.decision == fluxdb_app::AiAuditDecision::Deny).count(),
    };
    let filter = panel.audit_filter;
    let query = panel.audit_filter_text.trim().to_ascii_lowercase();
    let visible: Vec<&fluxdb_app::AiAuditRow> = rows
        .iter()
        .filter(|row| filter.is_none() || filter == Some(row.decision))
        .filter(|row| {
            query.is_empty()
                || row.tool_id.to_ascii_lowercase().contains(&query)
                || row.target.to_ascii_lowercase().contains(&query)
                || row.rule_ids.iter().any(|id| id.to_ascii_lowercase().contains(&query))
        })
        .collect();

    let mut panel_body = div().flex().flex_col().gap_3().child(
        settings_panel_group("安全审计", colors).child(
            div()
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(app_icon(AppIcon::Check, 14., colors.success))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.text)
                                .child("安全审计始终开启（只读）"),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("参数或目标变化会让旧审计与审批失效；审计无法执行或保存时，操作不会继续。规则在「内置插件 → 调用前安全审计 → 管理」中调整。"),
                ),
        ),
    );

    // 统计卡即筛选器：点一张筛一类，再点同一张回到全部。
    let mut stats = div().flex().items_center().gap_2();
    for (decision, label, count) in [
        (None, "全部判断", counts.all),
        (Some(fluxdb_app::AiAuditDecision::Allow), "允许", counts.allow),
        (Some(fluxdb_app::AiAuditDecision::RequireApproval), "需审批", counts.approval),
        (Some(fluxdb_app::AiAuditDecision::Deny), "拒绝", counts.deny),
    ] {
        stats = stats.child(ai_audit_stat_card(
            decision,
            label,
            count,
            filter == decision,
            colors,
            cx,
        ));
    }
    panel_body = panel_body.child(stats);

    // 搜索与筛选只作用于当前快照；清空筛选按钮在有筛选时可见。
    let mut toolbar = div().flex().items_center().gap_2().child(ai_session_search_field(
        &panel.audit_filter_input,
        colors,
        cx,
    ));
    if filter.is_some() || !query.is_empty() {
        toolbar = toolbar.child(
            Button::new("ai-audit-clear-filter")
                .debug_selector(|| "ai-audit-clear-filter".into())
                .small()
                .ghost()
                .cursor_pointer()
                .child("清除筛选")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.clear_ai_audit_filter(window, cx)
                })),
        );
    }
    toolbar = toolbar.child(div().flex_1()).child(
        div()
            .text_size(px(11.))
            .text_color(colors.muted)
            .child(format!(
                "保留最近 {} 条 · 仅追加，不可修改",
                fluxdb_app::AI_AUDIT_PAGE_SIZE
            )),
    );
    panel_body = panel_body.child(toolbar);

    // 错误态优先于空态：读库失败不能显示成"暂无记录"。
    let list_body = if let Some(error) = state.ai_audit_error.clone() {
        ai_audit_error_state(&error, colors)
    } else if visible.is_empty() {
        ai_empty_state(
            if rows.is_empty() {
                "暂无审计记录。"
            } else {
                "当前筛选下没有匹配的审计记录。"
            },
            colors,
        )
    } else {
        let mut list = div().flex().flex_col();
        for row in &visible {
            list = list.child(ai_audit_row(row, panel, colors, cx));
        }
        list.into_any_element()
    };

    panel_body.child(
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
                    .child(ai_table_head("调用 / 来源", 240., colors))
                    .child(ai_table_head("目标", 190., colors))
                    .child(ai_table_head("判断", 90., colors))
                    .child(ai_table_head("匹配规则", 210., colors))
                    .child(div().flex_1()),
            )
            .child(list_body),
    )
}

/// 一张统计卡：既是数字也是筛选入口。
fn ai_audit_stat_card(
    decision: Option<fluxdb_app::AiAuditDecision>,
    label: &str,
    count: usize,
    selected: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let selector = format!(
        "ai-audit-stat:{}",
        decision.map(fluxdb_app::AiAuditDecision::key).unwrap_or("all")
    );
    let dot = decision.map(|d| match d {
        fluxdb_app::AiAuditDecision::Allow => colors.success,
        fluxdb_app::AiAuditDecision::RequireApproval => colors.warning,
        fluxdb_app::AiAuditDecision::Deny => colors.danger,
    });
    // 自绘卡片而非 Button：组件 Button 的行高被 small() 压扁，撑不开"标题+数字"两行；
    // 这里保留同样的点击/筛选语义（点击即筛选，再点取消）。
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector({
            let selector = selector.clone();
            move || selector.clone()
        })
        .test_support()
        .cursor_pointer()
        .min_w(px(132.))
        .px_3()
        .py_2()
        .rounded(colors.radius)
        .border_1()
        .border_color(if selected { colors.info } else { colors.border })
        .bg(colors.panel_bg)
        .hover(|style| style.bg(colors.hover))
        .flex()
        .flex_col()
        .items_start()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(if selected { colors.text } else { colors.muted })
                        .child(label.to_string()),
                )
                .when_some(dot, |this, color| {
                    this.child(div().size(px(6.)).rounded_full().bg(color))
                }),
        )
        .child(
            div()
                .text_size(px(18.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(if selected { colors.text } else { colors.muted })
                .child(count.to_string()),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_ai_audit_filter(decision, cx);
        }))
        .into_any_element()
}

/// 审计行：摘要行 + 可展开详情。
fn ai_audit_row(
    row: &fluxdb_app::AiAuditRow,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let expanded = panel.expanded_audit.as_deref() == Some(row.id.as_str());
    let id = row.id.clone();
    let click_id = row.id.clone();
    let (tag_label, tag_color) = ai_audit_decision_tag(row.decision, colors);
    let row_selector = format!("ai-audit-row:{}", row.id);
    let mut summary = div()
        .id(SharedString::from(row_selector.clone()))
        .debug_selector({
            let row_selector = row_selector.clone();
            move || row_selector.clone()
        })
        .test_support()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .child(
            div()
                .w(px(240.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child(row.tool_id.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(format!("{} · {}", row.origin, row.id)),
                ),
        )
        .child(div().w(px(190.)).text_size(px(12.)).text_color(colors.text).child(row.target.clone()))
        .child(div().w(px(90.)).child(ai_pill(tag_label, tag_color, colors)))
        .child(
            div()
                .w(px(210.))
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(ai_audit_rule_label(row)),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(if expanded { "收起" } else { "详情" }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_ai_audit_detail(&click_id, cx);
        }));
    let _ = id;
    if expanded {
        summary = summary.bg(colors.border_soft);
    }
    let mut container = div().flex().flex_col().child(summary);
    if expanded {
        container = container.child(ai_audit_detail(row, colors));
    }
    container.into_any_element()
}

/// 命中规则的展示：机器键直接显示，未命中显示占位符。
fn ai_audit_rule_label(row: &fluxdb_app::AiAuditRow) -> String {
    if row.rule_ids.is_empty() {
        "—".to_string()
    } else {
        row.rule_ids.join(" · ")
    }
}

/// 展开详情：最终计划、命中规则与原因码、plan hash 与规则集版本，以及不泄漏秘密的说明。
fn ai_audit_detail(row: &fluxdb_app::AiAuditRow, colors: UiColors) -> Div {
    let mut detail = div()
        .px_4()
        .pb_3()
        .flex()
        .flex_col()
        .gap_2()
        .bg(colors.border_soft);
    let line = |key: &str, value: gpui::AnyElement| {
        div()
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(px(88.))
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(key.to_string()),
            )
            .child(value)
    };
    if !row.statement.is_empty() {
        detail = detail.child(line(
            "最终计划",
            div()
                .flex_1()
                .min_w(px(0.))
                .px_3()
                .py_2()
                .rounded(colors.radius)
                .bg(colors.panel_bg)
                .border_1()
                .border_color(colors.border_soft)
                .text_size(px(11.))
                .text_color(colors.text)
                .child(row.statement.clone())
                .into_any_element(),
        ));
    }
    detail = detail.child(line(
        "命中规则",
        div()
            .flex_1()
            .min_w(px(0.))
            .text_size(px(11.))
            .text_color(colors.text)
            .child(if row.reason_codes.is_empty() {
                ai_audit_rule_label(row)
            } else {
                format!("{} · {}", ai_audit_rule_label(row), row.reason_codes.join("、"))
            })
            .into_any_element(),
    ));
    detail = detail.child(line(
        "plan hash",
        div()
            .flex_1()
            .min_w(px(0.))
            .text_size(px(11.))
            .text_color(colors.muted)
            .child(format!(
                "{} · {} · 规则集 {}",
                row.plan_hash, row.stage, row.ruleset_version
            ))
            .into_any_element(),
    ));
    detail.child(line(
        "说明",
        div()
            .flex_1()
            .min_w(px(0.))
            .text_size(px(11.))
            .text_color(colors.muted)
            .child("审计只记录规则与对象，不含参数值之外的密钥或密文。")
            .into_any_element(),
    ))
}

/// 判定标签：颜色与工具卡的允许/需审批/拒绝一致（绿/黄/红）。
fn ai_audit_decision_tag(decision: fluxdb_app::AiAuditDecision, colors: UiColors) -> (&'static str, gpui::Rgba) {
    match decision {
        fluxdb_app::AiAuditDecision::Allow => (decision.label(), colors.success),
        fluxdb_app::AiAuditDecision::RequireApproval => (decision.label(), colors.warning),
        fluxdb_app::AiAuditDecision::Deny => (decision.label(), colors.danger),
    }
}

/// 审计列表读库失败：给出可见原因，不伪装成空态。
fn ai_audit_error_state(message: &str, colors: UiColors) -> gpui::AnyElement {
    div()
        .px_4()
        .py_6()
        .flex()
        .items_center()
        .gap_2()
        .child(app_icon(AppIcon::CircleSlash, 14., colors.danger))
        .child(
            div()
                .debug_selector(|| "ai-audit-error".into())
                .text_size(px(12.))
                .text_color(colors.danger)
                .child(format!("审计记录加载失败：{message}")),
        )
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
        AiToolsTab::Tools => ai_tools_catalog_panel(state,panel, colors, cx),
        AiToolsTab::Mcp => ai_mcp_panel(state, panel, colors, cx),
        AiToolsTab::Plugins => ai_plugins_panel(state, colors, cx),
        AiToolsTab::Audit => ai_audit_panel(state, panel, colors, cx),
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
    use super::{AI_AGENT_TOOL_CATALOG, AI_MCP_ONLY_TOOL_CATALOG};

    /// 设置页目录必须与实际下发给模型的工具定义一致：漏一个会让用户看到的工具清单
    /// 与真实能力不符（新增工具时本用例失败即提醒同步）。
    #[test]
    fn catalog_lists_every_registered_redis_tool() {
        let registered = fluxdb_app::standard_redis_tool_specs()
            .expect("Redis 工具定义")
            .into_iter()
            .map(|spec| spec.id)
            .collect::<std::collections::BTreeSet<_>>();
        let listed = AI_AGENT_TOOL_CATALOG
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
        for (id, ..) in AI_AGENT_TOOL_CATALOG.iter().chain(AI_MCP_ONLY_TOOL_CATALOG) {
            assert!(seen.insert((*id).to_string()), "工具目录重复: {id}");
        }
    }

    #[test]
    fn agent_catalog_excludes_mcp_only_operation_status() {
        assert!(
            AI_AGENT_TOOL_CATALOG.iter().any(|(id, ..)| *id == "connections.list")
        );
        assert!(
            !AI_AGENT_TOOL_CATALOG.iter().any(|(id, ..)| *id == "operation.status")
        );
        assert!(
            AI_MCP_ONLY_TOOL_CATALOG.iter().any(|(id, ..)| *id == "operation.status")
        );
    }
}
