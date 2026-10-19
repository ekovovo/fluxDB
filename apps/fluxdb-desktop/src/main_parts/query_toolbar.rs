fn query_toolbar(
    this: &mut NavicatMain,
    tab_id: TabId,
    editor: &QueryEditorState,
    sql_editor: Entity<editor_component::Editor>,
    connection: String,
    database: String,
    accent: gpui::Rgba,
    output_placement: ResultsPlacement,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let has_text = !editor.text.trim().is_empty();
    let soft_wrap = sql_editor.read(cx).soft_wrap();
    let can_explain = !editor.running && this.selected_explain_sql_text(tab_id, cx).is_some();
    div()
        .h(px(40.))
        .border_b_1()
        .border_color(colors.border)
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .bg(colors.panel_alt)
        .child(query_context_group(connection, database, accent, colors))
        .child(
            query_toolbar_icon_button(
                if editor.running {
                    tr("app.query.toolbar.running")
                } else {
                    tr("app.query.toolbar.run")
                },
                AppIcon::Play,
                has_text && !editor.running,
                editor.running,
                rgb(0x20c76a),
                colors,
            )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let sql_editor = sql_editor.clone();
                        move |this, _, window, cx| {
                            let text = sql_editor.read(cx).text();
                            if !text.trim().is_empty() {
                                this.dispatch(AppCommand::UpdateQueryText { tab_id, text }, cx);
                                this.start_query_execution(tab_id, window, cx);
                                cx.stop_propagation();
                            }
                        }
                    }),
                ),
        )
        .child(
            query_toolbar_icon_button(
                tr("app.query.toolbar.format"),
                AppIcon::AlignLeft,
                has_text,
                false,
                rgb(0xa45cff),
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let sql_editor = sql_editor.clone();
                    move |this, _, _, cx| {
                        if has_text && this.format_query_editor_sql(tab_id, sql_editor.clone(), cx)
                        {
                            cx.stop_propagation();
                        }
                    }
                }),
            ),
        )
        .child(
            query_toolbar_icon_button(
                tr("app.query.toolbar.compact"),
                AppIcon::List,
                has_text,
                false,
                rgb(0x38bdf8),
                colors,
            )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let sql_editor = sql_editor.clone();
                        move |this, _, _, cx| {
                            if has_text
                                && this.compress_query_editor_sql(tab_id, sql_editor.clone(), cx)
                            {
                                cx.stop_propagation();
                            }
                        }
                    }),
                ),
        )
        .child(
            query_toolbar_icon_button(
                tr("app.query.toolbar.stop"),
                AppIcon::Square,
                editor.running,
                false,
                rgb(0xff5c5c),
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.request_query_cancel(tab_id, cx);
                    cx.stop_propagation();
                }),
            ),
        )
        .child(query_toolbar_icon_button(
            tr("app.query.toolbar.explain"),
            AppIcon::FileSearch,
            can_explain,
            false,
            rgb(0x45a3ff),
            colors,
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let sql_editor = sql_editor.clone();
                move |this, _, window, cx| {
                    if this.start_query_explain_execution(tab_id, sql_editor.clone(), window, cx) {
                        cx.stop_propagation();
                    }
                }
            }),
        ))
        .child(div().flex_1())
        .child(Button::new("query-open-file")
            .debug_selector(||"query-open-file".into())
            .small().ghost().cursor_pointer().disabled(this.query_files.loading)
            .loading(this.query_files.loading)
            .child(app_icon(AppIcon::Folder,16.,colors.muted))
            .tooltip(tr("app.query.toolbar.open_file"))
            .on_click(cx.listener(move|this,_,_,cx|this.choose_local_sql_file(tab_id,cx))))
        .child(query_toolbar_icon_button(
            tr("app.common.save"),
            AppIcon::Save,
            true,
            false,
            rgb(0x4b8dff),
            colors,
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                this.save_active_query(window, cx);
                cx.stop_propagation();
            }),
        ))
        .child(query_toolbar_icon_button(
            tr("app.query.toolbar.builder"),
            AppIcon::Workflow,
            false,
            false,
            rgb(0xf0b400),
            colors,
        ))
        .child(query_toolbar_icon_button(
            tr("app.query.toolbar.snippets"),
            AppIcon::FileSql,
            false,
            false,
            rgb(0x28c7d7),
            colors,
        ))
        .child(
            query_toolbar_icon_button(
                tr("app.query.toolbar.history"),
                AppIcon::CalendarClock,
                true,
                false,
                rgb(0xf59e0b),
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.show_query_history_quick_search(window, cx);
                    cx.stop_propagation();
                }),
            ),
        )
        .child(
            query_toolbar_icon_button(
                query_editor_wrap_toggle_label(soft_wrap),
                query_editor_wrap_toggle_icon(soft_wrap),
                true,
                false,
                rgb(0x5f8ff7),
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let sql_editor = sql_editor.clone();
                    move |this, _, _, cx| {
                        let mut settings = this.controller.state().settings.clone();
                        settings.editor_word_wrap = !soft_wrap;
                        save_settings_from_ui(
                            this,
                            settings,
                            tr("app.settings.message.word_wrap_updated"),
                            cx,
                        );
                        let line_height = this.controller.state().settings.editor_line_height.clamp(13, 28) as f32;
                        sql_editor.update(cx, |editor, _editor_cx| {
                            editor.apply_settings(editor.font_size, line_height, !soft_wrap, 0);
                        });
                        cx.stop_propagation();
                    }
                }),
            ),
        )
        .child(
            query_toolbar_icon_button(
                query_output_layout_toggle_label(output_placement),
                query_output_layout_toggle_icon(output_placement),
                true,
                false,
                rgb(0x45a3ff),
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.results_placement = this.results_placement.toggled();
                    this.query_output_resize_start = None;
                    cx.notify();
                    cx.stop_propagation();
                }),
            ),
        )
}

fn query_editor_wrap_toggle_icon(soft_wrap: bool) -> AppIcon {
    if soft_wrap {
        AppIcon::Text
    } else {
        AppIcon::WrapText
    }
}

fn query_editor_wrap_toggle_label(soft_wrap: bool) -> SharedString {
    if soft_wrap {
        tr("app.query.toolbar.wrap_off")
    } else {
        tr("app.query.toolbar.wrap_auto")
    }
}

fn query_output_layout_toggle_icon(placement: ResultsPlacement) -> AppIcon {
    match placement {
        ResultsPlacement::Bottom => AppIcon::PanelBottom,
        ResultsPlacement::Right => AppIcon::PanelRight,
    }
}

fn query_output_layout_toggle_label(placement: ResultsPlacement) -> SharedString {
    match placement {
        ResultsPlacement::Bottom => tr("app.query.toolbar.results_bottom"),
        ResultsPlacement::Right => tr("app.query.toolbar.results_right"),
    }
}

fn query_toolbar_icon_button(
    label: impl Into<SharedString>,
    icon: AppIcon,
    enabled: bool,
    loading: bool,
    accent: gpui::Rgba,
    colors: UiColors,
) -> Stateful<Div> {
    let label = label.into();
    let label_for_id = label.clone();
    let bg = colors.panel_bg;
    let icon_color = if enabled { accent } else { colors.muted };
    div()
        .id(label_for_id)
        .size(px(30.))
        .rounded(colors.radius)
        .border_1()
        .border_color(if enabled {
            if colors.is_dark {
                rgb(0x3a414d)
            } else {
                rgb(0xd7dde8)
            }
        } else {
            colors.border
        })
        .bg(if enabled { bg } else { colors.panel_alt })
        .flex()
        .items_center()
        .justify_center()
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when(enabled, |this| {
            this.cursor_pointer().hover(move |style| {
                style.bg(if colors.is_dark {
                    rgb(0x2a3039)
                } else {
                    rgb(0xf4f7fb)
                })
            })
        })
        .child(if loading {
            loading_spinner_with_color(17., icon_color).into_any_element()
        } else {
            app_icon_box(icon, 20., 17., icon_color).into_any_element()
        })
}

fn query_context_group(
    connection: String,
    database: String,
    accent: gpui::Rgba,
    colors: UiColors,
) -> Div {
    div()
        .h(px(30.))
        .min_w(px(0.))
        .max_w(px(360.))
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border_soft)
        .bg(colors.panel_bg)
        .flex()
        .items_center()
        .overflow_hidden()
        .child(query_context_item(connection, Some(accent), colors))
        .child(div().w(px(1.)).h(px(14.)).flex_none().bg(colors.border_soft))
        .child(query_context_item(database, None, colors))
}

fn query_context_item(
    label: String,
    identity_color: Option<gpui::Rgba>,
    colors: UiColors,
) -> Div {
    let leading: gpui::AnyElement = match identity_color {
        Some(color) => div()
            .size(px(6.))
            .flex_none()
            .rounded_full()
            .bg(color)
            .into_any_element(),
        None => app_icon(AppIcon::Database, 13., colors.muted).into_any_element(),
    };
    div()
        .h_full()
        .min_w(px(0.))
        .max_w(px(170.))
        .px_2()
        .flex()
        .items_center()
        .gap_1p5()
        .child(leading)
        .child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(12.))
                .text_color(colors.text)
                .child(label),
        )
}

