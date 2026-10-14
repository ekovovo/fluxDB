// MCP Server 管理页（T060）：Profile、资源授权与活动记录；由 ai.rs include! 引入。
//
// 边界：UI 只读 AppState 中的公开列表并派发 AppCommand，不接触 SQLite 或凭据；
// 按用户要求不做客户端配置导出，也不做活动记录导出。

/// 标签 + 输入框；与 `ai_field` 同视觉，但 debug selector 挂在输入容器上，
/// headless 交互测试可以真实点击聚焦并输入（AGENTS.md：不直接调用回调代替点击）。
fn ai_mcp_field(
    selector: &'static str,
    label: &str,
    input: &Entity<InputState>,
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
        .child(
            div()
                .debug_selector(|| selector.into())
                .child(Input::new(input).text_size(px(13.))),
        )
}

/// 对外可公开的 MCP 工具目录（设计 §20.5）。Profile 的 `allowed_tool_ids` 只能从这里勾选，
/// 避免写入不存在的工具名导致 `tools/list` 永远为空。
const AI_MCP_SERVER_TOOLS: &[(&str, &str)] = &[
    ("fluxdb_connections_list", "列出已授权连接资源"),
    ("fluxdb_search_objects", "搜索表、视图与字段"),
    ("fluxdb_describe_object", "读取列、索引与关系"),
    ("fluxdb_query", "单条受控只读查询"),
    ("fluxdb_explain", "非执行式查询计划"),
    ("fluxdb_execute", "单条修改（需 scoped 预授权）"),
    ("fluxdb_redis_scan", "渐进扫描 Key"),
    ("fluxdb_redis_inspect", "按类型有界预览值与 TTL"),
    ("fluxdb_redis_execute", "受控 Redis 命令"),
    ("fluxdb_redis_subscribe", "限时限量订阅"),
    ("fluxdb_redis_server_info", "受限版本与诊断"),
    ("fluxdb_result_page", "读取已取得的结果快照"),
    ("fluxdb_operation_status", "查询操作状态"),
    ("fluxdb_operation_cancel", "取消自身操作"),
];

/// 写模式标签：`desktop_approval` 依赖尚未实现的桌面审批桥，UI 标注不可用。
fn ai_mcp_write_mode_label(mode: &str) -> (&'static str, bool) {
    match mode {
        "scoped" => ("受限写（预授权）", true),
        "desktop_approval" => ("桌面审批桥（未实现）", false),
        _ => ("只读", true),
    }
}

/// 活动状态标签：状态文字 + 颜色，不只靠颜色区分。
fn ai_mcp_request_status_pill(status: &str, colors: UiColors) -> impl IntoElement {
    let (label, color) = match status {
        "succeeded" => ("成功", colors.success),
        "failed" => ("失败", colors.danger),
        "denied" => ("已拒绝", colors.warning),
        "cancelled" => ("已取消", colors.muted),
        "unknown" => ("结果未知", colors.danger),
        "running" | "prepared" => ("执行中", colors.info),
        "waiting_approval" => ("等待批准", colors.warning),
        _ => (status, colors.muted),
    };
    ai_pill(label, color, colors)
}

/// 授权对象范围的人读摘要（不把整段 JSON 塞进行内）。
fn ai_mcp_object_scope_summary(grant: &fluxdb_app::AiMcpServerGrantItem) -> String {
    let value: serde_json::Value =
        serde_json::from_str(&grant.object_policy_json).unwrap_or_default();
    if value.get("all_objects").and_then(serde_json::Value::as_bool) == Some(true) {
        return "整库".to_string();
    }
    if value.get("all_keys").and_then(serde_json::Value::as_bool) == Some(true) {
        return "整个 DB".to_string();
    }
    let list = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let tables = list("table_allowlist");
    if !tables.is_empty() {
        return format!("表：{tables}");
    }
    let prefixes = list("key_prefixes");
    if !prefixes.is_empty() {
        return format!("前缀：{prefixes}");
    }
    "未放行任何对象".to_string()
}

/// 一行资源授权：Profile 内按连接/库展示，右侧开关就地收窄/放宽并派发保存。
fn ai_mcp_grant_row(
    grant: &fluxdb_app::AiMcpServerGrantItem,
    colors: UiColors,
    this: WeakEntity<NavicatMain>,
) -> gpui::AnyElement {
    let selected = grant.enabled;
    let id = grant.id.clone();
    let scope = ai_mcp_object_scope_summary(grant);
    let target = if grant.resource_kind == "redis" {
        format!("{} · DB {}", grant.connection_name, grant.database_name)
    } else if grant.namespace.is_empty() {
        format!("{} · {}", grant.connection_name, grant.database_name)
    } else {
        format!(
            "{} · {} · {}",
            grant.connection_name, grant.database_name, grant.namespace
        )
    };
    let edit_toggle = this.clone();
    let toggle = this.clone();
    let revision = grant.revision;
    let edit_id = grant.id.clone();
    let toggle_id = edit_id.clone();
    let edit_debug_id = edit_id.clone();
    let toggle_debug_id = edit_id.clone();
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
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child(target),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(format!(
                            "{scope} · 最多 {} 行",
                            grant.max_rows
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ai_pill(
                    if grant.allow_write { "读写" } else { "只读" },
                    if grant.allow_write {
                        colors.warning
                    } else {
                        colors.success
                    },
                    colors,
                ))
                .when(!grant.allow_write, |this| {
                    this.child(ai_pill("无写授权", colors.muted, colors))
                }),
        )
        .child(
            div()
                .w(px(96.))
                .flex()
                .items_center()
                .justify_end()
                .child(
                    Button::new(SharedString::from(format!("ai-mcp-grant-edit:{id}")))
                        .small()
                        .ghost()
                        .debug_selector(move || format!("ai-mcp-grant-edit:{}", edit_debug_id))
                        .child("编辑")
                        .on_click(move |_, window, cx| {
                            if let Some(view) = edit_toggle.upgrade() {
                                view.update(cx, |this, cx| {
                                    this.open_ai_mcp_grant_editor(Some(edit_id.clone()), window, cx);
                                    cx.notify();
                                });
                            }
                        }),
                ),
        )
        .child(
            div()
                .w(px(72.))
                .flex()
                .items_center()
                .justify_end()
                .child(
                    div()
                        .debug_selector(move || format!("ai-mcp-grant-toggle:{}", toggle_debug_id))
                        .child(
                            Switch::new(SharedString::from(format!("ai-mcp-grant-toggle:{id}")))
                                .checked(selected)
                                .on_click(move |checked, _, cx| {
                                    if let Some(view) = toggle.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.toggle_ai_mcp_grant(
                                                &toggle_id,
                                                *checked,
                                                revision,
                                                cx,
                                            );
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                ),
        )
        .into_any_element()
}

/// Profile 编辑器：启用、写模式、工具白名单与限额；失败保留草稿并内联提示。
fn ai_mcp_profile_editor_form(
    editor: &AiMcpServerProfileEditor,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let (write_label, write_enabled) = ai_mcp_write_mode_label(&editor.write_mode);
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

    // 工具白名单：只列对外 MCP 名称，空集合明确提示"不开放任何工具"。
    let mut tool_list = div().flex().flex_col().gap_1();
    for (name, desc) in AI_MCP_SERVER_TOOLS {
        let name_owned = (*name).to_string();
        let checked = editor.selected_tools.contains(*name);
        tool_list = tool_list.child(
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
                        .child(*name),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(*desc),
                )
                .child(
                    Switch::new(SharedString::from(format!("ai-mcp-tool:{name}")))
                        .checked(checked)
                        .on_click(cx.listener(move |this, checked, _, cx| {
                            if let Some(editor) = this.ai_mcp_server_profile_editor.as_mut() {
                                if *checked {
                                    editor.selected_tools.insert(name_owned.clone());
                                } else {
                                    editor.selected_tools.remove(name_owned.as_str());
                                }
                            }
                            cx.notify();
                        })),
                ),
        );
    }
    let tool_empty_hint = div()
        .rounded(colors.radius)
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(colors.warning.alpha(0.12))
        .child(app_icon(AppIcon::Info, 14., colors.warning))
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.warning)
                .child("未勾选任何工具：该 Profile 不会对外开放任何能力（空表示不放行，不是全部）"),
        );

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(ai_mcp_field(
            "ai-mcp-profile-name",
            "名称",
            &editor.inputs.display_name,
            colors,
        ))
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
                        .child("启用 Server Profile"),
                )
                .child(
                    Switch::new("ai-mcp-profile-enabled")
                        .checked(editor.enabled)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            if let Some(editor) = this.ai_mcp_server_profile_editor.as_mut() {
                                editor.enabled = *checked;
                            }
                            cx.notify();
                        })),
                ),
        )
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
                        .child("写模式"),
                )
                .child(
                    div()
                        .debug_selector(|| "ai-mcp-write-mode".into())
                        .rounded(colors.radius)
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.input_bg)
                        .px_3()
                        .h(px(34.))
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
                                .child(write_label),
                        )
                        .child(app_icon(
                            if editor.write_mode_menu_open {
                                AppIcon::ChevronUp
                            } else {
                                AppIcon::ChevronDown
                            },
                            14.,
                            colors.muted,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if let Some(editor) =
                                    this.ai_mcp_server_profile_editor.as_mut()
                                {
                                    editor.write_mode_menu_open = !editor.write_mode_menu_open;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ),
                ),
        )
        .when(editor.write_mode_menu_open, |this| {
            let mut menu = ai_default_options(colors);
            for (mode, label, enabled) in [
                ("disabled", "只读（写工具不开放）", true),
                ("scoped", "受限写（用户预授权范围）", true),
                ("desktop_approval", "桌面审批桥（当前未实现）", false),
            ] {
                let mode_owned = mode.to_string();
                let selected = editor.write_mode == mode;
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("ai-mcp-write-mode-opt:{mode}")))
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
                                .child(label),
                        )
                        .when(!enabled, |this| {
                            this.child(ai_pill("未实现", colors.muted, colors))
                        })
                        .when(selected, |this| {
                            this.child(app_icon(AppIcon::Check, 14., colors.info))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if !enabled {
                                    this.show_message(
                                        "桌面审批桥尚未实现，不能启用该写模式",
                                        AppMessageKind::Warning,
                                        cx,
                                    );
                                    return;
                                }
                                if let Some(editor) =
                                    this.ai_mcp_server_profile_editor.as_mut()
                                {
                                    editor.write_mode = mode_owned.clone();
                                    editor.write_mode_menu_open = false;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ),
                );
            }
            this.child(menu)
        })
        .when(!write_enabled, |this| {
            this.child(
                div()
                    .rounded(colors.radius)
                    .px_3()
                    .py_2()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("该写模式依赖桌面审批桥，当前版本明确不支持，不伪装已实现"),
            )
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .id("ai-mcp-tools-toggle")
                        .debug_selector(|| "ai-mcp-tools-toggle".into())
                        .h(px(28.))
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .child(format!(
                            "对外工具白名单（已选 {} / {}）",
                            editor.selected_tools.len(),
                            AI_MCP_SERVER_TOOLS.len()
                        ))
                        .child(app_icon(
                            if editor.tools_expanded {
                                AppIcon::ChevronUp
                            } else {
                                AppIcon::ChevronDown
                            },
                            14.,
                            colors.muted,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if let Some(editor) =
                                    this.ai_mcp_server_profile_editor.as_mut()
                                {
                                    editor.tools_expanded = !editor.tools_expanded;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ),
                )
                .when(editor.selected_tools.is_empty(), |this| this.child(tool_empty_hint))
                .when(editor.tools_expanded, |this| this.child(tool_list)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child("执行限额"),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(ai_mcp_field(
                            "ai-mcp-profile-max-inflight",
                            "并发请求",
                            &editor.inputs.max_inflight,
                            colors,
                        ))
                        .child(ai_mcp_field(
                            "ai-mcp-profile-max-request-ms",
                            "单请求时限(ms)",
                            &editor.inputs.max_request_ms,
                            colors,
                        ))
                        .child(ai_mcp_field(
                            "ai-mcp-profile-max-request-bytes",
                            "请求字节上限",
                            &editor.inputs.max_request_bytes,
                            colors,
                        ))
                        .child(ai_mcp_field(
                            "ai-mcp-profile-max-response-bytes",
                            "响应字节上限",
                            &editor.inputs.max_response_bytes,
                            colors,
                        )),
                ),
        )
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
                    Button::new("ai-mcp-profile-cancel")
                        .small()
                        .ghost()
                        .child("取消")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_mcp_server_profile_editor = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-mcp-profile-save")
                        .small()
                        .primary()
                        .debug_selector(|| "ai-mcp-profile-save".into())
                        .child(if editor.profile_id.is_none() {
                            "创建 Profile"
                        } else {
                            "保存 Profile"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_ai_mcp_profile_editor(cx);
                        })),
                ),
        )
}

/// 资源授权编辑器：连接、库/DB、读/写、对象/Key 范围。
fn ai_mcp_grant_editor_form(
    state: &AppState,
    editor: &AiMcpServerGrantEditor,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
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
    // Redis 只暴露 Key 前缀规则；关系型只暴露表清单规则（设计 §28.6.1）。
    let scope_field = if editor.resource_kind == "redis" {
        ai_mcp_field(
            "ai-mcp-grant-key-prefixes",
            "Key 前缀（逗号分隔）",
            &editor.inputs.key_prefixes,
            colors,
        )
    } else {
        ai_mcp_field(
            "ai-mcp-grant-tables",
            "限定表（逗号分隔）",
            &editor.inputs.table_allowlist,
            colors,
        )
    };
    div()
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
                        .child("连接"),
                )
                .child(
                    div()
                        .debug_selector(|| "ai-mcp-grant-connection".into())
                        .rounded(colors.radius)
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.input_bg)
                        .px_3()
                        .h(px(34.))
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
                                .child(editor.connection_label.clone()),
                        )
                        .child(app_icon(
                            if editor.connection_menu_open {
                                AppIcon::ChevronUp
                            } else {
                                AppIcon::ChevronDown
                            },
                            14.,
                            colors.muted,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                    grant.connection_menu_open = !grant.connection_menu_open;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ),
                ),
        )
        .when(editor.connection_menu_open, |this| {
            let connections: Vec<(i64, String, String)> = state
                .connections
                .iter()
                .map(|c| {
                    (
                        c.config.id.0 as i64,
                        c.config.name.clone(),
                        if c.config.kind == fluxdb_core::DatabaseKind::Redis {
                            "redis".to_string()
                        } else {
                            "sql".to_string()
                        },
                    )
                })
                .collect();
            let mut menu = ai_default_options(colors);
            for (id, name, kind) in connections {
                let id_owned = id;
                let name_owned = name.clone();
                let kind_owned = kind.clone();
                let selected = editor.connection_id == id;
                let option_debug_id = format!("ai-mcp-grant-conn-opt:{id}");
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("ai-mcp-grant-conn-opt:{id}")))
                        .debug_selector(move || option_debug_id.clone())
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
                                .child(format!(
                                    "{} · {}",
                                    name,
                                    if kind == "redis" { "Redis" } else { "关系型" }
                                )),
                        )
                        .when(selected, |this| {
                            this.child(app_icon(AppIcon::Check, 14., colors.info))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                    grant.connection_id = id_owned;
                                    grant.connection_label = name_owned.clone();
                                    grant.resource_kind = kind_owned.clone();
                                    grant.connection_menu_open = false;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ),
                );
            }
            this.child(menu)
        })
        .child(
            div()
                .flex()
                .gap_3()
                .child(ai_mcp_field(
                    "ai-mcp-grant-database",
                    if editor.resource_kind == "redis" {
                        "Redis DB"
                    } else {
                        "库名"
                    },
                    &editor.inputs.database_name,
                    colors,
                ))
                .child(ai_mcp_field(
                    "ai-mcp-grant-namespace",
                    "Schema（可选）",
                    &editor.inputs.namespace,
                    colors,
                )),
        )
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
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child("启用授权"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child("关闭后该资源不可被外部调用"),
                        ),
                )
                .child(
                    Switch::new("ai-mcp-grant-enabled")
                        .checked(editor.enabled)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                grant.enabled = *checked;
                            }
                            cx.notify();
                        })),
                ),
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
                        .child("允许读取"),
                )
                .child(
                    Switch::new("ai-mcp-grant-read")
                        .checked(editor.allow_read)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                grant.allow_read = *checked;
                            }
                            cx.notify();
                        })),
                ),
        )
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
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child("允许写入"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child("仍需 Profile 为 scoped 且写规则命中；意图文字不是权限"),
                        ),
                )
                .child(
                    Switch::new("ai-mcp-grant-write")
                        .checked(editor.allow_write)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                grant.allow_write = *checked;
                            }
                            cx.notify();
                        })),
                ),
        )
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
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child(if editor.resource_kind == "redis" {
                                    "整个 DB"
                                } else {
                                    "整库"
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child("关闭则只放行下方明确列出的范围"),
                        ),
                )
                .child(
                    Switch::new("ai-mcp-grant-all")
                        .checked(editor.all_scope)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            if let Some(grant) = this.ai_mcp_server_grant_editor.as_mut() {
                                grant.all_scope = *checked;
                            }
                            cx.notify();
                        })),
                ),
        )
        .when(!editor.all_scope, |this| this.child(scope_field))
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
                        .child("单次最大行数"),
                )
                .child(
                    div()
                        .w(px(120.))
                        .debug_selector(|| "ai-mcp-grant-max-rows".into())
                        .child(Input::new(&editor.inputs.max_rows).text_size(px(13.))),
                ),
        )
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
                        .child("受限写规则 JSON（可选）"),
                )
                .child(
                    div()
                        .debug_selector(|| "ai-mcp-grant-write-policy".into())
                        .child(
                            gpui_component::input::Textarea::new(&editor.inputs.write_policy)
                                .h(px(70.)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("留空按 {} 保存；只有 allow_write 且规则命中时才可能执行写"),
                ),
        )
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
                    Button::new("ai-mcp-grant-cancel")
                        .small()
                        .ghost()
                        .child("取消")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_mcp_server_grant_editor = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-mcp-grant-save")
                        .small()
                        .primary()
                        .debug_selector(|| "ai-mcp-grant-save".into())
                        .child(if editor.grant_id.is_none() {
                            "创建授权"
                        } else {
                            "保存授权"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_ai_mcp_grant_editor(cx);
                        })),
                ),
        )
}

/// 「本地 MCP」页签：左侧 Profile 列表，右侧编辑表单 / 资源授权 / 活动记录。
fn ai_mcp_panel(
    state: &AppState,
    panel: &AiSettingsPanelState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected_profile_id = panel
        .mcp_server_profile_editor
        .as_ref()
        .and_then(|editor| editor.profile_id.clone());

    // 左侧：Profile 列表 + 新建。
    let profile_rows: Vec<gpui::AnyElement> = if state.ai_mcp_server_profiles.is_empty() {
        vec![ai_empty_state("暂无 Server Profile。点击「新建」创建。", colors)]
    } else {
        state
            .ai_mcp_server_profiles
            .iter()
            .map(|profile| {
                let id = profile.id.clone();
                let selected = selected_profile_id.as_deref() == Some(profile.id.as_str());
                let (write_label, _) = ai_mcp_write_mode_label(&profile.write_mode);
                div()
                    .id(SharedString::from(format!("ai-mcp-profile-item:{}", profile.id)))
                    .debug_selector(move || format!("ai-mcp-profile-item:{}", profile.id))
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
                            .child(profile.display_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(format!(
                                "{} · {} · {} 工具",
                                if profile.enabled { "已启用" } else { "已关闭" },
                                write_label,
                                profile.allowed_tool_ids.len()
                            )),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.open_ai_mcp_profile_editor(Some(id.clone()), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    };

    // 右侧：选中 Profile 的编辑表单 / 授权 / 活动。
    let body: gpui::AnyElement = match panel.mcp_server_profile_editor.as_ref() {
        None => ai_empty_state("从左侧选择一个 Server Profile，或点击「新建」。", colors),
        Some(profile_editor) => {
            let profile_id = profile_editor.profile_id.clone().unwrap_or_default();
            let grants = state
                .ai_mcp_server_grants
                .iter()
                .filter(|grant| grant.profile_id == profile_id)
                .collect::<Vec<_>>();
            let requests = state
                .ai_mcp_server_requests
                .iter()
                .filter(|request| request.profile_id == profile_id)
                .collect::<Vec<_>>();
            let this = cx.entity().downgrade();
            let grant_rows: Vec<gpui::AnyElement> = if grants.is_empty() {
                vec![ai_empty_state("该 Profile 尚无授权资源。外部调用不会看到任何连接。", colors)]
            } else {
                grants
                    .iter()
                    .map(|grant| ai_mcp_grant_row(grant, colors, this.clone()))
                    .collect()
            };
            let request_rows: Vec<gpui::AnyElement> = if requests.is_empty() {
                vec![div()
                    .debug_selector(|| "ai-mcp-activity-empty".into())
                    .child(ai_empty_state(
                        "暂无活动记录。外部 Client 调用会显示在这里。",
                        colors,
                    ))
                    .into_any_element()]
            } else {
                requests
                    .iter()
                    .take(50)
                    .map(|request| {
                        let target = if request.target_json == "{}" {
                            "—".to_string()
                        } else {
                            request.target_json.clone()
                        };
                        div()
                            .border_t_1()
                            .border_color(colors.border_soft)
                            .px_4()
                            .py_2()
                            .min_h(px(44.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(220.))
                                    .text_size(px(12.))
                                    .text_color(colors.text)
                                    .child(request.tool_id.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(target),
                            )
                            .child(
                                div()
                                    .w(px(150.))
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(ai_session_timestamp(request.started_at_ms)),
                            )
                            .debug_selector(|| "ai-mcp-activity-row".into())
                            .child(ai_mcp_request_status_pill(&request.status, colors))
                            .into_any_element()
                    })
                    .collect()
            };

            let profile_form = ai_mcp_profile_editor_form(profile_editor, colors, cx);
            // 授权编辑器（可展开）在 Profile 表单之前：它的保存栏必须在首屏可达，
            // 否则用户改完授权要滚动整张 Profile 表单才能提交。
            let grant_editor_body: gpui::AnyElement = match panel.mcp_server_grant_editor.as_ref() {
                None => div().flex_1().into_any_element(),
                Some(grant_editor) => {
                    ai_mcp_grant_editor_form(state, grant_editor, colors, cx).into_any_element()
                }
            };

            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    settings_panel_group("资源授权", colors)
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
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(colors.muted)
                                        .child(format!("{} 条授权", grants.len())),
                                )
                                .child(
                                    Button::new("ai-mcp-grant-new")
                                        .small()
                                        .ghost()
                                        .debug_selector(|| "ai-mcp-grant-new".into())
                                        .child("新增授权")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_ai_mcp_grant_editor(None, window, cx);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(div().children(grant_rows)),
                )
                .when(
                    panel.mcp_server_grant_editor.is_some(),
                    |this| this.child(grant_editor_body),
                )
                .child(
                    settings_panel_group("活动记录", colors)
                        .child(
                            div()
                                .border_t_1()
                                .border_color(colors.border_soft)
                                .px_4()
                                .py_2()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(ai_table_head("工具", 220., colors))
                                .child(ai_table_head("目标", 220., colors))
                                .child(ai_table_head("时间", 150., colors))
                                .child(ai_table_head("状态", 120., colors))
                                .child(div().flex_1()),
                        )
                        .child(div().children(request_rows)),
                )
                .child(profile_form)
                .into_any_element()
        }
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
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child("本地 MCP Server（stdio）"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("配置对外服务、授权资源并查看调用活动；导出客户端配置不在本页"),
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
                        .child(
                            div()
                                .border_b_1()
                                .border_color(colors.border_soft)
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(colors.text)
                                        .child("Server Profile"),
                                )
                                .child(
                                    Button::new("ai-mcp-profile-new")
                                        .small()
                                        .primary()
                                        .debug_selector(|| "ai-mcp-profile-new".into())
                                        .child("新建")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_ai_mcp_profile_editor(None, window, cx);
                                        })),
                                ),
                        )
                        .children(profile_rows),
                )
                .child(div().flex_1().min_w(px(0.)).child(body)),
        )
}
