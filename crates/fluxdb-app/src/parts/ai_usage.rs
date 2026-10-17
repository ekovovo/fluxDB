/// 统一模型宿主：每次模型调用均登记 token 请求，异常/usage 缺失明确 unknown。
struct MeteredProvider<'a> {
    provider: &'a dyn fluxdb_core::GenerationProvider,
    storage: &'a fluxdb_storage::FileStorage,
    job: &'a AiConversationJob,
    /// 本次请求实际使用的模型档案：摘要可用独立模型，不能一律记成主模型。
    model_profile_id: &'a str,
    purpose: &'static str,
}
impl fluxdb_core::GenerationProvider for MeteredProvider<'_> {
    fn capabilities(&self) -> fluxdb_core::ModelCapabilities {
        self.provider.capabilities()
    }
    fn stream(
        &self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            let mut events = Vec::new();
            self.stream_events(request, cancel, &mut |event| events.push(event.clone()))
                .await?;
            Ok(events)
        })
    }

    fn stream_events<'a>(
        &'a self,
        request: fluxdb_core::ModelRequest,
        cancel: fluxdb_core::CancelFlag,
        emit: &'a mut (dyn FnMut(&fluxdb_core::ModelEvent) + Send),
    ) -> fluxdb_core::AiFuture<'a, ()> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                emit(&fluxdb_core::ModelEvent::Finished(
                    fluxdb_core::FinishReason::Cancelled,
                ));
                return Ok(());
            }
            let model = with_ai_storage(self.storage, |conn| {
                fluxdb_storage::ai_platform::models::load_model(conn, self.model_profile_id)
            })?;
            let tools_tokens = estimate_tools_tokens(&request.tools);
            if exceeds_input_budget(
                estimate_messages_tokens(&request.messages) + tools_tokens,
                CompactionTrigger {
                    context_window_tokens: model.context_window_tokens,
                    max_output_tokens: request.max_output_tokens,
                    trigger_percent: 95,
                },
            ) {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "累计上下文（含工具定义与回执）超出模型输入预算，未发送请求；请缩小读取范围或调整上下文",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let descriptor = fluxdb_storage::ai_platform::usage_ledger::UsageRequest {
                request_id: id.clone(),
                session_id: self.job.session_id.clone(),
                turn_id: self.job.turn_id.clone(),
                model_profile_id: self.model_profile_id.to_string(),
                purpose: self.purpose.into(),
                title: None,
            };
            if !with_ai_storage(self.storage, |conn| {
                fluxdb_storage::ai_platform::usage_ledger::start_request(
                    conn,
                    ai_chat_now(),
                    &descriptor,
                )
            })? {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "重复模型请求，拒绝再次发送",
                ));
            }
            // future 在取消/超时中被宿主丢弃时，也要把 pending 用量结算为 unknown。
            let mut settlement = UsageSettlement {
                storage: self.storage,
                request_id: id,
                usage: None,
                settled: false,
            };
            let result = self
                .provider
                .stream_events(request, cancel, &mut |event| {
                    if let fluxdb_core::ModelEvent::Usage(usage) = event {
                        settlement.usage = Some(*usage);
                    }
                    emit(event);
                })
                .await;
            settlement.finish()?;
            result
        })
    }
}

struct UsageSettlement<'a> {
    storage: &'a fluxdb_storage::FileStorage,
    request_id: String,
    usage: Option<fluxdb_core::Usage>,
    settled: bool,
}

impl UsageSettlement<'_> {
    fn finish(&mut self) -> fluxdb_core::Result<()> {
        with_ai_storage(self.storage, |conn| {
            fluxdb_storage::ai_platform::usage_ledger::finish_request(
                conn,
                ai_chat_now(),
                &self.request_id,
                self.usage,
            )
        })?;
        self.settled = true;
        Ok(())
    }
}

impl Drop for UsageSettlement<'_> {
    fn drop(&mut self) {
        if !self.settled && self.finish().is_err() {
            tracing::warn!(request_id=%self.request_id, "模型用量结算失败");
        }
    }
}
