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
            .debug_selector({
                let namespace = format!(
                    "{}/{}",
                    state.object.database.clone().unwrap_or_default(),
                    state.object.name
                );
                move || format!("mongo-collection-page:{namespace}")
            })
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
        let collapsed_documents = self
            .mongo_collapsed_documents
            .get(&tab_id)
            .cloned()
            .unwrap_or_default();
        let expanded_fields = self
            .mongo_expanded_fields
            .get(&tab_id)
            .cloned()
            .unwrap_or_default();
        let selected = state.selected_document.and_then(|index| {
            state
                .page
                .as_ref()
                .and_then(|page| page.documents.get(index))
                .map(|document| (index, document))
        });
        let json_fold_toggles = self
            .mongo_json_fold_toggles
            .get(&tab_id)
            .cloned()
            .unwrap_or_default();
        let documents = MongoDocumentView {
            tab_id,
            collapsed_documents: &collapsed_documents,
            expanded_fields: &expanded_fields,
            json_fold_toggles: &json_fold_toggles,
            colors,
        };
        let results = mongo_results(state, &documents, colors, cx);
        // 抽屉只有两种形态：编辑中的文档给编辑器，其余给只读详情。
        let drawer = match (&self.mongo_document_editor, selected) {
            (Some(editor), _) if editor.tab_id == tab_id => {
                Some(mongo_document_editor_drawer(editor, colors, cx))
            }
            (_, Some((index, document))) => {
                Some(mongo_inspector(tab_id, index, document, colors, cx))
            }
            _ => None,
        };
        let body = match drawer {
            Some(drawer) => div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .child(results)
                .child(drawer),
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
            .child(backup_text_button(
                tr("app.mongo.aggregation.run"),
                colors,
                cx.listener(move |this, _, _, cx| {
                    this.run_mongo_aggregation(tab_id, cx);
                    cx.stop_propagation();
                }),
            ));

        let body = match &state.aggregation {
            LoadState::Loading => center_loading_message(tr("app.mongo.loading"), colors),
            LoadState::Failed(error) => {
                mongo_error_block(tab_id, MongoRetry::Aggregation, error, colors, cx)
            }
            LoadState::Loaded(page) if !page.documents.is_empty() => div()
                .flex_1()
                .min_h(px(0.))
                .child(div().overflow_y_scrollbar().flex().flex_col().children(
                    page.documents.iter().map(|document| {
                        let mut component = JsonComponent::new(JsonEditorConfig::default());
                        component.load(&document.json);
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(colors.border_soft)
                            .child(component.render_preview(colors))
                    }),
                )),
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
                .child(backup_text_button(
                    tr("app.mongo.query.find"),
                    colors,
                    cx.listener(move |this, _, _, cx| {
                        this.run_mongo_query(tab_id, cx);
                        cx.stop_propagation();
                    }),
                ))
                .child(backup_text_button(
                    tr("app.mongo.query.reset"),
                    colors,
                    cx.listener(move |this, _, window, cx| {
                        this.reset_mongo_query(tab_id, window, cx);
                        cx.stop_propagation();
                    }),
                )),
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
        .map(|(mode, label)| {
            // 展示方式页签需要稳定选择器，无头用例才能点击 JSON 视图。
            let mode = *mode;
            Tab::from(tr(label)).debug_selector(move || format!("mongo-display-tab:{mode:?}"))
        })
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
        .when(!state.read_only, |bar| {
            bar.child(mongo_results_insert_button(tab_id, colors, cx))
                .child(mongo_results_import_button(tab_id, colors, cx))
        })
        .child(mongo_results_refresh_button(
            tab_id,
            state.loading,
            colors,
            cx,
        ))
}

/// 结果条右侧的「新增文档」按钮：打开与编辑 / 复制同一个抽屉，初值只有 `_id`。
///
/// 只读对象（视图）不渲染：视图不能写入，摆一个点了就报错的按钮只会误导用户。
fn mongo_results_insert_button(
    tab_id: TabId,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    mongo_card_icon_button(
        format!("mongo-results-insert-{}", tab_id.0),
        "mongo-results-insert".to_string(),
        AppIcon::Plus,
        tr("app.mongo.insert_document"),
        colors.text,
        true,
        false,
        colors,
    )
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |this, _, window, cx| {
            this.open_mongo_document_insert(tab_id, window, cx);
            cx.stop_propagation();
        }),
    )
}

/// 结果条右侧的「导入文档」按钮：从 JSON / CSV 文件批量写入当前集合。
///
/// 与新增按钮同样只读对象不渲染；导入过程中弹窗自带中止入口。
fn mongo_results_import_button(
    tab_id: TabId,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    mongo_card_icon_button(
        format!("mongo-results-import-{}", tab_id.0),
        "mongo-results-import".to_string(),
        AppIcon::FolderInput,
        tr("app.mongo.import.action"),
        colors.text,
        true,
        false,
        colors,
    )
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |this, _, _, cx| {
            this.open_mongo_import_dialog(tab_id, cx);
            cx.stop_propagation();
        }),
    )
}

/// 结果条右侧的刷新按钮。
///
/// 结果条只有 38px 高、旁边是 11px 统计文字：通用 30px 描边工具栏按钮在这行里显得过大，
/// 改用卡片头 / 抽屉头同款的 24px 无边框图标按钮。查询在途让刷新图标自身旋转：
/// 反馈就落在用户刚点的那颗按钮上，不额外引入第二种图形。
fn mongo_results_refresh_button(
    tab_id: TabId,
    loading: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Stateful<Div> {
    mongo_card_icon_button(
        format!("mongo-results-refresh-{}", tab_id.0),
        "mongo-results-refresh".to_string(),
        AppIcon::Refresh,
        tr("app.common.refresh"),
        colors.text,
        !loading,
        // 刷新在途：图标自转，而不是换成另一个图形。
        loading,
        colors,
    )
    // 在途时按钮降为不可用样式，但点击仍要吞掉，避免连点重复发查询。
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |this, _, _, cx| {
            if !loading {
                this.run_mongo_query(tab_id, cx);
            }
            cx.stop_propagation();
        }),
    )
}

fn mongo_results(
    state: &MongoCollectionState,
    documents: &MongoDocumentView<'_>,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> Div {
    let tab_id = documents.tab_id;
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
        // 结果区容器默认不是 flex，直接塞居中提示会只按文本高度布局而贴顶；
        // 空态分支显式开 flex，让提示占满结果区并垂直居中。
        let container = container.flex();
        return container.child(if state.loading {
            center_loading_message(tr("app.mongo.loading"), colors)
        } else {
            center_message(tr("app.mongo.empty"), colors)
        });
    };
    if page.documents.is_empty() {
        return container
            .flex()
            .child(center_message(tr("app.mongo.empty"), colors));
    }
    // 文档与 JSON 共用卡片列表（交互稿同一套卡头），只有正文不同；表格单独成表。
    match state.display {
        MongoDisplayMode::Documents | MongoDisplayMode::Json => container.child(
            div()
                .overflow_y_scrollbar()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .children(page.documents.iter().enumerate().map(|(index, document)| {
                    mongo_document_card(documents, index, document, state, cx)
                })),
        ),
        // 表格的数据区横向滚动（操作列固定在右侧不动），纵向滚动由表格自身承担。
        MongoDisplayMode::Table => {
            container.child(mongo_document_table(documents, page, state, colors, cx))
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
                    // 与编辑 / 复制抽屉同一档：38px 头部里放 30px 描边按钮会顶满整条，显得笨重。
                    mongo_card_icon_button(
                        format!("mongo-inspector-close-{}", tab_id.0),
                        "mongo-inspector-close".to_string(),
                        AppIcon::Close,
                        tr("app.common.close"),
                        colors.text,
                        true,
                        false,
                        colors,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.close_mongo_document_drawer(tab_id, cx);
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
            // 提示靠 flex_1 撑满：容器不开 flex 时只按文本高度布局而贴顶。
            container
                .flex()
                .child(center_loading_message(tr("app.mongo.loading"), colors))
        }
        LoadState::Failed(error) => container.child(mongo_error_block(
            tab_id,
            MongoRetry::Indexes,
            error,
            colors,
            cx,
        )),
        LoadState::Loaded(indexes) if indexes.is_empty() => container
            .flex()
            .child(center_message(tr("app.mongo.indexes.empty"), colors)),
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
            // 同索引页：提示需要 flex 容器才能垂直居中。
            container
                .flex()
                .child(center_loading_message(tr("app.mongo.loading"), colors))
        }
        LoadState::Failed(error) => container.child(mongo_error_block(
            tab_id,
            MongoRetry::Info,
            error,
            colors,
            cx,
        )),
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
