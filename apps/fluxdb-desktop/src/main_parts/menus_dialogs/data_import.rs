// Included in crate-root scope by ../menus_dialogs.rs; 表数据导入向导弹窗。

/// 导入向导步骤。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableDataImportStep {
    Source,
    Mapping,
    Execute,
}

impl TableDataImportStep {
    fn label(self) -> &'static str {
        match self {
            Self::Source => "解析选项",
            Self::Mapping => "字段映射",
            Self::Execute => "执行导入",
        }
    }
}

/// 导入目标方式；“创建新表”暂未支持，保留为可见但置灰的预留项。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableDataImportTarget {
    Existing,
    #[allow(dead_code)] // 预留：后续开放“从文件创建新表”时启用。
    Create,
}

/// 导入状态。
#[derive(Clone, Debug)]
struct TableDataImportForm {
    object: ObjectPath,
    source: PathBuf,
    format: fluxdb_core::TableImportFormat,
    options: fluxdb_core::TableImportOptions,
    target: TableDataImportTarget,
    step: TableDataImportStep,
    preview: Option<fluxdb_core::TableImportPreview>,
    mappings: Vec<fluxdb_core::TableImportColumnMapping>,
    error: Option<String>,
    running: bool,
    loading: bool,
    progress: Option<String>,
}

impl TableDataImportForm {
    fn new(
        object: ObjectPath,
        source: PathBuf,
        format: fluxdb_core::TableImportFormat,
    ) -> Self {
        Self {
            object,
            source,
            format,
            options: fluxdb_core::TableImportOptions::new(),
            target: TableDataImportTarget::Existing,
            step: TableDataImportStep::Source,
            preview: None,
            mappings: Vec::new(),
            error: None,
            running: false,
            loading: false,
            progress: None,
        }
    }

    /// 清空模式在部分数据库无法回滚，需要在界面上明确提示。
    fn truncate_warning(
        &self,
        kind: DatabaseKind,
    ) -> Option<String> {
        if self.options.mode != fluxdb_core::TableImportMode::Truncate {
            return None;
        }
        Some(match kind {
            DatabaseKind::Postgres | DatabaseKind::Sqlite => {
                tr("app.data.data_import.truncate_rollback_safe").to_string()
            }
            _ => tr("app.data.data_import.truncate_rollback_unsafe").to_string(),
        })
    }
}

impl NavicatMain {
    /// 从数据编辑页打开导入向导（工具栏入口）。
    fn open_table_import_wizard_for_tab(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let object = self
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.kind {
                TabKind::DataEditor(editor) => Some(editor.object.clone()),
                _ => None,
            });
        let Some(object) = object else {
            self.show_message(
                tr("app.data.data_import.open_failed").as_ref(),
                AppMessageKind::Warning,
                cx,
            );
            return;
        };
        self.open_table_import_wizard(object, window, cx);
    }

    /// 打开导入向导：先选择文件，再进入解析选项。
    fn open_table_import_wizard(
        &mut self,
        object: ObjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self._data_import_task.is_some() {
            self.show_message(
                tr("app.table.import.running").as_ref(),
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("app.table.import.choose_file")),
        });
        self._file_picker_task = Some(cx.spawn_in(window, async move |view, cx| {
            let selected = receiver.await;
            let path = match selected {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let message = error.to_string();
                    let _ = cx.update(|_, cx| {
                        let Some(view) = view.upgrade() else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.show_message(
                                format!(
                                    "{}: {message}",
                                    tr("app.table.import.choose_file_failed")
                                ),
                                AppMessageKind::Error,
                                cx,
                            );
                        });
                    });
                    None
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = cx.update(|_, cx| {
                        let Some(view) = view.upgrade() else {
                            return;
                        };
                        view.update(cx, |this, cx| {
                            this.show_message(
                                format!(
                                    "{}: {message}",
                                    tr("app.table.import.choose_file_failed")
                                ),
                                AppMessageKind::Error,
                                cx,
                            );
                        });
                    });
                    None
                }
            };
            let Some(path) = path else {
                return;
            };
            let object_path = object.clone();
            let _ = cx.update(|_, cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    let Some(format) = fluxdb_app::TableImportFormat::from_path(&path) else {
                        this.show_message(
                            tr("app.table.import.format_unsupported").as_ref(),
                            AppMessageKind::Warning,
                            cx,
                        );
                        return;
                    };
                    this.pending_data_import =
                        Some(TableDataImportForm::new(object_path.clone(), path, format));
                    this.reload_table_import_preview(cx);
                    cx.notify();
                });
            });
        }));
    }

    /// 解析文件样本并刷新预览；不写数据库。
    fn reload_table_import_preview(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.pending_data_import.clone() else {
            return;
        };
        if form.loading {
            return;
        }
        if let Some(current) = self.pending_data_import.as_mut() {
            current.loading = true;
            current.error = None;
        }
        let controller = self.controller.clone();
        let object = form.object.clone();
        let source = form.source.clone();
        let options = form.options.clone();
        let task = cx.spawn(async move |view, cx| {
            let preview = cx
                .background_spawn(async move {
                    controller.preview_table_import(&object, &source, options)
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    if let Some(current) = this.pending_data_import.as_mut() {
                        current.loading = false;
                        match preview {
                            Ok(preview) => {
                                current.mappings = preview
                                    .columns
                                    .iter()
                                    .map(|source| fluxdb_core::TableImportColumnMapping {
                                        source: source.clone(),
                                        target: preview
                                            .target_columns
                                            .iter()
                                            .find(|column| {
                                                column.name.eq_ignore_ascii_case(source)
                                            })
                                            .map(|column| column.name.clone()),
                                    })
                                    .collect();
                                current.preview = Some(preview);
                            }
                            Err(error) => {
                                current.error = Some(error.message);
                                current.preview = None;
                            }
                        }
                    }
                    cx.notify();
                });
            });
        });
        self._data_import_task = Some(task);
    }

    fn cancel_table_import_wizard(&mut self, cx: &mut Context<Self>) {
        self.pending_data_import = None;
        self._data_import_task = None;
        cx.notify();
    }

    fn set_table_import_mode(
        &mut self,
        mode: fluxdb_core::TableImportMode,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.pending_data_import.as_mut() {
            form.options.mode = mode;
        }
        cx.notify();
    }

    fn set_table_import_mapping(
        &mut self,
        source: String,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.pending_data_import.as_mut() {
            if let Some(mapping) = form
                .mappings
                .iter_mut()
                .find(|mapping| mapping.source == source)
            {
                mapping.target = target;
            }
        }
        cx.notify();
    }

    /// 更新解析选项后重新解析；只有影响解析的字段变化时才需要重载预览。
    fn set_table_import_options(
        &mut self,
        update: impl FnOnce(&mut fluxdb_core::TableImportOptions),
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.pending_data_import.as_mut() {
            update(&mut form.options);
        }
        self.reload_table_import_preview(cx);
        cx.notify();
    }

    /// 执行导入；确认步骤由调用方负责。
    fn confirm_table_import(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.pending_data_import.clone() else {
            return;
        };
        if form.running {
            return;
        }
        if form
            .preview
            .as_ref()
            .map(|preview| preview.rows.is_empty())
            .unwrap_or(true)
        {
            if let Some(current) = self.pending_data_import.as_mut() {
                current.error = Some("没有可导入的数据行".into());
            }
            cx.notify();
            return;
        }
        if let Some(current) = self.pending_data_import.as_mut() {
            current.running = true;
            current.error = None;
            current.step = TableDataImportStep::Execute;
        }
        let controller = self.controller.clone();
        let object = form.object.clone();
        let source = form.source.clone();
        let options = form.options.clone();
        let mappings = form.mappings.clone();
        let task = cx.spawn(async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch_database_task(
                        AppCommand::ImportTableData {
                            object: object.clone(),
                            source,
                            options,
                            column_mapping: mappings,
                        },
                        &std::sync::atomic::AtomicBool::new(false),
                        &mut |_| {},
                    )
                })
                .await;
            cx.update(|cx| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                view.update(cx, |this, cx| {
                    this._data_import_task = None;
                    match event {
                        AppEvent::TableDataImported { object, report } => {
                            this.pending_data_import = None;
                            let mut message = tr("app.table.import.succeeded")
                                .replace("%{rows}", &report.imported_rows.to_string());
                            if !report.warnings.is_empty() {
                                message.push_str(": ");
                                message.push_str(
                                    &report
                                        .warnings
                                        .iter()
                                        .map(|warning| crate::localize_backend_error_text(warning))
                                        .collect::<Vec<_>>()
                                        .join("; "),
                                );
                            }
                            this.show_message(message, AppMessageKind::Success, cx);
                            this.request_data_editor_refresh_for_object(&object, cx);
                        }
                        event => {
                            let message = match event {
                                AppEvent::Failed(error) => error.message,
                                _ => tr("app.table.import.no_result").to_string(),
                            };
                            if let Some(current) = this.pending_data_import.as_mut() {
                                current.running = false;
                                current.error = Some(message);
                            }
                        }
                    }
                    cx.notify();
                });
            });
        });
        self._data_import_task = Some(task);
    }
}

/// 导入向导弹窗：解析选项 → 字段映射 → 执行。
fn table_data_import_modal(
    form: TableDataImportForm,
    kind: DatabaseKind,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let can_continue = form.preview.is_some() && !form.loading && !form.running;
    let can_import = can_continue
        && form
            .mappings
            .iter()
            .any(|mapping| mapping.target.is_some());

    let panel = div()
        .w(px(860.))
        .max_w(px(860.))
        .h(px(620.))
        .rounded(colors.radius_lg)
        .border_1()
        .border_color(colors.border)
        .bg(menu_surface_bg(colors))
        .shadow(vec![box_shadow(
            px(0.),
            px(18.),
            px(42.),
            px(0.),
            hsla(0., 0., 0., if colors.is_dark { 0.42 } else { 0.18 }),
        )])
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_size(px(13.))
        .text_color(colors.text)
        .key_context("TableDataImportModal")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(table_data_import_header(&form, colors, cx))
        .child(table_data_import_steps(form.step, colors))
        .child(match form.step {
            TableDataImportStep::Source => table_data_import_source_body(&form, kind, colors, cx)
                .into_any_element(),
            TableDataImportStep::Mapping => {
                table_data_import_mapping_body(&form, colors, cx).into_any_element()
            }
            TableDataImportStep::Execute => {
                table_data_import_execute_body(&form, colors).into_any_element()
            }
        })
        .child(table_data_import_footer(
            &form,
            can_continue,
            can_import,
            colors,
            cx,
        ));

    div()
        .absolute()
        .inset_0()
        .occlude()
        .bg(if colors.is_dark {
            opaque_grey(0.02, 0.58)
        } else {
            opaque_grey(0.75, 0.28)
        })
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
            this.cancel_table_import_wizard(cx);
            cx.stop_propagation();
        }))
        .child(panel)
}

fn table_data_import_header(
    form: &TableDataImportForm,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .h(px(56.))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(colors.border_soft)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tr("app.data.toolbar.import")),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(format!(
                            "{} · {} · {}",
                            form.object.name,
                            form.format.label(),
                            form.source
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        )),
                ),
        )
        .child(
            Button::new("data-import-close")
                .label(tr("app.common.cancel"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_table_import_wizard(cx);
                    cx.stop_propagation();
                })),
        )
}

fn table_data_import_steps(step: TableDataImportStep, colors: UiColors) -> Div {
    let mut row = div()
        .h(px(40.))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(colors.border_soft);
    for (index, current) in [
        TableDataImportStep::Source,
        TableDataImportStep::Mapping,
        TableDataImportStep::Execute,
    ]
    .into_iter()
    .enumerate()
    {
        let active = current == step;
        let done = index < step_index(step);
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(11.))
                .text_color(if active {
                    colors.info
                } else if done {
                    colors.text
                } else {
                    colors.muted
                })
                .child(format!("{}. {}", index + 1, current.label())),
        );
        if index < 2 {
            row = row.child(div().text_size(px(11.)).text_color(colors.muted).child("›"));
        }
    }
    row
}

fn step_index(step: TableDataImportStep) -> usize {
    match step {
        TableDataImportStep::Source => 0,
        TableDataImportStep::Mapping => 1,
        TableDataImportStep::Execute => 2,
    }
}

fn table_data_import_source_body(
    form: &TableDataImportForm,
    kind: DatabaseKind,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let mut body = div()
        .flex_1()
        .min_h(px(0.))
        .px_5()
        .py_4()
        .flex()
        .flex_col()
        .gap_4();

    // 目标方式：创建新表保留为可见但置灰的预留项。
    body = body.child(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.muted)
                    .child(tr("app.data.data_import.target_mode")),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(table_data_import_target_option(
                        "data-import-target-existing",
                        tr("app.data.data_import.target_existing").as_ref(),
                        tr("app.data.data_import.target_existing_hint").as_ref(),
                        form.target == TableDataImportTarget::Existing,
                        true,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            if let Some(form) = this.pending_data_import.as_mut() {
                                form.target = TableDataImportTarget::Existing;
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ))
                    .child(table_data_import_target_option(
                        "data-import-target-create",
                        tr("app.data.data_import.target_create").as_ref(),
                        tr("app.data.data_import.target_create_disabled").as_ref(),
                        false,
                        false,
                        colors,
                        // 置灰项不绑定点击：保留外观，便于后续开放。
                        |_, _, _| {},
                    )),
            ),
    );

    // 导入模式：追加为默认，清空后导入可切换。
    body = body.child(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.muted)
                    .child(tr("app.data.data_import.mode")),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(table_data_import_mode_option(
                        "data-import-mode-append",
                        fluxdb_core::TableImportMode::Append,
                        form.options.mode,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.set_table_import_mode(fluxdb_core::TableImportMode::Append, cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(table_data_import_mode_option(
                        "data-import-mode-truncate",
                        fluxdb_core::TableImportMode::Truncate,
                        form.options.mode,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.set_table_import_mode(fluxdb_core::TableImportMode::Truncate, cx);
                            cx.stop_propagation();
                        }),
                    )),
            ),
    );

    if let Some(warning) = form.truncate_warning(kind) {
        body = body.child(
            div()
                .rounded(colors.radius)
                .border_1()
                .border_color(colors.warning)
                .px_3()
                .py_2()
                .text_size(px(11.))
                .text_color(colors.warning)
                .child(warning),
        );
    }

    // 解析选项：只对分隔文本生效的项按格式显示。
    body = body.child(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.muted)
                    .child(tr("app.data.data_import.parse_options")),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(table_data_import_encoding_options(form, colors, cx))
                    .when(
                        matches!(
                            form.format,
                            fluxdb_core::TableImportFormat::Txt
                                | fluxdb_core::TableImportFormat::Csv
                        ),
                        |this| this.child(table_data_import_toggle_option(
                            "data-import-has-header",
                            tr("app.data.data_import.has_header").to_string(),
                            form.options.has_header,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.set_table_import_options(
                                    |options| options.has_header = !options.has_header,
                                    cx,
                                );
                                cx.stop_propagation();
                            }),
                        )),
                    )
                    .when(
                        matches!(
                            form.format,
                            fluxdb_core::TableImportFormat::Txt
                                | fluxdb_core::TableImportFormat::Csv
                        ),
                        |this| {
                            let mut this = this.child(table_data_import_toggle_option(
                                "data-import-trim",
                                tr("app.data.data_import.trim").to_string(),
                                form.options.trim_whitespace,
                                colors,
                                cx.listener(|this, _, _, cx| {
                                    this.set_table_import_options(
                                        |options| {
                                            options.trim_whitespace = !options.trim_whitespace
                                        },
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            ));
                            this = this.child(table_data_import_toggle_option(
                                "data-import-empty-null",
                                tr("app.data.data_import.empty_as_null").to_string(),
                                form.options.empty_as_null,
                                colors,
                                cx.listener(|this, _, _, cx| {
                                    this.set_table_import_options(
                                        |options| {
                                            options.empty_as_null = !options.empty_as_null
                                        },
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            ));
                            this
                        },
                    ),
            ),
    );

    if form.loading {
        body = body.child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(tr("app.data.data_import.parsing")),
        );
    }
    if let Some(error) = &form.error {
        body = body.child(
            div()
                .text_size(px(11.))
                .text_color(colors.danger)
                .child(error.clone()),
        );
    }
    if let Some(preview) = &form.preview {
        body = body.child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(format!(
                    "{} · {} 列 · 预览 {} 行{}",
                    preview.encoding,
                    preview.columns.len(),
                    preview.rows.len(),
                    if preview.truncated { "（仅展示前 50 行）" } else { "" }
                )),
        );
    }
    body
}

fn table_data_import_encoding_options(
    form: &TableDataImportForm,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Vec<gpui::AnyElement> {
    ["UTF-8", "GB18030"]
        .into_iter()
        .map(|encoding| {
            let selected = if form.options.encoding.trim().is_empty() {
                encoding == "UTF-8"
            } else {
                form.options.encoding.eq_ignore_ascii_case(encoding)
            };
            table_data_import_toggle_option(
                format!("data-import-encoding-{encoding}"),
                encoding.to_string(),
                selected,
                colors,
                cx.listener(move |this, _, _, cx| {
                    this.set_table_import_options(
                        |options| {
                            options.encoding = if encoding == "UTF-8" {
                                String::new()
                            } else {
                                encoding.to_string()
                            };
                        },
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
        })
        .collect()
}

fn table_data_import_toggle_option<F>(
    id: impl Into<gpui::ElementId>,
    label: String,
    selected: bool,
    colors: UiColors,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded(colors.radius)
        .border_1()
        .border_color(if selected { colors.info } else { colors.border })
        .when(selected, |this| this.bg(colors.hover))
        .cursor_pointer()
        .text_size(px(10.5))
        .hover(move |style| style.bg(colors.hover))
        .on_mouse_down(MouseButton::Left, on_click)
        .child(label)
}

fn table_data_import_target_option<F>(
    id: &'static str,
    title: &str,
    hint: &str,
    selected: bool,
    enabled: bool,
    colors: UiColors,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
{
    let title = title.to_string();
    let hint = hint.to_string();
    div()
        .id(id)
        .w(px(240.))
        .px_3()
        .py_2()
        .rounded(colors.radius)
        .border_1()
        .border_color(if selected { colors.info } else { colors.border })
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(move |style| style.bg(colors.hover))
                .on_mouse_down(MouseButton::Left, on_click)
        })
        .when(!enabled, |this| this.opacity(0.5))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(10.5))
                .text_color(colors.muted)
                .child(hint),
        )
}

fn table_data_import_mode_option<F>(
    id: &'static str,
    mode: fluxdb_core::TableImportMode,
    selected: fluxdb_core::TableImportMode,
    colors: UiColors,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
{
    let active = mode == selected;
    let label = match mode {
        fluxdb_core::TableImportMode::Append => tr("app.data.data_import.mode_append"),
        fluxdb_core::TableImportMode::Truncate => tr("app.data.data_import.mode_truncate"),
    };
    div()
        .id(id)
        .w(px(180.))
        .px_3()
        .py_2()
        .rounded(colors.radius)
        .border_1()
        .border_color(if active { colors.info } else { colors.border })
        .cursor_pointer()
        .hover(move |style| style.bg(colors.hover))
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_size(px(12.))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .child(label),
        )
}

fn table_data_import_mapping_body(
    form: &TableDataImportForm,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let Some(preview) = &form.preview else {
        return div()
            .flex_1()
            .px_5()
            .py_4()
            .text_color(colors.muted)
            .child(tr("app.data.data_import.no_preview"))
            .into_any_element();
    };

    // 表头：源列 / 目标列。
    let mut table = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_3()
                .px_2()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.muted)
                .child(div().w(px(220.)).child(tr("app.data.data_import.source_column")))
                .child(div().flex_1().child(tr("app.data.data_import.target_column"))),
        );

    for source in &preview.columns {
        let current = form
            .mappings
            .iter()
            .find(|mapping| &mapping.source == source)
            .and_then(|mapping| mapping.target.clone());
        let matched = current.is_some();
        let mut options = div().flex().flex_wrap().gap_1();
        // 跳过该列。
        options = options.child(table_data_import_mapping_chip(
            format!("skip:{source}"),
            tr("app.data.data_import.skip").to_string(),
            !matched,
            colors,
            {
                let source = source.clone();
                cx.listener(move |this, _, _, cx| {
                    this.set_table_import_mapping(source.clone(), None, cx);
                    cx.stop_propagation();
                })
            },
        ));
        for column in &preview.target_columns {
            let selected = current.as_deref() == Some(column.name.as_str());
            let target = column.name.clone();
            let label = match &column.type_name {
                Some(type_name) => format!("{} · {}", column.name, type_name),
                None => column.name.clone(),
            };
            options = options.child(table_data_import_mapping_chip(
                format!("map:{source}:{}", column.name),
                label,
                selected,
                colors,
                {
                    let source = source.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.set_table_import_mapping(
                            source.clone(),
                            Some(target.clone()),
                            cx,
                        );
                        cx.stop_propagation();
                    })
                },
            ));
        }
        table = table.child(
            div()
                .flex()
                .gap_3()
                .items_start()
                .px_2()
                .py_1()
                .child(
                    div()
                        .w(px(220.))
                        .min_w(px(0.))
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(source.clone()),
                )
                .child(options),
        );
    }
    div()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scrollbar()
        .px_5()
        .py_4()
        .child(table)
        .into_any_element()
}

fn table_data_import_mapping_chip(
    id: impl Into<gpui::ElementId>,
    label: String,
    selected: bool,
    colors: UiColors,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded(colors.radius)
        .border_1()
        .border_color(if selected { colors.info } else { colors.border })
        .when(selected, |this| this.bg(colors.hover))
        .cursor_pointer()
        .text_size(px(10.5))
        .on_mouse_down(MouseButton::Left, on_click)
        .child(label)
}

fn table_data_import_execute_body(
    form: &TableDataImportForm,
    colors: UiColors,
) -> Div {
    let mut body = div()
        .flex_1()
        .min_h(px(0.))
        .px_5()
        .py_4()
        .flex()
        .flex_col()
        .gap_3();
    if form.running {
        body = body.child(
            div()
                .text_color(colors.muted)
                .child(tr("app.data.data_import.running")),
        );
    }
    if let Some(progress) = &form.progress {
        body = body.child(div().text_size(px(11.)).text_color(colors.muted).child(progress.clone()));
    }
    if let Some(error) = &form.error {
        body = body.child(
            div()
                .text_size(px(11.))
                .text_color(colors.danger)
                .child(error.clone()),
        );
    }
    body
}

fn table_data_import_footer(
    form: &TableDataImportForm,
    can_continue: bool,
    can_import: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let step = form.step;
    div()
        .h(px(58.))
        .flex_none()
        .px_5()
        .border_t_1()
        .border_color(colors.border_soft)
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .gap_2()
                .when(step != TableDataImportStep::Source, |this| {
                    this.child(
                        Button::new("data-import-back")
                            .label(tr("app.data.data_import.back"))
                            .disabled(form.running)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(form) = this.pending_data_import.as_mut() {
                                    form.step = match form.step {
                                        TableDataImportStep::Execute => {
                                            TableDataImportStep::Mapping
                                        }
                                        _ => TableDataImportStep::Source,
                                    };
                                }
                                cx.notify();
                                cx.stop_propagation();
                            })),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("data-import-cancel")
                        .label(tr("app.common.cancel"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_table_import_wizard(cx);
                            cx.stop_propagation();
                        })),
                )
                .when(step == TableDataImportStep::Source, |this| {
                    this.child(
                        Button::new("data-import-next")
                            .label(tr("app.data.data_import.next"))
                            .disabled(!can_continue)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(form) = this.pending_data_import.as_mut() {
                                    form.step = TableDataImportStep::Mapping;
                                }
                                cx.notify();
                                cx.stop_propagation();
                            })),
                    )
                })
                .when(step == TableDataImportStep::Mapping, |this| {
                    this.child(
                        Button::new("data-import-start")
                            .label(tr("app.data.data_import.start"))
                            .disabled(!can_import)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_table_import(cx);
                                cx.stop_propagation();
                            })),
                    )
                }),
        )
}


