// MongoDB 集合工作台（按 docs/prototype/mongodb-ui.html）：分节页签 + 查询面板 +
// 文档/表格/JSON 三种展示 + 右侧文档详情抽屉。
//
// 只读边界：视图（`read_only`）不提供任何写入入口；文档值只在本页渲染，
// 不进入 AI 上下文（AI 侧另行按需走有界只读工具）。

const MONGO_QUERY_ROW_HEIGHT: f32 = 34.;
const MONGO_SECTION_HEIGHT: f32 = 38.;
const MONGO_TOOLBAR_HEIGHT: f32 = 38.;
const MONGO_PAGER_HEIGHT: f32 = 34.;
const MONGO_INSPECTOR_WIDTH: f32 = 380.;
/// 表格单元格最大字符数：超长值截断，完整内容看详情抽屉。
const MONGO_VALUE_MAX_CHARS: usize = 240;

/// 分节页签顺序与文案 key（顺序即页签顺序）。
const MONGO_SECTIONS: [(MongoSection, &str); 4] = [
    (MongoSection::Documents, "app.mongo.section.documents"),
    (MongoSection::Aggregation, "app.mongo.section.aggregation"),
    (MongoSection::Indexes, "app.mongo.section.indexes"),
    (MongoSection::Info, "app.mongo.section.info"),
];

/// 结果展示方式页签（顺序即页签顺序）。
const MONGO_DISPLAY_MODES: [(MongoDisplayMode, &str); 3] = [
    (MongoDisplayMode::Documents, "app.mongo.display.documents"),
    (MongoDisplayMode::Table, "app.mongo.display.table"),
    (MongoDisplayMode::Json, "app.mongo.display.json"),
];

/// 错误块的重试动作：按出错区域决定重跑哪条命令。
#[derive(Clone, Copy)]
enum MongoRetry {
    Documents,
    Aggregation,
    Info,
    Indexes,
}

impl NavicatMain {
    pub(crate) fn mongo_collection_content(
        &mut self,
        tab_id: TabId,
        state: &MongoCollectionState,
        window: &mut Window,
        colors: UiColors,
        cx: &mut Context<NavicatMain>,
    ) -> Div {
        let inputs = self.mongo_query_inputs_for(tab_id, window, cx);
        let section_bar = mongo_section_bar(tab_id, state, colors, cx);
        let body = match state.section {
            MongoSection::Documents => {
                self.mongo_documents_section(tab_id, state, inputs, colors, cx)
            }
            MongoSection::Aggregation => {
                self.mongo_aggregation_section(tab_id, state, inputs, colors, cx)
            }
            MongoSection::Indexes => mongo_indexes_section(tab_id, state, colors, cx),
            MongoSection::Info => mongo_info_section(tab_id, state, colors, cx),
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .bg(colors.panel_bg)
            .flex()
            .flex_col()
            .child(mongo_namespace_strip(state, colors))
            .child(section_bar)
            .child(body)
    }

    fn mongo_documents_section(
        &mut self,
        tab_id: TabId,
        state: &MongoCollectionState,
        inputs: Entity<MongoQueryInputs>,
        colors: UiColors,
        cx: &mut Context<NavicatMain>,
    ) -> Div {
        let selected = state.selected_document.and_then(|index| {
            state
                .page
                .as_ref()
                .and_then(|page| page.documents.get(index))
                .map(|document| (index, document))
        });
        let results = mongo_results(tab_id, state, colors, cx);
        let body = match selected {
            Some((index, document)) => div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .child(results)
                .child(mongo_inspector(tab_id, index, document, colors, cx)),
            None => results,
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(mongo_query_panel(tab_id, inputs, colors, cx))
            .child(mongo_results_toolbar(tab_id, state, colors, cx))
            .child(body)
            .child(mongo_pager(tab_id, state, colors, cx))
    }

    fn mongo_aggregation_section(
        &mut self,
        tab_id: TabId,
        state: &MongoCollectionState,
        inputs: Entity<MongoQueryInputs>,
        colors: UiColors,
        cx: &mut Context<NavicatMain>,
    ) -> Div {
        let running = matches!(state.aggregation, LoadState::Loading);
        let textarea = inputs.read(cx).aggregation.clone();
        let editor = div()
            .h(px(150.))
            .border_b_1()
            .border_color(colors.border)
            .p_2()
            .child(Textarea::new(&textarea).h(px(138.)).bordered(false));
        let bar = div()
            .h(px(MONGO_QUERY_ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(if running {
                        tr("app.mongo.loading")
                    } else {
                        tr("app.mongo.aggregation.hint")
                    }),
            )
            .child(div().flex_1())
            .child(
                backup_text_button(tr("app.mongo.aggregation.run"), colors, cx.listener(
                    move |this, _, _, cx| {
                        this.run_mongo_aggregation(tab_id, cx);
                        cx.stop_propagation();
                    },
                )),
            );

        let body = match &state.aggregation {
            LoadState::Loading => center_loading_message(tr("app.mongo.loading"), colors),
            LoadState::Failed(error) => {
                mongo_error_block(tab_id, MongoRetry::Aggregation, error, colors, cx)
            }
            LoadState::Loaded(page) if !page.documents.is_empty() => div()
                .flex_1()
                .min_h(px(0.))
                .child(
                    div()
                        .overflow_y_scrollbar()
                        .flex()
                        .flex_col()
                        .children(page.documents.iter().map(|document| {
                            let mut component = JsonComponent::new(JsonEditorConfig::default());
                            component.load(&document.json);
                            div()
                                .p_2()
                                .border_b_1()
                                .border_color(colors.border_soft)
                                .child(component.render_preview(colors))
                        })),
                ),
            LoadState::Loaded(_) | LoadState::NotLoaded => {
                center_message(tr("app.mongo.aggregation.empty"), colors)
            }
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(editor)
            .child(bar)
            .child(body)
    }
}

/// 命名空间条：`库 / 集合`，视图额外标注只读。
fn mongo_namespace_strip(state: &MongoCollectionState, colors: UiColors) -> Div {
    div()
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(colors.border)
        .bg(colors.panel_alt)
        .text_size(px(12.))
        .text_color(colors.muted)
        .child(app_icon(AppIcon::Database, 13., colors.muted))
        .child(SharedString::from(
            state.object.database.clone().unwrap_or_default(),
        ))
        .child(SharedString::new_static("/"))
        .child(
            div()
                .text_color(colors.text)
                .child(SharedString::from(state.object.name.clone())),
        )
        .when(state.read_only, |this| {
            this.child(
                div()
                    .px_1p5()
                    .rounded(colors.radius)
                    .border_1()
                    .border_color(colors.border)
                    .text_color(colors.warning)
                    .child(tr("app.mongo.read_only")),
            )
        })
}

fn mongo_section_bar(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected = MONGO_SECTIONS
        .iter()
        .position(|(section, _)| *section == state.section)
        .unwrap_or(0);
    let view = cx.entity().downgrade();
    let tab_bar = MONGO_SECTIONS
        .iter()
        .map(|(_, label)| Tab::from(tr(label)))
        .fold(
            TabBar::new(("mongo-section-tabs", tab_id.0))
                .segmented()
                .small()
                .selected_index(selected)
                .on_click(move |index, _, cx| {
                    if let Some((section, _)) = MONGO_SECTIONS.get(*index) {
                        let section = *section;
                        let _ = view.update(cx, |this, cx| {
                            this.select_mongo_section(tab_id, section, cx);
                        });
                    }
                }),
            |bar, tab| bar.child(tab),
        );

    div()
        .h(px(MONGO_SECTION_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(colors.border)
        .child(tab_bar)
        .child(div().flex_1())
        .when_some(mongo_index_count(state), |this, count| {
            this.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(SharedString::from(count.to_string())),
            )
        })
}

fn mongo_index_count(state: &MongoCollectionState) -> Option<usize> {
    match &state.indexes {
        LoadState::Loaded(indexes) => Some(indexes.len()),
        _ => None,
    }
}

/// 查询面板：Filter 一行，Project / Sort / Limit 第二行（与原型一致，常显不折叠）。
fn mongo_query_panel(
    tab_id: TabId,
    inputs: Entity<MongoQueryInputs>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let inputs = inputs.read(cx);
    div()
        .px_2()
        .py_1p5()
        .border_b_1()
        .border_color(colors.border)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(mongo_query_input(
                    tr("app.mongo.query.filter"),
                    &inputs.filter,
                    true,
                    colors,
                ))
                .child(
                    backup_text_button(tr("app.mongo.query.find"), colors, cx.listener(
                        move |this, _, _, cx| {
                            this.run_mongo_query(tab_id, cx);
                            cx.stop_propagation();
                        },
                    )),
                )
                .child(
                    backup_text_button(tr("app.mongo.query.reset"), colors, cx.listener(
                        move |this, _, window, cx| {
                            this.reset_mongo_query(tab_id, window, cx);
                            cx.stop_propagation();
                        },
                    )),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(mongo_query_input(
                    tr("app.mongo.query.projection"),
                    &inputs.projection,
                    true,
                    colors,
                ))
                .child(mongo_query_input(
                    tr("app.mongo.query.sort"),
                    &inputs.sort,
                    true,
                    colors,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .child(tr("app.mongo.query.limit")),
                        )
                        .child(div().w(px(80.)).child(Input::new(&inputs.limit).small())),
                ),
        )
}

fn mongo_query_input(
    label: SharedString,
    input: &Entity<InputState>,
    flexible: bool,
    colors: UiColors,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .when(flexible, |this| this.flex_1())
        .child(
            div()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(label),
        )
        .child(div().flex_1().child(Input::new(input).small()))
}

fn mongo_results_toolbar(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected = MONGO_DISPLAY_MODES
        .iter()
        .position(|(mode, _)| *mode == state.display)
        .unwrap_or(0);
    let view = cx.entity().downgrade();
    let bar = MONGO_DISPLAY_MODES
        .iter()
        .map(|(_, label)| Tab::from(tr(label)))
        .fold(
            TabBar::new(("mongo-display-tabs", tab_id.0))
                .segmented()
                .small()
                .selected_index(selected)
                .on_click(move |index, _, cx| {
                    if let Some((mode, _)) = MONGO_DISPLAY_MODES.get(*index) {
                        let mode = *mode;
                        let _ = view.update(cx, |this, cx| {
                            this.select_mongo_display(tab_id, mode, cx);
                        });
                    }
                }),
            |bar, tab| bar.child(tab),
        );
    let summary: SharedString = state
        .page
        .as_ref()
        .map(|page| match page.total {
            Some(total) => SharedString::from(
                tr("app.mongo.result.total").replace("%{count}", &total.to_string()),
            ),
            None => SharedString::from(
                tr("app.mongo.result.returned").replace("%{count}", &page.returned.to_string()),
            ),
        })
        .unwrap_or_else(|| tr("app.mongo.result.none"));

    div()
        .h(px(MONGO_TOOLBAR_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(colors.border)
        .child(bar)
        .child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(summary),
        )
        .child(div().flex_1())
        .child(
            query_toolbar_icon_button(
                tr("app.common.refresh"),
                AppIcon::Refresh,
                !state.loading,
                state.loading,
                colors.text,
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.run_mongo_query(tab_id, cx);
                    cx.stop_propagation();
                }),
            ),
        )
}

fn mongo_results(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let container = div().flex_1().min_w(px(0.)).min_h(px(0.));
    if let Some(error) = &state.error {
        return container.child(mongo_error_block(
            tab_id,
            MongoRetry::Documents,
            error,
            colors,
            cx,
        ));
    }
    let Some(page) = &state.page else {
        return container.child(if state.loading {
            center_loading_message(tr("app.mongo.loading"), colors)
        } else {
            center_message(tr("app.mongo.empty"), colors)
        });
    };
    if page.documents.is_empty() {
        return container.child(center_message(tr("app.mongo.empty"), colors));
    }
    match state.display {
        MongoDisplayMode::Documents => container.child(
            div()
                .overflow_y_scrollbar()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .children(page.documents.iter().enumerate().map(|(index, document)| {
                    mongo_document_card(tab_id, index, document, state, colors, cx)
                })),
        ),
        MongoDisplayMode::Table => {
            container.child(div().overflow_y_scrollbar().child(mongo_document_table(page, colors)))
        }
        MongoDisplayMode::Json => {
            let mut component = JsonComponent::new(JsonEditorConfig::default());
            component.load(&mongo_page_json(page));
            container.child(
                div()
                    .p_2()
                    .overflow_y_scrollbar()
                    .child(component.render_preview(colors)),
            )
        }
    }
}

fn mongo_error_block(
    tab_id: TabId,
    retry: MongoRetry,
    error: &fluxdb_core::UserFacingError,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    div()
        .flex_1()
        .p_4()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(13.))
                .text_color(colors.danger)
                .child(SharedString::from(error.title.clone())),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(SharedString::from(error.message.clone())),
        )
        .when_some(error.detail.clone(), |this, detail| {
            this.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(SharedString::from(detail)),
            )
        })
        .child(backup_text_button(
            tr("app.common.retry"),
            colors,
            cx.listener(move |this, _, _, cx| {
                this.retry_mongo_load(tab_id, retry, cx);
                cx.stop_propagation();
            }),
        ))
}

/// 一条文档卡片：序号 + `_id`、复制、打开详情；正文是折叠好的只读 JSON。
fn mongo_document_card(
    tab_id: TabId,
    index: usize,
    document: &fluxdb_core::MongoDocument,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let selected = state.selected_document == Some(index);
    let mut component = JsonComponent::new(JsonEditorConfig::default());
    component.load(&document.json);
    let json = document.json.clone();
    let view = cx.entity().downgrade();
    div()
        .rounded(colors.radius)
        .border_1()
        .border_color(if selected { colors.info } else { colors.border })
        .bg(colors.panel_alt)
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(colors.border_soft)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(SharedString::from(format!("#{}", index + 1))),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(SharedString::from(format!("_id: {}", document.id))),
                )
                .child(
                    query_toolbar_icon_button(
                        tr("app.mongo.copy_document"),
                        AppIcon::Copy,
                        true,
                        false,
                        colors.text,
                        colors,
                    )
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(json.clone()));
                        cx.stop_propagation();
                    }),
                )
                .child(
                    query_toolbar_icon_button(
                        tr("app.mongo.document_detail"),
                        AppIcon::PanelRight,
                        true,
                        false,
                        if selected { colors.info } else { colors.text },
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_mongo_document(
                                tab_id,
                                if selected { None } else { Some(index) },
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    ),
                ),
        )
        .child(
            div()
                .p_2()
                .max_h(px(260.))
                .overflow_hidden()
                .child(component.render_preview(colors)),
        )
        // 点卡片正文同样打开详情，与原型一致。
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.select_mongo_document(tab_id, Some(index), cx);
            });
        })
}

/// 表格视图：`_id` 一列 + 各顶层字段一列；缺字段的文档留空。
fn mongo_document_table(page: &fluxdb_core::MongoDocumentsPage, colors: UiColors) -> Div {
    let rows = page
        .documents
        .iter()
        .map(|document| mongo_document_fields(&document.json))
        .collect::<Vec<_>>();
    let columns = rows
        .iter()
        .flat_map(|(_, fields)| fields.iter().map(|(key, _)| key.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .border_b_1()
                .border_color(colors.border)
                .bg(colors.panel_alt)
                .child(mongo_table_cell(tr("app.mongo.table.id"), true, colors))
                .children(columns.iter().map(|column| {
                    mongo_table_cell(SharedString::from(column.clone()), true, colors)
                })),
        )
        .children(rows.into_iter().map(|(id, fields)| {
            div()
                .flex()
                .border_b_1()
                .border_color(colors.border_soft)
                .child(mongo_table_cell(SharedString::from(id), false, colors))
                .children(columns.iter().map(|column| {
                    let value = fields
                        .iter()
                        .find(|(key, _)| key == column)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(|| tr("app.mongo.table.missing").to_string());
                    mongo_table_cell(SharedString::from(value), false, colors)
                }))
        }))
}

fn mongo_table_cell(text: SharedString, header: bool, colors: UiColors) -> Div {
    let text: String = text.chars().take(MONGO_VALUE_MAX_CHARS).collect();
    div()
        .w(px(180.))
        .flex_none()
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .overflow_hidden()
        .text_size(px(11.))
        .text_color(if header { colors.muted } else { colors.text })
        .child(SharedString::from(text))
}

/// 文档 JSON 的 `_id` 与顶层字段（值取紧凑文本）：表格列头 / 单元格取值。
fn mongo_document_fields(json: &str) -> (String, Vec<(String, String)>) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json) else {
        return (String::new(), Vec::new());
    };
    let id = map
        .get("_id")
        .map(|value| match value {
            serde_json::Value::Object(object) => object
                .get("$oid")
                .and_then(|oid| oid.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
            other => other
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| other.to_string()),
        })
        .unwrap_or_default();
    let fields = map
        .into_iter()
        .filter(|(key, _)| key != "_id")
        .map(|(key, value)| {
            let text = match &value {
                serde_json::Value::Object(object) => format!("{{ {} }}", object.len()),
                serde_json::Value::Array(items) => format!("[ {} ]", items.len()),
                other => other
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| other.to_string()),
            };
            (key, text)
        })
        .collect();
    (id, fields)
}

fn mongo_page_json(page: &fluxdb_core::MongoDocumentsPage) -> String {
    let documents = page
        .documents
        .iter()
        .map(|document| {
            serde_json::from_str::<serde_json::Value>(&document.json)
                .unwrap_or(serde_json::Value::Null)
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&documents).unwrap_or_else(|_| "[]".to_string())
}

/// 右侧详情抽屉：整篇文档的 Extended JSON（只读）。
fn mongo_inspector(
    tab_id: TabId,
    index: usize,
    document: &fluxdb_core::MongoDocument,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let mut component = JsonComponent::new(JsonEditorConfig::default());
    component.load(&document.json);
    div()
        .w(px(MONGO_INSPECTOR_WIDTH))
        .flex_none()
        .h_full()
        .border_l_1()
        .border_color(colors.border)
        .bg(colors.panel_alt)
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(MONGO_TOOLBAR_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(colors.border)
                .child(app_icon(AppIcon::FileSearch, 14., colors.muted))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.))
                        .text_color(colors.text)
                        .child(tr("app.mongo.document_detail")),
                )
                .child(
                    query_toolbar_icon_button(
                        tr("app.common.close"),
                        AppIcon::Close,
                        true,
                        false,
                        colors.text,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_mongo_document(tab_id, None, cx);
                            cx.stop_propagation();
                        }),
                    ),
                ),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(colors.border_soft)
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(tr("app.mongo.extended_json"))
                .child(div().flex_1())
                .child(tr("app.mongo.bson_preserved")),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scrollbar()
                .p_2()
                .child(component.render_preview(colors)),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(colors.border)
                .text_size(px(10.))
                .text_color(colors.muted)
                .child(tr("app.mongo.not_ai_context")),
        )
        .child(
            div()
                .text_size(px(0.))
                .child(SharedString::from(format!("mongo-inspector-{}", index))),
        )
}

fn mongo_pager(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let offset = state.pagination.offset;
    let limit = state.pagination.limit.max(1);
    let page_no = offset / limit + 1;
    let has_more = state.page.as_ref().is_some_and(|page| page.has_more);
    let range = state
        .page
        .as_ref()
        .map(|page| {
            if page.documents.is_empty() {
                "0".to_string()
            } else {
                format!("{}–{}", offset + 1, offset + page.returned)
            }
        })
        .unwrap_or_else(|| "0".to_string());

    div()
        .h(px(MONGO_PAGER_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_t_1()
        .border_color(colors.border)
        .text_size(px(11.))
        .text_color(colors.muted)
        .child(SharedString::from(format!(
            "{} {range}",
            tr("app.mongo.pager.range")
        )))
        .child(div().flex_1())
        .child(
            query_toolbar_icon_button(
                tr("app.mongo.pager.previous"),
                AppIcon::ChevronLeft,
                offset > 0 && !state.loading,
                false,
                colors.text,
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.set_mongo_offset(tab_id, offset.saturating_sub(limit), cx);
                    cx.stop_propagation();
                }),
            ),
        )
        .child(SharedString::from(format!(
            "{} / {}",
            page_no,
            if has_more {
                "…".to_string()
            } else {
                page_no.to_string()
            }
        )))
        .child(
            query_toolbar_icon_button(
                tr("app.mongo.pager.next"),
                AppIcon::ChevronRight,
                has_more && !state.loading,
                false,
                colors.text,
                colors,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.set_mongo_offset(tab_id, offset + limit, cx);
                    cx.stop_propagation();
                }),
            ),
        )
}

fn mongo_indexes_section(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let container = div().flex_1().min_h(px(0.));
    match &state.indexes {
        LoadState::Loading | LoadState::NotLoaded => {
            container.child(center_loading_message(tr("app.mongo.loading"), colors))
        }
        LoadState::Failed(error) => {
            container.child(mongo_error_block(tab_id, MongoRetry::Indexes, error, colors, cx))
        }
        LoadState::Loaded(indexes) if indexes.is_empty() => {
            container.child(center_message(tr("app.mongo.indexes.empty"), colors))
        }
        LoadState::Loaded(indexes) => container.child(
            div()
                .overflow_y_scrollbar()
                .flex()
                .flex_col()
                .p_2()
                .gap_1()
                .children(indexes.iter().map(|index| {
                    div()
                        .rounded(colors.radius)
                        .border_1()
                        .border_color(colors.border)
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .child(SharedString::from(index.name.clone()))
                                .when(index.unique, |this| {
                                    this.child(mongo_index_badge(
                                        tr("app.mongo.indexes.unique"),
                                        colors.info,
                                        colors,
                                    ))
                                })
                                .when(index.sparse, |this| {
                                    this.child(mongo_index_badge(
                                        tr("app.mongo.indexes.sparse"),
                                        colors.muted,
                                        colors,
                                    ))
                                })
                                .when(!index.ready, |this| {
                                    this.child(mongo_index_badge(
                                        tr("app.mongo.indexes.building"),
                                        colors.warning,
                                        colors,
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(SharedString::from(index.keys_json.clone())),
                        )
                        .when_some(index.partial_filter.clone(), |this, filter| {
                            this.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(SharedString::from(filter)),
                            )
                        })
                })),
        ),
    }
}

fn mongo_index_badge(label: SharedString, color: gpui::Rgba, colors: UiColors) -> Div {
    div()
        .px_1()
        .rounded(colors.radius)
        .bg(colors.panel_alt)
        .text_size(px(10.))
        .text_color(color)
        .child(label)
}

fn mongo_info_section(
    tab_id: TabId,
    state: &MongoCollectionState,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let container = div().flex_1().min_h(px(0.));
    match &state.info {
        LoadState::Loading | LoadState::NotLoaded => {
            container.child(center_loading_message(tr("app.mongo.loading"), colors))
        }
        LoadState::Failed(error) => {
            container.child(mongo_error_block(tab_id, MongoRetry::Info, error, colors, cx))
        }
        LoadState::Loaded(info) => {
            let rows = [
                (tr("app.mongo.info.name"), info.name.clone()),
                (
                    tr("app.mongo.info.type"),
                    match info.kind {
                        fluxdb_core::MongoCollectionKind::Collection => {
                            tr("app.mongo.info.kind.collection").to_string()
                        }
                        fluxdb_core::MongoCollectionKind::View => {
                            tr("app.mongo.info.kind.view").to_string()
                        }
                        fluxdb_core::MongoCollectionKind::Timeseries => {
                            tr("app.mongo.info.kind.timeseries").to_string()
                        }
                    },
                ),
                (
                    tr("app.mongo.info.database"),
                    state.object.database.clone().unwrap_or_default(),
                ),
                (
                    tr("app.mongo.info.documents"),
                    mongo_optional_number(info.document_count),
                ),
                (
                    tr("app.mongo.info.avg_size"),
                    mongo_optional_bytes(info.avg_document_size),
                ),
                (
                    tr("app.mongo.info.data_size"),
                    mongo_optional_bytes(info.data_size),
                ),
                (
                    tr("app.mongo.info.storage_size"),
                    mongo_optional_bytes(info.storage_size),
                ),
                (
                    tr("app.mongo.info.indexes"),
                    mongo_optional_number(info.index_count),
                ),
            ];
            container.child(
                div()
                    .overflow_y_scrollbar()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rows.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .py_0p5()
                            .text_size(px(12.))
                            .child(div().w(px(140.)).text_color(colors.muted).child(label))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(colors.text)
                                    .child(SharedString::from(value)),
                            )
                    }))
                    .when_some(info.view_on.clone(), |this, view_on| {
                        this.child(
                            div()
                                .mt_2()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .child(SharedString::from(format!(
                                    "{}: {view_on}",
                                    tr("app.mongo.info.view_on")
                                ))),
                        )
                    })
                    .when_some(info.view_pipeline.clone(), |this, pipeline| {
                        let mut component = JsonComponent::new(JsonEditorConfig::default());
                        component.load(&pipeline);
                        this.child(div().mt_2().child(component.render_preview(colors)))
                    }),
            )
        }
    }
}

fn mongo_optional_number(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| tr("app.mongo.info.unknown").to_string())
}

fn mongo_optional_bytes(value: Option<u64>) -> String {
    match value {
        Some(bytes) => mongo_human_bytes(bytes),
        None => tr("app.mongo.info.unknown").to_string(),
    }
}

fn mongo_human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
