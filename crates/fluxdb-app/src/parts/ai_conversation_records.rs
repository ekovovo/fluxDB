/// `ai_results` 行 ID 的稳定约定：产物来源表按它引用真实结果行。
pub(crate) fn ai_result_row_id(turn_id: &str, call_id: &str) -> String {
    format!("{turn_id}-result-{call_id}")
}

/// `ai_tool_calls` 行 ID 的稳定约定。
pub(crate) fn ai_tool_call_row_id(turn_id: &str, call_id: &str) -> String {
    format!("{turn_id}-toolcall-{call_id}")
}

/// 审计行 ID：同一调用在同一阶段只写一行，重复登记按主键冲突暴露而不是静默追加。
fn ai_tool_audit_row_id(call_row: &str, stage: &str) -> String {
    format!("{call_row}-audit-{stage}")
}

/// 把强制审计事实写入 `ai_tool_audits`（append-only）。
///
/// 必须在**已有** `ai_tool_calls` 行的事务里调用：`tool_call_id` 是外键，
/// 审计事实不能先于调用事实存在。写失败按 fail closed 返回错误，调用方不得继续执行。
fn ai_conversation_record_tool_audit(
    conn: &AiConn,
    stage: fluxdb_storage::ai_platform::audits::AuditStage,
    call_row: &str,
    plan_hash: &str,
    risk: &str,
    facts: &AuditFacts,
) -> fluxdb_core::Result<()> {
    use fluxdb_storage::ai_platform::audits::{
        AuditDecision, ToolAuditEdit, insert_tool_audit,
    };
    let decision = match facts.outcome {
        AuditOutcome::Allow => AuditDecision::Allow,
        AuditOutcome::RequireApproval => AuditDecision::RequireApproval,
        AuditOutcome::Deny => AuditDecision::Deny,
    };
    let stage_label = match stage {
        fluxdb_storage::ai_platform::audits::AuditStage::PreExecute => "pre_execute",
        fluxdb_storage::ai_platform::audits::AuditStage::Revalidate => "revalidate",
    };
    let edit = ToolAuditEdit {
        id: ai_tool_audit_row_id(call_row, stage_label),
        tool_call_id: Some(call_row.to_string()),
        mcp_request_id: None,
        stage,
        hook_id: "kernel.security-audit".into(),
        hook_version: "1".into(),
        ruleset_version: facts.ruleset_version.clone(),
        plan_hash: plan_hash.to_string(),
        decision,
        // 风险等级以工具 spec 为准；Hook 不推断风险，缺失时留空而不是编造等级。
        risk: risk.to_string(),
        rule_ids: facts.rule_ids.clone(),
        reason_codes: facts.reason_codes.clone(),
    };
    insert_tool_audit(conn, ai_chat_now(), &edit)
}

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
    call: &fluxdb_core::ToolCallRequest,
    plan_hash: &str,
    tool_version: u32,
    target_json: &str,
    text_offset: usize,
    audit: &AuditFacts,
) -> fluxdb_core::Result<()> {
    let call_row = ai_tool_call_row_id(&job.turn_id, &call.provider_call_id);
    // 审计判定与规则随工具卡一起落片段：时间线不用回查审计表就能显示"这次判了什么"。
    let payload=serde_json::json!({
        "tool":call.tool_id,
        "call_id":call.provider_call_id,
        "plan_hash":plan_hash,
        "text_offset":text_offset,
        "audit_decision":audit.outcome.key(),
        "audit_rule_ids":audit.rule_ids,
        "audit_reason_codes":audit.reason_codes,
    }).to_string();
    with_ai_storage(storage, |conn| {
        // 审批恢复和拒绝回执都会新增事实行，序号必须从落盘状态分配而非内存计数。
        let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))?;
        let conn = &tx;
        let (iteration, ordinal) =
            fluxdb_storage::ai_platform::sessions::next_tool_call_ordinal(conn, &job.turn_id)?;
        fluxdb_storage::ai_platform::sessions::start_tool_call(
            conn,
            ai_chat_now(),
            &call_row,
            &job.session_id,
            &job.turn_id,
            iteration,
            ordinal,
            &call.provider_call_id,
            &call.tool_id,
            tool_version,
            None,
            &ai_conversation_redact_arguments(&call.arguments_json),
            target_json,
            plan_hash,
        )?;
        // 审计事实与调用事实同事务：审计写不出来就整笔失败，不允许"执行了但没记录"。
        ai_conversation_record_tool_audit(
            conn,
            fluxdb_storage::ai_platform::audits::AuditStage::PreExecute,
            &call_row,
            plan_hash,
            &audit.risk,
            audit,
        )?;
        fluxdb_storage::ai_platform::conversation::upsert_event_part_in_transaction(
            &tx,
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
        )?;
        tx.commit().map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))
    })
}

/// 从回执里的 `result_ref` 找该句柄的 spill 事实，转成 `ai_results` 的保留登记。
///
/// 回执不含路径（路径只对模型隐藏），所以这里以回执给出的句柄 ID 回查仓库；
/// 查不到（Redis 等自带分页、未走结果仓库的工具）按纯内存登记。
///
/// `tool_id` 是本次调用的工具：只有句柄的产出工具与它一致，这条 spill 事实才属于本次调用。
/// 否则 `result.read_page` 这类「回显来源句柄」的工具会把来源的 spill 文件登记成自己的落点。
fn spill_retention(
    results: Option<&Arc<Mutex<ResultStore>>>,
    tool_id: &str,
    call_id: &str,
    receipt_json: &str,
) -> Option<fluxdb_storage::ai_platform::sessions::ResultRetention> {
    let store = results?.lock().ok()?;
    let id = serde_json::from_str::<serde_json::Value>(receipt_json)
        .ok()
        .and_then(|value| value.get("result_ref")?.get("id")?.as_str().map(str::to_owned));
    let id = store.reference_for_call(call_id).map(|reference| reference.id).or(id)?;
    let fact = store.spill_fact(&id)?;
    if fact.tool_id != tool_id || store.reference_for_call(call_id).is_none() {
        return None;
    }
    // 仓库锁已被 poison 或句柄已不可查时不能伪造路径：退化成内存登记，
    // 结果仍留在受控文件里，只是重启后按内存语义过期。
    Some(fluxdb_storage::ai_platform::sessions::ResultRetention::Retained {
        relative_path: fact.relative_path,
        byte_count: fact.byte_count,
        content_hash: fact.content_hash,
    })
}

/// 记录工具真实回执：写 `ai_results` 句柄 + 结果卡；回执原文只进 receipt 列，不被投影改写。
fn ai_conversation_record_tool_result(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    receipt: &fluxdb_core::ToolReceipt,
    text_offset: usize,
    results: Option<&Arc<Mutex<ResultStore>>>,
    audit: &AuditFacts,
) -> fluxdb_core::Result<()> {
    let call_row = ai_tool_call_row_id(&job.turn_id, &receipt.call_id);
    let result_row = ai_result_row_id(&job.turn_id, &receipt.call_id);
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
        // 内核拒绝不经过 ToolStarted，判定只在结果路径出现；这里补上，拒绝也可见。
        "audit_decision":audit.outcome.key(),
        "audit_rule_ids":audit.rule_ids,
        "audit_reason_codes":audit.reason_codes,
    })
    .to_string();
    let byte_count = receipt.result_json.len() as i64;
    let snapshot = results.and_then(|store| store.lock().ok()?.reference_for_call(&receipt.call_id));
    let declared_result = snapshot.as_ref().map(|reference| reference.id.clone()).or_else(|| serde_json::from_str::<serde_json::Value>(&receipt.result_json)
        .ok().and_then(|value| value.get("result_ref")?.get("id")?.as_str().map(str::to_owned)));
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
        // 放行路径的审计行已由 ToolStarted 写入；内核拒绝不经过 ToolStarted，
        // 这里补写 pre_execute，保证「拒绝」也有 append-only 判断可查。
        //
        // `plan_hash` 是审计行的必填项：回执里没有它（例如翻页回执、早期来源）时
        // 不补写——审计事实挂不上计划，写一行空 hash 只会伪造"已审计"的假象。
        if !plan_hash.is_empty()
            && !fluxdb_storage::ai_platform::audits::has_tool_audit(
                conn,
                &call_row,
                fluxdb_storage::ai_platform::audits::AuditStage::PreExecute,
            )?
        {
            ai_conversation_record_tool_audit(
                conn,
                fluxdb_storage::ai_platform::audits::AuditStage::PreExecute,
                &call_row,
                &plan_hash,
                &audit.risk,
                audit,
            )?;
        }
        // 拒绝不是"执行结果"：只留调用事实与卡片，不登记结果句柄。
        let result_ref = if receipt.status == fluxdb_core::ToolReceiptStatus::Denied {
            None
        } else {
            // 大结果的 spill 事实只有在结果仓库里才知道：溢写过的句柄按 retained 登记
            // 相对路径与内容哈希，重启后仍可校验/清理；未溢写的仍是纯内存句柄。
            let retention = spill_retention(
                results.as_deref(),
                &receipt.tool_id,
                &receipt.call_id,
                &receipt.result_json,
            )
            .unwrap_or(fluxdb_storage::ai_platform::sessions::ResultRetention::Memory);
            fluxdb_storage::ai_platform::sessions::insert_result(
                conn,
                ai_chat_now(),
                &result_row,
                &job.session_id,
                &job.turn_id,
                &call_row,
                &serde_json::json!({"tool":receipt.tool_id,"call_id":receipt.call_id,"result_ref":declared_result,"scope":"turn","expires_on":"execution_end"}).to_string(),
                "available",
                byte_count,
                &retention,
            )?;
            // spill 是受控临时快照，不承诺跨轮可读；为遗留文件设置清理期限。
            conn.execute(
                "UPDATE ai_results SET expires_at_ms=?2 WHERE id=?1",
                rusqlite::params![result_row, ai_chat_now() + 24 * 60 * 60 * 1000],
            ).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
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
        // 关系候选需要用户逐条决定：把已校验批次投影成可持久化的 notice 卡，
        // 让聊天、重启后的历史都能重新打开审阅弹框。它不进入审批流程，也不触发写入。
        if let Some(proposal_payload) = er_proposal_notice_payload(receipt) {
            let part_id = format!(
                "{}-er-proposals-{}",
                job.assistant_message_id, receipt.call_id
            );
            if fluxdb_storage::ai_platform::sessions::load_part(conn, &part_id).is_err() {
                fluxdb_storage::ai_platform::sessions::append_part(
                    conn,
                    ai_chat_now(),
                    &part_id,
                    &job.assistant_message_id,
                    fluxdb_storage::ai_platform::sessions::PartKind::Notice,
                    fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                    &proposal_payload,
                    None,
                    None,
                    None,
                )?;
            }
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

/// 只投影关系建议结果中的公开候选，不把工具回执原文塞进会话片段。
fn er_proposal_notice_payload(receipt: &fluxdb_core::ToolReceipt) -> Option<String> {
    if receipt.tool_id != "er.propose_relationships"
        || receipt.status != fluxdb_core::ToolReceiptStatus::Succeeded
    {
        return None;
    }
    let result: serde_json::Value = serde_json::from_str(&receipt.result_json).ok()?;
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("er_relationship_proposals") {
        return None;
    }
    let total = result
        .get("counts")
        .and_then(|counts| counts.get("total"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(
        serde_json::json!({
            "kind": "er_relationship_proposals",
            "batch_id": result.get("batch_id"),
            "target": result.get("target"),
            "items": result.get("items"),
            "counts": result.get("counts"),
            "written": false,
            "tool_call_id": receipt.call_id,
            "text": format!("AI 生成 {total} 条关系建议，等待逐条审阅"),
        })
        .to_string(),
    )
}

/// 记录"等待审批"：工具调用事实行（waiting_approval）+ 审批行（pending）+ 可见审批片段。
///
/// 返回 approval_id（UI 用它提交批准/拒绝）。
#[allow(clippy::too_many_arguments)]
fn ai_conversation_record_approval(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    call: &fluxdb_core::ToolCallRequest,
    tool_version: u32,
    plan_hash: &str,
    scope_json: &str,
    details_json: &str,
    target_json: &str,
    text_offset: usize,
    audit: &AuditFacts,
) -> fluxdb_core::Result<String> {
    let call_row = ai_tool_call_row_id(&job.turn_id, &call.provider_call_id);
    let approval_id = format!("{}-approval-{}", job.turn_id, call.provider_call_id);
    with_ai_storage(storage, |conn| {
        // 审批恢复和拒绝回执都会新增事实行，序号必须从落盘状态分配而非内存计数。
        let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))?;
        let conn = &tx;
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
            &call.provider_call_id,
            &call.tool_id,
            tool_version,
            None,
            &ai_conversation_redact_arguments(&call.arguments_json),
            target_json,
            plan_hash,
            "waiting_approval",
        )?;
        // 等待审批同样是审计结论：审批行与审计事实一起落，拒绝/超时后仍可回溯判定。
        ai_conversation_record_tool_audit(
            conn,
            fluxdb_storage::ai_platform::audits::AuditStage::PreExecute,
            &call_row,
            plan_hash,
            &audit.risk,
            audit,
        )?;
        let approval = fluxdb_storage::ai_platform::audits::create_approval(
            conn,
            ai_chat_now(),
            &approval_id,
            &call_row,
            plan_hash,
            scope_json,
            // 人工审批不计时：一直等到用户批准/拒绝，或授权/策略变化使该审批失效。
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
            "audit_decision":audit.outcome.key(),
            "audit_rule_ids":audit.rule_ids,
            "audit_reason_codes":audit.reason_codes,
        }).to_string();
        fluxdb_storage::ai_platform::conversation::upsert_event_part_in_transaction(
            &tx,
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
        )?;
        tx.commit().map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))
    })?;
    Ok(approval_id)
}

#[cfg(test)]
mod er_proposal_notice_tests {
    use super::*;

    fn receipt(tool_id: &str, status: fluxdb_core::ToolReceiptStatus, result: serde_json::Value) -> fluxdb_core::ToolReceipt {
        fluxdb_core::ToolReceipt {
            call_id: "call-1".into(),
            tool_id: tool_id.into(),
            status,
            result_json: result.to_string(),
            error_code: None,
            affected_rows: None,
            started_at_ms: 0,
            finished_at_ms: 1,
        }
    }

    #[test]
    fn proposal_notice_keeps_only_successful_candidate_payload() {
        let result = serde_json::json!({
            "kind": "er_relationship_proposals",
            "batch_id": "batch-1",
            "target": {"connection_id": 1, "database": "sales", "schema": null},
            "items": [],
            "counts": {"total": 2, "ready": 1, "unavailable": 1, "invalid": 0}
        });
        let payload = er_proposal_notice_payload(&receipt(
            "er.propose_relationships",
            fluxdb_core::ToolReceiptStatus::Succeeded,
            result.clone(),
        ))
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
        .expect("proposal notice");

        assert_eq!(payload["kind"], "er_relationship_proposals");
        assert_eq!(payload["written"], false);
        assert_eq!(payload["text"], "AI 生成 2 条关系建议，等待逐条审阅");
        assert!(er_proposal_notice_payload(&receipt(
            "er.propose_relationships",
            fluxdb_core::ToolReceiptStatus::Failed,
            result.clone(),
        ))
        .is_none());
        assert!(er_proposal_notice_payload(&receipt(
            "db.get_relationships",
            fluxdb_core::ToolReceiptStatus::Succeeded,
            result,
        ))
        .is_none());
    }
}
