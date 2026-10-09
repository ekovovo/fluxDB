//! 所有模型请求共用预留/结算账本；短事务不跨网络，unknown 费用不当作免费。
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
    pub estimated_input_tokens: i64,
    pub max_output_tokens: i64,
    /// 请求产物标题；标题生成等轻量请求必须提供，主对话保持 `None`。
    pub title: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_micros: i64,
    pub unknown_requests: i64,
}
fn cost(usage: Usage, prices: [Option<i64>; 3]) -> Option<i64> {
    if usage.input_tokens < 0
        || usage.output_tokens < 0
        || usage.cached_input_tokens < 0
        || usage.cached_input_tokens > usage.input_tokens
    {
        return None;
    }
    let uncached = usage.input_tokens - usage.cached_input_tokens;
    let component = |tokens: i64, price: Option<i64>| {
        if tokens == 0 {
            Some(0i128)
        } else {
            price.map(|p| i128::from(tokens) * i128::from(p))
        }
    };
    let amount = component(uncached, prices[0])?
        + component(usage.output_tokens, prices[1])?
        + component(usage.cached_input_tokens, prices[2])?;
    i64::try_from((amount + 999_999) / 1_000_000).ok()
}
/// 返回 false 表示同一 request 已预留；调用方不得再次请求供应商。
pub fn reserve(conn: &Connection, now: i64, request: &UsageRequest) -> Result<bool> {
    if request.request_id.trim().is_empty()
        || !["main", "decision", "compaction", "artifact", "title"]
            .contains(&request.purpose.as_str())
        || request.estimated_input_tokens < 0
        || request.max_output_tokens < 0
    {
        return Err(Error::new(ErrorKind::Internal, "非法计量请求"));
    }
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    // 在任何余额读取前获得写锁，避免并发请求消费同一余额。
    tx.execute(
        "UPDATE ai_sessions SET updated_at_ms=updated_at_ms WHERE id=?1",
        [&request.session_id],
    )
    .map_err(sqlite_error)?;
    let existing: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_budget_reservations WHERE request_id=?1 AND attempt=1)",
            [&request.request_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if existing {
        return Ok(false);
    }
    let turn = super::sessions::load_turn(&tx, &request.turn_id)?;
    if turn.session_id != request.session_id
        || (turn.status.is_terminal()
            || turn.status == super::sessions::TurnStatus::CancelRequested)
    {
        return Err(Error::new(
            ErrorKind::Internal,
            "计量请求归属或任务状态无效",
        ));
    }
    let model = super::models::load_model(&tx, &request.model_profile_id)?;
    let prices = [
        model.input_price_micros_per_million,
        model.output_price_micros_per_million,
        model.cached_input_price_micros_per_million,
    ];
    let amount = cost(
        Usage {
            input_tokens: request.estimated_input_tokens,
            output_tokens: request.max_output_tokens,
            cached_input_tokens: 0,
        },
        prices,
    );
    let policy_id:Option<String>=tx.query_row("SELECT a.budget_policy_id FROM ai_sessions s JOIN ai_agents a ON a.id=s.agent_id WHERE s.id=?1",[&request.session_id],|r|r.get(0)).map_err(sqlite_error)?;
    if let Some(id) = policy_id.as_deref() {
        let policy = super::mcp::load_budget_policy(&tx, id)?;
        if policy.enabled {
            let active:i64=tx.query_row("SELECT COUNT(*) FROM ai_budget_reservations WHERE budget_policy_id=?1 AND status='held'",[id],|r|r.get(0)).map_err(sqlite_error)?;
            if active >= policy.max_model_concurrency {
                return Err(Error::new(ErrorKind::Internal, "模型请求并发已达上限"));
            }
            if policy.session_limit_micros.is_some() || policy.daily_limit_micros.is_some() {
                let amount = amount
                    .ok_or_else(|| Error::new(ErrorKind::Internal, "严格预算缺少模型价格"))?;
                for (limit, sql, key) in [
                    (
                        policy.session_limit_micros,
                        "SELECT COALESCE(SUM(amount_micros),0) FROM ai_budget_reservations WHERE session_id=?1 AND status!='released'",
                        request.session_id.as_str(),
                    ),
                    (
                        policy.daily_limit_micros,
                        "SELECT COALESCE(SUM(amount_micros),0) FROM ai_budget_reservations WHERE budget_policy_id=?1 AND status!='released' AND created_at_ms>=?2",
                        id,
                    ),
                ] {
                    if let Some(limit) = limit {
                        let used: i64 = if sql.contains("?2") {
                            tx.query_row(sql, params![key, now - now.rem_euclid(86_400_000)], |r| {
                                r.get(0)
                            })
                        } else {
                            tx.query_row(sql, [key], |r| r.get(0))
                        }
                        .map_err(sqlite_error)?;
                        if used.checked_add(amount).is_none_or(|v| v > limit) {
                            return Err(Error::new(ErrorKind::Internal, "模型请求超出预算"));
                        }
                    }
                }
            }
        }
    }
    let pricing=serde_json::json!({"input":prices[0],"output":prices[1],"cache":prices[2],"model_revision":model.revision}).to_string();
    tx.execute("INSERT INTO ai_budget_reservations(id,request_id,attempt,session_id,turn_id,budget_policy_id,amount_micros,status,created_at_ms,updated_at_ms) VALUES(?1,?1,1,?2,?3,?4,?5,'held',?6,?6)",params![request.request_id,request.session_id,request.turn_id,policy_id,amount.unwrap_or(0),now]).map_err(sqlite_error)?;
    tx.execute("INSERT INTO ai_usage_ledger(id,session_id,turn_id,request_id,attempt,model_profile_id,budget_policy_id,purpose,measurement,pricing_snapshot_json,title,created_at_ms) VALUES(?1,?2,?3,?1,1,?4,?5,?6,'unknown',?7,?8,?9)",params![request.request_id,request.session_id,request.turn_id,request.model_profile_id,policy_id,request.purpose,pricing,request.title,now]).map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}
/// 仅 held 可结算，重复回调返回 false；usage 缺失保留预留并明确 unknown。
pub fn settle(conn: &Connection, now: i64, request_id: &str, usage: Option<Usage>) -> Result<bool> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    tx.execute(
        "UPDATE ai_budget_reservations SET updated_at_ms=updated_at_ms WHERE request_id=?1",
        [request_id],
    )
    .map_err(sqlite_error)?;
    let row:Option<(String,String)>=tx.query_row("SELECT r.status,l.pricing_snapshot_json FROM ai_budget_reservations r JOIN ai_usage_ledger l ON l.request_id=r.request_id AND l.attempt=r.attempt WHERE r.request_id=?1 AND r.attempt=1",[request_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sqlite_error)?;
    let Some((status, pricing)) = row else {
        return Err(Error::new(ErrorKind::Internal, "计量预留不存在"));
    };
    if status != "held" {
        return Ok(false);
    }
    let prices: serde_json::Value = serde_json::from_str(&pricing)
        .map_err(|_| Error::new(ErrorKind::Internal, "价格快照损坏"))?;
    if let Some(u) = usage {
        if u.input_tokens < 0
            || u.output_tokens < 0
            || u.cached_input_tokens < 0
            || u.cached_input_tokens > u.input_tokens
        {
            return Err(Error::new(ErrorKind::Internal, "模型 usage 无效"));
        }
        let amount = cost(
            u,
            [
                prices["input"].as_i64(),
                prices["output"].as_i64(),
                prices["cache"].as_i64(),
            ],
        );
        tx.execute("UPDATE ai_usage_ledger SET input_tokens=?2,output_tokens=?3,cached_input_tokens=?4,cost_micros=?5,measurement='actual' WHERE request_id=?1",params![request_id,u.input_tokens,u.output_tokens,u.cached_input_tokens,amount]).map_err(sqlite_error)?;
        tx.execute("UPDATE ai_budget_reservations SET status='settled',amount_micros=COALESCE(?2,amount_micros),updated_at_ms=?3 WHERE request_id=?1",params![request_id,amount,now]).map_err(sqlite_error)?;
    } else {
        tx.execute("UPDATE ai_budget_reservations SET status='unknown',updated_at_ms=?2 WHERE request_id=?1",params![request_id,now]).map_err(sqlite_error)?;
    }
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}
pub fn totals(conn: &Connection, session_id: &str) -> Result<UsageTotals> {
    conn.query_row("SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),COALESCE(SUM(cost_micros),0),COALESCE(SUM(measurement='unknown' OR cost_micros IS NULL),0) FROM ai_usage_ledger WHERE session_id=?1",[session_id],|r|Ok(UsageTotals{requests:r.get(0)?,input_tokens:r.get(1)?,output_tokens:r.get(2)?,cost_micros:r.get(3)?,unknown_requests:r.get(4)?})).map_err(sqlite_error)
}
