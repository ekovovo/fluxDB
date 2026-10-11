//! 运行保护策略与 MCP Client 设置的读取/保存（T010/T012）。
//!
//! 本期 MCP 仅作为 Client（接入外部工具）配置，默认禁用；对外 Server 另表。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 运行保护策略视图；沿用 `ai_budget_policies` 表名，但不包含金额或 token 总量上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetPolicyView {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub max_iterations: i64,
    pub max_wall_time_ms: i64,
    pub max_model_concurrency: i64,
    pub revision: i64,
}

/// 读取运行保护策略。
pub fn load_budget_policy(conn: &Connection, id: &str) -> Result<BudgetPolicyView> {
    conn.query_row(
        "SELECT id, display_name, enabled,
                max_iterations, max_wall_time_ms, max_model_concurrency, revision
           FROM ai_budget_policies WHERE id=?1",
        [id],
        |row| {
            Ok(BudgetPolicyView {
                id: row.get(0)?,
                display_name: row.get(1)?,
                enabled: row.get::<_, i64>(2)? != 0,
                max_iterations: row.get(3)?,
                max_wall_time_ms: row.get(4)?,
                max_model_concurrency: row.get(5)?,
                revision: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| Error::new(ErrorKind::Internal, "运行保护策略不存在"))
}

/// 保存运行保护策略（对象级 CAS）。
pub fn save_budget_policy(
    conn: &Connection,
    now_ms: i64,
    policy: &BudgetPolicyView,
    expected_revision: Option<i64>,
) -> Result<BudgetPolicyView> {
    for value in [
        policy.max_iterations,
        policy.max_wall_time_ms,
        policy.max_model_concurrency,
    ] {
        if value <= 0 {
            return Err(Error::new(ErrorKind::Internal, "执行保护上限必须为正"));
        }
    }
    let sql = "INSERT INTO ai_budget_policies (id, display_name, enabled,
            max_iterations, max_wall_time_ms, max_model_concurrency,
            options_json, revision, created_at_ms, updated_at_ms)
        VALUES (?1,?2,?3,?4,?5,?6,'{}',1,?7,?7)
        ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, enabled=excluded.enabled,
            max_iterations=excluded.max_iterations, max_wall_time_ms=excluded.max_wall_time_ms,
            max_model_concurrency=excluded.max_model_concurrency,
            revision=ai_budget_policies.revision+1, updated_at_ms=excluded.updated_at_ms
        WHERE ai_budget_policies.revision=?8";
    conn.execute(
        sql,
        params![
            policy.id,
            policy.display_name,
            policy.enabled as i64,
            policy.max_iterations,
            policy.max_wall_time_ms,
            policy.max_model_concurrency,
            now_ms,
            expected_revision.unwrap_or(0),
        ],
    )
    .map_err(sqlite_error)?;
    load_budget_policy(conn, &policy.id)
}

/// MCP Client 设置视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSettingView {
    pub id: String,
    pub display_name: String,
    pub transport: String,
    pub enabled: bool,
    pub config_json: String,
    pub revision: i64,
}

/// 列出全部 MCP Client 设置。
pub fn list_mcp_settings(conn: &Connection) -> Result<Vec<McpSettingView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, display_name, transport, enabled, config_json, revision
               FROM ai_mcp_settings ORDER BY display_name, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(McpSettingView {
                id: row.get(0)?,
                display_name: row.get(1)?,
                transport: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                config_json: row.get(4)?,
                revision: row.get(5)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// 保存 MCP Client 设置（默认禁用；配置 JSON 必须是合法对象且不含明文密钥）。
pub fn save_mcp_setting(
    conn: &Connection,
    now_ms: i64,
    setting: &McpSettingView,
    expected_revision: Option<i64>,
) -> Result<McpSettingView> {
    if !matches!(setting.transport.as_str(), "stdio" | "http") {
        return Err(Error::new(
            ErrorKind::Internal,
            "MCP 传输方式仅支持 stdio/http",
        ));
    }
    let config: serde_json::Value = serde_json::from_str(&setting.config_json)
        .map_err(|_| Error::new(ErrorKind::Internal, "MCP 配置不是合法 JSON"))?;
    if !config.is_object() {
        return Err(Error::new(ErrorKind::Internal, "MCP 配置必须是对象"));
    }
    // 配置里不得出现凭据字段：凭据只允许存到加密的 integration secrets。
    for forbidden in ["api_key", "token", "password", "authorization"] {
        if config.get(forbidden).is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "MCP 配置不得包含凭据字段，请使用密钥管理",
            ));
        }
    }
    let sql = "INSERT INTO ai_mcp_settings (id, display_name, transport, enabled, config_json,
            tool_filter_json, permissions_json, revision, created_at_ms, updated_at_ms)
        VALUES (?1,?2,?3,?4,?5,'[]','{}',1,?6,?6)
        ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, transport=excluded.transport,
            enabled=excluded.enabled, config_json=excluded.config_json,
            revision=ai_mcp_settings.revision+1, updated_at_ms=excluded.updated_at_ms
        WHERE ai_mcp_settings.revision=?7";
    conn.execute(
        sql,
        params![
            setting.id,
            setting.display_name,
            setting.transport,
            // 新建时强制默认禁用，避免保存即生效。
            if expected_revision.is_none() {
                0
            } else {
                setting.enabled as i64
            },
            setting.config_json,
            now_ms,
            expected_revision.unwrap_or(0),
        ],
    )
    .map_err(sqlite_error)?;
    let loaded = list_mcp_settings(conn)?
        .into_iter()
        .find(|s| s.id == setting.id)
        .ok_or_else(|| Error::new(ErrorKind::Internal, "MCP 设置保存后读取失败"))?;
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_platform::ensure_schema;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("外键");
        ensure_schema(&conn).expect("建表");
        conn
    }

    fn policy() -> BudgetPolicyView {
        BudgetPolicyView {
            id: "b1".into(),
            display_name: "默认".into(),
            enabled: true,
            max_iterations: 40,
            max_wall_time_ms: 600_000,
            max_model_concurrency: 2,
            revision: 0,
        }
    }

    #[test]
    fn budget_policy_stores_execution_limits() {
        let conn = setup();
        let saved = save_budget_policy(&conn, 1_000, &policy(), None).expect("保存");
        assert_eq!(saved.max_model_concurrency, 2);
        assert_eq!(saved.revision, 1);
    }

    #[test]
    fn budget_policy_rejects_nonpositive_limits() {
        let conn = setup();
        let mut p = policy();
        p.max_iterations = 0;
        assert!(save_budget_policy(&conn, 1_000, &p, None).is_err());
    }

    #[test]
    fn mcp_new_server_is_disabled_by_default() {
        let conn = setup();
        let setting = McpSettingView {
            id: "x1".into(),
            display_name: "外部工具".into(),
            transport: "stdio".into(),
            enabled: true, // 用户勾选了启用
            config_json: "{}".into(),
            revision: 0,
        };
        let saved = save_mcp_setting(&conn, 1_000, &setting, None).expect("保存");
        assert!(!saved.enabled, "新建默认禁用，避免保存即生效");
    }

    #[test]
    fn mcp_config_rejects_credentials() {
        let conn = setup();
        let setting = McpSettingView {
            id: "x1".into(),
            display_name: "外部工具".into(),
            transport: "http".into(),
            enabled: false,
            config_json: r#"{"url":"https://x","token":"secret"}"#.into(),
            revision: 0,
        };
        let err = save_mcp_setting(&conn, 1_000, &setting, None).expect_err("应拒绝");
        assert!(err.message.contains("凭据"));
    }
}
