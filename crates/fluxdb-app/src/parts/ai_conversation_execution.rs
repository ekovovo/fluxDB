use fluxdb_core::FinishReason;

/// 终态错误：`message` 给人/UI 看，`code` 落 `ai_turns.error_code` 供诊断聚合。
///
/// 两者都不允许出现凭据、密文或加密元数据；码一律取宿主映射的受控码
/// （如 `http_429`/`timeout`），不把中文文案写进码列。
pub struct TurnError<'a> {
    pub message: &'a str,
    pub code: Option<&'a str>,
}

/// 保存累计正文与公开摘要并返回数据库投影；非终态节流由执行方负责，终态必须保存。
pub fn persist_ai_conversation_event(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    text: &str,
    summary: &str,
    status: Option<fluxdb_storage::ai_platform::sessions::TurnStatus>,
    error: Option<TurnError<'_>>,
) -> fluxdb_core::Result<AiConversationView> {
    with_ai_storage(storage, |conn| {
        checkpoint_ai_conversation(conn, job, text, summary, status, error.as_ref().and_then(|error| error.code))?;
        let mut view = ai_chat_project(fluxdb_storage::ai_platform::conversation::load(
            conn,
            Some(&job.session_id),
        )?);
        view.error = error.map(|error| error.message.to_owned());
        Ok(view)
    })
}

/// 保存正文检查点，不重读/复制整个会话历史；流式热路径只需这个写边界。
///
/// `error_code` 是 Turn 行的受控错误码（不是给人看的文案）。
fn checkpoint_ai_conversation(
    conn: &AiConn,
    job: &AiConversationJob,
    text: &str,
    summary: &str,
    status: Option<fluxdb_storage::ai_platform::sessions::TurnStatus>,
    error_code: Option<&str>,
) -> fluxdb_core::Result<()> {
    fluxdb_storage::ai_platform::conversation::checkpoint(
        conn, ai_chat_now(), &job.session_id, &job.turn_id, &job.assistant_message_id,
        text, summary, status, error_code,
    )
}

include!("ai_conversation_stream.rs");

/// 可替换 source 只决定模型事件，保存与回桥仍经过同一个生产路径。fixture 不伪造数据库结果。
pub fn run_ai_conversation_source(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    source: impl FnOnce(
        &mut dyn FnMut(&fluxdb_core::ModelEvent),
    ) -> fluxdb_core::Result<Vec<fluxdb_core::ModelEvent>>,
) {
    let mut text = String::new();
    let mut summary = String::new();
    let mut saved = std::time::Instant::now();
    let mut finish = None;
    let mut persist_error = None;
    let mut stream = AiConversationStreamPublisher::new(0);
    let mut emit = |event: &fluxdb_core::ModelEvent| {
        match event {
            fluxdb_core::ModelEvent::TextDelta(delta) => text.push_str(delta),
            fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta) => summary.push_str(delta),
            fluxdb_core::ModelEvent::Finished(reason) => finish = Some(*reason),
            _ => {}
        }
        stream.publish(
            sender,
            job,
            &text,
            &summary,
        );
        if saved.elapsed() >= std::time::Duration::from_millis(300) && persist_error.is_none() {
            match persist_ai_conversation_event(storage, job, &text, &summary, None, None) {
                Ok(view) => {
                    let _ =
                        sender.try_publish(AiConversationUpdate::view(job.clone(), view, false));
                }
                Err(e) => persist_error = Some(e.message),
            };
            saved = std::time::Instant::now();
        }
    };
    let response = source(&mut emit);
    drop(emit);
    // 失败事实在错误对象还活着时取：只有文案时码与可重试性无法事后重建。
    let source_error = response.err();
    let persist_failed = persist_error.is_some();
    let error = persist_error
        .clone()
        .or_else(|| source_error.as_ref().map(|error| error.message.clone()))
        .or_else(|| {
            if finish.is_none() && !cancel.is_cancelled() {
                Some("模型连接提前结束，未收到完整结束事件".into())
            } else {
                None
            }
        });
    let failure_fact = match (&source_error, persist_failed) {
        (Some(error), _) => Some(FailureFact::from_error(FailureStage::ModelRequest, error)),
        (None, true) => Some(FailureFact {
            stage: FailureStage::Internal,
            code: "persist_failed".into(),
            retryable: false,
        }),
        (None, false) if error.is_some() => Some(FailureFact {
            stage: FailureStage::ModelRequest,
            code: "incomplete_stream".into(),
            retryable: false,
        }),
        _ => None,
    };
    let status = if cancel.is_cancelled() || finish == Some(fluxdb_core::FinishReason::Cancelled) {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled
    } else if error.is_some() || finish == Some(fluxdb_core::FinishReason::Error) {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
    } else {
        fluxdb_storage::ai_platform::sessions::TurnStatus::Completed
    };
    if finish == Some(fluxdb_core::FinishReason::Length) {
        text.push_str("\n[模型输出达到本轮上限，内容可能不完整。]");
    }
    // 即使没有回复文字也写出可见状态，空响应不能伪装成功。
    if text.is_empty() {
        text = error.clone().unwrap_or_else(|| {
            if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled {
                "已停止后续生成。".into()
            } else {
                "模型未返回正文。".into()
            }
        });
    }
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        &summary,
        Some(status),
        error.as_deref().map(|message| TurnError {
            message,
            code: failure_fact.as_ref().map(|fact| fact.code.as_str()),
        }),
    ) {
        Ok(view) => {
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
        Err(e) => {
            tracing::warn!(turn_id=%job.turn_id,"AI 回复保存失败");
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("回复保存失败：{}", e.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
    }
}

include!("ai_conversation_history.rs");
include!("ai_conversation_context.rs");

/// 保存一条事件片段（工具卡/结果/通知）；失败只记错误，不让持久化问题吞掉真实回执。
fn ai_conversation_save_part(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    part_id: &str,
    kind: fluxdb_storage::ai_platform::sessions::PartKind,
    status: fluxdb_storage::ai_platform::sessions::PartStatus,
    payload_json: &str,
    tool_call_id: Option<&str>,
    result_id: Option<&str>,
) -> fluxdb_core::Result<()> {
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::conversation::upsert_event_part(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            part_id,
            kind,
            status,
            payload_json,
            tool_call_id,
            result_id,
        )
    })
}

/// 工具回合：用 `TurnRunner` 跑完整链路（schema → 审计 → 授权 → 执行 → 有界投影）。
///
/// 只有**真实回执**才允许写成工具结果片段；模型文案不会被执行结果替代或伪造。
pub fn run_ai_conversation_turn(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
) {
    run_ai_conversation_turn_with_summary(
        storage, job, cancel, sender, provider, None, tools, audit, permission,
    );
}

/// 同上，但显式指定摘要 Provider（脚本化测试注入；生产用 `None` 由存储按档案装配）。
#[allow(clippy::too_many_arguments)]
pub fn run_ai_conversation_turn_with_summary(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    provider: &dyn fluxdb_core::GenerationProvider,
    summary_provider: Option<&dyn fluxdb_core::GenerationProvider>,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
) {
    ai_conversation_run_turn(
        storage,
        job,
        cancel,
        sender,
        provider,
        summary_provider,
        tools,
        audit,
        permission,
        Vec::new(),
        String::new(),
        None,
    );
}

/// 跑一轮工具回合的公共实现；`extra_messages`/`initial_text` 供"审批后恢复"续写同一条助手消息。
#[allow(clippy::too_many_arguments)]
fn ai_conversation_run_turn(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    provider: &dyn fluxdb_core::GenerationProvider,
    summary_provider: Option<&dyn fluxdb_core::GenerationProvider>,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
    extra_messages: Vec<fluxdb_core::ModelMessage>,
    initial_text: String,
    runtime: Option<&RuntimeTurn>,
) {
    let context = match ai_conversation_turn_messages(storage, job) {
        Ok(value) => value,
        Err(error) => {
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(error.message);
            view.session_id = Some(job.session_id.clone());
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
            return;
        }
    };

    let AiTurnContext {
        system_messages,
        target_json,
        history,
        previous_summary,
        next_generation,
        context_window_tokens,
        max_output_tokens,
        trigger_percent,
        max_iterations,
        compaction_model,
        context_references,
    } = context;

    // Jev 是可选的相关性建议：只在拿到建议时调整引用呈现顺序，拿不到就保持原样。
    // 这条链路不参与任何权限判断，失败也不会让本轮失败。
    let system_messages = ai_conversation_reference_message(
        storage,
        job,
        system_messages,
        &context_references,
        cancel,
    );
    // —— 按模型窗口百分比/输出预留触发，自动压缩（无需用户确认）。 ——
    // 摘要不可用/失败时：仍在窗口内带提示继续；已超预算则可见暂停，不发出超窗请求。
    let system_tokens = estimate_messages_tokens(&system_messages)
        + estimate_tools_tokens(&loop_tool_descriptors(tools));
    let history = match ai_conversation_maybe_compact(
        storage,
        job,
        sender,
        provider,
        summary_provider,
        cancel,
        history,
        system_tokens,
        previous_summary,
        next_generation,
        context_window_tokens,
        max_output_tokens,
        trigger_percent,
        &compaction_model,
    ) {
        CompactionOutcome::Ready(history) => history,
        CompactionOutcome::Paused(reason) => {
            ai_conversation_pause_for_compaction(storage, job, sender, reason);
            return;
        }
    };

    let mut messages = system_messages;
    messages.extend(history.into_iter().map(|(_, message)| message));
    messages.extend(extra_messages);
    let mut text = initial_text;
    let mut summary = String::new();
    let mut saved = std::time::Instant::now();
    let mut persist_error: Option<String> = None;
    let mut paused_reason: Option<String> = None;
    let mut notice_count: usize = 0;
    let mut approval_id: Option<String> = None;
    let mut part_error: Option<String> = None;
    // 本轮非成功工具回执轨迹：同工具后续成功即视为模型已自愈，不再在回合末尾重复提示。
    struct ToolFailure {
        tool_id: String,
        code: String,
        recovered: bool,
    }
    let mut tool_failures: Vec<ToolFailure> = Vec::new();

    let metered_provider = MeteredProvider {
        provider,
        storage,
        job,
        model_profile_id: &job.model_profile_id,
        purpose: "main",
    };
    let runner = TurnRunner {
        provider: &metered_provider,
        tools,
        limits: LoopLimits {
            max_iterations,
            ..LoopLimits::default()
        },
        permission,
        audit,
        progress: sender.tool_progress_source(job),
    };
    let target_json_for_parts = target_json.clone();
    let state = TurnState {
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        model: job.model_profile_id.clone(),
        messages,
        max_output_tokens,
        target_json,
    };
    let mut stream = AiConversationStreamPublisher::new(text.len());
    let mut summary_checkpoint_len = summary.len();
    // 「生命周期诊断」：只在插件启用时累计阶段耗时/错误；关闭时为全空操作，
    // 不影响任何执行事实或审计落盘。
    let telemetry_enabled =
        with_ai_storage(storage, |conn| lifecycle_telemetry_enabled(conn, &job.session_id))
            .unwrap_or(false);
    let mut diagnostics = TurnDiagnostics::new(telemetry_enabled);
    let outcome = runner.run_with_steering(
        state,
        cancel,
        &mut || runtime.map(RuntimeTurn::take_steering).unwrap_or_default(),
        &mut |event| {
            let force = !matches!(
                &event,
                LoopEvent::TextDelta(_) | LoopEvent::ReasoningSummaryDelta(_)
                    | LoopEvent::StageTiming { .. } | LoopEvent::ToolCallRequested(_)
            );
            match event {
                LoopEvent::TextDelta(delta) => text.push_str(&delta),
                LoopEvent::ReasoningSummaryDelta(delta) => summary.push_str(&delta),
                // 仅表示"模型打算调用"：还没校验/审计/授权，不写任何持久化事实。
                LoopEvent::ToolCallRequested(_call) => {}
                // 工具开始执行：此时已通过 schema 校验、审计与授权，才写下事实行与工具卡。
                LoopEvent::ToolStarted {
                    call,
                    plan_hash,
                    tool_version,
                    audit,
                } => {
                    let saved_part = ai_conversation_record_tool_start(
                        storage,
                        job,
                        &call,
                        &plan_hash,
                        tool_version,
                        &target_json_for_parts,
                        text.len(),
                        &audit,
                    );
                    if let Err(error) = &saved_part {
                        part_error.get_or_insert_with(|| error.message.clone());
                    }
                }
                // 工具真实回执：写 ai_tool_calls/ai_results 事实行 + 结果卡。
                LoopEvent::ToolFinished { receipt, audit } => {
                    if receipt.is_success() {
                        for failure in tool_failures
                            .iter_mut()
                            .filter(|failure| !failure.recovered && failure.tool_id == receipt.tool_id)
                        {
                            failure.recovered = true;
                        }
                    } else {
                        tool_failures.push(ToolFailure {
                            tool_id: receipt.tool_id.clone(),
                            code: receipt
                                .error_code
                                .clone()
                                .unwrap_or_else(|| "unknown".into()),
                            recovered: false,
                        });
                    }
                    let saved =
                        ai_conversation_record_tool_result(
                            storage,
                            job,
                            &receipt,
                            text.len(),
                            tools.results(),
                            &audit,
                        );
                    if let Err(error) = &saved {
                        part_error.get_or_insert_with(|| error.message.clone());
                    }
                }
                LoopEvent::Notice(notice) => {
                    let payload =
                        serde_json::json!({"text":notice,"text_offset":text.len()}).to_string();
                    let saved = ai_conversation_save_part(
                        storage,
                        job,
                        &format!("{}-notice-{notice_count}", job.assistant_message_id),
                        fluxdb_storage::ai_platform::sessions::PartKind::Notice,
                        fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                        &payload,
                        None,
                        None,
                    );
                    if let Err(error) = &saved {
                        part_error.get_or_insert_with(|| error.message.clone());
                    }
                    notice_count += 1;
                }
                // 同一助手消息只有一份公开计划：固定 ID 复用 upsert，
                // 更新载荷与 revision，不新增卡片或改变首次出现的位置。
                LoopEvent::PlanUpdated { plan } => {
                    let payload = crate::plan_part_payload(&plan);
                    let saved = ai_conversation_save_part(
                        storage,
                        job,
                        &format!("{}-plan", job.assistant_message_id),
                        fluxdb_storage::ai_platform::sessions::PartKind::Plan,
                        fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                        &payload,
                        None,
                        None,
                    );
                    if let Err(error) = &saved {
                        part_error.get_or_insert_with(|| error.message.clone());
                    }
                }
                // 需要审批：写工具调用事实行（waiting_approval）+ 审批行（pending）+ 可见审批片段。
                LoopEvent::ApprovalRequired {
                    call,
                    tool_version,
                    plan_hash,
                    scope,
                    details,
                    audit,
                } => {
                    let saved = ai_conversation_record_approval(
                        storage,
                        job,
                        &call,
                        tool_version,
                        &plan_hash,
                        &scope.to_json(),
                        &serde_json::to_string(&details).unwrap_or_default(),
                        &target_json_for_parts,
                        text.len(),
                        &audit,
                    );
                    match saved {
                        Ok(id) => approval_id = Some(id),
                        Err(error) => {
                            part_error.get_or_insert_with(|| error.message.clone());
                        }
                    }
                }
                LoopEvent::IterationStarted { .. } => {}
                // 阶段耗时：累计进本轮诊断并即时落一条阶段事件；关闭插件时为空操作。
                LoopEvent::StageTiming {
                    stage,
                    iteration,
                    tool_id,
                    duration_ms,
                    attempts,
                    ok,
                    error_code,
                    retryable,
                } => {
                    diagnostics.record_stage(
                        storage,
                        &job.turn_id,
                        StageFact {
                            stage,
                            iteration,
                            tool_id,
                            duration_ms,
                            attempts,
                            ok,
                            error_code,
                            retryable,
                        },
                    );
                }
            }
            stream.publish(sender, job, &text, &summary);
            // 操作边界发布权威快照；正文期间只保存低频检查点，不重复加载整段历史。
            if (force || saved.elapsed() >= std::time::Duration::from_millis(300))
                && persist_error.is_none()
            {
                // 公开摘要沿用已有折叠卡的低频投影；纯正文不重读历史。
                let result = if force || summary.len() != summary_checkpoint_len {
                    persist_ai_conversation_event(storage, job, &text, &summary, None, None)
                        .map(|view| { let _ = sender.try_publish(AiConversationUpdate::view(job.clone(), view, false)); })
                } else {
                    with_ai_storage(storage, |conn| checkpoint_ai_conversation(conn, job, &text, &summary, None, None))
                };
                if let Err(error) = result { persist_error = Some(error.message); }
                summary_checkpoint_len = summary.len();
                saved = std::time::Instant::now();
            }
        },
    );

    let mut truncated = false;
    // 失败事实在错误对象还活着时取；受控码与可重试性事后无法从文案重建。
    let mut failure_fact: Option<FailureFact> = None;
    let status = match outcome {
        TurnOutcome::Completed { truncated: cut, .. } => {
            truncated = cut;
            fluxdb_storage::ai_platform::sessions::TurnStatus::Completed
        }
        TurnOutcome::Cancelled => fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled,
        TurnOutcome::Failed { stage, error } => {
            failure_fact = Some(FailureFact::from_error(stage, &error));
            persist_error.get_or_insert(error.message);
            fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
        }
        TurnOutcome::Paused { reason } => {
            paused_reason = Some(reason);
            // 只有真的产生了待审批项才算 waiting_approval；达到迭代上限等属于 waiting_user。
            if approval_id.is_some() {
                fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval
            } else {
                fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingUser
            }
        }
    };
    diagnostics.record_failure(failure_fact.clone());
    // 轮末汇总：一轮结束后落一条，供排查看"慢在哪/错在哪"。写失败只告警。
    diagnostics.persist(storage, &job.turn_id, ai_chat_now(), status.as_str());

    let error = persist_error.clone().or(part_error.clone());
    let error_code = failure_fact.as_ref().map(|fact| fact.code.clone());
    // 失败原因必须在会话里留下可重读的片段：只在正文为空时写进正文，会让
    // "有正文的失败轮"在重新打开会话后看不到任何原因。正文已承载原因时不重复写。
    if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Failed
        && !text.is_empty()
        && let Some(message) = error.as_deref()
    {
        let _ = ai_conversation_save_part(
            storage,
            job,
            &format!("{}-notice-failure", job.assistant_message_id),
            fluxdb_storage::ai_platform::sessions::PartKind::Notice,
            fluxdb_storage::ai_platform::sessions::PartStatus::Error,
            &serde_json::json!({"text":message,"text_offset":text.len()}).to_string(),
            None,
            None,
        );
    }
    if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Completed {
        // 失败的工具卡本身已写在发生位置；这里只兜底"没人再成功的那些"，
        // 且聚合为一条，避免同一失败既在内联卡上又在回答末尾重复出现。
        let unrecovered: Vec<&ToolFailure> = tool_failures
            .iter()
            .filter(|failure| !failure.recovered)
            .collect();
        if !unrecovered.is_empty() {
            let message = match unrecovered.as_slice() {
                [failure] => format!("工具 {} 未成功：{}", failure.tool_id, failure.code),
                failures => format!(
                    "本轮 {} 项工具调用未成功：{}",
                    failures.len(),
                    failures
                        .iter()
                        .map(|failure| format!("{}({})", failure.tool_id, failure.code))
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            };
            if let Err(save_error) = ai_conversation_save_part(
                storage,
                job,
                &format!("{}-notice-tool-failure", job.assistant_message_id),
                fluxdb_storage::ai_platform::sessions::PartKind::Notice,
                fluxdb_storage::ai_platform::sessions::PartStatus::Error,
                &serde_json::json!({"text":message,"text_offset":text.len()}).to_string(),
                None,
                None,
            ) {
                persist_error.get_or_insert(save_error.message);
            }
        }
    }
    // 审批卡已表达暂停原因，不再追加一条重复的 pending 提示。
    if let Some(reason) = paused_reason.as_ref().filter(|_| approval_id.is_none()) {
        let _ = ai_conversation_save_part(
            storage,
            job,
            &format!("{}-notice-paused", job.assistant_message_id),
            fluxdb_storage::ai_platform::sessions::PartKind::Notice,
            fluxdb_storage::ai_platform::sessions::PartStatus::Pending,
            &serde_json::json!({"text":reason,"text_offset":text.len()}).to_string(),
            None,
            None,
        );
    }
    if text.is_empty() {
        text = error.clone().unwrap_or_else(|| match status {
            fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled => {
                "已停止后续生成。".into()
            }
            fluxdb_storage::ai_platform::sessions::TurnStatus::WaitingApproval => {
                paused_reason.clone().unwrap_or_else(|| "等待审批。".into())
            }
            // 推理模型在输出上限内把额度全部用于思考时，正文一个字都不会产生；
            // 这种"长度截断但无正文"必须提示调大输出上限，不能笼统说成空响应。
            _ if truncated => {
                "模型在输出上限内未产出正文（额度可能全部用于推理）。请调大该模型的最大输出后重试。"
                    .into()
            }
            _ => "模型未返回正文。".into(),
        });
    }
    // 首轮成功后生成标题；放在最终保存前，标题结果在终端事件前落库。
    if status == fluxdb_storage::ai_platform::sessions::TurnStatus::Completed && !cancel.is_cancelled() {
        generate_session_title(storage, job, provider, &text);
    }
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        &summary,
        Some(status),
        error
            .as_deref()
            .map(|message| TurnError { message, code: error_code.as_deref() }),
    ) {
        Ok(view) => {
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
        Err(save_error) => {
            tracing::warn!(turn_id=%job.turn_id,"AI 回复保存失败");
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("回复保存失败：{}", save_error.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
    }
}

/// 明确失败：在会话里留下可见错误并结束该 Turn（不能静默无响应，也不能伪造成功）。
pub fn run_ai_conversation_failure(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    sender: &dyn ConversationEventSink,
    stage: FailureStage,
    error: fluxdb_core::Error,
) {
    let fact = FailureFact::from_error(stage, &error);
    let message = error.message;
    let text = format!("本轮未能开始：{message}");
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        "",
        Some(fluxdb_storage::ai_platform::sessions::TurnStatus::Failed),
        Some(TurnError {
            message: &message,
            code: Some(&fact.code),
        }),
    ) {
        Ok(view) => {
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
        Err(error) => {
            let mut view = with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))
            })
            .map(ai_chat_project)
            .unwrap_or_default();
            view.error = Some(format!("{message}（保存也失败：{}）", error.message));
            view.session_id = Some(job.session_id.clone());
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
    }
}

/// 压缩无法在预算内完成：本轮以可见说明结束，不发出超窗请求，也不删除任何历史。
///
/// 用终态失败（而不是 `waiting_user`）：当前会话对有进行中的 Turn 会拒绝新消息且没有恢复入口，
/// 置为等待会把这个会话锁死；终态下用户可调整输入/模型后直接重发，旧 checkpoint 与原始历史仍在。
fn ai_conversation_pause_for_compaction(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    sender: &dyn ConversationEventSink,
    reason: String,
) {
    // 暂停原因已经由压缩 notice 承载，这里只写可见的两份：notice + 助手正文。
    let text = format!("本轮已暂停：{reason}");
    match persist_ai_conversation_event(
        storage,
        job,
        &text,
        "",
        Some(fluxdb_storage::ai_platform::sessions::TurnStatus::Failed),
        Some(TurnError {
            message: &reason,
            code: Some("compaction_paused"),
        }),
    ) {
        Ok(view) => {
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
        Err(error) => {
            tracing::warn!(turn_id=%job.turn_id, "压缩暂停状态保存失败");
            run_ai_conversation_failure(
                storage,
                job,
                sender,
                FailureStage::Compaction,
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("{reason}（暂停状态保存也失败：{}）", error.message),
                ),
            );
        }
    }
}

/// 读取当前助手消息已保存的正文（恢复续写时不能覆盖已展示内容）。
fn ai_conversation_existing_text(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> String {
    with_ai_storage(storage, |conn| {
        let data = fluxdb_storage::ai_platform::conversation::load(conn, Some(&job.session_id))?;
        Ok(ai_chat_project(data)
            .messages
            .into_iter()
            .find(|m| m.id == job.assistant_message_id)
            .and_then(|m| m.parts.into_iter().find(|p| p.kind == "text"))
            .and_then(|p| serde_json::from_str::<serde_json::Value>(&p.payload_json).ok())
            .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))
            .unwrap_or_default())
    })
    .unwrap_or_default()
}

/// 审批通过后恢复执行：按 plan_hash 复核批准（fail closed）→ 执行该工具 → 把真实回执交给模型继续本轮。
///
/// 批准只解除"需要审批"，**不放宽** schema 校验与连接/内核权限：参数会重新校验，工具仍走同一实现。
#[allow(clippy::too_many_arguments)]
pub fn run_ai_conversation_resume_approval(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
    approval_id: &str,
) {
    resume_ai_approved_turn(
        storage,
        job,
        cancel,
        sender,
        provider,
        tools,
        audit,
        permission,
        approval_id,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn resume_ai_approved_turn(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    cancel: &fluxdb_core::CancelFlag,
    sender: &dyn ConversationEventSink,
    provider: &dyn fluxdb_core::GenerationProvider,
    tools: &ToolCatalog,
    audit: &dyn SecurityAuditHook,
    permission: TurnPermissionContext,
    approval_id: &str,
    runtime: Option<&RuntimeTurn>,
) {
    // 关闭时不认领批准、不执行工具；checkpoint 只收尾原有任务。
    if cancel.is_cancelled() {
        cancel_ai_conversation(storage, job, sender);
        return;
    }
    let loaded = claim_ai_approved_call(storage, job, tools, audit, &permission, approval_id);
    let (call, message_id, audit_facts) = match loaded {
        Ok(value) => value,
        Err(error) => {
            run_ai_conversation_failure(
                storage,
                job,
                sender,
                FailureStage::ApprovalResume,
                error,
            );
            return;
        }
    };
    let job = AiConversationJob {
        session_id: job.session_id.clone(),
        turn_id: call.turn_id.clone(),
        assistant_message_id: message_id,
        model_profile_id: job.model_profile_id.clone(),
    };

    let prepared = match tools.prepare(&call.tool_id, &call.arguments_redacted_json) {
        Ok(prepared) => prepared,
        Err(error) => {
            run_ai_conversation_failure(
                storage,
                &job,
                sender,
                FailureStage::ApprovalResume,
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("批准后的参数未通过校验：{}", error.message),
                ),
            );
            return;
        }
    };
    let Some(tool) = tools.executable(&call.tool_id) else {
        run_ai_conversation_failure(
            storage,
            &job,
            sender,
            FailureStage::ApprovalResume,
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Unsupported,
                format!("工具 {} 没有可执行实现", call.tool_id),
            ),
        );
        return;
    };
    if cancel.is_cancelled() {
        let saved = with_ai_storage(storage, |conn| {
            fluxdb_storage::ai_platform::sessions::finish_tool_call(
                conn,
                &call.id,
                "cancelled",
                None,
                None,
                Some(ai_chat_now()),
            )
        });
        match saved {
            Ok(()) => cancel_ai_conversation(storage, &job, sender),
            Err(error) => {
                run_ai_conversation_failure(
                    storage,
                    &job,
                    sender,
                    FailureStage::ApprovalResume,
                    error,
                )
            }
        }
        return;
    }
    let ctx = fluxdb_core::ToolExecutionContext {
        automatic_approval: false,
        bounded_write: false,
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        iteration: 1,
        call_id: call.provider_call_id.clone(),
        progress: None,
        cancel: cancel.clone(),
    };
    let receipt = match crate::ai_executor::block_on_cancellable(
        tool.execute(ctx, prepared.to_json_string()),
        cancel,
    ) {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(error)) | Err(error) => {
            run_ai_conversation_failure(
                storage,
                &job,
                sender,
                FailureStage::ApprovalResume,
                fluxdb_core::Error::new(
                    error.kind,
                    format!("工具执行失败：{}", error.message),
                ),
            );
            return;
        }
    };
    let existing = ai_conversation_existing_text(storage, &job);
    if let Err(error) = ai_conversation_record_tool_result(
        storage,
        &job,
        &receipt,
        existing.len(),
        tools.results(),
        &audit_facts,
    ) {
        // 工具已经真实执行，但回执没能落盘：绝不能让调用停在 running 假装"还在跑"。
        // 先尽力标记 unknown（T029 按未知副作用处理），再把错误显式交给用户。
        tracing::warn!(turn_id=%job.turn_id, error=%error.message, "工具回执保存失败");
        let _ = with_ai_storage(storage, |conn| {
            fluxdb_storage::ai_platform::sessions::finish_tool_call(
                conn,
                &ai_tool_call_row_id(&job.turn_id, &receipt.call_id),
                "unknown",
                Some(&receipt.result_json),
                Some(receipt.started_at_ms),
                Some(receipt.finished_at_ms),
            )
        });
        run_ai_conversation_failure(
            storage,
            &job,
            sender,
            FailureStage::ApprovalResume,
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!(
                    "工具已执行但回执保存失败，已标记为结果未知：{}",
                    error.message
                ),
            ),
        );
        return;
    }

    let message = fluxdb_core::ModelMessage::new(
        fluxdb_core::ModelRole::Tool,
        format!(
            "{{\"tool\":\"{}\",\"status\":\"{}\",\"result\":{}}}",
            receipt.tool_id,
            if receipt.is_success() {
                "succeeded"
            } else {
                "failed"
            },
            if receipt.result_json.trim().is_empty() {
                "null".to_string()
            } else {
                receipt.result_json.clone()
            }
        ),
    );
    ai_conversation_run_turn(
        storage,
        &job,
        cancel,
        sender,
        provider,
        None,
        tools,
        audit,
        permission,
        vec![message],
        existing,
        runtime,
    );
}

/// 未开始执行的任务只收尾持久化状态；已存在的正文与摘要不被取消提示覆盖。
fn cancel_ai_conversation(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    sender: &dyn ConversationEventSink,
) {
    let text = ai_conversation_existing_text(storage, job);
    let text = if text.is_empty() {
        "已停止后续生成。"
    } else {
        &text
    };
    match persist_ai_conversation_event(
        storage,
        job,
        text,
        "",
        Some(fluxdb_storage::ai_platform::sessions::TurnStatus::Cancelled),
        None,
    ) {
        Ok(view) => {
            let _ = sender.publish_terminal(AiConversationUpdate::view(job.clone(), view, true));
        }
        Err(error) => run_ai_conversation_failure(
            storage,
            job,
            sender,
            FailureStage::Recovery,
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("取消状态保存失败：{}", error.message),
            ),
        ),
    }
}
