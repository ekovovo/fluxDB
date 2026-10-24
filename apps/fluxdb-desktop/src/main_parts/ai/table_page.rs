#[derive(Clone)]
struct AiTablePageBinding {
    turn_id: String,
    page_ref: String,
    tab_id: TabId,
    load_generation: u64,
    object: ObjectPath,
    columns: Vec<GdbColumn>,
    pagination: fluxdb_core::Pagination,
    filters: Vec<FilterSpec>,
    sort: Vec<SortSpec>,
    draft_filters: Vec<DataFilterRule>,
    draft_sort: Vec<DataSortRule>,
    filter_text: String,
    sort_text: String,
    queried: bool,
}

#[cfg(test)]
fn run_page_query_fixture(
    object: ObjectPath,
    query: fluxdb_app::AiTableQuery,
    job: fluxdb_app::AiConversationJob,
    actions: std::sync::Arc<dyn fluxdb_app::UiActionSink>,
    connections: Vec<ConnectionConfig>,
    cancel: &fluxdb_core::CancelFlag,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>> {
    let mut catalog = fluxdb_app::ToolCatalog::new(Vec::new());
    fluxdb_app::add_ai_table_page_tools(&mut catalog, &connections, actions)?;
    let run = |tool: &str, args: serde_json::Value| {
        let args = args.to_string();
        catalog.prepare(tool, &args).expect("fixture schema");
        let receipt = fluxdb_app::ai_executor::block_on_cancellable(
            catalog
                .executable(tool)
                .expect("production page tool")
                .execute(
                    fluxdb_core::ToolExecutionContext {
                        session_id: job.session_id.clone(),
                        turn_id: job.turn_id.clone(),
                        iteration: 1,
                        call_id: format!("page-query-{tool}"),
                        cancel: cancel.clone(),
                        automatic_approval: false,
                        bounded_write: false,
                        progress: None,
                    },
                    args,
                ),
            cancel,
        )??;
        if !receipt.is_success() {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                receipt
                    .error_code
                    .unwrap_or_else(|| "page_query_failed".into()),
            ));
        }
        Ok(receipt.result_json)
    };
    let found = run("page.find_table", serde_json::json!({"table":object.name}))?;
    let location = serde_json::from_str::<serde_json::Value>(&found)
        .ok()
        .and_then(|value| value["items"].as_array()?.first().cloned())
        .ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "page_query_failed")
        })?;
    let object: ObjectPath = serde_json::from_value(location).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "page_query_invalid")
    })?;
    let opened = run("page.open_table", serde_json::json!({"object":object}))?;
    let page_ref = serde_json::from_str::<serde_json::Value>(&opened)
        .ok()
        .and_then(|v| v["page_ref"].as_str().map(str::to_string))
        .ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "page_query_failed")
        })?;
    let mut args = serde_json::to_value(query).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "page_query_invalid")
    })?;
    args["page_ref"] = serde_json::json!(page_ref);
    run("page.query_table", args)?;
    let event = fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop);
    emit(&event);
    Ok(vec![event])
}

impl NavicatMain {
    /// 用户在表页重新应用筛选/排序时接管该页面：丢弃 AI 待完成的页面任务，
    /// 让它的等待端收到取消，避免迟到的 AI 结果覆盖用户刚发起的查询。
    fn take_over_ai_table_page(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if !self
            .ai_chat
            .table_page_binding
            .as_ref()
            .is_some_and(|binding| binding.tab_id == tab_id)
        {
            return;
        }
        if self.ai_chat.table_page_task.take().is_some() {
            if let Some(request) = self.ai_chat.table_page_request.take() {
                request.complete(Err("page_query_cancelled"));
            }
            self.show_message(
                tr("app.ai.page_query.user_took_over"),
                AppMessageKind::Info,
                cx,
            );
        }
    }

    fn table_page_binding_matches(&self, binding: &AiTablePageBinding, loading: bool) -> bool {
        self.controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == binding.tab_id)
            .is_some_and(|tab| match &tab.kind {
                TabKind::DataEditor(editor) => {
                    editor.object == binding.object
                        && editor.load_generation == binding.load_generation
                        && editor.pagination == binding.pagination
                        && editor.loading == loading
                        && !editor.submitting
                        && !editor.changes.as_ref().is_some_and(|c| !c.is_empty())
                        && editor
                            .page
                            .as_ref()
                            .is_some_and(|page| page.columns == binding.columns)
                }
                _ => false,
            })
            && self.data_filter_specs_for_tab(binding.tab_id) == binding.filters
            && self.data_sort_specs_for_tab(binding.tab_id) == binding.sort
            && self
                .data_filter_draft_rules
                .get(&binding.tab_id)
                .cloned()
                .unwrap_or_default()
                == binding.draft_filters
            && self
                .data_sort_draft_rules
                .get(&binding.tab_id)
                .cloned()
                .unwrap_or_default()
                == binding.draft_sort
            && self
                .data_filter_texts
                .get(&binding.tab_id)
                .cloned()
                .unwrap_or_default()
                == binding.filter_text
            && self
                .data_sort_texts
                .get(&binding.tab_id)
                .cloned()
                .unwrap_or_default()
                == binding.sort_text
            && self
                .data_filter_modes
                .get(&binding.tab_id)
                .is_none_or(|mode| *mode == DataFilterMode::Builder)
    }

    fn capture_table_page_binding(
        &self,
        turn_id: String,
        tab_id: TabId,
    ) -> Option<AiTablePageBinding> {
        let editor = self
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.kind {
                TabKind::DataEditor(editor) => Some(editor),
                _ => None,
            })?;
        Some(AiTablePageBinding {
            page_ref: format!("table-page:{}:{}", turn_id, tab_id.0),
            turn_id,
            tab_id,
            load_generation: editor.load_generation,
            object: editor.object.clone(),
            columns: editor.page.as_ref()?.columns.clone(),
            pagination: editor.pagination,
            filters: self.data_filter_specs_for_tab(tab_id),
            sort: self.data_sort_specs_for_tab(tab_id),
            draft_filters: self
                .data_filter_draft_rules
                .get(&tab_id)
                .cloned()
                .unwrap_or_default(),
            draft_sort: self
                .data_sort_draft_rules
                .get(&tab_id)
                .cloned()
                .unwrap_or_default(),
            filter_text: self
                .data_filter_texts
                .get(&tab_id)
                .cloned()
                .unwrap_or_default(),
            sort_text: self
                .data_sort_texts
                .get(&tab_id)
                .cloned()
                .unwrap_or_default(),
            queried: self.ai_chat.table_query_ran,
        })
    }

    fn fail_ai_table_page(
        &mut self,
        request: &fluxdb_app::AiTablePageRequest,
        code: &'static str,
        cx: &mut Context<Self>,
    ) {
        request.complete(Err(code));
        self.ai_chat.table_page_request = None;
        self.show_message(
            localize_backend_error_text(code),
            AppMessageKind::Warning,
            cx,
        );
    }

    fn apply_ai_table_page(
        &mut self,
        request: std::sync::Arc<fluxdb_app::AiTablePageRequest>,
        cx: &mut Context<Self>,
    ) {
        if !request.active()
            || !self
                .ai_chat
                .running
                .as_ref()
                .is_some_and(|job| job.turn_id == request.turn_id)
        {
            self.fail_ai_table_page(&request, "page_query_cancelled", cx);
            return;
        }
        if self.ai_chat.table_page_task.is_some() {
            self.fail_ai_table_page(&request, "page_query_busy", cx);
            return;
        }
        self.ai_chat.table_page_request = Some(request.clone());
        if let fluxdb_app::AiTablePageAction::Query(query) = &request.action {
            self.apply_ai_table_query(request.clone(), query.clone(), cx);
            if self.ai_chat.table_page_task.is_none()
                && self.ai_chat.table_page_request.is_some()
            {
                // 预填等同步完成路径不会再进后台任务，及时释放请求引用。
                self.ai_chat.table_page_request = None;
            }
            return;
        }
        if let fluxdb_app::AiTablePageAction::Find(name) = &request.action {
            let configs = request.configs();
            let find_name = name.clone();
            let cancel = request.cancel_flag();
            let controller = self.controller.clone();
            self.ai_chat.table_page_task = Some(cx.spawn(async move |view, cx| {
                let result = cx
                    .background_spawn(async move {
                        controller.find_table_in_connections(&configs, &find_name, &cancel)
                    })
                    .await;
                let _ = view.update(cx, |this, cx| {
                    this.ai_chat.table_page_task = None;
                    this.ai_chat.table_page_request = None;
                    match result {
                        Ok(value) => request.complete(Ok(value)),
                        Err(code) => this.fail_ai_table_page(&request, code, cx),
                    }
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        if let fluxdb_app::AiTablePageAction::Open(object) = &request.action {
            let source = self
                .controller
                .state()
                .connections
                .iter()
                .find(|connection| connection.config.id == object.connection_id);
            let known = source.is_some_and(|connection| {
                connection
                    .objects
                    .iter()
                    .any(|item| &item.path == object)
            });
            let has_level0 = source.is_some_and(|connection| {
                connection.objects.iter().any(|item| {
                    matches!(
                        item.path.kind,
                        ObjectKind::Database | ObjectKind::Schema | ObjectKind::RedisDb
                    )
                })
            });
            if !has_level0 {
                // 按名称找表只带回表节点；先补齐连接的全部库，否则侧边栏在手动刷新前
                // 只会显示目标表所在的库。
                let object = object.clone();
                self.reload_ai_table_level0(request, object, cx);
                return;
            }
            if !known {
                let event = self
                    .controller
                    .dispatch(AppCommand::RememberAiTableObject(object.clone()));
                if matches!(event, AppEvent::Failed(_)) {
                    self.fail_ai_table_page(&request, "page_query_unavailable", cx);
                    return;
                }
            }
        }
        let (connection_id, command) = match &request.action {
            fluxdb_app::AiTablePageAction::Browse {
                connection_id,
                parent,
                ..
            } => (
                *connection_id,
                AppCommand::BrowseAiTableObjects {
                    connection_id: *connection_id,
                    parent: parent.clone(),
                },
            ),
            fluxdb_app::AiTablePageAction::Open(object) => (
                object.connection_id,
                AppCommand::LoadAiTableColumns(object.clone()),
            ),
            _ => return,
        };
        let source = self
            .controller
            .state()
            .connections
            .iter()
            .find(|c| c.config.id == connection_id);
        if !source
            .is_some_and(|c| request.allows(connection_id) && request.matches_connection(&c.config))
            || self.connecting_connections.contains(&connection_id)
        {
            self.fail_ai_table_page(&request, "page_query_unavailable", cx);
            return;
        }
        let was_connected = source.expect("validated connection").connected;
        let active_tab = self.controller.state().active_tab;
        self.connecting_connections.insert(connection_id);
        let mut controller = self.controller.clone();
        self.ai_chat.table_page_task = Some(cx.spawn(async move |view, cx| {
            let event = cx.background_spawn(async move { controller.dispatch(command) }).await;
            let _ = view.update(cx, |this, cx| {
                this.ai_chat.table_page_task = None;
                this.ai_chat.table_page_request = None;
                this.connecting_connections.remove(&connection_id);
                let unchanged = this.controller.state().connections.iter().find(|c|c.config.id == connection_id)
                    .is_some_and(|c| request.matches_connection(&c.config) && c.connected == was_connected);
                if !request.active() || !unchanged || this.controller.state().active_tab != active_tab {
                    this.fail_ai_table_page(&request, "page_query_conflict", cx);
                    return;
                }
                match event {
                    AppEvent::AiTableObjectsLoaded { connection_id, parent, objects } => {
                        let offset = match &request.action {
                            fluxdb_app::AiTablePageAction::Browse { offset, .. } => *offset, _ => 0,
                        };
                        let total = objects.len();
                        let items = objects.iter().skip(offset).take(50).map(|object| serde_json::json!({
                            "path":object.path,
                            "comment":object.comment.as_ref().map(|s|s.chars().take(256).collect::<String>())
                        })).collect::<Vec<_>>();
                        let event = this.controller.dispatch(AppCommand::FinishAiTableObjects {
                            connection_id, parent:parent.clone(), objects,
                        });
                        if matches!(event, AppEvent::Failed(_)) {
                            this.fail_ai_table_page(&request, "page_query_failed", cx);
                            return;
                        }
                        this.show_connection_browser = true;
                        if let Some(parent) = parent {
                            this.expand_ai_table_path(&parent);
                            let database = parent.database.as_deref().unwrap_or(&parent.name);
                            this.loaded_database_children.insert(object_group_children_key(connection_id, database, parent.schema.as_deref()));
                        }
                        request.complete(Ok(serde_json::json!({"items":items,"total":total,
                            "next_offset":(offset.saturating_add(50)<total).then_some(offset.saturating_add(50)),
                            "scope":"侧边栏对象元数据，不包含数据行"})));
                        cx.notify();
                    }
                    AppEvent::AiTableColumnsLoaded(object, columns) => this.open_ai_table_page(request, object, columns, cx),
                    AppEvent::Failed(error) => {
                        this.show_message(localize_backend_error_text(&error.message), AppMessageKind::Error, cx);
                        request.complete(Err("page_query_failed"));
                    }
                    _ => this.fail_ai_table_page(&request, "page_query_failed", cx),
                }
            });
        }));
        cx.notify();
    }

    /// 找表命中后，连接树里可能只有目标表、没有任何库节点。先取回连接第一层
    /// （库/schema），合并后再继续打开表，保证侧边栏与手动刷新后的结果一致。
    fn reload_ai_table_level0(
        &mut self,
        request: std::sync::Arc<fluxdb_app::AiTablePageRequest>,
        object: ObjectPath,
        cx: &mut Context<Self>,
    ) {
        let connection_id = object.connection_id;
        let allowed = self.controller.state().connections.iter().find(|c| c.config.id == connection_id)
            .is_some_and(|c| request.allows(connection_id) && request.matches_connection(&c.config));
        if !allowed || self.connecting_connections.contains(&connection_id) {
            self.fail_ai_table_page(&request, "page_query_unavailable", cx);
            return;
        }
        self.connecting_connections.insert(connection_id);
        let mut controller = self.controller.clone();
        self.ai_chat.table_page_task = Some(cx.spawn(async move |view, cx| {
            let event = cx.background_spawn(async move {
                controller.dispatch(AppCommand::BrowseAiTableObjects {
                    connection_id,
                    parent: None,
                })
            }).await;
            let _ = view.update(cx, |this, cx| {
                this.ai_chat.table_page_task = None;
                this.ai_chat.table_page_request = None;
                this.connecting_connections.remove(&connection_id);
                if !request.active() {
                    this.fail_ai_table_page(&request, "page_query_cancelled", cx);
                    return;
                }
                match event {
                    AppEvent::AiTableObjectsLoaded { connection_id, parent, objects } => {
                        let event = this.controller.dispatch(AppCommand::FinishAiTableObjects {
                            connection_id,
                            parent: parent.clone(),
                            objects,
                        });
                        if matches!(event, AppEvent::Failed(_)) {
                            this.fail_ai_table_page(&request, "page_query_failed", cx);
                            return;
                        }
                        this.ai_chat.table_page_task = None;
                        this.ai_chat.table_page_request = None;
                        this.apply_ai_table_page(request, cx);
                    }
                    _ => this.fail_ai_table_page(&request, "page_query_failed", cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn expand_ai_table_path(&mut self, object: &ObjectPath) {
        if let Some(group_id) = self
            .controller
            .state()
            .sidebar_layout
            .connection_group(object.connection_id)
            && self
                .controller
                .state()
                .sidebar_layout
                .groups
                .iter()
                .any(|group| group.id == group_id && group.collapsed)
        {
            self.controller
                .dispatch(AppCommand::ToggleConnectionGroup(group_id));
        }
        let database = object.database.as_deref().unwrap_or("main");
        self.expanded_databases
            .insert(database_tree_key(object.connection_id, database), true);
        if let Some(schema) = object.schema.as_deref() {
            self.expanded_databases.insert(
                schema_tree_key(object.connection_id, database, schema),
                true,
            );
        }
        for group in [ObjectGroup::Tables, ObjectGroup::Views] {
            self.expanded_object_groups.insert(
                object_group_tree_key_scoped(
                    object.connection_id,
                    database,
                    object.schema.as_deref(),
                    group,
                ),
                true,
            );
        }
    }

    fn open_ai_table_page(
        &mut self,
        request: std::sync::Arc<fluxdb_app::AiTablePageRequest>,
        object: ObjectPath,
        columns: Vec<GdbColumn>,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .controller
            .state()
            .tabs
            .iter()
            .find_map(|tab| match &tab.kind {
                TabKind::DataEditor(editor) if editor.object == object => Some((tab.id, editor)),
                _ => None,
            });
        if existing.is_some_and(|(_, editor)| {
            editor.loading
                || editor.submitting
                || editor
                    .changes
                    .as_ref()
                    .is_some_and(|changes| !changes.is_empty())
        }) {
            self.fail_ai_table_page(&request, "page_query_conflict", cx);
            return;
        }
        let already_loaded = existing.is_some_and(|(_, editor)| editor.page.is_some());
        let event = self
            .controller
            .dispatch(AppCommand::OpenDataEditor(object.clone()));
        let tab_id = match event {
            AppEvent::TabOpened(id) | AppEvent::TabActivated(id) => id,
            _ => {
                self.fail_ai_table_page(&request, "page_query_failed", cx);
                return;
            }
        };
        self.show_connection_browser = true;
        self.expand_ai_table_path(&object);
        self.data_filter_panels.insert(tab_id);
        let rows = self
            .sidebar_tree_rows(
                self.controller.state(),
                &self.loading_databases,
                &self.loaded_database_children,
                &self.pinned_databases,
                &self.pinned_tables,
                &self.table_folders,
                &self.table_folder_assignments,
                &self.expanded_databases,
                &self.expanded_object_groups,
                &self.saved_queries,
                &self.sidebar_search,
            )
            .0;
        if let Some(index) = rows
            .iter()
            .position(|row| row.object.as_ref().is_some_and(|item| item.path == object))
        {
            self.sidebar_tree_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        if already_loaded {
            self.complete_ai_table_open(request, tab_id, object, columns, cx);
            return;
        }
        let generation = match self
            .controller
            .dispatch(AppCommand::BeginDataPageLoad(tab_id))
        {
            AppEvent::DataPageLoadStarted { generation, .. } => generation,
            _ => {
                self.fail_ai_table_page(&request, "page_query_failed", cx);
                return;
            }
        };
        let mut controller = self.controller.clone();
        self.ai_chat.table_page_task = Some(cx.spawn(async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch(AppCommand::LoadDataPageWithSort {
                        tab_id,
                        generation,
                        sort: Vec::new(),
                        filters: Vec::new(),
                        expression: None,
                    })
                })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.ai_chat.table_page_task = None;
                this.ai_chat.table_page_request = None;
                if matches!(event, AppEvent::DataPageLoadDiscarded { .. }) {
                    this.fail_ai_table_page(&request, "page_query_conflict", cx);
                    cx.notify();
                    return;
                }
                let result = match event {
                    AppEvent::DataLoaded(_, page) => Ok(page),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(fluxdb_core::UserFacingError {
                        title: tr("app.data.error.load").to_string(),
                        message: tr("app.data.error.no_load_result").to_string(),
                        detail: None,
                        retryable: true,
                    }),
                };
                let applied = this
                    .controller
                    .dispatch(AppCommand::FinishDataPageLoad { tab_id, generation, result });
                this.apply_app_event(&applied, cx);
                match applied {
                    AppEvent::DataLoaded(_, page) if request.active() => {
                        this.complete_ai_table_open(
                            request,
                            tab_id,
                            object,
                            page.columns.clone(),
                            cx,
                        );
                    }
                    AppEvent::Failed(error) => {
                        this.show_message(
                            localize_backend_error_text(&error.message),
                            AppMessageKind::Error,
                            cx,
                        );
                        this.fail_ai_table_page(&request, "page_query_failed", cx);
                    }
                    _ => this.fail_ai_table_page(&request, "page_query_conflict", cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn complete_ai_table_open(
        &mut self,
        request: std::sync::Arc<fluxdb_app::AiTablePageRequest>,
        tab_id: TabId,
        object: ObjectPath,
        columns: Vec<GdbColumn>,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = self.capture_table_page_binding(request.turn_id.clone(), tab_id) else {
            self.fail_ai_table_page(&request, "page_query_failed", cx);
            return;
        };
        // 已有表页只复用，不清空结果、筛选或未保存条件。
        let response = serde_json::json!({"page_ref":binding.page_ref,"object":object,
            "columns":columns.iter().map(|c|serde_json::json!({"name":c.name,"type":c.type_name})).collect::<Vec<_>>(),
            "default_limit":100,"has_existing_conditions":!binding.filters.is_empty() || !binding.sort.is_empty()
                || !binding.draft_filters.is_empty() || !binding.draft_sort.is_empty(),
            "queried":false});
        self.ai_chat.table_page_binding = Some(binding);
        self.ai_chat.table_page_request = None;
        request.complete(Ok(response));
        self.show_message(tr("app.ai.page_query.opened"), AppMessageKind::Info, cx);
        cx.notify();
    }

    fn apply_ai_table_query(
        &mut self,
        request: std::sync::Arc<fluxdb_app::AiTablePageRequest>,
        query: fluxdb_app::AiTableQuery,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = self.ai_chat.table_page_binding.clone().filter(|binding| {
            binding.turn_id == request.turn_id && binding.page_ref == query.page_ref
        }) else {
            self.fail_ai_table_page(&request, "page_query_unavailable", cx);
            return;
        };
        if query.execute && binding.queried {
            self.fail_ai_table_page(&request, "page_query_already_run", cx);
            return;
        }
        let source = self
            .controller
            .state()
            .connections
            .iter()
            .find(|c| c.config.id == binding.object.connection_id);
        if !source.is_some_and(|c| {
            c.connected && request.allows(c.config.id) && request.matches_connection(&c.config)
        }) || !self.table_page_binding_matches(&binding, false)
        {
            self.fail_ai_table_page(&request, "page_query_conflict", cx);
            return;
        }
        let Ok((filters, sort)) = query.specs(&binding.columns) else {
            self.fail_ai_table_page(&request, "page_query_invalid", cx);
            return;
        };
        if (!binding.filters.is_empty()
            || !binding.draft_filters.is_empty()
            || !binding.sort.is_empty()
            || !binding.draft_sort.is_empty())
            && (filters != binding.filters
                || sort != binding.sort
                || data_filter_specs_from_rules(&binding.draft_filters) != filters)
        {
            self.fail_ai_table_page(&request, "page_query_existing", cx);
            return;
        }
        let mut rules = Vec::new();
        for item in &query.filters {
            if item
                .values
                .iter()
                .any(|v| normalized_data_filter_value(v) != *v)
            {
                self.fail_ai_table_page(&request, "page_query_invalid", cx);
                return;
            }
            let operator = match item.op.as_str() {
                "eq" => DataFilterOperator::Eq,
                "ne" => DataFilterOperator::Ne,
                "lt" => DataFilterOperator::Lt,
                "le" => DataFilterOperator::Le,
                "gt" => DataFilterOperator::Gt,
                "ge" => DataFilterOperator::Ge,
                "contains" => DataFilterOperator::Contains,
                "is_null" => DataFilterOperator::IsNull,
                "is_not_null" => DataFilterOperator::IsNotNull,
                "in" => DataFilterOperator::InList,
                "not_in" => DataFilterOperator::NotInList,
                _ => {
                    self.fail_ai_table_page(&request, "page_query_invalid", cx);
                    return;
                }
            };
            rules.push(DataFilterRule {
                enabled: true,
                field: Some(item.field.clone()),
                operator,
                values: item.values.iter().cloned().collect(),
                advanced_expression: None,
                typed_values: None,
                connector: fluxdb_core::FilterCombinator::And,
                group_depth: 0,
                        group_end: 0,
            });
        }
        let sorts = query
            .sort
            .iter()
            .map(|item| DataSortRule {
                enabled: true,
                field: item.field.clone(),
                ascending: !item.descending,
            })
            .collect::<Vec<_>>();
        let tab_id = binding.tab_id;
        self.data_filter_rules.insert(tab_id, rules.clone());
        self.data_filter_draft_rules.insert(tab_id, rules);
        self.data_sort_rules.insert(tab_id, sorts.clone());
        self.data_sort_draft_rules.insert(tab_id, sorts);
        self.data_filter_modes
            .insert(tab_id, DataFilterMode::Builder);
        self.sync_data_filter_builder_to_text(tab_id);
        let updated = self
            .capture_table_page_binding(request.turn_id.clone(), tab_id)
            .expect("validated table page");
        self.ai_chat.table_page_binding = Some(updated.clone());
        if !query.execute {
            request.complete(Ok(serde_json::json!({"status":"prefilled","queried":false,"page_ref":updated.page_ref})));
            self.show_message(
                tr("app.ai.page_query.prefilled"),
                AppMessageKind::Success,
                cx,
            );
            return;
        }
        let generation = match self.controller.dispatch(AppCommand::BeginAiTableQuery {
            tab_id,
            limit: query.limit,
        }) {
            AppEvent::DataPageLoadStarted { generation, .. } => generation,
            _ => {
                self.fail_ai_table_page(&request, "page_query_conflict", cx);
                return;
            }
        };
        let mut expected = self
            .capture_table_page_binding(request.turn_id.clone(), tab_id)
            .expect("query page");
        expected.page_ref = updated.page_ref;
        expected.queried = true;
        self.ai_chat.table_page_binding = Some(expected.clone());
        self.ai_chat.table_query_ran = true;
        let mut controller = self.controller.clone();
        let object = expected.object.clone();
        self.ai_chat.table_page_task = Some(cx.spawn(async move |view,cx| {
            let event = cx.background_spawn(async move {
                controller.dispatch(AppCommand::LoadAiTablePage {
                    tab_id,
                    generation,
                    object,
                    query,
                })
            }).await;
            let _ = view.update(cx,|this,cx| {
                this.ai_chat.table_page_task = None;
                this.ai_chat.table_page_request = None;
                let source_unchanged = this.controller.state().connections.iter().find(|c| c.config.id == expected.object.connection_id)
                    .is_some_and(|c| c.connected && request.matches_connection(&c.config));
                if !request.active() || !source_unchanged || !this.table_page_binding_matches(&expected,true) {
                    // 不把迟到结果覆盖到用户已编辑或已切换的页面。
                    if this.table_page_binding_matches(&expected,true) {
                        this.controller.dispatch(AppCommand::FinishDataPageLoad {tab_id,generation:expected.load_generation,result:Err(
                            fluxdb_core::UserFacingError {title:String::new(),message:localize_backend_error_text("page_query_cancelled"),
                                detail:None,retryable:false})});
                    }
                    this.fail_ai_table_page(&request,"page_query_conflict",cx);
                    return;
                }
                match event {
                    AppEvent::DataLoaded(_,page) => {
                        let response=serde_json::json!({"status":"queried","queried":true,
                            "page_ref":expected.page_ref,"loaded_rows":page.rows.len(),"has_more":page.has_more,
                            "scope":"数据行仅在表页面显示，不向模型返回行值"});
                        let event=this.controller.dispatch(AppCommand::FinishDataPageLoad {tab_id,generation:expected.load_generation,result:Ok(page)});
                        this.apply_app_event(&event,cx);
                        request.complete(Ok(response));
                        this.show_message(tr("app.ai.page_query.queried"),AppMessageKind::Success,cx);
                    }
                    AppEvent::Failed(error) => {
                        this.controller.dispatch(AppCommand::FinishDataPageLoad {tab_id,generation:expected.load_generation,result:Err(error.clone())});
                        this.show_message(localize_backend_error_text(&error.message),AppMessageKind::Error,cx);
                        request.complete(Err("page_query_failed"));
                    }
                    _=>this.fail_ai_table_page(&request,"page_query_failed",cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
