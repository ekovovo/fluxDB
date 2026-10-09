/// 参数脱敏：保留工具与 SQL 结构，去掉疑似凭据字段后入库（审计可读，秘密不入库）。
fn ai_conversation_redact_arguments(arguments_json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(arguments_json) else {
        return "{}".to_string();
    };
    ai_conversation_redact_value(&mut value);
    value.to_string()
}

/// 递归脱敏：嵌套对象/数组里的疑似凭据同样要删掉，不能只过滤顶层键。
fn ai_conversation_redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.retain(|key, _| !ai_conversation_is_secret_key(key));
            for child in object.values_mut() {
                ai_conversation_redact_value(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                ai_conversation_redact_value(item);
            }
        }
        _ => {}
    }
}

fn ai_conversation_is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

/// 记录"工具开始执行"：写 `ai_tool_calls` 事实行 + 工具卡片段（外键要求先有事实行）。
fn ai_conversation_record_tool_start(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    ordinal: usize,
    call: &fluxdb_core::ToolCallRequest,
    plan_hash: &str,
    tool_version: u32,
    target_json: &str,
    text_offset: usize,
) -> fluxdb_core::Result<()> {
    let call_row = format!("{}-toolcall-{}", job.turn_id, call.provider_call_id);
    let payload=serde_json::json!({"tool":call.tool_id,"call_id":call.provider_call_id,"plan_hash":plan_hash,"text_offset":text_offset}).to_string();
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::start_tool_call(
            conn,
            ai_chat_now(),
            &call_row,
            &job.session_id,
            &job.turn_id,
            ordinal as i64 + 1,
            ordinal as i64,
            &call.provider_call_id,
            &call.tool_id,
            tool_version,
            None,
            &ai_conversation_redact_arguments(&call.arguments_json),
            target_json,
            plan_hash,
        )?;
        fluxdb_storage::ai_platform::conversation::upsert_event_part(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            &format!(
                "{}-tool-{}",
                job.assistant_message_id, call.provider_call_id
            ),
            fluxdb_storage::ai_platform::sessions::PartKind::ToolCallRef,
            fluxdb_storage::ai_platform::sessions::PartStatus::Streaming,
            &payload,
            Some(&call_row),
            None,
        )
    })
}

/// 记录工具真实回执：写 `ai_results` 句柄 + 结果卡；回执原文只进 receipt 列，不被投影改写。
fn ai_conversation_record_tool_result(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    receipt: &fluxdb_core::ToolReceipt,
    text_offset: usize,
) -> fluxdb_core::Result<()> {
    let call_row = format!("{}-toolcall-{}", job.turn_id, receipt.call_id);
    let result_row = format!("{}-result-{}", job.turn_id, receipt.call_id);
    let call_status = match receipt.status {
        fluxdb_core::ToolReceiptStatus::Succeeded => "succeeded",
        fluxdb_core::ToolReceiptStatus::Failed => "failed",
        fluxdb_core::ToolReceiptStatus::Denied => "denied",
        fluxdb_core::ToolReceiptStatus::Cancelled => "cancelled",
        fluxdb_core::ToolReceiptStatus::Unknown => "unknown",
    };
    let part_status = match receipt.status {
        fluxdb_core::ToolReceiptStatus::Succeeded => {
            fluxdb_storage::ai_platform::sessions::PartStatus::Complete
        }
        fluxdb_core::ToolReceiptStatus::Cancelled => {
            fluxdb_storage::ai_platform::sessions::PartStatus::Interrupted
        }
        _ => fluxdb_storage::ai_platform::sessions::PartStatus::Error,
    };
    let payload = serde_json::json!({
        "tool":receipt.tool_id,
        "status":call_status,
        "error_code":receipt.error_code,
        "affected_rows":receipt.affected_rows,
        "duration_ms":(receipt.finished_at_ms - receipt.started_at_ms).max(0),
        "truncated":false,
        "text_offset":text_offset,
    })
    .to_string();
    let byte_count = receipt.result_json.len() as i64;
    let plan_hash = serde_json::from_str::<serde_json::Value>(&receipt.result_json)
        .ok()
        .and_then(|value| {
            value
                .get("plan_hash")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    with_ai_storage(storage, |conn| {
        // 行 ID 可能是运行时约定、也可能来自别的来源：先按 ID 找，再按 (turn, provider_call_id) 找，
        // 都找不到才补行（内核拒绝不经过 ToolStarted，本来就没有事实行）。
        let call_row = if fluxdb_storage::ai_platform::sessions::load_tool_call(conn, &call_row).is_ok() {
            call_row
        } else if let Some(existing) = fluxdb_storage::ai_platform::sessions::find_tool_call_id_by_provider_call(conn, &job.turn_id, &receipt.call_id)? {
            existing
        } else {
            let target = fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)
                .map(|turn| turn.target_json)
                .unwrap_or_else(|_| "{}".into());
            let (iteration, ordinal) =
                fluxdb_storage::ai_platform::sessions::next_tool_call_ordinal(conn, &job.turn_id)?;
            fluxdb_storage::ai_platform::sessions::create_tool_call(
                conn,
                ai_chat_now(),
                &call_row,
                &job.session_id,
                &job.turn_id,
                iteration,
                ordinal,
                &receipt.call_id,
                &receipt.tool_id,
                1,
                None,
                "{}",
                &target,
                &plan_hash,
                "denied",
            )?;
            call_row
        };
        fluxdb_storage::ai_platform::sessions::finish_tool_call(
            conn,
            &call_row,
            call_status,
            Some(&receipt.result_json),
            Some(receipt.started_at_ms),
            Some(receipt.finished_at_ms),
        )?;
        // 拒绝不是"执行结果"：只留调用事实与卡片，不登记结果句柄。
        let result_ref = if receipt.status == fluxdb_core::ToolReceiptStatus::Denied {
            None
        } else {
            fluxdb_storage::ai_platform::sessions::insert_result(
                conn,
                ai_chat_now(),
                &result_row,
                &job.session_id,
                &job.turn_id,
                &call_row,
                &serde_json::json!({"tool":receipt.tool_id,"call_id":receipt.call_id}).to_string(),
                "available",
                byte_count,
            )?;
            Some(result_row.clone())
        };
        // 恢复横幅若还在说"尚未执行"，执行完必须同步更新，不能让 UI 与事实相反。
        let notice = format!("{}-restart-notice", job.turn_id);
        if let Ok(part) = fluxdb_storage::ai_platform::sessions::load_part(conn, &notice) {
            let text = match call_status {
                "succeeded" => "重启后的调用已执行完成，结果见下方结果引用。",
                "failed" => "重启后的调用已执行但失败，详见结果卡与错误码。",
                "unknown" => "重启后的调用已执行，但结果无法确认；请先核对数据库/外部系统。",
                "denied" => "重启后的调用已被拒绝，没有执行。",
                _ => "重启后的调用已处理。",
            };
            fluxdb_storage::ai_platform::sessions::update_part(
                conn,
                ai_chat_now(),
                &notice,
                fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                &serde_json::json!({"text":text,"recovery_status":"resolved"}).to_string(),
                part.revision,
            )?;
        }
        fluxdb_storage::ai_platform::conversation::upsert_event_part(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            &format!("{}-result-{}", job.assistant_message_id, receipt.call_id),
            fluxdb_storage::ai_platform::sessions::PartKind::ToolResultRef,
            part_status,
            &payload,
            Some(&call_row),
            result_ref.as_deref(),
        )
    })
}

/// 记录"等待审批"：工具调用事实行（waiting_approval）+ 审批行（pending）+ 可见审批片段。
///
/// 返回 approval_id（UI 用它提交批准/拒绝）。
#[allow(clippy::too_many_arguments)]
fn ai_conversation_record_approval(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    ordinal: usize,
    call: &fluxdb_core::ToolCallRequest,
    tool_version: u32,
    plan_hash: &str,
    scope_json: &str,
    details_json: &str,
    target_json: &str,
    text_offset: usize,
) -> fluxdb_core::Result<String> {
    let call_row = format!("{}-toolcall-{}", job.turn_id, call.provider_call_id);
    let approval_id = format!("{}-approval-{}", job.turn_id, call.provider_call_id);
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::create_tool_call(
            conn,
            ai_chat_now(),
            &call_row,
            &job.session_id,
            &job.turn_id,
            ordinal as i64 + 1,
            ordinal as i64,
            &call.provider_call_id,
            &call.tool_id,
            tool_version,
            None,
            &ai_conversation_redact_arguments(&call.arguments_json),
            target_json,
            plan_hash,
            "waiting_approval",
        )?;
        let approval = fluxdb_storage::ai_platform::audits::create_approval(
            conn,
            ai_chat_now(),
            &approval_id,
            &call_row,
            plan_hash,
            scope_json,
            None,
        )?;
        let payload=serde_json::json!({
            "text":format!("工具 {} 需要你先批准后才会执行。",call.tool_id),
            "approval_id":approval_id,
            "plan_hash":plan_hash,
            "approval_revision":approval.revision,
            "text_offset":text_offset,
            "scope":serde_json::from_str::<serde_json::Value>(scope_json).unwrap_or(serde_json::Value::Null),
            "details":serde_json::from_str::<serde_json::Value>(details_json).unwrap_or(serde_json::Value::Null),
        }).to_string();
        fluxdb_storage::ai_platform::conversation::upsert_event_part(
            conn,
            ai_chat_now(),
            &job.session_id,
            &job.turn_id,
            &job.assistant_message_id,
            &format!(
                "{}-approval-{}",
                job.assistant_message_id, call.provider_call_id
            ),
            fluxdb_storage::ai_platform::sessions::PartKind::Notice,
            fluxdb_storage::ai_platform::sessions::PartStatus::Pending,
            &payload,
            Some(&call_row),
            None,
        )
    })?;
    Ok(approval_id)
}
