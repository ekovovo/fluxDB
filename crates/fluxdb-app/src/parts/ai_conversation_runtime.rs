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
                    if let Err(error) = purge_expired_result_files(storage) {
                        tracing::warn!(%error, "临时结果清理失败，继续准备工具");
                    }
                    // T050：先清可判定的无引用文件（失败产物快照 + 未登记孤儿），再统计当前占用。
                    // 清理失败只告警，不阻断本轮任务；`ready` 产物（供报告读取）一律保留。
                    if let Err(error) = purge_failed_artifact_files(storage) {
                        tracing::warn!(%error, "失败产物快照清理失败，继续准备工具");
                    }
                    if let Err(error) = purge_orphan_controlled_files(storage) {
                        tracing::warn!(%error, "受控孤儿文件清理失败，继续准备工具");
                    }
                    match ai_storage_usage(storage) {
                        Ok(usage) => tracing::debug!(
                            artifact_files = usage.artifact_files,
                            artifact_bytes = usage.artifact_bytes,
                            result_files = usage.result_files,
                            result_bytes = usage.result_bytes,
                            "AI 受控存储用量"
                        ),
                        Err(error) => tracing::warn!(%error, "AI 受控存储用量统计失败"),
                    }
                    let provider =
                        AiConversationProvider::from_storage(storage, &job.model_profile_id)?;
                    let security =
                        ai_turn_security_context(storage, &job.turn_id)?;
                    let connection_id = security.connection_id;
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
                    // 结果仓库按 Turn 绑定归属：ResultRef 只能被本 session/turn 读取；
                    // spill 写在存储根的受控子目录，重启后由恢复流程按保留策略处理。
                    let results = Arc::new(Mutex::new(ResultStore::for_turn(
                        RESULT_STORE_MEMORY_BYTES,
                        &job.session_id,
                        &job.turn_id,
                        Some(storage.root_dir().join("ai-results")),
                    )));
                    let mut tools = if pinned
                        .as_ref()
                        .is_some_and(|config| config.kind == fluxdb_core::DatabaseKind::Redis)
                    {
                        standard_redis_catalog(resolver, include_write)?
                    } else {
                        standard_sql_catalog_with_results(
                            resolver,
                            std::sync::Arc::clone(&results),
                            include_write,
                        )?
                    };
                    tools.add_tool(Box::new(AiConnectionsListTool::new(
                        &connections,
                        pinned.as_ref().map(|config| config.id),
                    )))?;
                    // 全局停用的工具不进目录：模型既发现不到，也无法绕过发现直接调用。
                    tools = tools.with_hooks(ai_builtin_hooks(storage, job)?);
                    add_ai_file_tools(&mut tools, storage, job)?;
                    add_ai_reference_tools(&mut tools, storage, job)?;
                    // 通用产物工具始终装配；来源兼容性由实际产物能力校验。
                    add_ai_artifact_tools(
                        &mut tools,
                        storage,
                        job,
                        std::sync::Arc::clone(&results),
                    )?;
                    // 编辑器全文由宿主在发送时登记，取走即释放：不写运行快照，
                    // 本轮结束后工具目录随之消失（设计 §13.2 不持久化整份内容）。
                    add_ai_editor_read_tools(&mut tools, take_editor_grant(&job.turn_id))?;
                    add_ai_page_tools(&mut tools, take_page_data_grant(&job.turn_id), job)?;
                    if tools.results().is_none() {
                        let spec = result_read_page_spec()?;
                        tools = tools.with_results(std::sync::Arc::clone(&results));
                        tools.add_tool_with_shape(
                            Box::new(AiResultReadPageTool::new(std::sync::Arc::clone(&results))?),
                            spec.output_shape,
                        )?;
                    }
                    tools.without_tools(&security.disabled_tools);
                    // 审批模型与本轮 Agent 用的是同一个模型：Jev 决策不可用时由它回退审批。
                    tools=tools.with_execution_security(ExecutionSecurity::with_connections(storage.clone(),connections.clone(),job.model_profile_id.clone()));
                    let permission = TurnPermissionContext {
                        agent_allowed_tools: security.agent_allowed_tools,
                        disabled_tools: security.disabled_tools,
                        policy: kernel_policy_for_connection(pinned.as_ref()),
                        plugin_digest: None,
                    };
                    Ok::<_, fluxdb_core::Error>((provider, tools, permission))
                })();
                // 规则集在每轮开始时读一次：规则变更作用于下一个调用边界。
                // 读不出来就 fail closed（审计不可用 = 本轮回绝所有工具），不按"没有规则"放行。
                let assembled = assembled.and_then(|(provider, tools, permission)| {
                    let audit = ConfiguredAuditHook::from_storage(storage)?;
                    Ok((provider, tools, permission, audit))
                });
                match assembled {
                    Ok((provider, tools, permission, audit)) => {
                        match resume_approval.as_deref() {
                            Some(approval_id) => resume_ai_approved_turn(
                                storage,
                                job,
                                &turn.cancel,
                                sender,
                                &provider,
                                &tools,
                                &audit,
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
                                None,
                                &tools,
                                &audit,
                                permission,
                                Vec::new(),
                                String::new(),
                                Some(turn),
                            ),
                        }
                    }
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
                clear_current_page_grants(&job.turn_id);
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
