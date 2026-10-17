// 审批与安全审计持久化（设计 §25、§29、§42.11，T020）。
//
// 两条不可破坏的规则：
// 1. `ai_tool_audits` 是 **append-only**：本模块只提供插入与查询，没有 update/delete。
// 2. `ai_approvals` 与 `plan_hash` 绑定：参数/目标/插件摘要变化会换 hash，旧审批自然失效；
//    审批决策走行级 CAS，重复点击不会二次生效；撤销（invalidate）即时生效。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 审计阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditStage {
    /// 执行前对最终 plan 的强制审计。
    PreExecute,
    /// 执行前重新校验（资源/审批/参数变化时）。
    Revalidate,
}

impl AuditStage {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditStage::PreExecute => "pre_execute",
            AuditStage::Revalidate => "revalidate",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pre_execute" => Ok(AuditStage::PreExecute),
            "revalidate" => Ok(AuditStage::Revalidate),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知审计阶段: {other}"),
            )),
        }
    }
}

/// 审计判定（内核最严格结果优先；这里只存事实）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditDecision {
    Allow,
    Deny,
    RequireApproval,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditDecision::Allow => "allow",
            AuditDecision::Deny => "deny",
            AuditDecision::RequireApproval => "require_approval",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "allow" => Ok(AuditDecision::Allow),
            "deny" => Ok(AuditDecision::Deny),
            "require_approval" => Ok(AuditDecision::RequireApproval),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知审计判定: {other}"),
            )),
        }
    }
}

/// 一次审计记录（append-only）。
#[derive(Clone, Debug)]
pub struct ToolAuditEdit {
    pub id: String,
    /// 内部调用与 MCP 请求二选一。
    pub tool_call_id: Option<String>,
    pub mcp_request_id: Option<String>,
    pub stage: AuditStage,
    pub hook_id: String,
    pub hook_version: String,
    pub ruleset_version: String,
    pub plan_hash: String,
    pub decision: AuditDecision,
    pub risk: String,
    pub rule_ids: Vec<String>,
    pub reason_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolAuditView {
    pub id: String,
    pub tool_call_id: Option<String>,
    pub mcp_request_id: Option<String>,
    pub stage: AuditStage,
    pub hook_id: String,
    pub hook_version: String,
    pub ruleset_version: String,
    pub plan_hash: String,
    pub decision: AuditDecision,
    pub risk: String,
    pub rule_ids: Vec<String>,
    pub reason_codes: Vec<String>,
    pub created_at_ms: i64,
}

/// 审计列表的一行：审计事实 + 关联调用的可见投影。
///
/// 只带展示需要的字段：工具、来源、目标、判定、规则、计划指纹。**不含**参数值原文之外
/// 的任何秘密材料——`arguments_redacted_json` 是入库前已脱敏的版本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolAuditListRow {
    pub id: String,
    pub stage: AuditStage,
    /// 内部工具调用的 id；MCP 来源为 `None`。
    pub tool_call_id: Option<String>,
    /// MCP 请求的 id；内部来源为 `None`。
    pub mcp_request_id: Option<String>,
    /// 来源：`internal`（内置 Agent）或 `mcp`（对外 MCP）。
    pub origin: &'static str,
    pub tool_id: String,
    pub target_json: String,
    /// 已脱敏的参数（入库前处理过），供详情展示 SQL/命令。
    pub arguments_redacted_json: String,
    pub decision: AuditDecision,
    pub risk: String,
    pub rule_ids: Vec<String>,
    pub reason_codes: Vec<String>,
    pub ruleset_version: String,
    pub plan_hash: String,
    pub created_at_ms: i64,
}

/// 列出最近的审计事实（按时间倒序）。
///
/// - `session_id` 为 `Some` 时只返回该会话内产生的审计；`None` 表示跨会话。
/// - `limit` 由调用方给出上界，存储层不再二次放大。
///
/// 内部调用经 `ai_tool_calls.turn_id → ai_turns.session_id` 归属会话；
/// MCP 请求自带 `session_id`。两类来源用同一列输出，UI 不必区分。
pub fn list_recent_tool_audits(
    conn: &Connection,
    session_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ToolAuditListRow>> {
    let limit = limit.clamp(1, 1000);
    let sql = "
        SELECT a.id, a.stage, a.tool_call_id, a.mcp_request_id,
               a.decision, a.risk, a.rule_ids_json, a.reason_codes_json,
               a.ruleset_version, a.plan_hash, a.created_at_ms,
               COALESCE(c.tool_id, r.tool_id) AS tool_id,
               COALESCE(c.target_json, r.target_json) AS target_json,
               COALESCE(c.arguments_redacted_json, r.arguments_redacted_json) AS args_json,
               CASE WHEN a.mcp_request_id IS NULL THEN 'internal' ELSE 'mcp' END AS origin,
               COALESCE(t.session_id, r.session_id) AS owner_session
          FROM ai_tool_audits a
          LEFT JOIN ai_tool_calls c ON c.id = a.tool_call_id
          LEFT JOIN ai_turns t ON t.id = c.turn_id
          LEFT JOIN ai_mcp_server_requests r ON r.id = a.mcp_request_id
         WHERE (?1 IS NULL OR COALESCE(t.session_id, r.session_id) = ?1)
         ORDER BY a.created_at_ms DESC, a.id DESC
         LIMIT ?2";
    let mut stmt = conn.prepare(sql).map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![session_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, String>(14)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            stage,
            tool_call_id,
            mcp_request_id,
            decision,
            risk,
            rule_ids,
            reason_codes,
            ruleset_version,
            plan_hash,
            created_at_ms,
            tool_id,
            target_json,
            args_json,
            origin,
        ) = row.map_err(sqlite_error)?;
        out.push(ToolAuditListRow {
            id,
            stage: AuditStage::parse(&stage)?,
            tool_call_id,
            mcp_request_id,
            origin: if origin == "mcp" { "mcp" } else { "internal" },
            // 关联调用行缺失时（外键 RESTRICT，正常不会发生）留空而不是伪造工具名。
            tool_id: tool_id.unwrap_or_default(),
            target_json: target_json.unwrap_or_else(|| "{}".into()),
            arguments_redacted_json: args_json.unwrap_or_else(|| "{}".into()),
            decision: AuditDecision::parse(&decision)?,
            risk,
            rule_ids: parse_list(&rule_ids, "rule_ids")?,
            reason_codes: parse_list(&reason_codes, "reason_codes")?,
            ruleset_version,
            plan_hash,
            created_at_ms,
        });
    }
    Ok(out)
}

fn json_list(items: &[String], label: &str) -> Result<String> {
    serde_json::to_string(items)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("{label} 序列化失败: {e}")))
}

fn parse_list(raw: &str, label: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("{label} 解析失败: {e}")))
}

/// 追加一条审计。**没有对应的更新/删除接口**：审计是事实，只能追加。
/// 是否已存在同一调用同一阶段的审计行（用于避免重复登记）。
pub fn has_tool_audit(conn: &Connection, tool_call_id: &str, stage: AuditStage) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_tool_audits WHERE tool_call_id=?1 AND stage=?2)",
        params![tool_call_id, stage.as_str()],
        |row| row.get::<_, bool>(0),
    )
    .map_err(sqlite_error)
}

pub fn insert_tool_audit(conn: &Connection, now_ms: i64, edit: &ToolAuditEdit) -> Result<()> {
    if edit.plan_hash.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "审计必须绑定 plan_hash"));
    }
    if edit.tool_call_id.is_some() == edit.mcp_request_id.is_some() {
        return Err(Error::new(
            ErrorKind::Internal,
            "审计必须且只能绑定 tool_call_id 或 mcp_request_id 之一",
        ));
    }
    conn.execute(
        "INSERT INTO ai_tool_audits (id, tool_call_id, mcp_request_id, stage, hook_id,
            hook_version, ruleset_version, plan_hash, decision, risk, rule_ids_json,
            reason_codes_json, created_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            edit.id,
            edit.tool_call_id,
            edit.mcp_request_id,
            edit.stage.as_str(),
            edit.hook_id,
            edit.hook_version,
            edit.ruleset_version,
            edit.plan_hash,
            edit.decision.as_str(),
            edit.risk,
            json_list(&edit.rule_ids, "rule_ids")?,
            json_list(&edit.reason_codes, "reason_codes")?,
            now_ms,
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// 查询某次调用的审计（按时间）。append-only 表因此可以完整回放判断历史。
pub fn list_tool_audits(conn: &Connection, tool_call_id: &str) -> Result<Vec<ToolAuditView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, tool_call_id, mcp_request_id, stage, hook_id, hook_version,
                    ruleset_version, plan_hash, decision, risk, rule_ids_json,
                    reason_codes_json, created_at_ms
               FROM ai_tool_audits WHERE tool_call_id=?1 ORDER BY created_at_ms, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([tool_call_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            tool_call_id,
            mcp_request_id,
            stage,
            hook_id,
            hook_version,
            ruleset_version,
            plan_hash,
            decision,
            risk,
            rule_ids,
            reason_codes,
            created_at_ms,
        ) = row.map_err(sqlite_error)?;
        out.push(ToolAuditView {
            id,
            tool_call_id,
            mcp_request_id,
            stage: AuditStage::parse(&stage)?,
            hook_id,
            hook_version,
            ruleset_version,
            plan_hash,
            decision: AuditDecision::parse(&decision)?,
            risk,
            rule_ids: parse_list(&rule_ids, "rule_ids")?,
            reason_codes: parse_list(&reason_codes, "reason_codes")?,
            created_at_ms,
        });
    }
    Ok(out)
}

/// 审批状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Invalidated,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
            ApprovalStatus::Invalidated => "invalidated",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(ApprovalStatus::Pending),
            "approved" => Ok(ApprovalStatus::Approved),
            "denied" => Ok(ApprovalStatus::Denied),
            "expired" => Ok(ApprovalStatus::Expired),
            "invalidated" => Ok(ApprovalStatus::Invalidated),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知审批状态: {other}"),
            )),
        }
    }
}

/// 审批视图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalView {
    pub id: String,
    pub tool_call_id: Option<String>,
    pub mcp_request_id: Option<String>,
    pub plan_hash: String,
    pub scope_json: String,
    pub status: ApprovalStatus,
    pub reason_code: Option<String>,
    pub created_at_ms: i64,
    pub decided_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub revision: i64,
}

/// 新建待审批项（绑定 plan_hash；同一调用同一 hash 只允许一条）。
pub fn create_approval(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    tool_call_id: &str,
    plan_hash: &str,
    scope_json: &str,
    expires_at_ms: Option<i64>,
) -> Result<ApprovalView> {
    if plan_hash.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "审批必须绑定 plan_hash"));
    }
    serde_json::from_str::<serde_json::Value>(scope_json)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("审批范围不是合法 JSON: {e}")))?;
    conn.execute(
        "INSERT INTO ai_approvals (id, tool_call_id, mcp_request_id, plan_hash, scope_json,
            status, reason_code, created_at_ms, decided_at_ms, expires_at_ms, revision)
         VALUES (?1,?2,NULL,?3,?4,'pending',NULL,?5,NULL,?6,1)",
        params![
            id,
            tool_call_id,
            plan_hash,
            scope_json,
            now_ms,
            expires_at_ms
        ],
    )
    .map_err(|e| {
        let message = e.to_string();
        if message.contains("UNIQUE constraint failed") {
            Error::new(ErrorKind::Internal, "该调用的同一 plan 已有审批记录")
        } else {
            sqlite_error(e)
        }
    })?;
    load_approval(conn, id)
}

/// 读取审批。
pub fn load_approval(conn: &Connection, id: &str) -> Result<ApprovalView> {
    conn.query_row(
        "SELECT id, tool_call_id, mcp_request_id, plan_hash, scope_json, status, reason_code,
                created_at_ms, decided_at_ms, expires_at_ms, revision
           FROM ai_approvals WHERE id=?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, i64>(10)?,
            ))
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "审批记录不存在"))
    .and_then(
        |(
            id,
            tool_call_id,
            mcp_request_id,
            plan_hash,
            scope_json,
            status,
            reason_code,
            created_at_ms,
            decided_at_ms,
            expires_at_ms,
            revision,
        )| {
            Ok(ApprovalView {
                id,
                tool_call_id,
                mcp_request_id,
                plan_hash,
                scope_json,
                status: ApprovalStatus::parse(&status)?,
                reason_code,
                created_at_ms,
                decided_at_ms,
                expires_at_ms,
                revision,
            })
        },
    )
}

/// 审批决定（对象级 CAS；只有 pending 能决定，重复点击不再生效）。
pub fn decide_approval(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    approved: bool,
    reason_code: Option<&str>,
    expected_revision: i64,
) -> Result<ApprovalView> {
    let status = if approved { "approved" } else { "denied" };
    let changed = conn
        .execute(
            "UPDATE ai_approvals SET status=?2, reason_code=?3, decided_at_ms=?4,
                revision=revision+1
              WHERE id=?1 AND status='pending' AND revision=?5",
            params![id, status, reason_code, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "审批状态已变化（可能已被处理或失效），请刷新后重试",
        ));
    }
    load_approval(conn, id)
}

/// 撤销审批（参数/目标/插件摘要变化时调用）：即时生效，已批准的也不再可用。
pub fn invalidate_approval(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    reason_code: &str,
) -> Result<ApprovalView> {
    let changed = conn
        .execute(
            "UPDATE ai_approvals SET status='invalidated', reason_code=?2, decided_at_ms=?3,
                revision=revision+1
              WHERE id=?1 AND status IN ('pending','approved')",
            params![id, reason_code, now_ms],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "审批已不可撤销（已过期或已失效）",
        ));
    }
    load_approval(conn, id)
}

/// 查找仍然可用的批准：必须是 approved、未过期、且 **plan_hash 完全一致**。
///
/// 参数/目标/插件摘要变化会改变 plan_hash，因此旧批准查不到（自然失效）。
pub fn find_valid_approval(
    conn: &Connection,
    now_ms: i64,
    tool_call_id: &str,
    plan_hash: &str,
) -> Result<Option<ApprovalView>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM ai_approvals
              WHERE tool_call_id=?1 AND plan_hash=?2 AND status='approved'
                AND (expires_at_ms IS NULL OR expires_at_ms > ?3)
              ORDER BY decided_at_ms DESC, id LIMIT 1",
            params![tool_call_id, plan_hash, now_ms],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match id {
        Some(id) => Ok(Some(load_approval(conn, &id)?)),
        None => Ok(None),
    }
}

/// 列出某次调用的审批历史（UI 卡片回显 / 刷新后仍可处理）。
pub fn list_approvals_for_call(conn: &Connection, tool_call_id: &str) -> Result<Vec<ApprovalView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_approvals WHERE tool_call_id=?1 ORDER BY created_at_ms, id")
        .map_err(sqlite_error)?;
    let ids: Vec<String> = stmt
        .query_map([tool_call_id], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_approval(conn, &id)).collect()
}
