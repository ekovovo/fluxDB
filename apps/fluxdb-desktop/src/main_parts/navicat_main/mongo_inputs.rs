// MongoDB 集合工作台的查询面板输入框：按标签页懒建并缓存。
//
// 面板文本只在执行查询那一刻被读取（见 `mongo_loading`），平时不写回标签页状态：
// 这样输入框就是唯一草稿来源，不需要每个按键都走一次派发。

/// 一个集合标签页的查询面板输入框集合（订阅随结构一起 drop，标签页关闭即解绑）。
pub(crate) struct MongoQueryInputs {
    pub(crate) filter: Entity<InputState>,
    pub(crate) projection: Entity<InputState>,
    pub(crate) sort: Entity<InputState>,
    pub(crate) limit: Entity<InputState>,
    pub(crate) aggregation: Entity<TextareaState>,
}

impl NavicatMain {
    /// 取（或惰性创建）指定集合标签页的查询输入框。
    ///
    /// 初值取「最后一次真正执行的查询」，因此刷新、切走再切回来都还是原来那份条件。
    pub(crate) fn mongo_query_inputs_for(
        &mut self,

        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MongoQueryInputs> {
        if let Some(inputs) = self.mongo_query_inputs.get(&tab_id) {
            return inputs.clone();
        }
        let (query, pipeline) = mongo_query_seed(self.controller.state(), tab_id);
        let entity = cx.new(|cx| {
            let text = |value: &str,
                        placeholder: &'static str,
                        window: &mut Window,
                        cx: &mut Context<MongoQueryInputs>| {
                let value = value.to_string();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(tr(placeholder))
                        .default_value(value.clone())
                })
            };
            MongoQueryInputs {
                filter: text(
                    &query.filter,
                    "app.mongo.query.filter_placeholder",
                    window,
                    cx,
                ),
                projection: text(
                    &query.projection,
                    "app.mongo.query.projection_placeholder",
                    window,
                    cx,
                ),
                sort: text(&query.sort, "app.mongo.query.sort_placeholder", window, cx),
                limit: text(
                    &query.limit,
                    "app.mongo.query.limit_placeholder",
                    window,
                    cx,
                ),
                aggregation: {
                    let pipeline = pipeline.clone();
                    cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .placeholder(tr("app.mongo.query.aggregation_placeholder"))
                            .rows(6)
                            .default_value(pipeline.clone())
                    })
                },
            }
        });
        self.mongo_query_inputs.insert(tab_id, entity.clone());
        entity
    }

    /// 切换分节；索引/集合信息按需懒加载（各自只加载一次，重试走错误块）。
    pub(crate) fn select_mongo_section(
        &mut self,
        tab_id: TabId,
        section: MongoSection,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(AppCommand::SelectMongoSection { tab_id, section }, cx);
        let (load_info, load_indexes) = {
            let Some(state) = mongo_collection_state(self.controller.state(), tab_id) else {
                return;
            };
            (
                section == MongoSection::Info && matches!(state.info, LoadState::NotLoaded),
                section == MongoSection::Indexes && matches!(state.indexes, LoadState::NotLoaded),
            )
        };
        if load_info {
            self.load_mongo_collection_info(tab_id, cx);
        }
        if load_indexes {
            self.load_mongo_indexes(tab_id, cx);
        }
    }

    pub(crate) fn select_mongo_display(
        &mut self,
        tab_id: TabId,
        mode: MongoDisplayMode,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(AppCommand::SelectMongoDisplayMode { tab_id, mode }, cx);
    }

    /// 打开/关闭文档详情抽屉；`None` 关闭。
    pub(crate) fn select_mongo_document(
        &mut self,
        tab_id: TabId,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(AppCommand::SelectMongoDocument { tab_id, index }, cx);
    }

    /// 翻页：先改分页起点再按当前条件重查。
    pub(crate) fn set_mongo_offset(&mut self, tab_id: TabId, offset: u64, cx: &mut Context<Self>) {
        self.dispatch(AppCommand::SetMongoOffset { tab_id, offset }, cx);
        self.run_mongo_query(tab_id, cx);
    }

    /// 重置查询面板为默认查询（`{}`、无投影/排序、当前页大小）并立即重查。
    pub(crate) fn reset_mongo_query(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inputs) = self.mongo_query_inputs.get(&tab_id).cloned() else {
            return;
        };
        let limit = mongo_collection_state(self.controller.state(), tab_id)
            .map(|state| state.pagination.limit)
            .unwrap_or_else(|| self.controller.state().settings.data_table_page_size);
        let draft = MongoQueryDraft::new(limit);
        let (filter, projection, sort, limit) = {
            let inputs = inputs.read(cx);
            (
                inputs.filter.clone(),
                inputs.projection.clone(),
                inputs.sort.clone(),
                inputs.limit.clone(),
            )
        };
        for (input, value) in [
            (filter, draft.filter),
            (projection, draft.projection),
            (sort, draft.sort),
            (limit, draft.limit),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.run_mongo_query(tab_id, cx);
    }

    /// 错误块重试：按出错区域只重跑对应命令。
    pub(crate) fn retry_mongo_load(
        &mut self,
        tab_id: TabId,
        retry: MongoRetry,
        cx: &mut Context<Self>,
    ) {
        match retry {
            MongoRetry::Documents => self.run_mongo_query(tab_id, cx),
            MongoRetry::Aggregation => self.run_mongo_aggregation(tab_id, cx),
            MongoRetry::Info => self.load_mongo_collection_info(tab_id, cx),
            MongoRetry::Indexes => self.load_mongo_indexes(tab_id, cx),
        }
    }

    /// 关闭标签页时释放输入框实体。
    pub(crate) fn close_mongo_tab_state(&mut self, tab_id: TabId) {
        self.mongo_query_inputs.remove(&tab_id);
        self.mongo_collapsed_documents.remove(&tab_id);
        self.mongo_expanded_fields.remove(&tab_id);
        self.mongo_json_fold_toggles.remove(&tab_id);
        self._mongo_load_tasks.remove(&tab_id.0);
        self._mongo_aggregation_tasks.remove(&tab_id.0);
        self._mongo_info_tasks.remove(&tab_id.0);
        self._mongo_index_tasks.remove(&tab_id.0);
        // 编辑抽屉是纯界面会话：关标签即丢弃草稿，不留悬挂的编辑器实体。
        if self
            .mongo_document_editor
            .as_ref()
            .is_some_and(|editor| editor.tab_id == tab_id)
        {
            self.mongo_document_editor = None;
            self._mongo_document_editor_task = None;
            self._mongo_document_insert_task = None;
        }
    }
}

/// 输入框初值：已执行查询 + 聚合管道；无标签页状态时给默认值。
fn mongo_query_seed(state: &AppState, tab_id: TabId) -> (MongoQueryDraft, String) {
    state
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .and_then(|tab| match &tab.kind {
            TabKind::MongoCollection(state) => Some((
                state.applied_query.clone(),
                state.aggregation_pipeline.clone(),
            )),
            _ => None,
        })
        .unwrap_or_else(|| {
            (
                MongoQueryDraft::new(state.settings.data_table_page_size),
                MongoCollectionState::default_aggregation_pipeline(),
            )
        })
}
