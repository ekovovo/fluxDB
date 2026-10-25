// MongoDB 集合工作台的加载编排：首屏查询、聚合、索引与集合信息的后台任务。
//
// 与数据表页一致：UI 线程只发命令，连接器调用在 `spawn_controller_command` 的后台线程里跑，
// 结果经 `FinishMongo*` 命令回写标签页状态；同一个标签页只保留最近一次任务，连点刷新不会并发。

impl NavicatMain {
    /// 打开集合标签页后的首屏加载：文档页永远需要，元数据按当前分节懒加载。
    pub(crate) fn start_mongo_load_if_needed(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some((section, has_page, loading, has_info, has_indexes)) =
            mongo_collection_state(self.controller.state(), tab_id).map(|state| {
                (
                    state.section,
                    state.page.is_some(),
                    state.loading,
                    !matches!(state.info, LoadState::NotLoaded),
                    !matches!(state.indexes, LoadState::NotLoaded),
                )
            })
        else {
            return;
        };
        if !has_page && !loading {
            self.run_mongo_query(tab_id, cx);
        }
        if section == MongoSection::Info && !has_info {
            self.load_mongo_collection_info(tab_id, cx);
        }
        if section == MongoSection::Indexes && !has_indexes {
            self.load_mongo_indexes(tab_id, cx);
        }
    }

    /// 用查询面板的当前文本（面板未创建时用上次执行的查询）跑一次 `find`。
    pub(crate) fn run_mongo_query(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(query) = self.mongo_query_draft(tab_id, cx) else {
            return;
        };
        let AppEvent::MongoLoadStarted { generation, .. } =
            self.controller.dispatch(AppCommand::BeginMongoLoad(tab_id))
        else {
            return;
        };
        let task = self.spawn_controller_command(
            AppCommand::LoadMongoDocuments {
                tab_id,
                generation,
                query,
            },
            move |this, event, cx| {
                this._mongo_load_tasks.remove(&tab_id.0);
                if matches!(event, AppEvent::MongoLoadDiscarded { .. }) {
                    return;
                }
                let result = match event {
                    AppEvent::MongoDocumentsLoaded(_, page) => Ok(page),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(mongo_load_error()),
                };
                if tab_exists(this.controller.state(), tab_id) {
                    let event = this.controller.dispatch(AppCommand::FinishMongoDocuments {
                        tab_id,
                        generation,
                        result,
                    });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._mongo_load_tasks.insert(tab_id.0, task);
    }

    /// 执行聚合管道。管道文本优先取面板输入框，其次取标签页草稿。
    pub(crate) fn run_mongo_aggregation(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let pipeline = self
            .mongo_query_inputs
            .get(&tab_id)
            .map(|inputs| inputs.read(cx).aggregation.read(cx).value().to_string())
            .filter(|pipeline| !pipeline.trim().is_empty())
            .unwrap_or_else(MongoCollectionState::default_aggregation_pipeline);
        let event = self.dispatch(
            AppCommand::UpdateMongoAggregation {
                tab_id,
                pipeline: pipeline.clone(),
            },
            cx,
        );
        if !matches!(event, AppEvent::TabActivated(_)) {
            return;
        }
        let AppEvent::MongoAggregationStarted { generation, .. } =
            self.controller.dispatch(AppCommand::BeginMongoAggregation(tab_id))
        else {
            return;
        };
        let task = self.spawn_controller_command(
            AppCommand::RunMongoAggregation {
                tab_id,
                generation,
                pipeline,
            },
            move |this, event, cx| {
                this._mongo_aggregation_tasks.remove(&tab_id.0);
                if matches!(event, AppEvent::MongoLoadDiscarded { .. }) {
                    return;
                }
                let result = match event {
                    AppEvent::MongoAggregationLoaded(_, page) => Ok(page),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(mongo_load_error()),
                };
                if tab_exists(this.controller.state(), tab_id) {
                    let event = this.controller.dispatch(AppCommand::FinishMongoAggregation {
                        tab_id,
                        generation,
                        result,
                    });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._mongo_aggregation_tasks.insert(tab_id.0, task);
    }

    fn load_mongo_collection_info(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._mongo_info_tasks.contains_key(&tab_id.0) {
            return;
        }
        let task = self.spawn_controller_command(
            AppCommand::LoadMongoCollectionInfo(tab_id),
            move |this, event, cx| {
                this._mongo_info_tasks.remove(&tab_id.0);
                let result = match event {
                    AppEvent::MongoCollectionInfoLoaded(_, info) => Ok(info),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(mongo_load_error()),
                };
                if tab_exists(this.controller.state(), tab_id) {
                    let event = this.controller.dispatch(AppCommand::FinishMongoCollectionInfo {
                        tab_id,
                        result,
                    });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._mongo_info_tasks.insert(tab_id.0, task);
    }

    fn load_mongo_indexes(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._mongo_index_tasks.contains_key(&tab_id.0) {
            return;
        }
        let task = self.spawn_controller_command(
            AppCommand::LoadMongoIndexes(tab_id),
            move |this, event, cx| {
                this._mongo_index_tasks.remove(&tab_id.0);
                let result = match event {
                    AppEvent::MongoIndexesLoaded(_, indexes) => Ok(indexes),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(mongo_load_error()),
                };
                if tab_exists(this.controller.state(), tab_id) {
                    let event = this.controller.dispatch(AppCommand::FinishMongoIndexes {
                        tab_id,
                        result,
                    });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._mongo_index_tasks.insert(tab_id.0, task);
    }

    /// 从查询面板读取草稿；面板尚未创建时回落到上次执行的查询。
    fn mongo_query_draft(&self, tab_id: TabId, cx: &Context<Self>) -> Option<MongoQueryDraft> {
        let saved = mongo_collection_state(self.controller.state(), tab_id)?
            .applied_query
            .clone();
        let Some(inputs) = self.mongo_query_inputs.get(&tab_id) else {
            return Some(saved);
        };
        let inputs = inputs.read(cx);
        Some(MongoQueryDraft {
            filter: inputs.filter.read(cx).value().to_string(),
            projection: inputs.projection.read(cx).value().to_string(),
            sort: inputs.sort.read(cx).value().to_string(),
            limit: match inputs.limit.read(cx).value().trim() {
                "" => saved.limit,
                limit => limit.to_string(),
            },
        })
    }
}

fn mongo_collection_state(state: &AppState, tab_id: TabId) -> Option<&MongoCollectionState> {
    state
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .and_then(|tab| match &tab.kind {
            TabKind::MongoCollection(state) => Some(state),
            _ => None,
        })
}

fn tab_exists(state: &AppState, tab_id: TabId) -> bool {
    state.tabs.iter().any(|tab| tab.id == tab_id)
}

fn mongo_load_error() -> fluxdb_core::UserFacingError {
    fluxdb_core::UserFacingError {
        title: tr("app.mongo.error.title").to_string(),
        message: tr("app.mongo.error.no_result").to_string(),
        detail: None,
        retryable: true,
    }
}
