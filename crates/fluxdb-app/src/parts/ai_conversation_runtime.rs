impl AiRuntimeHandle {
    /// 提交已持久化的会话任务；配置、凭据与工具目录只在 actor 线程读取/装配。
    pub fn submit_conversation(
        &self,
        storage: fluxdb_storage::FileStorage,
        job: AiConversationJob,
        connections: Vec<fluxdb_core::ConnectionConfig>,
        resume_approval: Option<String>,
        cancel: CancelFlag,
    ) -> Result<ConversationEventReceiver, RuntimeError> {
        self.submit_conversation_execution(
            storage,
            job,
            cancel,
            move |storage, job, turn, sender| {
                if turn.cancel.is_cancelled() {
                    cancel_ai_conversation(storage, job, sender);
                    return;
                }
                let assembled = (|| {
                    let provider =
                        AiConversationProvider::from_storage(storage, &job.model_profile_id)?;
                    let (connection_id, agent_allowed_tools) =
                        ai_turn_security_context(storage, &job.turn_id)?;
                    let pinned = connection_id.and_then(|id| {
                        connections
                            .iter()
                            .find(|config| config.id.0 as i64 == id)
                            .cloned()
                    });
                    let include_write = pinned
                        .as_ref()
                        .is_some_and(|config| !connection_is_read_only(config));
                    let resolver = Arc::new(MapSqlResolver::new(pinned.iter().cloned()));
                    let mut tools = if pinned
                        .as_ref()
                        .is_some_and(|config| config.kind == fluxdb_core::DatabaseKind::Redis)
                    {
                        standard_redis_catalog(resolver, include_write)?
                    } else {
                        standard_sql_catalog(resolver, include_write)?
                    };
                    tools = tools.with_hooks(ai_builtin_hooks(storage, job)?);
                    add_ai_file_tools(&mut tools, storage, job)?;
                    let permission = TurnPermissionContext {
                        agent_allowed_tools,
                        policy: kernel_policy_for_connection(pinned.as_ref()),
                        plugin_digest: None,
                        target_dialect: pinned.as_ref().and_then(|config| dialect_for(config.kind)),
                    };
                    Ok::<_, fluxdb_core::Error>((provider, tools, permission))
                })();
                match assembled {
                    Ok((provider, tools, permission)) => match resume_approval.as_deref() {
                        Some(approval_id) => resume_ai_approved_turn(
                            storage,
                            job,
                            &turn.cancel,
                            sender,
                            &provider,
                            &tools,
                            &KernelAuditHook,
                            permission,
                            approval_id,
                            Some(turn),
                        ),
                        None => ai_conversation_run_turn(
                            storage,
                            job,
                            &turn.cancel,
                            sender,
                            &provider,
                            &tools,
                            &KernelAuditHook,
                            permission,
                            Vec::new(),
                            String::new(),
                            Some(turn),
                        ),
                    },
                    Err(error) => run_ai_conversation_failure(storage, job, sender, error.message),
                }
            },
        )
    }

    /// 注入模型事件来源；仍由同一 actor 调度并经过正式 checkpoint 与终态保存。
    pub fn submit_conversation_source(
        &self,
        storage: fluxdb_storage::FileStorage,
        job: AiConversationJob,
        cancel: CancelFlag,
        source: impl FnOnce(
            &CancelFlag,
            &mut dyn FnMut(&fluxdb_core::ModelEvent),
        ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>
        + Send
        + 'static,
    ) -> Result<ConversationEventReceiver, RuntimeError> {
        self.submit_conversation_execution(
            storage,
            job,
            cancel,
            move |storage, job, turn, sender| {
                run_ai_conversation_source(storage, job, &turn.cancel, sender, |emit| {
                    if turn.cancel.is_cancelled() {
                        emit(&fluxdb_core::ModelEvent::Finished(FinishReason::Cancelled));
                        Ok(Vec::new())
                    } else {
                        source(&turn.cancel, emit)
                    }
                });
            },
        )
    }

    fn submit_conversation_execution(
        &self,
        storage: fluxdb_storage::FileStorage,
        job: AiConversationJob,
        cancel: CancelFlag,
        execute: impl FnOnce(
            &fluxdb_storage::FileStorage,
            &AiConversationJob,
            &RuntimeTurn,
            &dyn ConversationEventSink,
        ) + Send
        + 'static,
    ) -> Result<ConversationEventReceiver, RuntimeError> {
        let (sender, receiver) = conversation_event_channel();
        self.submit(
            job.turn_id.clone(),
            cancel,
            Box::new(move |turn| {
                // 异常也必须交付终态；不把 panic 载荷拼入公开错误或附加日志。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&storage, &job, turn, &sender)
                }));
                if result.is_err() {
                    tracing::warn!(turn_id=%job.turn_id, "AI 任务异常，正在收尾");
                    run_ai_conversation_failure(
                        &storage,
                        &job,
                        &sender,
                        "AI 任务异常结束，请核对工具执行状态".into(),
                    );
                }
            }),
        )?;
        Ok(receiver)
    }
}
