impl NavicatMain {
    fn set_data_filter_mode(
        &mut self,
        tab_id: TabId,
        mode: DataFilterMode,
        dialect: sql_editor_adapter::SqlDialect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .data_filter_modes
            .get(&tab_id)
            .copied()
            .unwrap_or(DataFilterMode::Builder)
            == mode
        {
            return;
        }
        let dialect_kind = self.data_editor_database_kind(tab_id);
        if mode == DataFilterMode::Text
            && fluxdb_core::filter_dialect_for(dialect_kind).is_none()
        {
            self.show_message(
                tr("app.filter.input_unsupported").to_string(),
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        if mode == DataFilterMode::Text {
            // 表单是最新编辑来源：切到输入时先按表单生成条件文本。
            self.sync_data_filter_builder_to_text(tab_id);
        } else if let Err(error) = self.sync_data_filter_text_to_builder(tab_id) {
            self.show_message(error, AppMessageKind::Warning, cx);
            return;
        }
        self.data_filter_modes.insert(tab_id, mode);
        if mode == DataFilterMode::Text {
            let fields = self
                .controller
                .state()
                .tabs
                .iter()
                .find_map(|tab| match &tab.kind {
                    TabKind::DataEditor(editor) if tab.id == tab_id => editor.page.as_ref(),
                    _ => None,
                })
                .map(|page| {
                    page.columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.configure_data_filter_condition_completion(
                tab_id,
                fields,
                dialect,
                dialect_kind,
                window,
                cx,
            );
        }
        self.data_filter_popover = None;
        self.sync_table_hover_overlay_block(cx);
        self.sync_data_filter_text_inputs(tab_id, window, cx);
        cx.notify();
    }

    fn configure_data_filter_condition_completion(
        &mut self,
        tab_id: TabId,
        fields: Vec<String>,
        dialect: sql_editor_adapter::SqlDialect,
        database_kind: DatabaseKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let signature = data_filter_completion_signature(&fields, dialect, database_kind);
        if self
            .data_filter_condition_completion_signatures
            .get(&tab_id)
            .is_some_and(|current| current == &signature)
        {
            return;
        }
        self.data_filter_text_input.update(cx, |editor, cx| {
            configure_data_filter_condition_completion(
                editor,
                window,
                cx,
                fields,
                dialect,
                database_kind,
            );
        });
        self.data_filter_condition_completion_signatures
            .insert(tab_id, signature);
    }

    fn update_data_filter_condition_text(
        &mut self,
        value: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab_id) = self.active_data_filter_tab_id() {
            if value.trim().is_empty() {
                self.data_filter_texts.remove(&tab_id);
            } else {
                self.data_filter_texts.insert(tab_id, value);
            }
        }
        cx.notify();
    }

    fn update_data_sort_text(&mut self, value: String, cx: &mut Context<Self>) {
        if let Some(tab_id) = self.active_data_filter_tab_id() {
            if value.trim().is_empty() {
                self.data_sort_texts.remove(&tab_id);
            } else {
                self.data_sort_texts.insert(tab_id, value);
            }
        }
        cx.notify();
    }

    fn update_data_editor_sql_text(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_data_filter_tab_id() else {
            cx.notify();
            return;
        };
        let Some(parsed) = parse_data_editor_sql_text(&value) else {
            cx.notify();
            return;
        };

        if parsed.filter_text.is_empty() {
            self.data_filter_texts.remove(&tab_id);
            self.data_filter_draft_rules.remove(&tab_id);
        } else {
            if let Some(rules) = parse_data_filter_rules_text(&parsed.filter_text) {
                self.data_filter_draft_rules.insert(tab_id, rules);
            }
            self.data_filter_texts.insert(tab_id, parsed.filter_text);
        }

        if parsed.sort_text.is_empty() {
            self.data_sort_texts.remove(&tab_id);
            self.data_sort_draft_rules.remove(&tab_id);
        } else {
            if let Some(rules) = parse_data_sort_rules_text(&parsed.sort_text) {
                self.data_sort_draft_rules.insert(tab_id, rules);
            }
            self.data_sort_texts.insert(tab_id, parsed.sort_text);
        }
        cx.notify();
    }

    fn start_footer_sql_selection(
        &mut self,
        text: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let offset = sql_text_selection_offset(
            text,
            self.data_sql_footer_selection.bounds.as_ref(),
            position,
        );
        self.data_sql_footer_selection.anchor = offset;
        self.data_sql_footer_selection.cursor = offset;
        self.data_sql_footer_selection.selecting = true;
        cx.notify();
    }

    fn select_all_footer_sql(&mut self, text: &str, cx: &mut Context<Self>) {
        self.data_sql_footer_selection.anchor = 0;
        self.data_sql_footer_selection.cursor = text.len();
        self.data_sql_footer_selection.selecting = false;
        cx.notify();
    }

    fn update_footer_sql_selection(
        &mut self,
        text: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.data_sql_footer_selection.selecting {
            return;
        }
        self.data_sql_footer_selection.cursor = sql_text_selection_offset(
            text,
            self.data_sql_footer_selection.bounds.as_ref(),
            position,
        );
        cx.notify();
    }

    fn finish_footer_sql_selection(&mut self, cx: &mut Context<Self>) {
        self.data_sql_footer_selection.selecting = false;
        cx.notify();
    }

    fn sync_data_filter_builder_to_text(&mut self, tab_id: TabId) {
        let db_kind = self.data_editor_database_kind(tab_id);
        let text = self
            .data_filter_draft_rules
            .get(&tab_id)
            .map(|rules| data_filter_rules_sql_pretty(rules, db_kind))
            .unwrap_or_default();
        if text.is_empty() {
            self.data_filter_texts.remove(&tab_id);
        } else {
            self.data_filter_texts.insert(tab_id, text);
        }

        if let Some(sort_text) = self
            .data_sort_draft_rules
            .get(&tab_id)
            .map(|rules| data_sort_rules_text(rules, db_kind))
            .filter(|text| !text.is_empty())
        {
            self.data_sort_texts.insert(tab_id, sort_text);
        } else {
            self.data_sort_texts.remove(&tab_id);
        }
    }

    /// 输入切回表单时，优先使用结构化解析结果回填；不能无损拆分的子树
    /// 保留为只读高级条件行，避免切换模式改变用户条件。
    fn sync_data_filter_text_to_builder(&mut self, tab_id: TabId) -> Result<(), String> {
        let Some(dialect) = fluxdb_core::filter_dialect_for(self.data_editor_database_kind(tab_id))
        else {
            return Err(tr("app.filter.input_unsupported").to_string());
        };
        let text = self
            .data_filter_texts
            .get(&tab_id)
            .map(String::as_str)
            .unwrap_or_default()
            .trim();
        if text.is_empty() {
            self.data_filter_draft_rules.remove(&tab_id);
            return Ok(());
        }
        let parsed = dialect
            .parse(text)
            .map_err(|error| error.to_string())?;
        let projection = fluxdb_core::filter_form_projection_from_expression(
            &parsed.expression,
            |expression| dialect.render(expression).unwrap_or_default(),
        );
        let rules = projection
            .rules
            .into_iter()
            .map(|rule| {
                let advanced_expression = rule
                    .field
                    .is_none()
                    .then(|| rule.values.first().cloned())
                    .flatten()
                    .map(|value| fluxdb_core::filter_value_text(&value));
                DataFilterRule {
                    enabled: rule.enabled,
                    field: rule.field,
                    operator: data_filter_operator_from_core(rule.operator),
                    typed_values: Some(rule.values.clone()),
                    values: rule
                        .values
                        .iter()
                        .map(fluxdb_core::filter_value_text)
                        .collect(),
                    advanced_expression,
                    connector: projection.combinator,
                    grouped: rule.grouped,
                }
            })
            .collect::<Vec<_>>();
        self.data_filter_draft_rules
            .insert(tab_id, rules);
        Ok(())
    }

    /// 输入模式解析失败时返回错误，由应用动作显示，不退回旧条件。
    fn data_filter_rules_from_text(&self, tab_id: TabId) -> Option<Vec<DataFilterRule>> {
        match self.data_filter_texts.get(&tab_id) {
            Some(text) if !text.trim().is_empty() => parse_data_filter_rules_text(text),
            _ => Some(Vec::new()),
        }
    }

    fn sync_data_filter_text_inputs(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter_text = self
            .data_filter_texts
            .get(&tab_id)
            .cloned()
            .unwrap_or_default();
        let input_focused = self
            .data_filter_text_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !input_focused && self.data_filter_text_input.read(cx).value().as_ref() != filter_text {
            self.data_filter_text_input.update(cx, |input, cx| {
                input.set_value(filter_text, window, cx);
            });
        }

        let sort_text = self
            .data_sort_texts
            .get(&tab_id)
            .cloned()
            .unwrap_or_default();
        if self.data_sort_text_input.read(cx).value().as_ref() != sort_text {
            self.data_sort_text_input.update(cx, |input, cx| {
                input.set_value(sort_text, window, cx);
            });
        }
    }

}
