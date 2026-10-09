// 通用操作审批：目标、计划原文、必要风险提示与单次授权按钮。
// 数据只来自已持久化的审批片段与本轮固定目标，不改变内核授权校验。

/// 审批卡需要的连接事实（UI 从 AppState 解析，不从 payload 里取，避免卡片自说自话）。
struct AiApprovalContext<'a> {
    /// 连接名 + 类型（"Docker MySQL · 测试库"里的连接环境部分）。
    connection: Option<&'a fluxdb_core::ConnectionConfig>,
    /// 数据库 / schema / Redis DB / 对象。
    database: Option<String>,
    schema: Option<String>,
    redis_db: Option<u64>,
    object: Option<String>,
    busy: bool,
}

impl AiApprovalContext<'_> {
    /// 上下文行的值文本；没有可展示的定位信息时返回 None，由 UI 整行省略。
    fn scope_value(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(database) = self.database.as_deref().filter(|value| !value.is_empty()) {
            parts.push(database.to_string());
        }
        if let Some(schema) = self.schema.as_deref().filter(|value| !value.is_empty()) {
            parts.push(schema.to_string());
        }
        if let Some(redis_db) = self.redis_db {
            parts.push(format!("DB {redis_db}"));
        }
        if let Some(object) = self.object.as_deref().filter(|value| !value.is_empty()) {
            parts.push(object.to_string());
        }
        (!parts.is_empty()).then(|| parts.join(" / "))
    }
}

/// 从本轮目标解析审批卡的连接事实：连接名、库/schema。
///
/// 连接缺失或已删除时明确提示不可用，不换成当前界面上的其它连接。
fn ai_approval_context<'a>(
    target_json: Option<&str>,
    connections: &'a [fluxdb_app::ConnectionState],
    busy: bool,
) -> AiApprovalContext<'a> {
    let target: serde_json::Value = target_json
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(serde_json::Value::Null);
    let connection = target
        .get("connection_id")
        .and_then(serde_json::Value::as_i64)
        .and_then(|id| {
            connections
                .iter()
                .map(|state| &state.config)
                .find(|config| i64::try_from(config.id.0).ok() == Some(id))
        });
    AiApprovalContext {
        connection,
        database: target
            .get("database")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        schema: target
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        redis_db: target.get("redis_db").and_then(serde_json::Value::as_u64),
        object: target
            .get("object")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        busy,
    }
}

/// 风险色只强调徽标；卡片统一用琥珀边框，不把待决定状态渲染成执行失败。
fn ai_approval_risk_badge(risk: &str, colors: UiColors) -> gpui::AnyElement {
    let (label, tone) = match risk {
        "high" => ("风险：高", colors.danger),
        "low" => ("风险：低", colors.success),
        "medium" => ("风险：中", colors.warning),
        _ => ("风险：未知", colors.warning),
    };
    div()
        .id("risk")
        .aria_label(label)
        .test_support()
        .flex_none()
        .flex()
        .items_center()
        .px_1p5()
        .py(px(2.))
        .rounded(px(999.))
        .border_1()
        .border_color(tone.alpha(0.45))
        .bg(tone.alpha(0.10))
        .text_color(tone)
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
        .into_any_element()
}

/// 只读代码块：等宽字体 + 可复制。展示的就是即将执行的那一条，不是摘要。
///
/// 复制入口按片段与序号定位，避免相同长度语句共享交互身份。
fn ai_approval_code_block(
    text: &str,
    copy_id: String,
    index: usize,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let text = text.to_string();
    let copy_text = text.clone();
    div()
        .id(("statement", index))
        .aria_label(text.clone())
        .test_support()
        .mx_3()
        .mb_2()
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border_soft)
        .bg(colors.canvas_bg)
        .px_2p5()
        .py_2()
        .child(
            div().flex().justify_end().mb_1().child(
                Button::new(SharedString::from(format!(
                    "ai-approval-copy:{copy_id}:{index}"
                )))
                .debug_selector({
                    let id = format!("ai-approval-copy:{copy_id}:{index}");
                    move || id.clone()
                })
                .xsmall()
                .ghost()
                .rounded_md()
                .accessibility_id(format!("ai-approval-copy:{copy_id}:{index}"))
                .accessibility_label("复制内容")
                .cursor_pointer()
                .child(app_icon(AppIcon::Copy, 10., colors.muted))
                .child(div().text_size(px(10.)).child("复制内容"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                    this.show_message("内容已复制", AppMessageKind::Success, cx);
                })),
            ),
        )
        .child(
            div()
                .max_h(px(132.))
                .overflow_y_scrollbar()
                .text_size(px(11.))
                .line_height(px(17.))
                .font_family(PLATFORM_MONOSPACE_FONT)
                .text_color(colors.text)
                .child(text.clone()),
        )
        .into_any_element()
}

/// 上下文行的一格：固定标签宽度让多行值左对齐成列。
fn ai_approval_context_row(
    label: String,
    value: Option<String>,
    note: Option<String>,
    monospace: bool,
    colors: UiColors,
) -> gpui::AnyElement {
    let Some(value) = value else {
        return div().into_any_element();
    };
    let accessible_label = format!(
        "{label}：{value}{}",
        note.as_ref()
            .map(|note| format!("；{note}"))
            .unwrap_or_default()
    );
    div()
        .id(SharedString::from(label.clone()))
        .aria_label(accessible_label)
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(76.))
                .flex_none()
                .text_size(px(10.5))
                .text_color(colors.muted)
                .child(label),
        )
        .child(
            div()
                .min_w(px(0.))
                .flex_1()
                .flex()
                .items_center()
                .gap_1p5()
                .flex_wrap()
                .child(
                    div()
                        .min_w(px(0.))
                        .text_size(px(11.))
                        .text_color(colors.text)
                        .when(monospace, |row| row.font_family(PLATFORM_MONOSPACE_FONT))
                        .truncate()
                        .child(value),
                )
                .when_some(note, |row, note| {
                    row.child(
                        div()
                            .min_w(px(0.))
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .child(note),
                    )
                }),
        )
        .into_any_element()
}

/// 高风险操作保留内核判定依据，避免精简布局掩盖未知影响面。
fn ai_approval_guardrails(guardrails: &[String], colors: UiColors) -> gpui::AnyElement {
    div()
        .id("guardrails")
        .aria_label(format!("风险提示：{}", guardrails.join("；")))
        .test_support()
        .mx_3()
        .mb_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child("风险提示"),
        )
        .children(guardrails.iter().map(|guardrail| {
            let (icon, tone) = if guardrail.contains("已拒绝") {
                (AppIcon::CircleSlash, colors.danger)
            } else if guardrail.contains("未核实") || guardrail.contains("未记录") {
                (AppIcon::Info, colors.warning)
            } else {
                (AppIcon::Check, colors.info)
            };
            div()
                .flex()
                .items_start()
                .gap_1p5()
                .child(app_icon(icon, 11., tone))
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_1()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(colors.text)
                        .child(guardrail.clone()),
                )
        }))
        .into_any_element()
}

/// 原审批在决定后收成结果行；批准不代表执行成功，拒绝也不是执行失败。
fn ai_chat_approval_result(
    part: &fluxdb_app::AiConversationPart,
    payload: &serde_json::Value,
    decision: &str,
    expanded: bool,
    context: &AiApprovalContext<'_>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let (label, icon, tone) = match decision {
        "denied" => ("已拒绝 · 未执行", AppIcon::CircleSlash, colors.muted),
        "approved" => ("已批准一次", AppIcon::Check, colors.success),
        _ => ("审批已失效", AppIcon::Info, colors.warning),
    };
    let mut target = context
        .connection
        .map(|config| format!("{} · {}", config.name, database_kind_name(config.kind)))
        .unwrap_or_else(|| "目标连接不可用或未绑定".into());
    if let Some(scope) = context.scope_value() {
        target.push_str(&format!(" · {scope}"));
    }
    let id = part.id.clone();
    div()
        .id(SharedString::from(format!(
            "ai-approval-result:{}",
            part.id
        )))
        .flex()
        .flex_col()
        .min_w(px(0.))
        .child(
            Button::new(SharedString::from(format!("ai-part:{}", part.id)))
                .debug_selector(move || format!("ai-part:{id}"))
                .accessibility_label(format!("{label}，{target}"))
                .small()
                .ghost()
                .rounded_md()
                .w_full()
                .h(px(40.))
                .cursor_pointer()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(app_icon(icon, 12., tone))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .items_start()
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .line_height(px(16.))
                                        .text_color(tone)
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .max_w_full()
                                        .truncate()
                                        .text_size(px(10.))
                                        .line_height(px(14.))
                                        .text_color(colors.muted)
                                        .child(target),
                                ),
                        )
                        .child(app_icon(
                            if expanded {
                                AppIcon::ChevronDown
                            } else {
                                AppIcon::ChevronRight
                            },
                            10.,
                            colors.muted,
                        )),
                )
                .on_click(cx.listener({
                    let id = part.id.clone();
                    move |this, _, window, cx| {
                        let mut view = this.controller.state().ai_conversation.clone();
                        if let Some(index) = view.expanded_parts.iter().position(|part| part == &id)
                        {
                            view.expanded_parts.remove(index);
                        } else {
                            view.expanded_parts.push(id.clone());
                        }
                        this.controller.dispatch(AppCommand::AiConversation(
                            fluxdb_app::AiConversationCommand::Apply { view },
                        ));
                        this.save_ai_chat_ui(window, cx);
                        cx.notify();
                    }
                })),
        )
        .when(expanded, |column| {
            let statements = payload["details"]["statements"].as_array();
            let mut detail = div()
                .id(SharedString::from(format!("ai-part-detail:{}", part.id)))
                .aria_label("原执行内容")
                .test_support()
                .debug_selector({
                    let id = part.id.clone();
                    move || format!("ai-part-detail:{id}")
                })
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .mx_3()
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .child("原执行内容"),
                );
            let mut count = 0;
            if let Some(statements) = statements {
                for statement in statements.iter().filter_map(serde_json::Value::as_str) {
                    detail = detail.child(ai_approval_code_block(
                        statement,
                        part.id.clone(),
                        count,
                        colors,
                        cx,
                    ));
                    count += 1;
                }
            }
            if count == 0 {
                detail = detail.child(
                    div()
                        .mx_3()
                        .mb_2()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("该历史审批未保存执行内容。"),
                );
            }
            column.child(detail)
        })
        .into_any_element()
}

/// SQL、Redis argv 等计划共用审批布局；只显示有决策价值的事实。
///
/// 计划与分类来自审批片段，连接信息来自 AppState；不编造环境、影响行数或护栏。
fn ai_chat_approval_card(
    part_id: &str,
    payload: &serde_json::Value,
    context: &AiApprovalContext<'_>,
    revision: i64,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let approval_id = payload
        .get("approval_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let details = payload
        .get("details")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let statements: Vec<String> = details
        .get("statements")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut guardrails: Vec<String> = details
        .get("guardrails")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if guardrails.is_empty() {
        guardrails.push("操作影响尚未核实".into());
    }
    let estimated_rows = details
        .get("estimated_rows")
        .and_then(serde_json::Value::as_i64)
        .filter(|rows| *rows >= 0);
    let risk = details
        .get("risk")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let connection_label = match context.connection {
        Some(config) => format!("{} · {}", config.name, database_kind_name(config.kind)),
        None => "目标连接不可用或未绑定".to_string(),
    };
    let read_only = context
        .connection
        .is_some_and(|config| fluxdb_app::connection_is_read_only(config));
    // 紧凑信息头与风险徽标分开承载，窄面板下允许换行。
    let header = div()
        .id("heading")
        .aria_label("批准本次操作？")
        .test_support()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .bg(colors.warning.alpha(0.10))
        .border_b_1()
        .border_color(colors.warning.alpha(0.30))
        .child(app_icon(AppIcon::TriangleAlert, 13., colors.warning))
        .child(
            div()
                .min_w(px(0.))
                .flex_1()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.text)
                .child("批准本次操作？"),
        )
        .child(ai_approval_risk_badge(&risk, colors));

    let context_rows = div()
        .mx_3()
        .mt_2()
        .mb_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(ai_approval_context_row(
            "连接".into(),
            Some(connection_label),
            match context.connection {
                None => Some("无法核实可写状态".into()),
                Some(_) if read_only => Some("只读连接".into()),
                Some(_) => None,
            },
            false,
            colors,
        ))
        .child(ai_approval_context_row(
            "目标".into(),
            Some(context.scope_value().unwrap_or_else(|| "未记录".into())),
            None,
            false,
            colors,
        ));

    // 未核实的行数不生成固定占位卡；高风险仍明确显示内核依据。
    let impact = estimated_rows.map(|rows| {
        ai_approval_context_row(
            "预计影响行数".into(),
            Some(format!("{rows} 行")),
            None,
            false,
            colors,
        )
    });

    let approve_id = approval_id.clone();
    let deny_id = approval_id.clone();
    let actions = div()
        .flex()
        .items_center()
        .justify_end()
        .flex_wrap()
        .gap_2()
        .px_3()
        .pb_3()
        .when(context.busy, |row| {
            row.child(
                div()
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .child("正在提交审批…"),
            )
        })
        .child(
            Button::new(SharedString::from(format!("ai-deny:{approval_id}")))
                .debug_selector({
                    let id = approval_id.clone();
                    move || format!("ai-deny:{id}")
                })
                .small()
                .secondary()
                .rounded_md()
                .h(px(28.))
                .text_size(px(11.))
                .accessibility_id(format!("ai-deny:{approval_id}"))
                .label("拒绝")
                .disabled(context.busy)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.decide_ai_chat_approval(deny_id.clone(), false, revision, window, cx);
                })),
        )
        .child(
            Button::new(SharedString::from(format!("ai-approve:{approval_id}")))
                .debug_selector({
                    let id = approval_id.clone();
                    move || format!("ai-approve:{id}")
                })
                .small()
                .success()
                .rounded_md()
                .h(px(28.))
                .text_size(px(11.))
                .accessibility_id(format!("ai-approve:{approval_id}"))
                .label("批准一次")
                .disabled(context.busy)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.decide_ai_chat_approval(approve_id.clone(), true, revision, window, cx);
                })),
        );

    div()
        .id(SharedString::from(format!("ai-approval:{part_id}")))
        .debug_selector({
            let id = part_id.to_string();
            move || format!("ai-approval:{id}")
        })
        .my_1()
        .min_w(px(0.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.warning.alpha(0.55))
        .bg(colors.panel_bg)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(context_rows)
        .child(
            div()
                .mx_3()
                .mb_1()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child("执行内容"),
        )
        .when(!statements.is_empty(), |card| {
            card.children(statements.iter().enumerate().map(|(index, statement)| {
                ai_approval_code_block(statement, part_id.to_string(), index, colors, cx)
            }))
        })
        .when(statements.is_empty(), |card| {
            card.child(
                div()
                    .mx_3()
                    .mb_2()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("该历史审批未保存计划预览，请核对工具记录后再决定。"),
            )
        })
        .when_some(impact, |card, impact| {
            card.child(div().mx_3().mb_2().child(impact))
        })
        .when(risk == "high" || risk == "unknown", |card| {
            card.child(ai_approval_guardrails(&guardrails, colors))
        })
        .child(actions)
        .into_any_element()
}
