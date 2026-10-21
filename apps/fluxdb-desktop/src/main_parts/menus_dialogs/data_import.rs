// Included in crate-root scope by ../menus_dialogs.rs; 表数据导入向导弹窗。

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
    /// 文件与格式在弹框内选择；未选择时仍可看到解析选项和置灰的预留项。
    source: Option<PathBuf>,
    format: Option<fluxdb_core::TableImportFormat>,
    options: fluxdb_core::TableImportOptions,
    target: TableDataImportTarget,
    preview: Option<fluxdb_core::TableImportPreview>,
    mappings: Vec<fluxdb_core::TableImportColumnMapping>,
    error: Option<String>,
    running: bool,
    loading: bool,
}

impl TableDataImportForm {
    fn new(object: ObjectPath) -> Self {
        Self {
            object,
            source: None,
            format: None,
            options: fluxdb_core::TableImportOptions::new(),
            target: TableDataImportTarget::Existing,
            preview: None,
            mappings: Vec::new(),
            error: None,
            running: false,
            loading: false,
        }
    }

    fn source_name(&self) -> Option<String> {
        self.source
            .as_ref()?
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
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

    /// 打开导入向导。弹框先出现，文件在框内选择。
    fn open_table_import_wizard(
        &mut self,
        object: ObjectPath,
        _window: &mut Window,
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
        self.pending_data_import = Some(TableDataImportForm::new(object));
        cx.notify();
    }

    /// 在弹框内选择导入文件；选好后解析预览。
    fn choose_table_import_source(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(object) = self
            .pending_data_import
            .as_ref()
            .map(|form| form.object.clone())
        else {
            return;
        };
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
                    let Some(form) = this.pending_data_import.as_mut() else {
                        return;
                    };
                    if form.object != object {
                        return;
                    }
                    form.source = Some(path);
                    form.format = Some(format);
                    form.preview = None;
                    form.mappings.clear();
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
        let (Some(source), Some(_format)) = (form.source.clone(), form.format) else {
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
        let Some(source) = form.source.clone() else {
            if let Some(current) = self.pending_data_import.as_mut() {
                current.error = Some(tr("app.data.data_import.choose_first").to_string());
            }
            cx.notify();
            return;
        };
        if form
            .preview
            .as_ref()
            .map(|preview| preview.rows.is_empty())
            .unwrap_or(true)
        {
            if let Some(current) = self.pending_data_import.as_mut() {
                current.error = Some(tr("app.data.data_import.no_rows").to_string());
            }
            cx.notify();
            return;
        }
        if let Some(current) = self.pending_data_import.as_mut() {
            current.running = true;
            current.error = None;
        }
        let controller = self.controller.clone();
        let object = form.object.clone();
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

/// 导入弹窗：单页完成配置、映射与预览，避免多步骤来回切换。
fn table_data_import_modal(
    form: TableDataImportForm,
    kind: DatabaseKind,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let has_preview = form.preview.is_some();
    let can_import = has_preview
        && !form.loading
        && !form.running
        && form.mappings.iter().any(|mapping| mapping.target.is_some());

    let panel = div()
        .w(px(1080.))
        .max_w(px(1080.))
        .h(px(700.))
        .rounded(px(12.))
        .border_1()
        .border_color(colors.border)
        .bg(menu_surface_bg(colors))
        .shadow(vec![box_shadow(
            px(0.),
            px(28.),
            px(70.),
            px(0.),
            hsla(0., 0., 0., if colors.is_dark { 0.55 } else { 0.2 }),
        )])
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_size(px(13.))
        .text_color(colors.text)
        .key_context("TableDataImportModal")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(table_data_import_header(&form, colors, cx))
        .child(table_data_import_summary(&form, colors, cx))
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .px_6()
                .pb_4()
                .flex()
                .gap_4()
                .child(table_data_import_mapping_panel(&form, colors, cx))
                .child(table_data_import_preview_panel(&form, colors)),
        )
        .child(table_data_import_options_bar(&form, kind, colors, cx))
        .child(table_data_import_footer(&form, can_import, colors, cx));

    div()
        .absolute()
        .inset_0()
        .occlude()
        .bg(if colors.is_dark {
            opaque_grey(0.02, 0.6)
        } else {
            opaque_grey(0.75, 0.3)
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
        .h(px(58.))
        .flex_none()
        .px_5()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(app_icon(AppIcon::FolderInput, 17., colors.text))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tr("app.data.data_import.title")),
                ),
        )
        .child(
            Button::new("data-import-close")
                .ghost()
                .compact()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_table_import_wizard(cx);
                    cx.stop_propagation();
                })),
        )
        .when(form.running, |this| {
            this.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(tr("app.data.data_import.running")),
            )
        })
}

/// 目标表 + 选择文件 + 三项概览，对应 dbx 的首屏信息密度。
fn table_data_import_summary(
    form: &TableDataImportForm,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let mapped = form
        .mappings
        .iter()
        .filter(|mapping| mapping.target.is_some())
        .count();
    let total_columns = form
        .preview
        .as_ref()
        .map(|preview| preview.columns.len())
        .unwrap_or(0);
    let preview_rows = form
        .preview
        .as_ref()
        .map(|preview| preview.rows.len())
        .unwrap_or(0);

    div()
        .flex_none()
        .px_6()
        .pb_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_stretch()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(colors.border_soft)
                        .bg(colors.panel_alt)
                        .px_4()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(colors.muted)
                                .child(tr("app.data.data_import.target_table")),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(form.object.name.clone()),
                        ),
                )
                .child(
                    Button::new("data-import-choose-source")
                        .label(if form.source.is_some() {
                            tr("app.data.data_import.choose_file")
                        } else {
                            tr("app.data.data_import.choose_file")
                        })
                                .on_click(cx.listener(|this, _, window, cx| {
                            this.choose_table_import_source(window, cx);
                            cx.stop_propagation();
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(table_data_import_stat(
                    tr("app.data.data_import.summary_file").as_ref(),
                    &form
                        .source_name()
                        .unwrap_or_else(|| tr("app.data.data_import.no_file").to_string()),
                    colors,
                ))
                .child(table_data_import_stat(
                    tr("app.data.data_import.summary_rows").as_ref(),
                    &if form.preview.is_some() {
                        preview_rows.to_string()
                    } else {
                        "—".to_string()
                    },
                    colors,
                ))
                .child(table_data_import_stat(
                    tr("app.data.data_import.summary_mapped").as_ref(),
                    &if form.preview.is_some() {
                        format!("{mapped} / {total_columns}")
                    } else {
                        "—".to_string()
                    },
                    colors,
                )),
        )
        .when(form.loading, |this| {
            this.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(app_icon(AppIcon::Loader, 13., colors.info))
                    .child(tr("app.data.data_import.parsing")),
            )
        })
        .when_some(form.error.clone(), |this, error| {
            this.child(table_data_import_notice(error, colors.danger, colors))
        })
}

fn table_data_import_stat(label: &str, value: &str, colors: UiColors) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .rounded(px(10.))
        .border_1()
        .border_color(colors.border_soft)
        .bg(colors.panel_alt)
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(10.5))
                .text_color(colors.muted)
                .child(label.to_string()),
        )
        .child(
            div()
                .truncate()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value.to_string()),
        )
}

/// 左侧字段映射：源字段 → 目标字段下拉，行高压紧避免滚动疲劳。
fn table_data_import_mapping_panel(
    form: &TableDataImportForm,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let mut list = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2();
    if let Some(preview) = &form.preview {
        for source in &preview.columns {
            let current = form
                .mappings
                .iter()
                .find(|mapping| &mapping.source == source)
                .and_then(|mapping| mapping.target.clone());
            list = list.child(table_data_import_mapping_row(
                source,
                current,
                preview.target_columns.clone(),
                cx,
                colors,
            ));
        }
    } else {
        list = list.child(
            div()
                .p_4()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(tr("app.data.data_import.mapping_empty")),
        );
    }

    div()
        .w(px(430.))
        .flex_none()
        .min_h(px(0.))
        .rounded(px(10.))
        .border_1()
        .border_color(colors.border_soft)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .h(px(38.))
                .px_4()
                .border_b_1()
                .border_color(colors.border_soft)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tr("app.data.data_import.mapping_title")),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(colors.muted)
                        .child(tr("app.data.data_import.mapping_hint")),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scrollbar()
                .child(list),
        )
}

fn table_data_import_mapping_row(
    source: &str,
    current: Option<String>,
    columns: Vec<fluxdb_core::TableImportTargetColumn>,
    cx: &mut Context<NavicatMain>,
    colors: UiColors,
) -> Div {
    let skipped = current.is_none();
    let label = current
        .clone()
        .unwrap_or_else(|| tr("app.data.data_import.skip").to_string());
    let source_name = source.to_string();
    let selected = current.clone();
    let weak = cx.entity().downgrade();
    let menu_columns = columns.clone();

    div()
        .h(px(40.))
        .px_2()
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(140.))
                .min_w(px(0.))
                .truncate()
                .text_size(px(12.))
                .child(source_name.clone()),
        )
        .child(
            Button::new(SharedString::from(format!("data-import-map:{source}")))
                .label(label)
                .outline()
                .dropdown_caret(true)
                .compact()
                .w(px(210.))
                .when(skipped, |this| this.text_color(colors.muted))
                .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| {
                    let mut menu = menu.min_w(px(240.)).max_w(px(320.)).check_side(gpui_component::Side::Right);

                    let skip_view = weak.clone();
                    let skip_source = source_name.clone();
                    menu = menu.item(
                        PopupMenuItem::new(tr("app.data.data_import.skip"))
                            .checked(selected.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(view) = skip_view.upgrade() {
                                    view.update(cx, |this, cx| {
                                        this.set_table_import_mapping(skip_source.clone(), None, cx);
                                    });
                                }
                            }),
                    );

                    for column in &menu_columns {
                        let target = column.name.clone();
                        let map_view = weak.clone();
                        let map_source = source_name.clone();
                        let item_label = match &column.type_name {
                            Some(type_name) => format!("{} · {}", column.name, type_name),
                            None => column.name.clone(),
                        };
                        let checked = selected.as_deref() == Some(column.name.as_str());
                        menu = menu.item(
                            PopupMenuItem::new(item_label)
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    if let Some(view) = map_view.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.set_table_import_mapping(
                                                map_source.clone(),
                                                Some(target.clone()),
                                                cx,
                                            );
                                        });
                                    }
                                }),
                        );
                    }
                    menu
                }),
        )
}

/// 右侧数据预览：等宽列 + 斑马纹，最多展示预览行数。
fn table_data_import_preview_panel(form: &TableDataImportForm, colors: UiColors) -> Div {
    let mut content = div().flex().flex_col().px_3().py_2();
    if let Some(preview) = &form.preview {
        let mut header = div().flex().flex_none().h(px(32.));
        for column in &preview.columns {
            header = header.child(
                div()
                    .w(px(160.))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .truncate()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.muted)
                    .child(column.clone()),
            );
        }
        content = content.child(header);
        for (row_index, row) in preview.rows.iter().enumerate() {
            let mut line = div()
                .flex()
                .flex_none()
                .h(px(30.))
                .rounded(px(6.))
                .when(row_index % 2 == 1, |this| this.bg(colors.panel_alt));
            for column_index in 0..preview.columns.len() {
                let value = row.get(column_index).cloned().unwrap_or_default();
                line = line.child(
                    div()
                        .w(px(160.))
                        .flex_none()
                        .px_2()
                        .flex()
                        .items_center()
                        .truncate()
                        .text_size(px(11.5))
                        .child(value),
                );
            }
            content = content.child(line);
        }
    } else {
        content = content.child(
            div()
                .p_4()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(tr("app.data.data_import.preview_empty")),
        );
    }

    div()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .rounded(px(10.))
        .border_1()
        .border_color(colors.border_soft)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .h(px(38.))
                .px_4()
                .border_b_1()
                .border_color(colors.border_soft)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tr("app.data.data_import.preview_title")),
                )
                .when_some(form.preview.as_ref(), |this, preview| {
                    this.child(
                        div()
                            .text_size(px(10.5))
                            .text_color(colors.muted)
                            .child(if preview.truncated {
                                tr("app.data.data_import.preview_truncated").to_string()
                            } else {
                                format!("{} · {}", preview.encoding, preview.format)
                            }),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .overflow_x_scrollbar()
                .child(content),
        )
}

/// 底部选项条：导入模式与解析选项集中一行，减少纵向堆叠。
fn table_data_import_options_bar(
    form: &TableDataImportForm,
    kind: DatabaseKind,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let delimited = matches!(
        form.format,
        Some(fluxdb_core::TableImportFormat::Txt | fluxdb_core::TableImportFormat::Csv)
    );
    let mut row = div()
        .flex_none()
        .px_6()
        .pb_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            // 选项区与上方表格用分隔线切开，避免看起来是表格的一部分。
            div().h(px(1.)).w_full().bg(colors.border_soft),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_5()
                .child(table_data_import_field(
                    tr("app.data.data_import.mode").as_ref(),
                    table_data_import_mode_dropdown(form, cx).into_any_element(),
                    colors,
                ))
                .child(table_data_import_field(
                    tr("app.data.data_import.target_mode").as_ref(),
                    table_data_import_target_dropdown(form, cx).into_any_element(),
                    colors,
                ))
                .child(table_data_import_field(
                    tr("app.data.data_import.encoding").as_ref(),
                    table_data_import_encoding_dropdown(form, cx)
                        .into_any_element(),
                    colors,
                ))
                .child(div().flex_1())
                .when(delimited, |this| {
                    this.child(table_data_import_option_toggle(
                        "data-import-has-header",
                        tr("app.data.data_import.has_header").to_string(),
                        form.options.has_header,
                        colors,
                        cx,
                    ))
                    .child(table_data_import_option_toggle(
                        "data-import-trim",
                        tr("app.data.data_import.trim").to_string(),
                        form.options.trim_whitespace,
                        colors,
                        cx,
                    ))
                    .child(table_data_import_option_toggle(
                        "data-import-empty-null",
                        tr("app.data.data_import.empty_as_null").to_string(),
                        form.options.empty_as_null,
                        colors,
                        cx,
                    ))
                }),
        );

    if let Some(warning) = form.truncate_warning(kind) {
        row = row.child(table_data_import_notice(warning, colors.warning, colors));
    }
    row
}

/// 目标方式下拉：保留“创建新表”为置灰预留项，而不是隐藏。
fn table_data_import_target_dropdown(
    form: &TableDataImportForm,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let weak = cx.entity().downgrade();
    let current = form.target;
    Button::new("data-import-target")
        .label(match current {
            TableDataImportTarget::Existing => {
                tr("app.data.data_import.target_existing")
            }
            TableDataImportTarget::Create => tr("app.data.data_import.target_create"),
        })
        .outline()
        .dropdown_caret(true)
        .compact()
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(px(240.));
            let exist_view = weak.clone();
            menu = menu.item(
                PopupMenuItem::new(tr("app.data.data_import.target_existing"))
                    .checked(current == TableDataImportTarget::Existing)
                    .on_click(move |_, _, cx| {
                        if let Some(view) = exist_view.upgrade() {
                            view.update(cx, |this, cx| {
                                if let Some(form) = this.pending_data_import.as_mut() {
                                    form.target = TableDataImportTarget::Existing;
                                }
                                cx.notify();
                            });
                        }
                    }),
            );
            menu.item(
                PopupMenuItem::new(tr("app.data.data_import.target_create"))
                    .disabled(true)
                    .checked(false),
            )
        })
}

/// 文件编码下拉：选项固定且互斥，改选后立即重新解析预览。
fn table_data_import_encoding_dropdown(
    form: &TableDataImportForm,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let weak = cx.entity().downgrade();
    let current = if form.options.encoding.trim().is_empty() {
        "UTF-8".to_string()
    } else {
        form.options.encoding.trim().to_ascii_uppercase()
    };
    Button::new("data-import-encoding")
        .label(current.clone())
        .outline()
        .dropdown_caret(true)
        .compact()
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(px(180.));
            for encoding in ["UTF-8", "GB18030"] {
                let view = weak.clone();
                let value = encoding.to_string();
                let label = encoding.to_string();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(current == encoding)
                        .on_click(move |_, _, cx| {
                            if let Some(view) = view.upgrade() {
                                let value = value.clone();
                                view.update(cx, |this, cx| {
                                    this.set_table_import_options(
                                        move |options| {
                                            // UTF-8 是默认编码，空值表示默认，避免多存一份状态。
                                            options.encoding = if value == "UTF-8" {
                                                String::new()
                                            } else {
                                                value.clone()
                                            };
                                        },
                                        cx,
                                    );
                                });
                            }
                        }),
                );
            }
            menu
        })
}

fn table_data_import_mode_dropdown(
    form: &TableDataImportForm,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let weak = cx.entity().downgrade();
    let mode = form.options.mode;
    let label = match mode {
        fluxdb_core::TableImportMode::Append => tr("app.data.data_import.mode_append"),
        fluxdb_core::TableImportMode::Truncate => tr("app.data.data_import.mode_truncate"),
    };
    Button::new("data-import-mode")
        .label(label)
        .outline()
        .dropdown_caret(true)
        .compact()
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let mut menu = menu.min_w(px(200.));
            for option in [
                fluxdb_core::TableImportMode::Append,
                fluxdb_core::TableImportMode::Truncate,
            ] {
                let option_label = match option {
                    fluxdb_core::TableImportMode::Append => {
                        tr("app.data.data_import.mode_append")
                    }
                    fluxdb_core::TableImportMode::Truncate => {
                        tr("app.data.data_import.mode_truncate")
                    }
                };
                let view = weak.clone();
                menu = menu.item(
                    PopupMenuItem::new(option_label)
                        .checked(option == mode)
                        .on_click(move |_, _, cx| {
                            if let Some(view) = view.upgrade() {
                                view.update(cx, |this, cx| {
                                    this.set_table_import_mode(option, cx);
                                });
                            }
                        }),
                );
            }
            menu
        })
}

fn table_data_import_option_toggle(
    id: &'static str,
    label: String,
    selected: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> impl IntoElement {
    let key = id.to_string();
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .rounded(px(14.))
        .border_1()
        .border_color(if selected {
            colors.info
        } else {
            colors.border
        })
        .when(selected, |this| {
            this.bg(colors.panel_bg)
                .font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .when(!selected, |this| {
            this.bg(colors.input_bg).text_color(colors.muted)
        })
        .cursor_pointer()
        .text_size(px(11.))
        .flex()
        .items_center()
        .gap_1p5()
        .hover(move |style| style.bg(colors.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                let key = key.clone();
                this.set_table_import_options(
                    move |options| match key.as_str() {
                        "data-import-has-header" => options.has_header = !options.has_header,
                        "data-import-trim" => {
                            options.trim_whitespace = !options.trim_whitespace
                        }
                        "data-import-empty-null" => {
                            options.empty_as_null = !options.empty_as_null
                        }
                        _ => {}
                    },
                    cx,
                );
                cx.stop_propagation();
            }),
        )
        // 勾选图标让开关状态一眼可辨，而不是只靠颜色。
        .child(app_icon(
            if selected {
                AppIcon::Check
            } else {
                AppIcon::Circle
            },
            11.,
            if selected { colors.info } else { colors.muted },
        ))
        .child(label)
}

/// 表单字段：上方小标签、下方控件，让控件归属明确。
fn table_data_import_field(
    label: &str,
    control: gpui::AnyElement,
    colors: UiColors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(10.5))
                .text_color(colors.muted)
                .child(label.to_string()),
        )
        .child(control)
}

fn table_data_import_notice(text: String, tone: gpui::Rgba, colors: UiColors) -> Div {
    div()
        .w_full()
        .rounded(px(8.))
        .bg(colors.panel_alt)
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .child(app_icon(AppIcon::Info, 13., tone))
        .child(div().text_size(px(11.)).text_color(tone).child(text))
}

fn table_data_import_footer(
    form: &TableDataImportForm,
    can_import: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .h(px(64.))
        .flex_none()
        .px_5()
        .border_t_1()
        .border_color(colors.border_soft)
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .child(
            Button::new("data-import-cancel")
                .label(tr("app.common.cancel"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_table_import_wizard(cx);
                    cx.stop_propagation();
                })),
        )
        .child(
            Button::new("data-import-start")
                .label(tr("app.data.data_import.start"))
                .primary()
                .disabled(!can_import || form.running)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_table_import(cx);
                    cx.stop_propagation();
                })),
        )
}
