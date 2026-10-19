impl NavicatMain {
    fn start_data_page_load_if_needed(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self._data_load_tasks.contains_key(&tab_id.0) {
            return;
        }

        let Some(tab) = self
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .cloned()
        else {
            return;
        };

        let TabKind::DataEditor(editor) = tab.kind else {
            return;
        };
        if !editor.loading || editor.page.is_some() {
            return;
        }

        let sort = self.data_sort_specs_for_tab(tab_id);
        let filters = self.data_filter_specs_for_tab(tab_id);
        let task = self.spawn_controller_command(
            AppCommand::LoadDataPageWithSort {
                tab_id,
                sort,
                filters,
            },
            move |this, event, cx| {
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
                this._data_load_tasks.remove(&tab_id.0);
                if this
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .any(|tab| tab.id == tab_id)
                {
                    let event = this
                        .controller
                        .dispatch(AppCommand::FinishDataPageLoad { tab_id, result });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._data_load_tasks.insert(tab_id.0, task);
    }

    fn start_table_info_load_if_needed(
        &mut self,
        tab_id: TabId,
        info_tab: TableInfoTab,
        cx: &mut Context<Self>,
    ) {
        if info_tab == TableInfoTab::Columns {
            return;
        }
        let key = (tab_id, info_tab);
        if self._table_info_tasks.contains_key(&key) {
            return;
        }
        if !self.table_info_tab_is_loading(tab_id, info_tab) {
            return;
        }

        let task = self.spawn_controller_command(
            AppCommand::LoadTableInfo {
                tab_id,
                tab: info_tab,
            },
            move |this, event, cx| {
                let result = match event {
                    AppEvent::TableInfoLoaded(_, _, result) => Ok(result),
                    AppEvent::Failed(error) => Err(error),
                    _ => Err(fluxdb_core::UserFacingError {
                        title: tr("app.data.error.load").to_string(),
                        message: tr("app.data.error.no_table_info_result").to_string(),
                        detail: None,
                        retryable: true,
                    }),
                };
                this._table_info_tasks.remove(&(tab_id, info_tab));
                if this
                    .controller
                    .state()
                    .tabs
                    .iter()
                    .any(|tab| tab.id == tab_id)
                {
                    let event = this.controller.dispatch(AppCommand::FinishTableInfoLoad {
                        tab_id,
                        tab: info_tab,
                        result,
                    });
                    this.apply_app_event(&event, cx);
                }
            },
            cx,
        );
        self._table_info_tasks.insert(key, task);
    }

    fn table_info_tab_is_loading(&self, tab_id: TabId, info_tab: TableInfoTab) -> bool {
        self.controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.kind {
                TabKind::DataEditor(editor) => Some(match info_tab {
                    TableInfoTab::Columns => false,
                    TableInfoTab::Indexes => {
                        matches!(editor.table_info.indexes, LoadState::Loading)
                    }
                    TableInfoTab::ForeignKeys => {
                        matches!(editor.table_info.foreign_keys, LoadState::Loading)
                    }
                    TableInfoTab::Triggers => {
                        matches!(editor.table_info.triggers, LoadState::Loading)
                    }
                    TableInfoTab::Ddl => matches!(editor.table_info.ddl, LoadState::Loading),
                }),
                _ => None,
            })
            .unwrap_or(false)
    }
}
