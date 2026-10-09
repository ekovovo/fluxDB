// 询问AI 内联批准卡片（设计 §31.9）：头部、上下文行、计划内容、影响预估、
// 护栏清单、提示与按钮。数据只来自已持久化的审批片段与本轮固定目标。

/// 审批卡需要的连接事实（UI 从 AppState 解析，不从 payload 里取，避免卡片自说自话）。
struct AiApprovalContext<'a> {
    /// 连接名 + 类型（"Docker MySQL · 测试库"里的连接环境部分）。
    connection: Option<&'a fluxdb_core::ConnectionConfig>,
    /// 数据库 / schema / Redis DB / 对象。
    database: Option<String>,
    schema: Option<String>,
    redis_db: Option<u64>,
    object: Option<String>,
    /// 当前进程内的连接修订；用于展示，不作为授权凭证。
    connection_revision: u64,
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

/// 从本轮目标解析审批卡的连接事实：连接名、库/schema、连接修订。
///
/// 连接缺失或已删除时明确提示不可用，不换成当前界面上的其它连接。
fn ai_approval_context<'a>(
    target_json: Option<&str>,
    connections: &'a [fluxdb_app::ConnectionState],
    connection_revision: impl Fn(fluxdb_core::ConnectionId) -> u64,
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
        connection_revision: connection
            .map(|config| connection_revision(config.id))
            .unwrap_or(0),
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
) -> gpui::AnyElement {
    let text = text.to_string();
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
            div()
                .max_h(px(132.))
                .overflow_y_scrollbar()
                .text_size(px(11.))
                .line_height(px(17.))
                .font_family(PLATFORM_MONOSPACE_FONT)
                .text_color(colors.text)
                .child(text.clone()),
        )
        .child(
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
            .accessibility_label("复制计划内容")
            .cursor_pointer()
            .mt_1()
            .child(app_icon(AppIcon::Copy, 10., colors.muted))
            .child(div().text_size(px(10.)).child("复制语句"))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
            }),
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

/// 影响预估一格（设计 §31.9 第 4 项）。行数未核实就写"未知"，不拿估算冒充已核实。
fn ai_approval_impact_cell(
    label: String,
    value: String,
    note: Option<String>,
    colors: UiColors,
) -> gpui::AnyElement {
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
        .flex_1()
        .min_w(px(0.))
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border_soft)
        .bg(colors.canvas_bg)
        .px_2p5()
        .py_2()
        .child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child(label),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.text)
                .child(value),
        )
        .when_some(note, |cell, note| {
            cell.child(
                div()
                    .mt(px(2.))
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .child(note),
            )
        })
        .into_any_element()
}

/// 护栏清单：命中项是内核已判定的事实，不是可勾选的开关（设计 §31.9 第 5 项）。
fn ai_approval_guardrails(guardrails: &[String], colors: UiColors) -> gpui::AnyElement {
    div()
        .id("guardrails")
        .aria_label(format!("约束清单：{}", guardrails.join("；")))
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
                .child("约束清单"),
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

/// 内联批准卡片（设计 §31.9）：头部、上下文、计划内容、影响预估、护栏清单、提示、按钮。
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
    let guardrails: Vec<String> = details
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
    let estimated_rows = details
        .get("estimated_rows")
        .and_then(serde_json::Value::as_i64)
        .filter(|rows| *rows >= 0);
    let impact_basis = details
        .get("impact_basis")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("未核实")
        .to_string();
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
    let revision_note = match context.connection {
        Some(_) => format!(
            "配置变更后请重新核对（当前 rev {}）；执行前复核目标与权限",
            context.connection_revision
        ),
        None => "目标连接已不可用，批准后会失败并要求重新选择目标".to_string(),
    };

    // 紧凑信息头与风险徽标分开承载，窄面板下允许换行。
    let header = div()
        .id("heading")
        .aria_label("需要你批准这次写入")
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
                .child("需要你批准这次写入"),
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
            "连接环境".into(),
            Some(connection_label),
            Some(
                match context.connection {
                    None => "无法核实可写状态",
                    Some(_) if read_only => "只读连接",
                    Some(_) => "可写连接",
                }
                .into(),
            ),
            false,
            colors,
        ))
        .child(ai_approval_context_row(
            "数据库".into(),
            Some(context.scope_value().unwrap_or_else(|| "未记录".into())),
            None,
            false,
            colors,
        ))
        .child(ai_approval_context_row(
            "目标连接配置".into(),
            Some(
                context
                    .connection
                    .map(|_| format!("rev {}", context.connection_revision))
                    .unwrap_or_else(|| "未知".into()),
            ),
            Some(revision_note.clone()),
            true,
            colors,
        ));

    let impact = div()
        .mx_3()
        .mb_2()
        .flex()
        .gap_2()
        .child(ai_approval_impact_cell(
            "预计影响行数".into(),
            estimated_rows
                .map(|rows| format!("{rows} 行"))
                .unwrap_or_else(|| "未知".into()),
            Some("执行后回显实际受影响行数".into()),
            colors,
        ))
        .child(ai_approval_impact_cell(
            "依据".into(),
            impact_basis,
            Some("不按 SQL 文本推算影响行数".into()),
            colors,
        ));

    let hint = div()
        .id("hint")
        .aria_label("Esc / 点击抽屉遮罩 = 拒绝本次；写操作失败不自动重放")
        .test_support()
        .mx_3()
        .mb_2()
        .flex()
        .items_start()
        .gap_1p5()
        .rounded(colors.radius)
        .bg(colors.warning.alpha(0.08))
        .border_l_2()
        .border_color(colors.warning.alpha(0.55))
        .px_2()
        .py_1p5()
        .child(app_icon(AppIcon::Info, 11., colors.warning))
        .child(
            div()
                .min_w(px(0.))
                .flex_1()
                .text_size(px(10.5))
                .line_height(px(15.))
                .text_color(colors.muted)
                .child("Esc / 点击抽屉遮罩 = 拒绝本次；写操作失败不自动重放。"),
        );

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
                .child("执行计划"),
        )
        .when(!statements.is_empty(), |card| {
            card.children(statements.iter().enumerate().map(|(index, statement)| {
                ai_approval_code_block(statement, part_id.to_string(), index, colors)
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
        .child(impact)
        .when(!guardrails.is_empty(), |card| {
            card.child(ai_approval_guardrails(&guardrails, colors))
        })
        .when(guardrails.is_empty(), |card| {
            card.child(ai_approval_guardrails(
                &["护栏命中情况未记录".into()],
                colors,
            ))
        })
        .child(hint)
        .child(
            div()
                .mx_3()
                .mb_2()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child("批准后仍会复核 plan_hash，参数/目标变更即失效"),
        )
        .child(actions)
        .into_any_element()
}
