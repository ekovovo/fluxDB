// 仅用于内存中来源变化检测，不作为认证/安全哈希，不落盘、不写日志。
fn ai_catalog_source_fingerprint(dialog: &AiModelDialog, cx: &App) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(
        &dialog.inputs.provider.read(cx).selected_value(),
        &mut hasher,
    );
    std::hash::Hash::hash(&ai_input_text(&dialog.inputs.endpoint, cx), &mut hasher);
    std::hash::Hash::hash(
        &dialog.inputs.api_key.read(cx).value().as_ref(),
        &mut hasher,
    );
    std::hash::Hasher::finish(&hasher)
}

// 模型目录请求只传一次性凭据；迟到结果不能覆盖换 Provider/Endpoint 后的新草稿。
impl NavicatMain {
    fn invalidate_ai_model_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ai_model_dialog.as_mut() {
            let fingerprint = ai_catalog_source_fingerprint(dialog, cx);
            if dialog.catalog_source_fingerprint == Some(fingerprint) {
                return;
            }
            dialog.catalog_source_fingerprint = Some(fingerprint);
            dialog.fetch_generation = dialog.fetch_generation.wrapping_add(1);
            dialog.fetching_models = false;
            dialog.model_options.clear();
            dialog.model_capabilities.clear();
            dialog.model_options_open = false;
        }
        self.apply_ai_model_capacity(window, cx);
    }

    fn fetch_ai_model_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.ai_model_dialog.as_mut() else {
            return;
        };
        if dialog.fetching_models || dialog.endpoint_is_full_url {
            return;
        }
        // 与保存同理：下拉的值是展示名，取列表前必须换算成稳定标识，
// 否则会用 OpenAI Chat 的协议去打 Anthropic/Responses 地址。
        let provider = match dialog.inputs.provider.read(cx).selected_value().cloned() {
            Some(label) => match fluxdb_app::ModelProvider::from_label(&label) {
                Some(provider) => provider.stored_id().to_string(),
                None => {
                    self.show_message(
                        "Provider 取值无效，请重新选择",
                        AppMessageKind::Warning,
                        cx,
                    );
                    return;
                }
            },
            None => fluxdb_app::ModelProvider::OpenAiChat.stored_id().to_string(),
        };
        let endpoint = ai_input_text(&dialog.inputs.endpoint, cx);
        let api_key = dialog.inputs.api_key.read(cx).value().to_string();
        if api_key.trim().is_empty() {
            self.show_message(
                tr("app.ai.model_dialog.api_key_required"),
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        dialog.fetch_generation = dialog.fetch_generation.wrapping_add(1);
        let source_fingerprint = ai_catalog_source_fingerprint(dialog, cx);
        dialog.catalog_source_fingerprint = Some(source_fingerprint);
        let generation = dialog.fetch_generation;
        let instance = dialog.inputs.model_key.entity_id();
        dialog.fetching_models = true;
        dialog.model_options_open = false;
        cx.notify();
        let mut controller = self.controller.clone();
        cx.spawn_in(window, async move |view, cx| {
            let event = cx
                .background_spawn(async move {
                    controller.dispatch(AppCommand::FetchAiModels {
                        provider,
                        endpoint: (!endpoint.is_empty()).then_some(endpoint),
                        endpoint_is_full_url: false,
                        api_key,
                    })
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                let Some(dialog) = this.ai_model_dialog.as_mut() else {
                    return;
                };
                if dialog.inputs.model_key.entity_id() != instance
                    || dialog.fetch_generation != generation
                    || ai_catalog_source_fingerprint(dialog, cx) != source_fingerprint
                {
                    return;
                }
                dialog.fetching_models = false;
                match event {
                    AppEvent::AiModelsFetched {
                        models,
                        capabilities,
                    } => {
                        dialog.model_options = models;
                        dialog.model_capabilities = capabilities;
                        dialog.model_options_open = true;
                        this.apply_ai_model_capacity(window, cx);
                    }
                    AppEvent::AiModelsFetchFailed { message } => {
                        this.show_message(message, AppMessageKind::Error, cx)
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
}
