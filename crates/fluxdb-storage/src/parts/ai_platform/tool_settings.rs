//! 标准工具的全局启停（设置页「工具与 MCP → 工具」）。
//!
//! 全局开关与 Agent 白名单是两层收窄：全局关闭的工具既不下发给模型，
//! 也不允许被任何 Agent 重新打开。已停用的工具 id 逐行保存，避免把
//! 目录之外或未来新增的工具误当作"已关闭"。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 一条工具的全局启停状态（对象级 CAS）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSettingView {
    pub tool_id: String,
    pub enabled: bool,
    pub revision: i64,
}

/// 工具 id 校验：与宿主 ToolId 同构，拒绝空白与超长 id。
fn validate_tool_id(tool_id: &str) -> Result<()> {
    let trimmed = tool_id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(Error::new(ErrorKind::Internal, "工具 ID 不合法"));
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(Error::new(ErrorKind::Internal, "工具 ID 不合法"));
    }
    Ok(())
}

/// 列出全部工具开关行；未落库的工具视为默认启用（不在结果中）。
pub fn list_tool_settings(conn: &Connection) -> Result<Vec<ToolSettingView>> {
    let mut stmt = conn
        .prepare("SELECT tool_id, enabled, revision FROM ai_tool_settings ORDER BY tool_id")
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ToolSettingView {
                tool_id: row.get(0)?,
                enabled: row.get::<_, i64>(1)? != 0,
                revision: row.get(2)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

/// 读取单个工具开关；没有行表示默认启用。
pub fn load_tool_setting(conn: &Connection, tool_id: &str) -> Result<Option<ToolSettingView>> {
    conn.query_row(
        "SELECT tool_id, enabled, revision FROM ai_tool_settings WHERE tool_id=?1",
        [tool_id],
        |row| {
            Ok(ToolSettingView {
                tool_id: row.get(0)?,
                enabled: row.get::<_, i64>(1)? != 0,
                revision: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// 保存单个工具启停（upsert + CAS；`expected_revision=0` 表示首次创建）。
pub fn save_tool_setting(
    conn: &Connection,
    now_ms: i64,
    tool_id: &str,
    enabled: bool,
    expected_revision: i64,
) -> Result<ToolSettingView> {
    validate_tool_id(tool_id)?;
    let changed = conn
        .execute(
            "INSERT INTO ai_tool_settings (tool_id, enabled, revision, updated_at_ms)
             VALUES (?1,?2,1,?3)
             ON CONFLICT(tool_id) DO UPDATE SET
                enabled=excluded.enabled,
                revision=ai_tool_settings.revision+1,
                updated_at_ms=excluded.updated_at_ms
              WHERE ai_tool_settings.revision=?4",
            params![tool_id, i64::from(enabled), now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "工具启停状态已被其他窗口修改，请重新加载",
        ));
    }
    load_tool_setting(conn, tool_id)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "工具启停状态写入后丢失"))
}

/// 已全局停用的工具 id 集合（供 Agent 白名单收窄与运行时过滤）。
pub fn disabled_tool_ids(conn: &Connection) -> Result<std::collections::BTreeSet<String>> {
    Ok(list_tool_settings(conn)?
        .into_iter()
        .filter(|setting| !setting.enabled)
        .map(|setting| setting.tool_id)
        .collect())
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

    #[test]
    fn missing_row_means_enabled_by_default() {
        let conn = setup();
        assert!(load_tool_setting(&conn, "db.query").unwrap().is_none());
        assert!(disabled_tool_ids(&conn).unwrap().is_empty());
    }

    #[test]
    fn toggle_round_trips_and_detects_stale_revision() {
        let conn = setup();
        let off = save_tool_setting(&conn, 1_000, "db.query", false, 0).expect("首次关闭");
        assert_eq!(off.revision, 1);
        assert!(!off.enabled);
        assert_eq!(
            disabled_tool_ids(&conn).unwrap(),
            ["db.query".to_string()].into()
        );
        // 旧 revision 不能再写：并发窗口已经改过状态。
        assert!(save_tool_setting(&conn, 2_000, "db.query", true, 0).is_err());
        let on = save_tool_setting(&conn, 2_000, "db.query", true, off.revision).expect("重新打开");
        assert!(on.enabled);
        assert_eq!(on.revision, 2);
        assert!(disabled_tool_ids(&conn).unwrap().is_empty());
    }

    #[test]
    fn settings_are_independent_per_tool() {
        let conn = setup();
        save_tool_setting(&conn, 1, "db.query", false, 0).unwrap();
        save_tool_setting(&conn, 2, "db.execute", true, 0).unwrap();
        let listed = list_tool_settings(&conn).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            disabled_tool_ids(&conn).unwrap(),
            ["db.query".to_string()].into()
        );
    }

    #[test]
    fn illegal_tool_id_is_rejected() {
        let conn = setup();
        assert!(save_tool_setting(&conn, 1, "", false, 0).is_err());
        assert!(save_tool_setting(&conn, 1, "db query", false, 0).is_err());
        assert!(save_tool_setting(&conn, 1, &"x".repeat(129), false, 0).is_err());
    }
}
