/// 统一模型宿主：每次主模型/摘要调用均先预留，异常/usage 缺失明确 unknown。
struct MeteredProvider<'a> {
    provider: &'a dyn fluxdb_core::GenerationProvider,
    storage: &'a fluxdb_storage::FileStorage,
    job: &'a AiConversationJob,
    purpose: &'static str,
}
impl fluxdb_core::GenerationProvider for MeteredProvider<'_> {
    fn capabilities(&self)->fluxdb_core::ModelCapabilities {self.provider.capabilities()}
    fn stream(&self,request:fluxdb_core::ModelRequest,cancel:fluxdb_core::CancelFlag)->fluxdb_core::AiFuture<'_,Vec<fluxdb_core::ModelEvent>> {
        Box::pin(async move {
            if cancel.is_cancelled() {return Ok(vec![fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Cancelled)]);}
            let id=uuid::Uuid::new_v4().to_string();
            let descriptor=fluxdb_storage::ai_platform::usage_ledger::UsageRequest {
                request_id:id.clone(),session_id:self.job.session_id.clone(),turn_id:self.job.turn_id.clone(),model_profile_id:self.job.model_profile_id.clone(),purpose:self.purpose.into(),
                estimated_input_tokens:estimate_messages_tokens(&request.messages)+estimate_tokens(&serde_json::to_string(&request.tools).unwrap_or_default()),
                max_output_tokens:request.max_output_tokens.unwrap_or(1024),
                title:None,
            };
            with_ai_storage(self.storage,|conn|fluxdb_storage::ai_platform::usage_ledger::reserve(conn,ai_chat_now(),&descriptor))?;
            let result=self.provider.stream(request,cancel).await;
            let usage=result.as_ref().ok().and_then(|events|events.iter().rev().find_map(|e|if let fluxdb_core::ModelEvent::Usage(u)=e {Some(*u)}else{None}));
            with_ai_storage(self.storage,|conn|fluxdb_storage::ai_platform::usage_ledger::settle(conn,ai_chat_now(),&id,usage))?;
            result
        })
    }
}
