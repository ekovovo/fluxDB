fn settings_appearance_panel(
    settings: &Settings,
    theme_mode: ThemeMode,
    colors: UiColors,
    radius_input: Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let accent: gpui::Rgba = ComponentTheme::global(cx).primary.into();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            settings_panel_group(tr("app.settings.appearance.group.language"), colors)
                .child(settings_language_row(&settings.locale, colors, cx)),
        )
        .child(
            settings_panel_group(tr("app.settings.appearance.group.theme"), colors)
                .child(settings_theme_mode_row(theme_mode, colors, cx))
                .child(settings_theme_palette(settings, theme_mode, colors, cx)),
        )
        .child(
            settings_panel_group(tr("app.settings.appearance.group.shape"), colors)
                .child(settings_radius_row(settings, radius_input, colors, window, cx))
                .child(
                    settings_checkbox_row(
                        tr("app.settings.appearance.show_shadows"),
                        tr("app.settings.appearance.show_shadows_detail"),
                        AppIcon::Square,
                        "settings-show-shadows",
                        settings.show_shadows,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings_editor_draft.show_shadows =
                                !this.settings_editor_draft.show_shadows;
                            this.preview_settings(cx);
                        }),
                    ),
                ),
        )
        .child(
            settings_panel_group(tr("app.settings.appearance.group.layout"), colors)
                .child(settings_choice_row_with_id(
                    "settings-density",
                    tr("app.settings.appearance.ui_density"),
                    tr("app.settings.appearance.ui_density_detail"),
                    AppIcon::PanelBottom,
                    density_value(settings.ui_density),
                    &[
                        ("Compact".into(), 0),
                        ("Standard".into(), 1),
                        ("Comfortable".into(), 2),
                    ],
                    |settings, value| {
                        settings.ui_density = match value {
                            0 => UiDensity::Compact,
                            2 => UiDensity::Comfortable,
                            _ => UiDensity::Standard,
                        }
                    },
                    colors,
                    true,
                    cx,
                ))
                .child(
                    settings_checkbox_row(
                        tr("app.settings.appearance.show_status_bar"),
                        tr("app.settings.appearance.show_status_bar_detail"),
                        AppIcon::PanelBottom,
                        "settings-show-status-bar",
                        settings.show_status_bar,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings_editor_draft.show_status_bar =
                                !this.settings_editor_draft.show_status_bar;
                            cx.notify();
                        }),
                    ),
                )
                .child(settings_scrollbar_mode_row(settings.scrollbar_mode, colors, cx)),
        )
        .child(
            settings_panel_group(tr("app.settings.appearance.group.accessibility"), colors)
                .child(
                    settings_checkbox_row(
                        tr("app.settings.appearance.focus_ring"),
                        tr("app.settings.appearance.focus_ring_detail"),
                        AppIcon::CircleSlash,
                        "settings-focus-ring",
                        settings.focus_ring,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings_editor_draft.focus_ring =
                                !this.settings_editor_draft.focus_ring;
                            this.preview_settings(cx);
                        }),
                    ),
                )
                .child(
                    settings_checkbox_row(
                        tr("app.settings.appearance.reduce_motion"),
                        tr("app.settings.appearance.reduce_motion_detail"),
                        AppIcon::Minus,
                        "settings-reduce-motion",
                        settings.reduce_motion,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings_editor_draft.reduce_motion =
                                !this.settings_editor_draft.reduce_motion;
                            this.preview_settings(cx);
                        }),
                    ),
                ),
        )
        .child(
            settings_panel_group(tr("app.settings.appearance.group.font"), colors)
                .child(settings_font_family_row(
                    &settings.global_font_family,
                    colors,
                    cx,
                )),
        )
        .child(settings_appearance_preview(colors, accent))
}

fn settings_appearance_header_actions(
    saved: &Settings,
    draft: &Settings,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let dirty = settings_section_changed(
        SettingsPanelSection::Appearance,
        saved,
        draft,
    );
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("settings-appearance-reset")
                .label(tr("app.settings.reset_defaults"))
                .small()
                .rounded(ComponentTheme::global(cx).radius)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_appearance_settings(cx);
                })),
        )
        .child(
            Button::new("settings-appearance-save")
                .label(tr("app.common.save"))
                .small()
                .rounded(ComponentTheme::global(cx).radius)
                .primary()
                .disabled(!dirty)
                .debug_selector(|| "settings-appearance-save".to_string())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_appearance_settings_saved(cx);
                })),
        )
}

fn settings_section_header_actions(
    section: SettingsPanelSection,
    saved: &Settings,
    draft: &Settings,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let dirty = settings_section_changed(section, saved, draft);
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new(SharedString::from(format!("settings-{section:?}-reset")))
                .label(tr("app.settings.reset_defaults"))
                .small()
                .rounded(ComponentTheme::global(cx).radius)
                .on_click(cx.listener(move |this, _, _, cx| {
                    reset_settings_section(this, section, cx);
                })),
        )
        .child(
            Button::new(SharedString::from(format!("settings-{section:?}-save")))
                .label(tr("app.common.save"))
                .small()
                .rounded(ComponentTheme::global(cx).radius)
                .primary()
                .disabled(!dirty)
                .on_click(cx.listener(move |this, _, _, cx| {
                    save_settings_section_from_ui(this, section, cx);
                })),
        )
}

fn settings_section_header_actions_disabled(colors: UiColors) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("settings-disabled-reset")
                .label(tr("app.settings.appearance.restore_defaults"))
                .small()
                .rounded(colors.radius)
                .disabled(true),
        )
        .child(
            Button::new("settings-disabled-save")
                .label(tr("app.common.save"))
                .small()
                .rounded(colors.radius)
                .primary()
                .disabled(true),
        )
}

fn settings_theme_mode_row(
    theme_mode: ThemeMode,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    settings_action_row_with_id(
        "settings-theme-mode",
        tr("app.settings.appearance.theme_mode"),
        tr("app.settings.appearance.theme_mode_detail"),
        AppIcon::Settings,
        colors,
    )
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
        div()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .rounded(colors.radius)
            .bg(colors.panel_alt)
            .child(settings_theme_mode_button(
                tr("app.common.light"),
                theme_mode == ThemeMode::Light,
                ThemeMode::Light,
                colors,
                cx,
            ))
            .child(settings_theme_mode_button(
                tr("app.common.dark"),
                theme_mode == ThemeMode::Dark,
                ThemeMode::Dark,
                colors,
                cx,
            ))
            .child(
                Button::new("settings-theme-system")
                    .label(tr("app.common.system"))
                    .small()
                    .rounded(colors.radius)
                    .disabled(true),
            ),
    )
}

/// 语言列表完全来自已加载语言包；这里不维护语言枚举或固定 branch。
fn settings_language_row(
    current: &str,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    let follow_system = current.is_empty();
    let active_locale = fluxdb_i18n::effective_locale(current);
    let current_label = if follow_system {
        tr("app.settings.appearance.follow_system")
    } else {
        fluxdb_i18n::locales()
            .into_iter()
            .find(|locale| locale.locale.eq_ignore_ascii_case(&active_locale))
            .map(|locale| SharedString::from(locale.native_name))
            .unwrap_or_else(|| SharedString::from(active_locale.clone()))
    };
    let owner = cx.entity().downgrade();
    let dropdown = Button::new("settings-language-dropdown")
        .debug_selector(|| "settings-language-dropdown".into())
        .small()
        .cursor_pointer()
        .min_w(px(170.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(current_label)
                .child(app_icon(AppIcon::ChevronDown, 12., colors.muted)),
        )
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu
                .check_side(gpui_component::Side::Right)
                .min_w(px(210.));
            let system_owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .id("settings-language-option:system")
                        .debug_selector(|| "settings-language-option:system".into())
                        .child(tr("app.settings.appearance.follow_system"))
                })
                .checked(follow_system)
                .on_click(move |_, window, cx| {
                    let _ = system_owner.update(cx, |this, cx| {
                        this.settings_editor_draft.locale.clear();
                        this.preview_locale(window, cx);
                    });
                }),
            );
            for locale in fluxdb_i18n::locales() {
                let option_owner = owner.clone();
                let selected = !follow_system
                    && active_locale.eq_ignore_ascii_case(&locale.locale);
                let option_locale = locale.locale.clone();
                let option_id = format!("settings-language-option:{}", option_locale);
                let debug_id = option_id.clone();
                let label = SharedString::from(locale.native_name);
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div()
                            .id(option_id.clone())
                            .debug_selector({
                                let id = debug_id.clone();
                                move || id.clone()
                            })
                            .child(label.clone())
                    })
                    .checked(selected)
                    .on_click(move |_, window, cx| {
                        let _ = option_owner.update(cx, |this, cx| {
                            this.settings_editor_draft.locale = option_locale.clone();
                            this.preview_locale(window, cx);
                        });
                    }),
                );
            }
            menu
        });
    settings_action_row_with_id(
        "settings-language",
        tr("app.settings.appearance.language"),
        tr("app.settings.appearance.language_detail"),
        AppIcon::Text,
        colors,
    )
    .child(dropdown)
}

fn settings_theme_palette(
    settings: &fluxdb_core::Settings,
    theme_mode: ThemeMode,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected = if theme_mode == ThemeMode::Dark {
        settings.dark_theme.as_str()
    } else {
        settings.light_theme.as_str()
    };
    let mut palette = div().flex().flex_wrap().gap_2();
    for config in theme_configs_for_mode(theme_mode, cx) {
        let is_selected = config.name == selected;
        let name = config.name.to_string();
        let background = config
            .colors
            .background
            .as_ref()
            .and_then(|value| parse_theme_hex_color(value).map(Hsla::from))
            .unwrap_or_else(|| colors.panel_bg.into());
        let foreground = config
            .colors
            .foreground
            .as_ref()
            .and_then(|value| parse_theme_hex_color(value).map(Hsla::from))
            .unwrap_or_else(|| colors.text.into());
        let accent = config
            .colors
            .primary
            .as_ref()
            .and_then(|value| parse_theme_hex_color(value).map(Hsla::from))
            .unwrap_or_else(|| colors.hover.into());
        palette = palette.child(
            Button::new(SharedString::from(format!("theme-palette-{}", config.name)))
                .w(px(132.))
                .h(px(52.))
                .p_2()
                .rounded(colors.radius)
                .compact()
                .border(px(if is_selected { 2. } else { 1. }))
                .border_color(if is_selected { colors.text } else { colors.border })
                .bg(background)
                .text_color(foreground)
                // 不要在这里再调 `Styled::hover`：`Button::render` 内部已对同一个 base
                // 设置过 hover 样式，叠加会触发 gpui 的 `debug_assert!("hover style already
                // set")`，在不能 unwind 的绘制帧里直接 abort。需要自定义 hover 观感时
                // 用组件自己的 `Button::hover(color)` 设背景色。
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_theme_name(theme_mode, name.clone(), cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size(px(10.)).rounded_full().bg(accent))
                        .child(div().text_xs().truncate().child(config.name.clone())),
                ),
        );
    }
    palette
}

fn settings_radius_row(
    settings: &Settings,
    radius_input: Entity<InputState>,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    settings_action_row_with_id(
        "settings-radius",
        tr("app.settings.appearance.radius"),
        tr("app.settings.appearance.radius_detail"),
        AppIcon::Square,
        colors,
    )
    .child(
        div()
            .flex()
            .items_center()
            .gap_1()
                .children(
                [
                    ("Square", (0_u8, 0_u8)),
                    ("Standard", (6_u8, 8_u8)),
                    ("Rounded", (8_u8, 12_u8)),
                    ("Soft", (10_u8, 16_u8)),
                ]
                    .into_iter()
                    .map(|(label, (radius, large_radius))| {
                        Button::new(("settings-radius", settings_status_id("settings-radius", label)))
                            .label(label)
                            .small()
                            .rounded(colors.radius)
                            .when(
                                settings.button_radius == radius
                                    && settings.large_radius == large_radius,
                                |button| button.primary(),
                            )
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                                this.settings_editor_draft.button_radius = radius;
                                this.settings_editor_draft.large_radius = large_radius;
                                this.preview_settings(cx);
                                cx.stop_propagation();
                            }))
                    }),
            )
            .child(settings_radius_input(settings.button_radius, &radius_input, colors, window, cx)),
    )
}

fn settings_radius_input(
    current: u8,
    input: &Entity<InputState>,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let expected = current.min(24).to_string();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    if !focused && input.read(cx).value().to_string() != expected {
        input.update(cx, |input, cx| input.set_value(expected.clone(), window, cx));
    }
    let focus_border = if colors.is_dark { rgb(0x8ab4ff) } else { rgb(0x111111) };
    div()
        .w(px(86.))
        .h(px(34.))
        .rounded(colors.radius)
        .border_1()
        .border_color(if focused { focus_border } else { colors.border })
        .bg(colors.input_bg)
        .flex()
        .items_center()
        .overflow_hidden()
        .cursor_text()
        .child(
            Input::new(input)
                .appearance(false)
                .focus_bordered(false)
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .px_2()
                .text_size(px(13.))
                .text_color(colors.text),
        )
        .child(div().mr_2().text_size(px(11.)).text_color(colors.muted).child("px"))
}

fn density_value(density: UiDensity) -> u64 {
    match density {
        UiDensity::Compact => 0,
        UiDensity::Standard => 1,
        UiDensity::Comfortable => 2,
    }
}

fn settings_scrollbar_mode_row(
    current: ScrollbarMode,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    settings_action_row_with_id(
        "settings-scrollbar",
        tr("app.settings.appearance.scrollbar"),
        tr("app.settings.appearance.scrollbar_detail"),
        AppIcon::List,
        colors,
    )
    .child(
        div().flex().items_center().gap_1().children(
            [
                ("Auto", ScrollbarMode::Scrolling),
                ("Hover", ScrollbarMode::Hover),
                ("Always", ScrollbarMode::Always),
            ]
            .into_iter()
            .map(|(label, mode)| {
                Button::new(("settings-scrollbar", settings_status_id("settings-scrollbar", label)))
                    .label(label)
                    .small()
                    .rounded(colors.radius)
                    .when(current == mode, |button| button.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_editor_draft.scrollbar_mode = mode;
                        this.preview_settings(cx);
                    }))
            }),
        ),
    )
}

fn settings_font_family_row(
    current: &str,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    settings_action_row_with_id(
        "settings-font",
        tr("app.settings.appearance.global_font"),
        tr("app.settings.appearance.global_font_detail"),
        AppIcon::Text,
        colors,
    )
    .child(
        div().flex().items_center().gap_1().children(
            [("System", ""), ("Inter", "Inter"), ("SF Pro", "SF Pro")]
                .into_iter()
                .map(|(label, family)| {
                    Button::new(("settings-font", settings_status_id("settings-font", label)))
                        .label(label)
                        .small()
                        .rounded(colors.radius)
                        .when(current == family, |button| button.primary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings_editor_draft.global_font_family = family.to_string();
                            this.preview_settings(cx);
                        }))
                }),
        ),
    )
}

fn settings_theme_mode_button(
    label: impl Into<SharedString>,
    active: bool,
    mode: ThemeMode,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Button {
    let label = label.into();
    Button::new(SharedString::from(format!(
        "settings-theme-mode-{mode:?}"
    )))
        .label(label)
        .small()
        .rounded(colors.radius)
        .when(active, |button| button.primary())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_theme_mode(mode, cx);
            this.show_message(
                if mode == ThemeMode::Dark {
                    tr("app.settings.appearance.dark_preview")
                } else {
                    tr("app.settings.appearance.light_preview")
                },
                AppMessageKind::Success,
                cx,
            );
        }))
}

fn settings_appearance_preview(colors: UiColors, accent: gpui::Rgba) -> GroupBox {
    settings_panel_group(tr("app.settings.appearance.group.preview"), colors).child(
        div()
            .border_t_1()
            .border_color(colors.border_soft)
            .p_3()
            .flex()
            .gap_3()
            .child(
                div()
                    .w(px(150.))
                    .h(px(176.))
                    .rounded(colors.radius)
                    .border_1()
                    .border_color(colors.border_soft)
                    .bg(colors.panel_alt)
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.muted)
                            .child(tr("app.settings.appearance.preview_connections")),
                    )
                    .child(settings_preview_tree_item("Local MySQL", true, accent, colors))
                    .child(settings_preview_tree_item("orders", false, accent, colors))
                    .child(settings_preview_tree_item("users", false, accent, colors)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h(px(176.))
                    .rounded(colors.radius)
                    .border_1()
                    .border_color(colors.border_soft)
                    .bg(colors.panel_bg)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(div().h(px(4.)).w_full().bg(accent))
                    .child(
                        div()
                            .h(px(38.))
                            .px_3()
                            .border_b_1()
                            .border_color(colors.border_soft)
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(colors.text)
                                    .child("orders"),
                            )
                            .child(
                                div()
                                    .h(px(24.))
                                    .px_2()
                                    .rounded(colors.radius)
                                    .bg(accent)
                                    .flex()
                                    .items_center()
                                    .text_size(px(11.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xffffff))
                                    .child(tr("app.settings.appearance.preview_filter")),
                            ),
                    )
                    .child(settings_preview_table(colors)),
            ),
    )
}

fn settings_preview_tree_item(
    label: &'static str,
    active: bool,
    accent: gpui::Rgba,
    colors: UiColors,
) -> Div {
    div()
        .h(px(28.))
        .rounded(colors.radius)
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(if active {
            colors.tree_selected
        } else {
            colors.panel_alt
        })
        .child(
            div()
                .size(px(7.))
                .rounded_full()
                .bg(if active { accent } else { colors.muted }),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active { colors.text } else { colors.muted })
                .child(label),
        )
}

fn settings_preview_table(colors: UiColors) -> Div {
    let rows = [
        ("1001", "Fusuwei", "paid", "$120.00"),
        ("1002", "Cloud", "pending", "$88.40"),
        ("1003", "Studio", "paid", "$310.20"),
    ];
    let mut table = div().flex().flex_col().p_3();
    table = table.child(settings_preview_table_row(
        ("id", "customer", "status", "total"),
        true,
        colors,
    ));
    for row in rows {
        table = table.child(settings_preview_table_row(row, false, colors));
    }
    table
}

fn settings_preview_table_row(
    row: (&'static str, &'static str, &'static str, &'static str),
    header: bool,
    colors: UiColors,
) -> Div {
    div()
        .h(px(if header { 30. } else { 28. }))
        .border_1()
        .border_color(colors.border_soft)
        .bg(if header { colors.panel_alt } else { colors.panel_bg })
        .flex()
        .items_center()
        .text_size(px(11.))
        .text_color(if header { colors.muted } else { colors.text })
        .child(settings_preview_table_cell(row.0, header, colors))
        .child(settings_preview_table_cell(row.1, header, colors))
        .child(settings_preview_table_cell(row.2, header, colors))
        .child(settings_preview_table_cell(row.3, header, colors))
}

fn settings_preview_table_cell(text: &'static str, header: bool, colors: UiColors) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .px_2()
        .font_family(if header { "JetBrains Mono" } else { "Inter" })
        .font_weight(if header {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(if header { colors.muted } else { colors.text })
        .child(text)
}

