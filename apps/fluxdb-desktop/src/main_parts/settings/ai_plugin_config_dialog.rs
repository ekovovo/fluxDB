// 插件「管理」弹框：宿主按插件自己声明的配置渲染表单（设计 §24）。
//
// 字段、标签、候选与校验全部来自应用层的配置声明（内置能力读代码注册表，外置插件读
// manifest 的 `config_declaration`）。这里不判断插件 id、不自己拼资源列表，因此新增
// 可配置插件不需要改这个文件；弹框只提交「字段键 + 选择」，落库口径由应用层决定。
//
// 视觉：标题区（图标 + 标题 + 一行副标题）/ 表单区（左标签、右控件）/ 底部操作区。
// 候选用浮层下拉（`dropdown_menu_with_anchor`）：展开不改变弹框高度，选项多也不会把
// 弹框撑成一条。

/// 插件管理弹框的数据；`None` 表示未打开，不常驻内存。
#[derive(Clone)]
struct AiPluginConfigDialog {
    instance_id: String,
    display_name: String,
    /// 与插件行一致的图标，避免弹框标题区与来源行对不上。
    icon: AppIcon,
    /// 打开时的字段快照（含宿主解析好的候选项）。
    fields: Vec<fluxdb_app::AiPluginConfigFieldItem>,
    /// 草稿选择：字段键 → 资源 ID；`None` 表示清空该字段。
    drafts: BTreeMap<String, Option<String>>,
    /// 运行设置 CAS 期望值（打开时快照）。
    expected_revision: i64,
    /// 内联错误：声明校验失败、CAS 冲突等；有值时保持弹框打开并保留草稿。
    error: Option<String>,
}

impl AiPluginConfigDialog {
    fn new(item: &fluxdb_app::AiPluginItem, expected_revision: i64) -> Self {
        let drafts = item
            .config_fields
            .iter()
            .map(|field| (field.key.clone(), field.selection.clone()))
            .collect();
        Self {
            instance_id: item.id.clone(),
            display_name: item.display_name.clone(),
            icon: ai_builtin_icon(&item.package_key),
            fields: item.config_fields.clone(),
            drafts,
            expected_revision,
            error: None,
        }
    }

    /// 草稿里某个字段的展示文案；选择已不在候选里时如实说明，不假装是选项。
    fn selection_label(&self, field: &fluxdb_app::AiPluginConfigFieldItem) -> String {
        let Some(id) = self.drafts.get(&field.key).and_then(|value| value.as_deref()) else {
            return "未配置".to_string();
        };
        field
            .choices
            .iter()
            .find(|choice| choice.id == id)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| "已选择（当前不可用）".to_string())
    }

    /// 提交给应用层的字段选择；清空与未选都按「未配置」提交。
    fn selections(&self) -> Vec<fluxdb_app::AiPluginFieldSelection> {
        self.fields
            .iter()
            .map(|field| fluxdb_app::AiPluginFieldSelection {
                key: field.key.clone(),
                resource_id: self
                    .drafts
                    .get(&field.key)
                    .and_then(|value| value.clone()),
            })
            .collect()
    }
}

/// 插件管理弹框；未打开时不渲染。
fn ai_plugin_config_dialog(
    dialog: Option<AiPluginConfigDialog>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Option<gpui::AnyElement> {
    let dialog = dialog.as_ref()?;
    let this = cx.entity().downgrade();
    let mask = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .bg(if colors.is_dark {
            hsla(220. / 360., 0.12, 0.08, 0.54)
        } else {
            hsla(210. / 360., 0.20, 0.20, 0.16)
        })
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.ai_plugin_config_dialog = None;
                cx.notify();
                cx.stop_propagation();
            }),
        );

    let mut body = div().flex().flex_col().gap_4();
    for field in &dialog.fields {
        body = body.child(ai_plugin_config_field(field, dialog, colors, cx));
    }

    let header = div()
        .px_5()
        .py_4()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(app_icon_box(dialog.icon, 30., 16., colors.info))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("管理 {}", dialog.display_name)),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child("配置项由插件声明，候选项与校验由 fluxDB 解析。"),
                        ),
                ),
        )
        .child(
            Button::new("ai-plugin-config-close")
                .debug_selector(|| "ai-plugin-config-close".into())
                .small()
                .ghost()
                .icon(IconName::Close)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ai_plugin_config_dialog = None;
                    cx.notify();
                })),
        );

    let button_row = div()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .child(
            Button::new("ai-plugin-config-cancel")
                .debug_selector(|| "ai-plugin-config-cancel".into())
                .small()
                .ghost()
                .child("取消")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ai_plugin_config_dialog = None;
                    cx.notify();
                })),
        )
        .child(
            Button::new("ai-plugin-config-save")
                .debug_selector(|| "ai-plugin-config-save".into())
                .small()
                .primary()
                .child("保存")
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |this, cx| this.submit_ai_plugin_config(cx));
                }),
        );

    let footer = div()
        .border_t_1()
        .border_color(colors.border_soft)
        .px_5()
        .py_3()
        .flex()
        .flex_col()
        .gap_2()
        .when_some(dialog.error.clone(), |this, message| {
            this.child(
                div()
                    .debug_selector(|| "ai-plugin-config-error".into())
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(app_icon(AppIcon::CircleSlash, 14., colors.danger))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.danger)
                            .child(message),
                    ),
            )
        })
        .child(button_row);

    let panel = div()
        .w(px(520.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.border)
        .bg(colors.panel_bg)
        .shadow(vec![box_shadow(
            px(0.),
            px(18.),
            px(48.),
            px(0.),
            hsla(220. / 360., 0.3, 0.1, 0.28),
        )])
        .flex()
        .flex_col()
        .overflow_hidden()
        // 内部点击不穿透到遮罩。
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, _, cx| cx.stop_propagation()),
        )
        .on_action(cx.listener(|this, _: &CancelDialog, _, cx| {
            // Esc 等同取消，不写入任何数据。
            this.ai_plugin_config_dialog = None;
            cx.notify();
            cx.stop_propagation();
        }))
        .child(header)
        .child(div().px_5().pb_5().child(body))
        .child(footer);
    Some(mask.child(panel).into_any_element())
}

/// 一个字段：左侧标签（含必填标记），右侧选择器与说明。
fn ai_plugin_config_field(
    field: &fluxdb_app::AiPluginConfigFieldItem,
    dialog: &AiPluginConfigDialog,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let unset = dialog
        .drafts
        .get(&field.key)
        .and_then(|value| value.as_deref())
        .is_none();
    let label = dialog.selection_label(field);
    let choices = field
        .choices
        .iter()
        .map(|choice| (choice.id.clone(), choice.label.clone(), choice.detail.clone()))
        .collect::<Vec<_>>();
    let key = field.key.clone();
    let current_key = field.key.clone();
    let current = dialog
        .drafts
        .get(&field.key)
        .and_then(|value| value.clone());
    let owner = cx.entity().downgrade();
    let trigger_id = format!("ai-plugin-config-field:{}", field.key);
    let trigger = Button::new(SharedString::from(trigger_id.clone()))
        .debug_selector({
            let id = trigger_id.clone();
            move || id.clone()
        })
        .small()
        .cursor_pointer()
        .w_full()
        .h(px(34.))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .min_w(px(0.))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(if unset { colors.muted } else { colors.text })
                        .child(label),
                )
                .child(app_icon(AppIcon::ChevronDown, 14., colors.muted)),
        )
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(px(280.)).max_w(px(340.));
            let selected = current.clone();
            for (id, name, detail) in &choices {
                let owner = owner.clone();
                let field_key = current_key.clone();
                let value = id.clone();
                let name = name.clone();
                let detail = detail.clone();
                let checked = selected.as_deref() == Some(id.as_str());
                let option_id = format!("ai-plugin-config-option:{}:{}", key, id);
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id(SharedString::from(option_id.clone()))
                            .debug_selector({
                                let id = option_id.clone();
                                move || id.clone()
                            })
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(name.clone())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(detail.clone()),
                            )
                            .test_support()
                    })
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            this.set_ai_plugin_config_draft(&field_key, Some(value.clone()), cx)
                        });
                    }),
                );
            }
            let owner = owner.clone();
            let field_key = current_key.clone();
            let clear_id = format!("ai-plugin-config-clear:{}", key);
            menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .id(SharedString::from(clear_id.clone()))
                        .debug_selector({
                            let id = clear_id.clone();
                            move || id.clone()
                        })
                        .text_color(colors.muted)
                        .child("未配置（不使用）")
                        .test_support()
                })
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.set_ai_plugin_config_draft(&field_key, None, cx)
                    });
                }),
            )
        });

    let mut controls = div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap_2()
        .child(trigger)
        // 候选口径由宿主按声明生成：插件只声明需求，不自己解释过滤规则。
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(field.hint.clone()),
        );
    // 插件自己写的后果说明与「选择失效」原因统一成软底注释块：这里说的是"会发生什么"，
    // 与上面那行"为什么只有这些候选"分层，避免两段同色灰字连读。
    if let Some(help) = field.help.clone() {
        controls = controls.child(ai_plugin_config_note(
            AppIcon::Info,
            colors.muted,
            help,
            colors,
        ));
    }
    if let Some(reason) = field.rejection.clone() {
        controls = controls.child(ai_plugin_config_note(
            AppIcon::CircleSlash,
            colors.warning,
            reason,
            colors,
        ));
    }

    div()
        .flex()
        .items_start()
        .gap_4()
        .child(
            div()
                .w(px(92.))
                .flex_shrink_0()
                .pt(px(9.))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(colors.text)
                        .child(field.label.clone()),
                )
                .when(field.required, |this| {
                    this.child(ai_pill("必填", colors.warning, colors))
                }),
        )
        .child(controls)
        .into_any_element()
}

/// 字段注释：软底 + 图标的说明块（提示与警示共用一套，只有图标与颜色不同）。
fn ai_plugin_config_note(
    icon: AppIcon,
    color: gpui::Rgba,
    text: String,
    colors: UiColors,
) -> Div {
    div()
        .rounded(colors.radius)
        .bg(colors.border_soft)
        .px_3()
        .py_2()
        .flex()
        .items_start()
        .gap_2()
        .child(app_icon(icon, 13., color))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(11.))
                .text_color(color)
                .child(text),
        )
}

impl NavicatMain {
    /// 打开插件管理弹框；字段与候选取当前列表快照（弹框内不再查库）。
    ///
    /// 无配置字段或声明解析失败的插件没有管理入口，这里也拒绝打开，避免出现空表单。
    fn open_ai_plugin_config_dialog(&mut self, instance_id: &str, cx: &mut Context<Self>) {
        let expected_revision = self.controller.state().ai_runtime_revision;
        let item = self
            .controller
            .state()
            .ai_plugin_instances
            .iter()
            .find(|plugin| plugin.id == instance_id)
            .cloned();
        let Some(item) = item else { return };
        if item.config_fields.is_empty() || item.config_error.is_some() {
            return;
        }
        self.ai_plugin_config_dialog = Some(AiPluginConfigDialog::new(&item, expected_revision));
        cx.notify();
    }

    /// 改一个字段的草稿选择（不落库，保存时才提交）。
    fn set_ai_plugin_config_draft(
        &mut self,
        key: &str,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.ai_plugin_config_dialog.as_mut() {
            dialog.drafts.insert(key.to_string(), value);
            dialog.error = None;
        }
        cx.notify();
    }

    /// 提交弹框：一次写入所有字段，只有保存成功才关框，失败保留草稿并内联提示。
    fn submit_ai_plugin_config(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_plugin_config_dialog.clone() else {
            return;
        };
        let event = self.controller.dispatch(AppCommand::SaveAiPluginConfig {
            instance_id: dialog.instance_id.clone(),
            selections: dialog.selections(),
            expected_revision: dialog.expected_revision,
        });
        match event {
            AppEvent::AiPluginConfigSaved { .. } => self.ai_plugin_config_dialog = None,
            AppEvent::AiSettingsError(message) => {
                if let Some(dialog) = self.ai_plugin_config_dialog.as_mut() {
                    dialog.error = Some(message);
                }
            }
            _ => {}
        }
        cx.notify();
    }
}
