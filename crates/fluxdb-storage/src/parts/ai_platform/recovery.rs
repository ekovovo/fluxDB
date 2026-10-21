//! 仅在宿主启动、尚未启动任何 Agent 任务时扫描。只修改本地事实，不连接数据库或调用模型。
use super::sessions::{self, PartKind, PartStatus};
use crate::sqlite::sqlite_error;
use fluxdb_core::Result;
use rusqlite::{Connection, TransactionBehavior, params};

/// 已取消的旧 SQL 兼容 ID 及其统一入口。迁移只保留用户真实的“启用/停用”
/// 意图，不再把这些旧 ID 留在 Agent 白名单或全局开关里。
const RETIRED_SQL_TOOL_IDS: &[&str] = &["db.query", "db.execute", "db.explain"];

/// staged 产物候选：文件核对与状态推进由调用方在事务外完成（这里只给出事实）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedArtifactCandidate {
    pub id: String,
    pub relative_path: String,
    pub byte_count: i64,
    pub content_hash: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecoverySummary {
    pub interrupted_turns: usize,
    pub unknown_turns: usize,
    pub waiting_approval_turns: usize,
    pub expired_results: usize,
    /// 本次过期、需要调用方在受控根下删除的保留结果落点。
    pub retained_paths_to_purge: Vec<String>,
    /// staged 产物候选：调用方核对落点文件后提升 ready 或置 failed。
    pub staged_artifacts: Vec<StagedArtifactCandidate>,
}

/// 无执行回执的 running 不代表失败或取消；读取中断，写/未知工具保守标 unknown。
/// prepared/waiting_approval 尚未执行；approved 也不是提交证明，必须重新审批。
pub fn recover_on_startup(conn: &mut Connection, now: i64) -> Result<RecoverySummary> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    // 启动时没有活跃供应商请求；遗留 pending 用量不可知，但不再占并发名额。
    tx.execute("UPDATE ai_usage_ledger SET measurement='unknown',updated_at_ms=?1 WHERE measurement='pending'", [now])
        .map_err(sqlite_error)?;
    let mut result = RecoverySummary::default();
    migrate_retired_sql_tools(&tx, now)?;
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
                if matches!(tool.as_str(), "db.search_objects" | "db.describe_object") {
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
    // T050：崩溃在 finalize 中间会留下 staged 产物 + 已写文件。把「等待核对」的候选收集起来，
    // 由调用方核对文件字节/指纹（文件系统操作不能在事务里做）后决定提升 ready 还是标 failed。
    let mut staged_stmt = tx
        .prepare(
            "SELECT id, relative_path, byte_count, content_hash FROM ai_artifacts
              WHERE status='staged' AND relative_path IS NOT NULL
              ORDER BY updated_at_ms, id",
        )
        .map_err(sqlite_error)?;
    result.staged_artifacts = staged_stmt
        .query_map([], |row| {
            Ok(StagedArtifactCandidate {
                id: row.get(0)?,
                relative_path: row.get(1)?,
                byte_count: row.get(2)?,
                content_hash: row.get(3)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(staged_stmt);
    result.expired_results=tx.execute("UPDATE ai_results SET status='expired' WHERE retention_kind='memory' AND status='available'",[]).map_err(sqlite_error)?;
    // 保留结果按自身 TTL 过期；未设 TTL 的长期保留不因重启失效。
    // 过期前先取回落点：文件删除属于文件系统操作，SQLite 事务里做不得，
    // 由调用方拿这些相对路径在受控根下清理。
    result.retained_paths_to_purge = expired_retained_paths(&tx, now)?;
    result.expired_results+=tx.execute("UPDATE ai_results SET status='expired' WHERE retention_kind='retained' AND status='available' AND expires_at_ms IS NOT NULL AND expires_at_ms<=?1",params![now]).map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(result)
}

fn migrate_retired_sql_tools(tx: &rusqlite::Transaction<'_>, now: i64) -> Result<()> {
    // `db.run` 的全局开关不存在行时默认启用；只有任一旧入口被显式关闭时，
    // 才写入统一的关闭行。已有的 `db.run` 设置优先，绝不反向覆盖。
    let legacy_disabled: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM ai_tool_settings
                 WHERE tool_id IN ('db.query','db.execute','db.explain') AND enabled=0
             )",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if legacy_disabled {
        tx.execute(
            "INSERT INTO ai_tool_settings (tool_id,enabled,revision,updated_at_ms)
             VALUES ('db.run',0,1,?1)
             ON CONFLICT(tool_id) DO NOTHING",
            [now],
        )
        .map_err(sqlite_error)?;
    }
    tx.execute(
        "DELETE FROM ai_tool_settings
          WHERE tool_id IN ('db.query','db.execute','db.explain')",
        [],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "UPDATE ai_tool_settings
            SET tool_id='toolbox.inspect',revision=revision+1
          WHERE tool_id='config.inspect'
            AND NOT EXISTS (SELECT 1 FROM ai_tool_settings WHERE tool_id='toolbox.inspect')",
        [],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "DELETE FROM ai_tool_settings WHERE tool_id='config.inspect'",
        [],
    )
    .map_err(sqlite_error)?;
    // 旧统一配置入口不是用户可见的单独能力；继续保存在设置表会让界面出现
    // 一个无法独立启停的幽灵行。
    tx.execute(
        "DELETE FROM ai_tool_settings WHERE tool_id='config.propose'",
        [],
    )
    .map_err(sqlite_error)?;

    let agents = {
        let mut stmt = tx
            .prepare("SELECT id,allowed_tools_json FROM ai_agents")
            .map_err(sqlite_error)?;
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?
    };
    for (id, raw) in agents {
        let Ok(mut tools) = serde_json::from_str::<Vec<String>>(&raw) else {
            continue;
        };
        let had_legacy = tools
            .iter()
            .any(|tool| RETIRED_SQL_TOOL_IDS.contains(&tool.as_str()));
        let had_old_inspect = tools.iter().any(|tool| tool == "config.inspect");
        if !had_legacy && !had_old_inspect {
            continue;
        }
        if !tools.iter().any(|tool| tool == "db.run") {
            if had_legacy {
                tools.push("db.run".into());
            }
        }
        tools.retain(|tool| !RETIRED_SQL_TOOL_IDS.contains(&tool.as_str()));
        if had_old_inspect && !tools.iter().any(|tool| tool == "toolbox.inspect") {
            tools.push("toolbox.inspect".into());
        }
        tools.retain(|tool| tool != "config.inspect");
        tx.execute(
            "UPDATE ai_agents SET allowed_tools_json=?2,revision=revision+1 WHERE id=?1",
            params![
                id,
                serde_json::to_string(&tools).map_err(|e| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string())
                })?
            ],
        )
        .map_err(sqlite_error)?;
    }
    Ok(())
}

/// 只读查询 staged 产物候选（供启动对账与测试复用，不做任何写入）。
pub fn staged_artifact_candidates(conn: &Connection) -> Result<Vec<StagedArtifactCandidate>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, relative_path, byte_count, content_hash FROM ai_artifacts
              WHERE status='staged' AND relative_path IS NOT NULL
              ORDER BY updated_at_ms, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StagedArtifactCandidate {
                id: row.get(0)?,
                relative_path: row.get(1)?,
                byte_count: row.get(2)?,
                content_hash: row.get(3)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// 本次要过期的保留结果落点（受控根内相对路径）；删除前先核对路径不越界。
fn expired_retained_paths(tx: &rusqlite::Transaction<'_>, now: i64) -> Result<Vec<String>> {
    let mut stmt = tx
        .prepare("SELECT relative_path FROM ai_results WHERE retention_kind='retained' AND status='available' AND expires_at_ms IS NOT NULL AND expires_at_ms<=?1")
        .map_err(sqlite_error)?;
    let paths = stmt
        .query_map(params![now], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(paths
        .into_iter()
        .filter(|path| {
            let path = std::path::Path::new(path);
            !path.is_absolute()
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        })
        .collect())
}

#[cfg(test)]
mod retired_tool_migration_tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        crate::ai_platform::ensure_schema(&conn).expect("schema");
        conn
    }

    #[test]
    fn retired_sql_and_inspect_ids_migrate_without_losing_intent() {
        let mut conn = test_conn();
        conn.execute(
            "INSERT INTO ai_tool_settings(tool_id,enabled,revision,updated_at_ms)
             VALUES ('db.execute',0,3,1),('config.inspect',0,2,1)",
            [],
        )
        .expect("legacy settings");
        conn.execute(
            "INSERT INTO ai_agents(id,display_name,enabled,system_prompt,allowed_tools_json,
                created_at_ms,updated_at_ms)
             VALUES ('agent-test','Test',1,'prompt','[\"db.query\",\"config.inspect\"]',1,1)",
            [],
        )
        .expect("legacy agent");

        recover_on_startup(&mut conn, 2).expect("recovery");

        let db_run_disabled: bool = conn
            .query_row(
                "SELECT enabled=0 FROM ai_tool_settings WHERE tool_id='db.run'",
                [],
                |row| row.get(0),
            )
            .expect("db.run migrated");
        assert!(db_run_disabled);
        let old_settings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_tool_settings
                  WHERE tool_id IN ('db.query','db.execute','db.explain','config.inspect')",
                [],
                |row| row.get(0),
            )
            .expect("old settings removed");
        assert_eq!(old_settings, 0);
        let inspect_disabled: bool = conn
            .query_row(
                "SELECT enabled=0 FROM ai_tool_settings WHERE tool_id='toolbox.inspect'",
                [],
                |row| row.get(0),
            )
            .expect("inspect migrated");
        assert!(inspect_disabled);

        let tools: String = conn
            .query_row(
                "SELECT allowed_tools_json FROM ai_agents WHERE id='agent-test'",
                [],
                |row| row.get(0),
            )
            .expect("agent tools");
        let tools: Vec<String> = serde_json::from_str(&tools).expect("tools json");
        assert_eq!(tools, ["db.run", "toolbox.inspect"]);
    }
}
