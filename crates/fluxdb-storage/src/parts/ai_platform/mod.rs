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
#[cfg(test)]
mod sessions_tests;
#[cfg(test)]
mod sessions_verify_tests;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite;
    use std::path::PathBuf;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("开启外键");
        conn
    }

    #[test]
    fn creates_all_required_tables() {
        let conn = memory_conn();
        ensure_schema(&conn).expect("首次建表");
        for table in REQUIRED_TABLES {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("查询表清单");
            assert_eq!(count, 1, "缺少表 {table}");
        }
        verify_ai_schema(&conn).expect("结构校验");
        let removed_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='ai_knowledge_entries'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(removed_table_count, 0, "新库不再创建已移除的知识表");
    }

    #[test]
    fn create_schema_is_idempotent() {
        let dir = unique_temp_dir();
        // 走真实的建表入口两次，模拟重复打开同一份数据库。
        sqlite::create_schema(&sqlite::open(&dir).expect("首次打开")).expect("首次建表");
        sqlite::create_schema(&sqlite::open(&dir).expect("再次打开")).expect("重复建表应幂等");
        verify_ai_schema(&sqlite::open(&dir).expect("校验打开")).expect("重复建表后结构仍有效");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let conn = memory_conn();
        ensure_schema(&conn).expect("建表");
        let result = conn.execute(
            "INSERT INTO ai_agents (id, display_name, enabled, revision, created_at_ms, updated_at_ms)
             VALUES ('a', 'Agent', 0, 1, 0, 0)",
            [],
        );
        assert!(result.is_ok());
        // 引用不存在的模型应被外键拒绝
        let bad = conn.execute(
            "UPDATE ai_agents SET default_model_profile_id='missing' WHERE id='a'",
            [],
        );
        assert!(bad.is_err(), "模型外键必须生效");
    }

    #[test]
    fn model_secret_columns_require_full_tuple() {
        let conn = memory_conn();
        ensure_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO ai_model_profiles
               (id, display_name, provider, api_protocol, model_kind, model, auth_mode,
                revision, created_at_ms, updated_at_ms)
             VALUES ('m', 'M', 'p', 'test', 'generation', 'mock', 'api_key', 1, 0, 0)",
            [],
        )
        .expect("插入模型");
        // 只填密文不填 nonce/版本应违反 CHECK
        let bad = conn.execute(
            "UPDATE ai_model_profiles SET credential_ciphertext=?1 WHERE id='m'",
            [vec![0u8; 32]],
        );
        assert!(bad.is_err(), "密文列必须成组出现");
        // 完整元组应可写入
        conn.execute(
            "UPDATE ai_model_profiles
                SET credential_ciphertext=?1, credential_nonce=?2, cipher_version=1,
                    key_id='k', secret_revision=1
              WHERE id='m'",
            rusqlite::params![vec![0u8; 32], vec![0u8; 12]],
        )
        .expect("完整密文元组");
    }

    #[test]
    fn verify_reports_missing_table() {
        let conn = memory_conn();
        ensure_schema(&conn).expect("建表");
        conn.execute_batch("DROP TABLE ai_tool_audits")
            .expect("删除表模拟损坏");
        let err = verify_ai_schema(&conn).expect_err("应报缺表");
        assert!(err.message.contains("ai_tool_audits"), "错误信息应指出缺表");
    }

    #[test]
    fn agent_prompt_is_direct_column_without_prompt_tables() {
        let conn = memory_conn();
        ensure_schema(&conn).expect("建表");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'ai_prompt%'",
                [],
                |row| row.get(0),
            )
            .expect("查询提示词表");
        assert_eq!(count, 0, "提示词直接存在 Agent 行，不建模板/版本表");
    }

    fn unique_temp_dir() -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("fluxdb-ai-schema-{nanos}"))
    }
}
