impl NavicatMain {
    fn ai_context_source_tab(&self) -> Option<&TabState> {
        let state = self.controller.state();
        let active = state.active_tab()?;
        if !matches!(active.kind, TabKind::AiChat { .. }) {
            return Some(active);
        }
        self.ai_chat
            .context_tab
            .and_then(|id| state.tabs.iter().find(|t| t.id == id))
    }

    fn capture_ai_active_context(&self, cx: &App) -> fluxdb_app::UiContextSnapshot {
        self.capture_ai_page_context(cx, false).snapshot
    }

    /// render 只展示轻量描述，发送才固定明确授权的已加载数据快照。
    fn capture_ai_page_context(&self, cx: &App, sending: bool) -> fluxdb_app::CurrentPageContext {
        use fluxdb_app::{CurrentPageContext, PageContextKind, TargetRef};
        let mut page = CurrentPageContext {
            kind: PageContextKind::Other,
            label: "无当前业务页面".into(),
            snapshot: fluxdb_app::capture(
                &fluxdb_app::CapturedTab::NoTab {
                    connection_id: None,
                    database: None,
                    schema: None,
                    last_object: None,
                },
                fluxdb_app::ContextLimits::default(),
            ),
            details: serde_json::json!({"available":false}),
            editor_grant: None,
            data_grant: None,
        };
        let Some(tab) = self.ai_context_source_tab() else {
            return page;
        };
        page.label = tab.title.clone();
        page.snapshot.tab_id = Some(tab.id.0);
        let share_data =
            self.ai_chat.page_data_enabled && self.ai_chat.page_data_tab == Some(tab.id);
        if let Some(workspace) = tab.workspace() {
            page.snapshot.target.connection_id = Some(workspace.connection_id.0 as i64);
            page.snapshot.target.database = Some(workspace.database);
        }
        match &tab.kind {
            TabKind::QueryEditor(query) => {
                let target = TargetRef {
                    connection_id: Some(query.connection_id.0 as i64),
                    database: query.database.clone(),
                    schema: query.schema.clone(),
                    ..Default::default()
                };
                let text = self
                    .query_editors
                    .get(&tab.id)
                    .map(|e| e.read(cx).buffer.snapshot())
                    .unwrap_or_else(|| {
                        fluxdb_editor_core::EditorBuffer::new_from(&query.text).snapshot()
                    });
                let selection = self
                    .query_editors
                    .get(&tab.id)
                    .map(|e| e.read(cx).selection_range())
                    .unwrap_or_else(|| fluxdb_editor_core::Range::new(0, 0));
                page.kind = PageContextKind::SqlEditor;
                page.snapshot =
                    fluxdb_app::capture_editor_snapshot(tab.id.0, target, &text, selection);
                let output = self
                    .query_output_tabs
                    .get(&tab.id)
                    .copied()
                    .filter(|output| query_output_tab_exists(*output, query))
                    .unwrap_or_else(|| {
                        if query_result_entry_count(query) > 0 {
                            QueryOutputTab::Result(0)
                        } else {
                            QueryOutputTab::Summary
                        }
                    });
                let result_index = output
                    .result_index()
                    .and_then(|i| query_result_page_index(query, i));
                let cursor = selection.end.min(text.len());
                let point = text.offset_to_point(cursor);
                let cursor_start = text.clamp_to_char_boundary(cursor.saturating_sub(512));
                let cursor_end =
                    text.clamp_to_char_boundary(cursor.saturating_add(1536).min(text.len()));
                page.details = serde_json::json!({
                    "total_bytes":text.len(),"total_lines":text.line_count(),"running":query.running,
                    "error":query.error.as_ref().map(|e|e.message.chars().take(1024).collect::<String>()),
                    "selection":{"start":selection.start,"end":selection.end},
                    "read_scope":"完整发送时 SQL 快照；选区优先展示但并不缩小工具范围",
                    "active_output":format!("{output:?}"),"active_result_index":result_index,
                    "cursor":{"offset":cursor,"line_number":point.row+1,"byte_column":point.column,
                        "preview":text.text_in_range(fluxdb_editor_core::Range::new(cursor_start,cursor_end)),
                        "scope":"光标附近有界片段，不代表完整语句"},
                    "results":result_index.and_then(|i|query.results.get(i)).into_iter().map(|p|serde_json::json!({
                        "loaded_rows":p.rows.len(),"offset":p.offset,"has_more":p.has_more,
                        "columns":fluxdb_app::page_columns_json(&p.columns)
                    })).collect::<Vec<_>>(),
                    "executions":query.summaries.iter().rev().take(3).map(|s|serde_json::json!({
                        "success":s.success,"message":s.message.chars().take(1024).collect::<String>(),
                        "returned_rows":s.returned_rows,"affected_rows":s.affected_rows,"elapsed_ms":s.elapsed_ms
                    })).collect::<Vec<_>>(),"data_shared":share_data,
                });
                if result_index.is_some() {
                    if let Some(table) = self.data_table_states.get(&tab.id) {
                        let delegate = table.read(cx).delegate();
                        let row = delegate
                            .selected_row
                            .or_else(|| delegate.selected_cell.map(|c| c.0))
                            .and_then(|r| delegate.source_row_indexes.get(r).copied());
                        page.details["selected_source_row"] = serde_json::json!(row);
                        page.details["selected_column"] =
                            serde_json::json!(delegate.selected_cell.map(|c| c.1));
                    }
                }
                if !text.is_empty() {
                    page.editor_grant = Some(fluxdb_app::EditorGrant {
                        tab_ref: format!("tab-{}", tab.id.0),
                        snapshot: text,
                    });
                }
                if sending && share_data && !query.running {
                    let data = result_index.and_then(|selected| {
                        query
                            .result_editors
                            .get(&selected)
                            .and_then(|d| d.page.as_ref())
                            .or_else(|| query.results.get(selected))
                    });
                    if let Some(data) = data {
                        page.data_grant = Some(fluxdb_app::PageDataGrant {
                            reference_id: page.reference_id(),
                            target: page.snapshot.target.clone(),
                            page: std::sync::Arc::new(data.clone()),
                        });
                    }
                }
            }
            TabKind::DataEditor(data) => {
                let redis = matches!(data.object.kind, ObjectKind::RedisDb | ObjectKind::RedisKey);
                page.kind = if redis {
                    PageContextKind::RedisBrowser
                } else if data.table_info.open {
                    PageContextKind::TableStructure
                } else {
                    PageContextKind::TableData
                };
                page.snapshot.target = TargetRef {
                    connection_id: Some(data.object.connection_id.0 as i64),
                    database: data.object.database.clone(),
                    schema: data.object.schema.clone(),
                    object: Some(data.object.name.clone()),
                    redis_db: redis.then(|| {
                        data.object
                            .database
                            .as_deref()
                            .unwrap_or("0")
                            .parse()
                            .unwrap_or(0)
                    }),
                };
                let mut selected = self.data_table_states.get(&tab.id).and_then(|table| {
                    let delegate = table.read(cx).delegate();
                    delegate
                        .selected_row
                        .or_else(|| delegate.selected_cell.map(|c| c.0))
                        .and_then(|r| delegate.source_row_indexes.get(r).copied())
                });
                if redis && self.redis_key_list_mode(tab.id).is_folder() {
                    selected = self
                        .redis_key_list_selected_leaf(tab.id)
                        .as_deref()
                        .and_then(|key| {
                            data.page
                                .as_ref()
                                .and_then(|p| redis_key_page_row_for_key(p, key))
                        });
                }
                page.details = serde_json::json!({
                    "object":data.object.name,
                    "columns":data.page.as_ref().map(|p|fluxdb_app::page_columns_json(&p.columns)),
                    "columns_truncated":data.page.as_ref().is_some_and(|p|p.columns.len()>32),
                    "loading":data.loading,"submitting":data.submitting,"error":data.error.as_ref().map(|e|e.message.chars().take(1024).collect::<String>()),
                    "selected_row":selected,"filter":self.data_filter_texts.get(&tab.id).map(|s|s.chars().take(1024).collect::<String>()),
                    "filter_rules":self.data_filter_rules.get(&tab.id).map(|rules|serde_json::json!({
                        "truncated":rules.len()>16,
                        "items":rules.iter().take(16).map(|r|serde_json::json!({
                            "enabled":r.enabled,"field":r.field,"operator":format!("{:?}",r.operator),
                            "values":r.values.iter().take(16).map(|v|v.chars().take(128).collect::<String>()).collect::<Vec<_>>(),
                            "values_truncated":r.values.len()>16,"grouped":r.grouped
                        })).collect::<Vec<_>>()
                    })),
                    "sort":self.data_sort_texts.get(&tab.id).map(|s|s.chars().take(1024).collect::<String>()),
                    "sort_rules":self.data_sort_rules.get(&tab.id).map(|rules|serde_json::json!({
                        "truncated":rules.len()>16,
                        "items":rules.iter().take(16).map(|r|serde_json::json!({
                            "enabled":r.enabled,"field":r.field,"ascending":r.ascending
                        })).collect::<Vec<_>>()
                    })),
                    "has_pending_changes":data.changes.is_some(),
                    "loaded_rows":data.page.as_ref().map(|p|p.rows.len()),"offset":data.page.as_ref().map(|p|p.offset),
                    "has_more":data.page.as_ref().map(|p|p.has_more),"data_shared":share_data,
                    "table_structure_tab":data.table_info.open.then(||format!("{:?}",data.table_info.active_tab)),
                });
                if data.table_info.open {
                    if let LoadState::Loaded(indexes) = &data.table_info.indexes {
                        page.details["indexes"] = serde_json::json!({
                            "total":indexes.len(),"truncated":indexes.len()>32,
                            "items":indexes.iter().take(32).map(|i|serde_json::json!({
                                "name":i.name,"columns":i.columns,"unique":i.is_unique,"primary":i.is_primary
                            })).collect::<Vec<_>>()
                        });
                    }
                    if let LoadState::Loaded(keys) = &data.table_info.foreign_keys {
                        page.details["foreign_keys"] = serde_json::json!({
                            "total":keys.len(),"truncated":keys.len()>32,
                            "items":keys.iter().take(32).map(|k|serde_json::json!({
                                "name":k.name,"columns":k.columns,"ref_schema":k.ref_schema,
                                "ref_table":k.ref_table,"ref_columns":k.ref_columns
                            })).collect::<Vec<_>>()
                        });
                    }
                    if let LoadState::Loaded(ddl) = &data.table_info.ddl {
                        page.snapshot.excerpt = Some(fluxdb_app::bound_excerpt(
                            ddl,
                            fluxdb_app::ContextLimits::default(),
                        ));
                        page.details["ddl_read_scope"] = serde_json::json!(
                            "已加载的 DDL 预览；详情通过授权的 db.describe_object 查询，不重新授予权限"
                        );
                    }
                }
                if redis {
                    if let Some((loaded, row)) = data
                        .page
                        .as_ref()
                        .and_then(|p| selected.and_then(|r| p.rows.get(r).map(|row| (p, row))))
                    {
                        if let Some(key) = redis_row_text(loaded, row, "键") {
                            page.kind = PageContextKind::RedisKey;
                            page.snapshot.target.object = Some(key.clone());
                            page.details["selected_key"] = serde_json::json!({
                            "key":key,"type":redis_row_text(loaded,row,"类型"),
                            "ttl":redis_row_text(loaded,row,"TTL"),"size":redis_row_text(loaded,row,"大小")});
                            if share_data {
                                let value = redis_row_text(loaded, row, "值").unwrap_or_default();
                                let preview = fluxdb_app::bound_excerpt(
                                    &value,
                                    fluxdb_app::ContextLimits::default(),
                                );
                                page.details["value"] = serde_json::json!({"preview":preview.text,"truncated":preview.truncated,
                                "scope":"已加载页面值预览；不是服务器上的完整值"});
                            }
                        }
                    }
                }
                if sending && share_data && !data.loading && !data.submitting {
                    if let Some(data_page) = &data.page {
                        let mut shared = data_page.clone();
                        if redis {
                            shared.rows = selected
                                .and_then(|r| shared.rows.get(r).cloned())
                                .into_iter()
                                .collect();
                        }
                        page.data_grant = Some(fluxdb_app::PageDataGrant {
                            reference_id: page.reference_id(),
                            target: page.snapshot.target.clone(),
                            page: std::sync::Arc::new(shared),
                        });
                    }
                }
            }
            TabKind::RedisWorkbench(workbench) => {
                page.kind = PageContextKind::RedisWorkbench;
                page.snapshot.target = TargetRef {
                    connection_id: Some(workbench.connection_id.0 as i64),
                    database: Some(workbench.database.to_string()),
                    redis_db: Some(workbench.database),
                    ..Default::default()
                };
                page.snapshot.excerpt = Some(fluxdb_app::bound_excerpt(
                    &workbench.text,
                    fluxdb_app::ContextLimits::default(),
                ));
                page.details = serde_json::json!({"running":workbench.running,
                    "error":workbench.error.as_ref().map(|e|&e.message),"execution_count":workbench.executions.len(),
                    "note":"只发送命令草稿与状态，不自动发送历史回复"});
            }
            _ => {
                page.details = serde_json::json!({"available":false,"note":"当前页面正文未接入 AI，仅提供目标定位；不发送设置表单与凭据"});
                page.details["page_type"] = serde_json::json!(match &tab.kind {
                    TabKind::RedisCli(_) => "redis_cli",
                    TabKind::RedisPubSub(_) => "redis_pubsub",
                    TabKind::CreateTable(_) => "table_design",
                    TabKind::ObjectList(_) => "object_list",
                    TabKind::ErDiagram(_) => "er_diagram",
                    TabKind::UserAdmin(_) => "user_admin",
                    TabKind::BackupList(_) => "backup_list",
                    TabKind::Settings(_) => "settings",
                    _ => "unsupported",
                });
                page.snapshot.notes = vec!["当前页面正文未接入 AI".into()];
            }
        }
        page
    }
}

fn ai_current_page_chip(
    this: &mut NavicatMain,
    ready: bool,
    colors: UiColors,
    cx: &mut Context<NavicatMain>,
) -> gpui::AnyElement {
    let page = this.capture_ai_page_context(cx, false);
    let label = if !this.ai_chat.page_context_enabled {
        "当前页面 · 已关闭".to_string()
    } else if page.kind == fluxdb_app::PageContextKind::Other {
        format!("当前页面 · {} · 正文未接入", page.label)
    } else {
        format!("当前页面 · {}", page.label)
    };
    let details = this.ai_chat.page_context_details_open;
    let description = match page.kind {
        fluxdb_app::PageContextKind::SqlEditor => {
            "SQL 文本/摘录、光标附近、当前结果结构与执行状态；完整 SQL 可搜索和续读"
        }
        fluxdb_app::PageContextKind::TableData => {
            "表定位、列类型/主键、筛选排序、选中位置、分页和状态"
        }
        fluxdb_app::PageContextKind::TableStructure => {
            "列结构、已加载索引/外键与有界 DDL；未加载结构不假装已读取"
        }
        fluxdb_app::PageContextKind::RedisKey => {
            "当前 Key、类型、TTL、大小和状态；值预览另须开启共享"
        }
        fluxdb_app::PageContextKind::RedisBrowser => {
            "Redis 数据页结构、分页与状态；不会自动上传 Key 值"
        }
        fluxdb_app::PageContextKind::RedisWorkbench => {
            "当前命令草稿与运行状态；历史回复尚未接入共享"
        }
        fluxdb_app::PageContextKind::Other => {
            "此页面正文未接入 AI；仅提供目标定位，不发送设置表单或凭据"
        }
    };
    let share_data = this.ai_chat.page_data_enabled
        && this.ai_chat.page_data_tab.map(|id| id.0) == page.snapshot.tab_id;
    div()
        // 面板高度受限时不要把这一行压缩成 0 高度；固定自身高度参与布局。
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(
                    Button::new("ai-current-page")
                        .debug_selector(|| "ai-current-page".into())
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .disabled(!ready)
                        .child(app_icon(AppIcon::FileSearch, 14., colors.muted))
                        .child(div().max_w(px(150.)).truncate().child(label))
                        .tooltip("当前页片段或结构会随消息发给模型；点击查看具体范围")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_chat.page_context_details_open =
                                !this.ai_chat.page_context_details_open;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-current-page-toggle")
                        .debug_selector(|| "ai-current-page-toggle".into())
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .disabled(!ready)
                        .child(app_icon(
                            if this.ai_chat.page_context_enabled {
                                AppIcon::Eye
                            } else {
                                AppIcon::EyeOff
                            },
                            14.,
                            colors.muted,
                        ))
                        .tooltip("开启或关闭当前页面上下文")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ai_chat.page_context_enabled = !this.ai_chat.page_context_enabled;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("ai-page-data-toggle")
                        .debug_selector(|| "ai-page-data-toggle".into())
                        .small()
                        .ghost()
                        .cursor_pointer()
                        .disabled(
                            !ready
                                || !this.ai_chat.page_context_enabled
                                || matches!(
                                    page.kind,
                                    fluxdb_app::PageContextKind::Other
                                        | fluxdb_app::PageContextKind::RedisWorkbench
                                ),
                        )
                        .child(if share_data {
                            "数据共享：开"
                        } else {
                            "数据共享：关"
                        })
                        .tooltip("开启后允许读取已加载页面数据值；结果片段会发送给模型服务")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let tab = this.ai_context_source_tab().map(|t| t.id);
                            let enabled =
                                this.ai_chat.page_data_enabled && this.ai_chat.page_data_tab == tab;
                            this.ai_chat.page_data_enabled = !enabled;
                            this.ai_chat.page_data_tab = if enabled { None } else { tab };
                            cx.notify();
                        })),
                ),
        )
        .when(details, |view| {
            view.child(
                div()
                    .id("ai-page-context-details")
                    .test_support()
                    .text_xs()
                    .text_color(colors.muted)
                    .child(if this.ai_chat.page_context_enabled {
                        format!(
                            "{}：{}。{}。{}",
                            page.label,
                            description,
                            if page.kind == fluxdb_app::PageContextKind::Other {
                                "页面正文不可见"
                            } else if page.snapshot.excerpt.as_ref().is_some_and(|e| e.truncated) {
                                "只发送摘录，按授权工具读取详情"
                            } else {
                                "文本或结构化信息"
                            },
                            if share_data {
                                "本页已允许共享已加载数据值，片段会发送给模型服务"
                            } else {
                                "不自动共享业务数据值"
                            }
                        )
                    } else {
                        "本轮不附带当前页面；显式 @ 引用和已有工具权限仍保留".into()
                    }),
            )
        })
        .into_any_element()
}
