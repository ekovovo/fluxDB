//! 会话 token 账本；短事务登记/完成请求，不跨网络持锁，不计算金额。
use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result, Usage};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug)]
pub struct UsageRequest {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub model_profile_id: String,
    pub purpose: String,
    /// 可选产物标题，不参与会话用量汇总过滤。
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// 缓存命中属于输入 token 的子集，不再次计入总量。
    pub cached_input_tokens: i64,
    pub total_tokens: i64,
    /// 包括尚未完成和未返回 usage 的请求，不能把已知总量当作完整用量。
    pub unknown_requests: i64,
}

/// 返回 false 表示同一 request 已登记；调用方不得再次请求供应商。
pub fn start_request(conn: &Connection, now: i64, request: &UsageRequest) -> Result<bool> {
    if request.request_id.trim().is_empty()
        || !["main", "decision", "compaction", "artifact", "title"]
            .contains(&request.purpose.as_str())
    {
        return Err(Error::new(ErrorKind::Internal, "非法计量请求"));
    }
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    // 先取写锁，再检查幂等和并发；不同连接不能同时抢到同一并发名额。
    tx.execute(
        "UPDATE ai_sessions SET updated_at_ms=updated_at_ms WHERE id=?1",
        [&request.session_id],
    )
    .map_err(sqlite_error)?;
    let existing: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_usage_ledger WHERE request_id=?1 AND attempt=1)",
            [&request.request_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if existing {
        return Ok(false);
    }
    let turn = super::sessions::load_turn(&tx, &request.turn_id)?;
    if turn.session_id != request.session_id
        || turn.status.is_terminal()
        || turn.status == super::sessions::TurnStatus::CancelRequested
    {
        return Err(Error::new(
            ErrorKind::Internal,
            "计量请求归属或任务状态无效",
        ));
    }
    super::models::load_model(&tx, &request.model_profile_id)?;
    let policy_id: Option<String> = tx
        .query_row(
            "SELECT a.budget_policy_id FROM ai_sessions s JOIN ai_agents a ON a.id=s.agent_id WHERE s.id=?1",
            [&request.session_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if let Some(id) = policy_id.as_deref() {
        let policy = super::mcp::load_budget_policy(&tx, id)?;
        if policy.enabled {
            let active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM ai_usage_ledger WHERE budget_policy_id=?1 AND measurement='pending'",
                    [id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if active >= policy.max_model_concurrency {
                return Err(Error::new(ErrorKind::Internal, "模型请求并发已达上限"));
            }
        }
    }
    tx.execute(
        "INSERT INTO ai_usage_ledger(id,session_id,turn_id,request_id,attempt,model_profile_id,
            budget_policy_id,purpose,measurement,title,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?1,1,?4,?5,?6,'pending',?7,?8,?8)",
        params![
            request.request_id,
            request.session_id,
            request.turn_id,
            request.model_profile_id,
            policy_id,
            request.purpose,
            request.title,
            now
        ],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// 仅 pending 可完成，重复回调返回 false；usage 缺失明确 unknown 并释放并发名额。
pub fn finish_request(
    conn: &Connection,
    now: i64,
    request_id: &str,
    usage: Option<Usage>,
) -> Result<bool> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    tx.execute(
        "UPDATE ai_usage_ledger SET updated_at_ms=updated_at_ms WHERE request_id=?1",
        [request_id],
    )
    .map_err(sqlite_error)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT measurement FROM ai_usage_ledger WHERE request_id=?1 AND attempt=1",
            [request_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(status) = status else {
        return Err(Error::new(ErrorKind::Internal, "计量请求不存在"));
    };
    if status != "pending" {
        return Ok(false);
    }
    if let Some(u) = usage {
        let total = u.input_tokens.checked_add(u.output_tokens);
        if u.input_tokens < 0
            || u.output_tokens < 0
            || u.cached_input_tokens < 0
            || u.cached_input_tokens > u.input_tokens
            || total.is_none()
        {
            return Err(Error::new(ErrorKind::Internal, "模型 usage 无效"));
        }
        tx.execute(
            "UPDATE ai_usage_ledger SET input_tokens=?2,output_tokens=?3,cached_input_tokens=?4,
                total_tokens=?5,measurement='actual',updated_at_ms=?6
             WHERE request_id=?1 AND attempt=1",
            params![
                request_id,
                u.input_tokens,
                u.output_tokens,
                u.cached_input_tokens,
                total,
                now
            ],
        )
        .map_err(sqlite_error)?;
    } else {
        tx.execute(
            "UPDATE ai_usage_ledger SET measurement='unknown',updated_at_ms=?2 WHERE request_id=?1 AND attempt=1",
            params![request_id,now],
        ).map_err(sqlite_error)?;
    }
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// 汇总当前会话所有用途的已知 token；缺失部分另计请求数，不伪造零用量。
pub fn totals(conn: &Connection, session_id: &str) -> Result<UsageTotals> {
    conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),
            COALESCE(SUM(cached_input_tokens),0),COALESCE(SUM(total_tokens),0),
            COALESCE(SUM(measurement<>'actual'),0)
         FROM ai_usage_ledger WHERE session_id=?1",
        [session_id],
        |r| {
            Ok(UsageTotals {
                requests: r.get(0)?,
                input_tokens: r.get(1)?,
                output_tokens: r.get(2)?,
                cached_input_tokens: r.get(3)?,
                total_tokens: r.get(4)?,
                unknown_requests: r.get(5)?,
            })
        },
    )
    .map_err(sqlite_error)
}
