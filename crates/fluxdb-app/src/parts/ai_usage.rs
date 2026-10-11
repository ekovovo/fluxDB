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
            if cancel.is_cancelled() {
                return Ok(vec![fluxdb_core::ModelEvent::Finished(
                    fluxdb_core::FinishReason::Cancelled,
                )]);
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
            let result = self.provider.stream(request, cancel).await;
            let usage = result.as_ref().ok().and_then(|events| {
                events.iter().rev().find_map(|e| {
                    if let fluxdb_core::ModelEvent::Usage(u) = e {
                        Some(*u)
                    } else {
                        None
                    }
                })
            });
            with_ai_storage(self.storage, |conn| {
                fluxdb_storage::ai_platform::usage_ledger::finish_request(
                    conn,
                    ai_chat_now(),
                    &id,
                    usage,
                )
            })?;
            result
        })
    }
}
