//! 仅在宿主启动、尚未启动任何 Agent 任务时扫描。只修改本地事实，不连接数据库或调用模型。
use super::sessions::{self, PartKind, PartStatus};
use crate::sqlite::sqlite_error;
use fluxdb_core::Result;
use rusqlite::{Connection, TransactionBehavior, params};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecoverySummary {
    pub interrupted_turns: usize,
    pub unknown_turns: usize,
    pub waiting_approval_turns: usize,
    pub expired_results: usize,
}

/// 无执行回执的 running 不代表失败或取消；读取中断，写/未知工具保守标 unknown。
/// prepared/waiting_approval 尚未执行；approved 也不是提交证明，必须重新审批。
pub fn recover_on_startup(conn: &mut Connection, now: i64) -> Result<RecoverySummary> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let mut result = RecoverySummary::default();
    let mut stmt = tx.prepare("SELECT id,session_id,status FROM ai_turns WHERE status IN ('queued','preparing','generating','executing_tools','cancel_requested','waiting_approval','waiting_user','interrupted','outcome_unknown') ORDER BY started_at_ms,id").map_err(sqlite_error)?;
    let turns = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(stmt);
    for (id, session, old_status) in turns {
        let mut stmt = tx
            .prepare("SELECT id,tool_id,status,receipt_json FROM ai_tool_calls WHERE turn_id=?1")
            .map_err(sqlite_error)?;
        let calls = stmt
            .query_map([&id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        drop(stmt);
        let mut unknown = old_status == "outcome_unknown";
        for (call, tool, status, _receipt) in calls {
            let interrupted_status = if status == "running" {
                // 不依据 SQL 前缀断言只读；只有已实现的专用只读工具可确认为读取中断。
                if matches!(
                    tool.as_str(),
                    "db.query" | "db.search_objects" | "db.explain"
                ) {
                    "cancelled"
                } else {
                    "unknown"
                }
            } else if status == "prepared" {
                "cancelled"
            } else {
                status.as_str()
            };
            unknown |= interrupted_status == "unknown";
            if interrupted_status != status {
                tx.execute(
                    "UPDATE ai_tool_calls SET status=?2,finished_at_ms=?3 WHERE id=?1",
                    params![call, interrupted_status, now],
                )
                .map_err(sqlite_error)?;
            }
            // 片段的 interrupted 只表示流停止；执行结果仍须显示 unknown，不能误写成“已取消”。
            tx.execute("UPDATE ai_message_parts SET payload_json=json_set(payload_json,'$.execution_status',?2),revision=revision+1,updated_at_ms=?3 WHERE tool_call_id=?1 AND kind='tool_call_ref' AND COALESCE(json_extract(payload_json,'$.execution_status'),'')<>?2", params![call,interrupted_status,now]).map_err(sqlite_error)?;
        }
        // 历史批准绝不能在重启后直接变成执行许可，未执行调用重新进入 pending（revision 使旧 UI 失效）。
        tx.execute("UPDATE ai_approvals SET status='pending',reason_code='restart_reapproval',decided_at_ms=NULL,revision=revision+1 WHERE status='approved' AND tool_call_id IN (SELECT id FROM ai_tool_calls WHERE turn_id=?1 AND status='waiting_approval') AND (expires_at_ms IS NULL OR expires_at_ms>?2)", params![id,now]).map_err(sqlite_error)?;
        tx.execute("UPDATE ai_approvals SET status='expired',reason_code='restart_expired',decided_at_ms=?2,revision=revision+1 WHERE status IN ('pending','approved') AND expires_at_ms<=?2 AND tool_call_id IN (SELECT id FROM ai_tool_calls WHERE turn_id=?1)", params![id,now]).map_err(sqlite_error)?;
        let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_approvals a JOIN ai_tool_calls c ON c.id=a.tool_call_id WHERE c.turn_id=?1 AND c.status='waiting_approval' AND a.status='pending')", [&id], |r| r.get(0)).map_err(sqlite_error)?;
        let status = if unknown {
            "outcome_unknown"
        } else if pending {
            "waiting_approval"
        } else if old_status == "waiting_user" {
            "waiting_user"
        } else {
            "interrupted"
        };
        let text = match status {
            "outcome_unknown" => {
                "应用已重启：有工具调用未收到执行回执，实际结果未知。不会自动重放；请先核对数据库/外部系统，未核实前禁止重复写入。"
            }
            "waiting_approval" => {
                "应用已重启：此调用尚未执行。需重新核对目标、参数和权限再审批；历史批准不会自动执行。"
            }
            "waiting_user" => "应用已重启：任务等待补充信息，没有自动继续或重放工具。",
            _ => {
                "应用已重启：上次任务已中断，保存的内容和草稿已恢复。没有自动重发请求或工具；可明确发起新的任务。"
            }
        };
        if status != old_status {
            tx.execute("UPDATE ai_turns SET status=?2,error_code=?3,revision=revision+1,finished_at_ms=CASE WHEN ?2 IN ('interrupted','outcome_unknown') THEN ?4 ELSE finished_at_ms END WHERE id=?1",params![id,status,if unknown {"restart_outcome_unknown"}else{"restart_interrupted"},now]).map_err(sqlite_error)?;
        }
        tx.execute("UPDATE ai_message_parts SET status='interrupted',revision=revision+1,updated_at_ms=?2 WHERE message_id IN (SELECT id FROM ai_messages WHERE turn_id=?1) AND status IN ('pending','streaming') AND json_extract(payload_json,'$.approval_id') IS NULL", params![id,now]).map_err(sqlite_error)?;
        // 待审批卡从审批事实刷新，避免仍持有旧 revision / 已过期的按钮。
        let mut stmt=tx.prepare("SELECT p.id,p.payload_json,p.revision,p.status FROM ai_message_parts p JOIN ai_messages m ON m.id=p.message_id WHERE m.turn_id=?1 AND p.kind='notice' AND json_extract(p.payload_json,'$.approval_id') IS NOT NULL").map_err(sqlite_error)?;
        let cards = stmt
            .query_map([&id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        drop(stmt);
        for (part, raw, revision, old_card_status) in cards {
            let mut payload: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
            })?;
            if let Some(approval_id) = payload.get("approval_id").and_then(|v| v.as_str()) {
                let mut approval = super::audits::load_approval(&tx, approval_id)?;
                // 卡片已明确决定/结束，但事实仍 pending 属不一致状态：fail closed，不重新开放按钮。
                // 唯一例外是本次扫描把历史批准明确退回 pending，必须让用户重新审批。
                if approval.status == super::audits::ApprovalStatus::Pending
                    && approval.reason_code.as_deref() != Some("restart_reapproval")
                    && (payload.get("decision").is_some() || old_card_status == "complete")
                {
                    approval = super::audits::invalidate_approval(
                        &tx,
                        now,
                        approval_id,
                        "restart_card_conflict",
                    )?;
                }
                payload["approval_revision"] = serde_json::json!(approval.revision);
                let card_status =
                    if !unknown && approval.status == super::audits::ApprovalStatus::Pending {
                        if approval.reason_code.as_deref() == Some("restart_reapproval") {
                            payload["text"] = serde_json::json!(
                                "上次批准已失效，请重新核对目标、参数和权限后决定是否执行。"
                            );
                        }
                        payload.as_object_mut().unwrap().remove("decision");
                        PartStatus::Pending
                    } else {
                        payload["decision"] = serde_json::json!(approval.status.as_str());
                        PartStatus::Interrupted
                    };
                if payload.to_string() != raw || card_status.as_str() != old_card_status {
                    sessions::update_part(
                        &tx,
                        now,
                        &part,
                        card_status,
                        &payload.to_string(),
                        revision,
                    )?;
                }
            }
        }
        let message = if let Some(message) = sessions::find_assistant_message_for_turn(&tx, &id)? {
            message
        } else {
            let message = format!("{id}-recovery-message");
            let seq:i64=tx.query_row("UPDATE ai_sessions SET next_message_seq=next_message_seq+1 WHERE id=?1 RETURNING next_message_seq-1",[&session],|r|r.get(0)).map_err(sqlite_error)?;
            tx.execute("INSERT INTO ai_messages(id,session_id,turn_id,seq,role,persistence_kind,created_at_ms) VALUES(?1,?2,?3,?4,'assistant','final',?5)",params![message,session,id,seq,now]).map_err(sqlite_error)?;
            message
        };
        let part = format!("{id}-restart-notice");
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_message_parts WHERE id=?1)",
                [&part],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let payload = serde_json::json!({"text":text,"recovery_status":status}).to_string();
        if exists {
            let (old, revision): (String, i64) = tx
                .query_row(
                    "SELECT payload_json,revision FROM ai_message_parts WHERE id=?1",
                    [&part],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(sqlite_error)?;
            if old != payload {
                sessions::update_part(&tx, now, &part, PartStatus::Complete, &payload, revision)?;
            }
        } else {
            sessions::append_part(
                &tx,
                now,
                &part,
                &message,
                PartKind::Notice,
                PartStatus::Complete,
                &serde_json::json!({"text":text,"recovery_status":status}).to_string(),
                None,
                None,
                None,
            )?;
        }
        match status {
            "outcome_unknown" => result.unknown_turns += 1,
            "waiting_approval" => result.waiting_approval_turns += 1,
            _ => result.interrupted_turns += 1,
        };
    }
    result.expired_results=tx.execute("UPDATE ai_results SET status='expired' WHERE retention_kind='memory' AND status='available'",[]).map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(result)
}
