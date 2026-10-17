//! AI 平台持久化（设计文档 §42）：管理实体 + 运行记录。
//!
//! 建表语句集中在 [`schema`]，各实体的读写由同目录下的对应模块负责。

pub mod agents;
pub mod artifacts;
pub mod audits;
pub mod context_checkpoints;
pub mod context_references;
pub mod conversation;
pub mod execution_policy;
pub mod mcp;
pub mod mcp_server;
pub mod mcp_server_runtime;
pub mod model_bindings;
pub mod model_secret;
pub mod model_selection;
pub mod models;
pub mod plugins;
pub mod recovery;
pub mod run_events;
pub mod schema;
pub mod sessions;
pub mod settings;
pub mod tool_calls;
pub mod tool_settings;
pub mod usage_ledger;

use rusqlite::Connection;

use fluxdb_core::{Error, ErrorKind, Result};

/// 本期必须存在的 AI 表（设计文档 §42.10/§42.11）。
pub const REQUIRED_TABLES: &[&str] = &[
    "ai_model_profiles",
    "ai_budget_policies",
    "ai_agents",
    "ai_plugin_packages",
    "ai_plugin_instances",
    "ai_agent_plugin_bindings",
    "ai_mcp_settings",
    "ai_mcp_server_profiles",
    "ai_mcp_server_datasources",
    "ai_agent_model_bindings",
    "ai_agent_mcp_bindings",
    "ai_runtime_settings",
    "ai_tool_settings",
    "ai_panel_preferences",
    "ai_integration_secrets",
    "ai_mcp_server_sessions",
    "ai_mcp_server_requests",
    "ai_sessions",
    "ai_turns",
    "ai_messages",
    "ai_message_parts",
    "ai_session_ui_state",
    "ai_tool_calls",
    "ai_approvals",
    "ai_tool_audits",
    "ai_run_events",
    "ai_results",
    "ai_artifacts",
    "ai_artifact_sources",
    "ai_context_checkpoints",
    "ai_usage_ledger",
];

/// 加载共享数据密钥并构造 AES-GCM（模型凭据加解密用）。
pub fn load_ai_cipher(
    root: &std::path::Path,
) -> Result<(aes_gcm::Aes256Gcm, crate::secret_crypto::KeySource)> {
    crate::secret_crypto::load_cipher(root).map_err(|e| crate::credential::to_storage_error(&e))
}

/// 幂等创建全部 AI 表（首次打开数据库时调用）。
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(schema::MANAGEMENT_SQL)
        .map_err(crate::sqlite::sqlite_error)?;
    conn.execute_batch(schema::RUNTIME_SQL)
        .map_err(crate::sqlite::sqlite_error)?;
    conn.execute_batch(schema::PANEL_PREFERENCES_SQL)
        .map_err(crate::sqlite::sqlite_error)?;
    // 内置插件包/实例随应用注册（幂等）；缺失时设置页会显示为空而不是内置能力清单。
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    plugins::seed_builtin_plugins(conn, now_ms)?;
    agents::seed_builtin_agent(conn, now_ms)?;
    Ok(())
}

/// 校验 AI 表结构与当前设计一致：表齐全、外键全部指向存在的表。
///
/// 只做结构存在性校验，不比较完整列定义（避免每次启动都遍历全部列）。
/// 若同名表被外部改造成不兼容结构，这里给出明确错误而不是静默继续。
pub fn verify_ai_schema(conn: &Connection) -> Result<()> {
    for table in REQUIRED_TABLES {
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .map_err(crate::sqlite::sqlite_error)?
            != 0;
        if !exists {
            return Err(Error::new(
                ErrorKind::Internal,
                format!("AI 数据表缺失: {table}，请删除损坏的 AI 表后重新创建"),
            ));
        }
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list({table})"))
            .map_err(crate::sqlite::sqlite_error)?;
        let fks = stmt
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(crate::sqlite::sqlite_error)?;
        for parent in fks {
            let parent = parent.map_err(crate::sqlite::sqlite_error)?;
            let parent_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [&parent],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(crate::sqlite::sqlite_error)?
                != 0;
            if !parent_exists {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("AI 表 {table} 的外键目标 {parent} 不存在"),
                ));
            }
        }
    }
    Ok(())
}
