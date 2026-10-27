fn cell_detail_panel(
    tab_id: TabId,
    editor: &DataEditorState,
    page: &DataPage,
    panel_height: f32,
    input: Entity<InputState>,
    temporal_part_input: Entity<InputState>,
    temporal_part_editing: Option<TemporalPartEditState>,
    colors: UiColors,
    window: &mut Window,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let Some(active_cell) = editor.cell_detail_panel.active_cell else {
        return div();
    };
    let Some(column) = page.columns.get(active_cell.column) else {
        return div();
    };
    let value = page
        .rows
        .get(active_cell.row)
        .and_then(|row| row.values.get(active_cell.column))
        .cloned()
        .unwrap_or(CellValue::Null);
    // 完整文本已按需加载时用它（复制/格式化/编辑都以完整值为准），否则显示带标记的预览。
    let value_text = editor
        .cell_detail_panel
        .full_value
        .clone()
        .unwrap_or_else(|| cell_value_label(&value));
    let is_json = matches!(value, CellValue::Json(_)) || looks_like_json(&value_text);
    let is_binary = matches!(value, CellValue::Bytes(_) | CellValue::BinarySummary(_));
    let row_number = page.offset + active_cell.row as u64 + 1;
    let type_name = column
        .type_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let type_color = data_type_color(&type_name).unwrap_or(colors.text);
    let length = cell_value_length(&value);
    let detail_value = editor.cell_detail_panel.edit_value.clone();
    let mode = editor.cell_detail_panel.mode;
    let temporal_kind = data_cell_temporal_kind(&type_name);

    if mode == CellDetailMode::Edit {
        sync_input_value(&input, &detail_value, window, cx);
    }

    let drawer_bg = if colors.is_dark {
        rgb(0x171b21)
    } else {
        rgb(0xfbfcfe)
    };
    let card_bg = if colors.is_dark {
        rgb(0x20252d)
    } else {
        rgb(0xffffff)
    };
    let field_bg = if colors.is_dark {
        rgb(0x13171d)
    } else {
        rgb(0xffffff)
    };
    let soft_border = if colors.is_dark {
        rgb(0x2f3642)
    } else {
        colors.border_soft
    };

    div()
        .h_full()
        .w_full()
        .flex_none()
        .border_t_1()
        .border_color(soft_border)
        .bg(drawer_bg)
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .child(cell_detail_drawer_resize_handle(
            tab_id,
            panel_height,
            colors,
            cx,
        ))
        .child(
            div()
                .h(px(54.))
                .w_full()
                .rounded(colors.radius * 0.5)
                .border_1()
                .border_color(soft_border)
                .bg(card_bg)
                .flex()
                .overflow_hidden()
                .child(cell_detail_meta_item(
                    tr("app.table_info.column_name"),
                    column.name.clone(),
                    colors,
                ))
                .child(cell_detail_meta_item(
                    tr("app.content.row_number"),
                    row_number.to_string(),
                    colors,
                ))
                .child(cell_detail_meta_item_colored(
                    tr("app.history.meta_type"),
                    type_name,
                    type_color,
                    colors,
                ))
                .child(cell_detail_meta_item(
                    "NULL",
                    matches!(value, CellValue::Null).to_string(),
                    colors,
                ))
                .child(cell_detail_meta_item(
                    tr("app.content.length"),
                    length.to_string(),
                    colors,
                ))
                .child(cell_detail_meta_item(
                    tr("app.table_info.comment"),
                    column
                        .comment
                        .clone()
                        .unwrap_or_else(|| tr("app.history.none").to_string()),
                    colors,
                ))
                .child(
                    div()
                        .w(px(42.))
                        .h_full()
                        .flex_none()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt_2()
                        .child(
                            cell_detail_icon_button(AppIcon::Close, tr("app.common.close"), colors)
                                .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.dispatch(AppCommand::CloseCellDetail(tab_id), cx);
                                    cx.stop_propagation();
                                }),
                            ),
                        ),
                ),
        )
        .child(cell_detail_value_header(
            tab_id,
            mode,
            value_text.clone(),
            is_json,
            is_binary,
            colors,
            cx,
        ))
        .child(if mode == CellDetailMode::Edit {
            if let Some(kind) = temporal_kind {
                div().w(px(match kind {
                    DataCellTemporalKind::Date => 290.,
                    DataCellTemporalKind::Time => 252.,
                    DataCellTemporalKind::DateTime => 300.,
                }))
                .child(cell_detail_temporal_editor(
                    kind,
                    input.clone(),
                    temporal_part_input,
                    temporal_part_editing,
                    tab_id,
                    cx.entity().downgrade(),
                    column.nullable,
                    colors.is_dark,
                    detail_value,
                ))
            } else {
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .rounded(colors.radius * 0.5)
                    .border_1()
                    .border_color(soft_border)
                    .bg(field_bg)
                    .overflow_hidden()
                    .child(
                        Input::new(&input)
                            .appearance(false)
                            .focus_bordered(false)
                            .w_full()
                            .h_full()
                            .text_size(px(13.)),
                    )
            }
        } else {
            cell_detail_value_preview(tab_id, &value, colors, cx)
        })
        .when(mode == CellDetailMode::Edit, |this| {
            this.child(cell_detail_footer(tab_id, is_binary, colors, cx))
        })
}

fn cell_detail_drawer_resize_handle(
    tab_id: TabId,
    panel_height: f32,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    div()
        .id("cell-detail-drawer-resize-handle")
        .h(px(8.))
        .mt(px(-6.))
        .mx(px(-8.))
        .cursor_ns_resize()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .h(px(1.))
                .w_full()
                .bg(colors.border_soft)
                .hover(|style| style.bg(rgb(0x3478f6))),
        )
        .hover(|style| style.bg(hsla(212. / 360., 0.92, 0.58, 0.10)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.cell_detail_drawer_resize_start = Some(CellDetailDrawerResizeStart {
                    tab_id,
                    y: f32::from(event.position.y),
                    height: panel_height,
                });
                this.sync_table_hover_overlay_block(cx);
                cx.stop_propagation();
            }),
        )
        .on_drag(CellDetailDrawerResizeDrag, |drag, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| drag.clone())
        })
        .on_drag_move(cx.listener(
            move |this, event: &DragMoveEvent<CellDetailDrawerResizeDrag>, _, cx| {
                if let Some(start) = this.cell_detail_drawer_resize_start {
                    if start.tab_id == tab_id {
                        let delta = start.y - f32::from(event.event.position.y);
                        this.cell_detail_drawer_heights.insert(
                            tab_id,
                            clamp_cell_detail_drawer_height(start.height + delta),
                        );
                        cx.notify();
                    }
                }
                cx.stop_propagation();
            },
        ))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.cell_detail_drawer_resize_start = None;
                cx.stop_propagation();
            }),
        )
}

fn clamp_cell_detail_drawer_height(height: f32) -> f32 {
    height.clamp(CELL_DETAIL_DRAWER_MIN_HEIGHT, CELL_DETAIL_DRAWER_MAX_HEIGHT)
}

#[cfg(test)]
mod cell_detail_drawer_height_tests {
    use super::*;

    #[test]
    fn minimum_height_keeps_edit_controls_and_value_area_visible() {
        let outer_padding = 16.;
        let flex_gaps = 4. * 8.;
        let fixed_rows = 8. + 54. + 22. + 26.;
        let minimum_value_area = 56.;

        assert!(
            CELL_DETAIL_DRAWER_MIN_HEIGHT
                >= outer_padding + flex_gaps + fixed_rows + minimum_value_area
        );
        assert_eq!(
            clamp_cell_detail_drawer_height(0.),
            CELL_DETAIL_DRAWER_MIN_HEIGHT
        );
    }
}

fn cell_detail_value_header(
    tab_id: TabId,
    mode: CellDetailMode,
    value_text: String,
    is_json: bool,
    is_binary: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let copy_value_text = value_text.clone();
    let format_value_text = value_text.clone();

    div()
        .h(px(22.))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.muted)
                .child(tr("app.placeholder.value")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .when(mode == CellDetailMode::View, |this| {
                    this.when(is_json, |this| {
                        this.child(
                            cell_detail_action_button(tr("app.cell_detail.format_json"), colors)
                                .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(formatted) = format_json_text(&format_value_text) {
                                        this.dispatch(AppCommand::StartCellDetailEdit(tab_id), cx);
                                        this.dispatch(
                                            AppCommand::UpdateCellDetailEditValue {
                                                tab_id,
                                                value: formatted,
                                            },
                                            cx,
                                        );
                                    }
                                    cx.stop_propagation();
                                }),
                            ),
                        )
                    })
                    .when(is_binary, |this| {
                        this.child(cell_detail_action_button(tr("app.cell_detail.download"), colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.download_active_cell(cx);
                                cx.stop_propagation();
                            }),
                        ))
                        .child(cell_detail_action_button(tr("app.cell_detail.replace_upload"), colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.upload_active_binary_cell(window, cx);
                                cx.stop_propagation();
                            }),
                        ))
                        .child(cell_detail_action_button(tr("app.cell_detail.edit_hex"), colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                let event = this
                                    .controller
                                    .dispatch(AppCommand::StartCellDetailEdit(tab_id));
                                if let AppEvent::Failed(error) = &event {
                                    this.show_message(
                                        error.message.clone(),
                                        AppMessageKind::Warning,
                                        cx,
                                    );
                                }
                                this.apply_app_event(&event, cx);
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ))
                        .child(
                            cell_detail_action_button(tr("app.cell_detail.set_null"), colors)
                                .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.set_active_binary_cell_null(tab_id, cx);
                                    cx.stop_propagation();
                                }),
                            ),
                        )
                    })
                    .when(!is_binary, |this| {
                        this.child(
                            cell_detail_icon_button(
                                AppIcon::Edit,
                                tr("app.cell_detail.edit"),
                                colors,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.dispatch(AppCommand::StartCellDetailEdit(tab_id), cx);
                                    cx.stop_propagation();
                                }),
                            ),
                        )
                    })
                    .child(
                        cell_detail_icon_button(AppIcon::Copy, tr("app.context.copy"), colors)
                            .on_mouse_down(
                            MouseButton::Left,
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_value_text.clone(),
                                ));
                                cx.stop_propagation();
                            },
                        ),
                    )
                })
                .when(mode == CellDetailMode::Edit, |this| {
                    this.when(is_json, |this| {
                        this.child(
                            cell_detail_action_button(tr("app.cell_detail.format_json"), colors)
                                .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    let Some(text) = this.active_cell_detail_edit_value(cx) else {
                                        return;
                                    };
                                    if let Some(formatted) = format_json_text(&text) {
                                        this.dispatch(
                                            AppCommand::UpdateCellDetailEditValue {
                                                tab_id,
                                                value: formatted,
                                            },
                                            cx,
                                        );
                                    }
                                    cx.stop_propagation();
                                }),
                            ),
                        )
                    })
                    .child(
                        cell_detail_action_button(tr("app.cell_detail.set_null"), colors)
                            .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if is_binary {
                                    this.set_active_binary_cell_null(tab_id, cx);
                                } else {
                                    this.dispatch(AppCommand::SetCellDetailNull(tab_id), cx);
                                    this.refresh_active_data_table(tab_id, cx);
                                }
                                cx.stop_propagation();
                            }),
                        ),
                    )
                }),
        )
}

fn cell_detail_value_preview(
    tab_id: TabId,
    value: &CellValue,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let (text, text_color) = match value {
        CellValue::Bytes(bytes) => (
            tr("app.cell_detail.binary_hint")
                .replace("%{bytes}", &bytes.len().to_string()),
            colors.muted,
        ),
        CellValue::BinarySummary(summary) if summary.is_null => ("NULL".to_string(), colors.text),
        CellValue::BinarySummary(summary) => (
            summary
                .preview_hex
                .as_ref()
                .map(|preview| {
                    tr("app.cell_detail.hex_preview").replace("%{preview}", preview)
                })
                .unwrap_or_else(|| tr("app.cell_detail.no_hex_preview").to_string()),
            colors.muted,
        ),
        // 预览是片段：完整值还在后台加载时明确标出来，别让人以为已经看全。
        CellValue::TruncatedText { total_chars, .. } => (
            tr("app.cell_detail.truncated_loading")
                .replace("%{chars}", &total_chars.to_string()),
            colors.muted,
        ),
        _ => {
            (cell_value_label(value), colors.text)
        }
    };

    div()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .rounded(colors.radius * 0.5)
        .border_1()
        .border_color(if colors.is_dark {
            rgb(0x2f3642)
        } else {
            colors.border_soft
        })
        .bg(if colors.is_dark {
            rgb(0x13171d)
        } else {
            rgb(0xffffff)
        })
        .px_2()
        .py_2()
        .overflow_hidden()
        .cursor_pointer()
        .flex()
        .items_start()
        .text_size(px(13.))
        .text_color(text_color)
        .hover(move |style| style.bg(colors.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.dispatch(AppCommand::StartCellDetailEdit(tab_id), cx);
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .size_full()
                .overflow_scrollbar()
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .whitespace_normal()
                        .child(text),
                ),
        )
}

fn cell_detail_footer(
    tab_id: TabId,
    is_binary: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .h(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .child(cell_detail_primary_button(tr("app.common.save"), colors).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if is_binary {
                    this.save_active_binary_hex_edit(tab_id, cx);
                } else {
                    this.dispatch(AppCommand::SaveCellDetailEdit(tab_id), cx);
                    this.refresh_active_data_table(tab_id, cx);
                }
                cx.stop_propagation();
            }),
        ))
        .child(cell_detail_action_button(tr("app.common.cancel"), colors).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.dispatch(AppCommand::CancelCellDetailEdit(tab_id), cx);
                cx.stop_propagation();
            }),
        ))
        .child(cell_detail_action_button(tr("app.cell_detail.restore_original"), colors).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.dispatch(AppCommand::RestoreCellDetailOriginalValue(tab_id), cx);
                cx.stop_propagation();
            }),
        ))
}

fn cell_detail_meta_item(
    label: impl Into<SharedString>,
    value: String,
    colors: UiColors,
) -> Div {
    cell_detail_meta_item_colored(label, value, colors.text, colors)
}

fn cell_detail_meta_item_colored(
    label: impl Into<SharedString>,
    value: String,
    value_color: gpui::Rgba,
    colors: UiColors,
) -> Div {
    let label = label.into();
    div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .px_3()
        .py_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap_1()
        .text_size(px(12.))
        .child(div().text_color(colors.muted).child(label))
        .child(
            div()
                .text_color(value_color)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn cell_detail_icon_button(
    icon: AppIcon,
    tooltip: impl Into<SharedString>,
    colors: UiColors,
) -> Stateful<Div> {
    let tooltip = tooltip.into();
    div()
        .id(tooltip.clone())
        .size(px(22.))
        .rounded(colors.radius * 0.5)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |style| style.bg(colors.hover))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(app_icon(icon, 15., colors.muted))
}

fn cell_detail_action_button(label: impl Into<SharedString>, colors: UiColors) -> Div {
    let label = label.into();
    div()
        .h(px(24.))
        .px_2()
        .rounded(colors.radius * 0.5)
        .border_1()
        .border_color(colors.border)
        .bg(colors.panel_alt)
        .text_size(px(12.))
        .text_color(colors.text)
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |style| style.bg(colors.hover))
        .child(label)
}

fn cell_detail_primary_button(label: impl Into<SharedString>, colors: UiColors) -> Div {
    cell_detail_action_button(label, colors)
        .border_color(if colors.is_dark {
            rgb(0x2f75ff)
        } else {
            rgb(0x2c6bed)
        })
        .bg(if colors.is_dark {
            rgb(0x173766)
        } else {
            rgb(0xe7f0ff)
        })
        .text_color(if colors.is_dark {
            rgb(0xa9c9ff)
        } else {
            rgb(0x1757bd)
        })
}
