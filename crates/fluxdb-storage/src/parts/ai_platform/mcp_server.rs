//! 对外本地 MCP Server 的 Profile / 资源授权 / session / request 持久化（设计 §28.6、§42.10–42.11、T057）。
//!
//! 与 `mcp.rs`（本应用作为 **Client** 接入外部工具）严格区分：这两组表描述的是
//! 本应用 **对外** 暴露的 stdio Server。四条不可破坏的规则：
//! 1. 对象级 CAS：Profile 与 grant 的保存都带 `expected_revision`，命中 0 行即冲突。
//! 2. 默认最小权限：新建 Profile 强制 `enabled=0`、`write_mode=disabled`；`allowed_tool_ids=[]`
//!    表示不开放任何工具，**不解释为"全部工具"**。
//! 3. 空规则不等于全库：授权的对象范围必须先验证形状（SQL 需要 all_objects 或 table_allowlist，
//!    Redis 需要 all_keys 或 key_prefixes），空规则一律拒绝保存。
//! 4. 协议 request ID 只作关联，不是永远唯一的幂等键：记录身份靠 `(session_id, seq)`。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 写模式（与 DDL CHECK 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// 写工具不列出，直接调用也拒绝。
    Disabled,
    /// 用户对具体资源预授权受限写；执行时仍逐次校验范围。
    Scoped,
    /// 只有实际实现桌面审批桥后才可用；未实现时按 Unsupported 处理，不假装已有能力。
    DesktopApproval,
}

impl WriteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteMode::Disabled => "disabled",
            WriteMode::Scoped => "scoped",
            WriteMode::DesktopApproval => "desktop_approval",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "disabled" => Ok(WriteMode::Disabled),
            "scoped" => Ok(WriteMode::Scoped),
            "desktop_approval" => Ok(WriteMode::DesktopApproval),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知 MCP 写模式: {other}"),
            )),
        }
    }
}

/// Server Profile 视图（不含任何秘密）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProfileView {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub write_mode: WriteMode,
    /// 允许对外公开的工具 ID；空表示不开放任何工具（不是"全部"）。
    pub allowed_tool_ids: Vec<String>,
    pub options_json: String,
    pub max_inflight: i64,
    pub max_request_ms: i64,
    pub max_request_bytes: i64,
    pub max_response_bytes: i64,
    pub revision: i64,
}

/// Profile 保存输入。
#[derive(Debug, Clone)]
pub struct ServerProfileEdit {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub write_mode: WriteMode,
    pub allowed_tool_ids: Vec<String>,
    pub options_json: String,
    pub max_inflight: i64,
    pub max_request_ms: i64,
    pub max_request_bytes: i64,
    pub max_response_bytes: i64,
}

fn validate_options_json(raw: &str, label: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| Error::new(ErrorKind::Internal, format!("{label}不是合法 JSON")))?;
    if !value.is_object() {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("{label}必须是对象"),
        ));
    }
    // 秘密不得落到 Profile 普通配置：凭据只走已有 AES 连接凭据。
    for forbidden in ["api_key", "token", "password", "authorization", "secret"] {
        if value.get(forbidden).is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "MCP Server 配置不得包含凭据字段",
            ));
        }
    }
    Ok(())
}

/// 列出全部 Server Profile（管理页用）。
pub fn list_server_profiles(conn: &Connection) -> Result<Vec<ServerProfileView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, display_name, enabled, write_mode, allowed_tool_ids_json,
                    options_json, max_inflight, max_request_ms, max_request_bytes,
                    max_response_bytes, revision
               FROM ai_mcp_server_profiles ORDER BY display_name, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            display_name,
            enabled,
            write_mode,
            allowed_tool_ids,
            options_json,
            max_inflight,
            max_request_ms,
            max_request_bytes,
            max_response_bytes,
            revision,
        ) = row.map_err(sqlite_error)?;
        out.push(ServerProfileView {
            id,
            display_name,
            enabled,
            write_mode: WriteMode::parse(&write_mode)?,
            allowed_tool_ids: parse_string_list(&allowed_tool_ids, "allowed_tool_ids")?,
            options_json,
            max_inflight,
            max_request_ms,
            max_request_bytes,
            max_response_bytes,
            revision,
        });
    }
    Ok(out)
}

/// 读取单个 Server Profile；不存在返回 `None`。
pub fn load_server_profile(conn: &Connection, id: &str) -> Result<Option<ServerProfileView>> {
    Ok(list_server_profiles(conn)?.into_iter().find(|p| p.id == id))
}

/// 保存 Server Profile（对象级 CAS；`expected_revision=None` 表示首次创建）。
///
/// 新建强制 `enabled=0` 与 `write_mode=disabled`：保存即生效会让"看一眼配置"意外放开对外服务。
pub fn save_server_profile(
    conn: &Connection,
    now_ms: i64,
    edit: &ServerProfileEdit,
    expected_revision: Option<i64>,
) -> Result<ServerProfileView> {
    if edit.id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "Server Profile ID 不能为空",
        ));
    }
    validate_options_json(&edit.options_json, "Server Profile 配置")?;
    for (label, value) in [
        ("最大并发请求数", edit.max_inflight),
        ("单请求时限", edit.max_request_ms),
        ("请求字节上限", edit.max_request_bytes),
        ("响应字节上限", edit.max_response_bytes),
    ] {
        if value <= 0 {
            return Err(Error::new(
                ErrorKind::Internal,
                format!("MCP Server {label}必须为正"),
            ));
        }
    }
    let first_create = expected_revision.is_none();
    let enabled = if first_create {
        0
    } else {
        i64::from(edit.enabled)
    };
    let write_mode = if first_create {
        WriteMode::Disabled
    } else {
        edit.write_mode
    };
    let allowed_tool_ids = serde_json::to_string(&edit.allowed_tool_ids)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("工具白名单序列化失败: {e}")))?;
    let sql = "INSERT INTO ai_mcp_server_profiles (id, display_name, enabled, transport,
            write_mode, allowed_tool_ids_json, options_json, max_inflight, max_request_ms,
            max_request_bytes, max_response_bytes, revision, created_at_ms, updated_at_ms)
        VALUES (?1,?2,?3,'stdio',?4,?5,?6,?7,?8,?9,?10,1,?11,?11)
        ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, enabled=excluded.enabled,
            write_mode=excluded.write_mode, allowed_tool_ids_json=excluded.allowed_tool_ids_json,
            options_json=excluded.options_json, max_inflight=excluded.max_inflight,
            max_request_ms=excluded.max_request_ms, max_request_bytes=excluded.max_request_bytes,
            max_response_bytes=excluded.max_response_bytes,
            revision=ai_mcp_server_profiles.revision+1, updated_at_ms=excluded.updated_at_ms
        WHERE ai_mcp_server_profiles.revision=?12";
    let changed = conn
        .execute(
            sql,
            params![
                edit.id,
                edit.display_name,
                enabled,
                write_mode.as_str(),
                allowed_tool_ids,
                edit.options_json,
                edit.max_inflight,
                edit.max_request_ms,
                edit.max_request_bytes,
                edit.max_response_bytes,
                now_ms,
                expected_revision.unwrap_or(0),
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "MCP Server Profile 已被其他窗口修改，请重新加载",
        ));
    }
    load_server_profile(conn, &edit.id)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "MCP Server Profile 保存后读取失败"))
}

/// 资源授权视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceGrantView {
    pub id: String,
    pub profile_id: String,
    pub connection_id: i64,
    pub database_name: String,
    pub namespace: String,
    pub enabled: bool,
    pub allow_read: bool,
    pub allow_write: bool,
    pub object_policy_json: String,
    pub write_policy_json: String,
    pub max_rows: i64,
    pub revision: i64,
}

/// 资源授权保存输入。
#[derive(Debug, Clone)]
pub struct ResourceGrantEdit {
    pub id: String,
    pub profile_id: String,
    pub connection_id: i64,
    pub database_name: String,
    pub namespace: String,
    pub enabled: bool,
    pub allow_read: bool,
    pub allow_write: bool,
    pub object_policy_json: String,
    pub write_policy_json: String,
    pub max_rows: i64,
}

/// 授权的目标资源类型：决定对象/Key 范围字段的形状校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantResourceKind {
    /// 关系型：需要 `all_objects=true` 或非空 `table_allowlist`。
    Sql,
    /// Redis：需要 `all_keys=true` 或非空 `key_prefixes`。
    Redis,
}

/// 校验授权对象范围：空规则不等于全库通行（设计 §28.6.1）。
fn validate_object_policy(raw: &str, kind: GrantResourceKind) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| Error::new(ErrorKind::Internal, "对象范围规则不是合法 JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Internal, "对象范围规则必须是对象"))?;
    let non_empty_list = |key: &str| -> bool {
        object
            .get(key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| !items.is_empty())
    };
    let flag = |key: &str| -> bool {
        object
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let ok = match kind {
        GrantResourceKind::Sql => flag("all_objects") || non_empty_list("table_allowlist"),
        GrantResourceKind::Redis => flag("all_keys") || non_empty_list("key_prefixes"),
    };
    if !ok {
        return Err(Error::new(
            ErrorKind::Internal,
            match kind {
                GrantResourceKind::Sql => {
                    "SQL 授权必须显式声明 all_objects 或非空 table_allowlist，空规则不放行整库"
                }
                GrantResourceKind::Redis => {
                    "Redis 授权必须显式声明 all_keys 或非空 key_prefixes，空规则不放行整 DB"
                }
            },
        ));
    }
    Ok(())
}

fn validate_write_policy(raw: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| Error::new(ErrorKind::Internal, "写规则不是合法 JSON"))?;
    if !value.is_object() {
        return Err(Error::new(ErrorKind::Internal, "写规则必须是对象"));
    }
    Ok(())
}

/// 列出某 Profile 的资源授权。
pub fn list_resource_grants(conn: &Connection, profile_id: &str) -> Result<Vec<ResourceGrantView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, profile_id, connection_id, database_name, namespace, enabled,
                    allow_read, allow_write, object_policy_json, write_policy_json,
                    max_rows, revision
               FROM ai_mcp_server_resource_grants
              WHERE profile_id=?1 ORDER BY connection_id, database_name, namespace, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([profile_id], |row| {
            Ok(ResourceGrantView {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                connection_id: row.get(2)?,
                database_name: row.get(3)?,
                namespace: row.get(4)?,
                enabled: row.get::<_, i64>(5)? != 0,
                allow_read: row.get::<_, i64>(6)? != 0,
                allow_write: row.get::<_, i64>(7)? != 0,
                object_policy_json: row.get(8)?,
                write_policy_json: row.get(9)?,
                max_rows: row.get(10)?,
                revision: row.get(11)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

/// 读取单个资源授权。
pub fn load_resource_grant(conn: &Connection, id: &str) -> Result<Option<ResourceGrantView>> {
    let profile_id: Option<String> = conn
        .query_row(
            "SELECT profile_id FROM ai_mcp_server_resource_grants WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match profile_id {
        Some(profile_id) => Ok(list_resource_grants(conn, &profile_id)?
            .into_iter()
            .find(|g| g.id == id)),
        None => Ok(None),
    }
}

/// 保存资源授权（对象级 CAS；`expected_revision=None` 表示首次创建）。
///
/// `kind` 用于按资源类型校验对象范围形状，防止借另一数据库类型的规则放行。
pub fn save_resource_grant(
    conn: &Connection,
    now_ms: i64,
    edit: &ResourceGrantEdit,
    kind: GrantResourceKind,
    expected_revision: Option<i64>,
) -> Result<ResourceGrantView> {
    if edit.id.trim().is_empty() || edit.profile_id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "资源授权 ID 与 Profile ID 不能为空",
        ));
    }
    if !(1..=1000).contains(&edit.max_rows) {
        return Err(Error::new(
            ErrorKind::Internal,
            "MCP 单次最大行数须在 1..=1000",
        ));
    }
    validate_object_policy(&edit.object_policy_json, kind)?;
    validate_write_policy(&edit.write_policy_json)?;
    let sql = "INSERT INTO ai_mcp_server_resource_grants (id, profile_id, connection_id,
            database_name, namespace, enabled, allow_read, allow_write, object_policy_json,
            write_policy_json, max_rows, revision, created_at_ms, updated_at_ms)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?12)
        ON CONFLICT(id) DO UPDATE SET connection_id=excluded.connection_id,
            database_name=excluded.database_name, namespace=excluded.namespace,
            enabled=excluded.enabled, allow_read=excluded.allow_read,
            allow_write=excluded.allow_write, object_policy_json=excluded.object_policy_json,
            write_policy_json=excluded.write_policy_json, max_rows=excluded.max_rows,
            revision=ai_mcp_server_resource_grants.revision+1, updated_at_ms=excluded.updated_at_ms
        WHERE ai_mcp_server_resource_grants.revision=?13";
    let changed = conn
        .execute(
            sql,
            params![
                edit.id,
                edit.profile_id,
                edit.connection_id,
                edit.database_name,
                edit.namespace,
                i64::from(edit.enabled),
                i64::from(edit.allow_read),
                i64::from(edit.allow_write),
                edit.object_policy_json,
                edit.write_policy_json,
                edit.max_rows,
                now_ms,
                expected_revision.unwrap_or(0),
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "MCP 资源授权已被其他窗口修改或唯一约束冲突，请重新加载",
        ));
    }
    load_resource_grant(conn, &edit.id)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "MCP 资源授权保存后读取失败"))
}

/// 删除资源授权（用户收窄范围）。
pub fn delete_resource_grant(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ai_mcp_server_resource_grants WHERE id=?1",
        [id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn parse_string_list(raw: &str, label: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("{label} 解析失败: {e}")))
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

    fn profile() -> ServerProfileEdit {
        ServerProfileEdit {
            id: "p1".into(),
            display_name: "本地".into(),
            enabled: true,
            write_mode: WriteMode::Scoped,
            allowed_tool_ids: vec!["db.query".into()],
            options_json: "{}".into(),
            max_inflight: 4,
            max_request_ms: 30_000,
            max_request_bytes: 262_144,
            max_response_bytes: 131_072,
        }
    }

    fn grant() -> ResourceGrantEdit {
        ResourceGrantEdit {
            id: "g1".into(),
            profile_id: "p1".into(),
            connection_id: 1,
            database_name: "sales".into(),
            namespace: "".into(),
            enabled: true,
            allow_read: true,
            allow_write: false,
            object_policy_json: r#"{"all_objects":true}"#.into(),
            write_policy_json: "{}".into(),
            max_rows: 200,
        }
    }

    #[test]
    fn new_profile_is_disabled_and_readonly() {
        let conn = setup();
        let saved = save_server_profile(&conn, 1_000, &profile(), None).expect("保存");
        assert!(!saved.enabled, "新建默认禁用");
        assert_eq!(saved.write_mode, WriteMode::Disabled, "新建默认只读");
        assert_eq!(saved.revision, 1);
    }

    #[test]
    fn profile_save_uses_object_cas() {
        let conn = setup();
        let saved = save_server_profile(&conn, 1_000, &profile(), None).expect("首次");
        let mut edit = profile();
        edit.display_name = "改".into();
        let updated = save_server_profile(&conn, 1_001, &edit, Some(saved.revision)).expect("更新");
        assert_eq!(updated.revision, saved.revision + 1);
        assert!(updated.enabled, "已有行允许启用");
        let err = save_server_profile(&conn, 1_002, &edit, Some(saved.revision)).unwrap_err();
        assert!(err.message.contains("修改"), "CAS 冲突: {}", err.message);
    }

    #[test]
    fn profile_rejects_credentials_in_options() {
        let conn = setup();
        let mut edit = profile();
        edit.options_json = r#"{"api_key":"x"}"#.into();
        assert!(save_server_profile(&conn, 1_000, &edit, None).is_err());
    }

    #[test]
    fn empty_object_policy_is_rejected_for_sql_and_redis() {
        let conn = setup();
        save_server_profile(&conn, 1_000, &profile(), None).expect("profile");
        let mut g = grant();
        g.object_policy_json = "{}".into();
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Sql, None).is_err());
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Redis, None).is_err());
    }

    #[test]
    fn redis_grant_requires_all_keys_or_prefixes() {
        let conn = setup();
        save_server_profile(&conn, 1_000, &profile(), None).expect("profile");
        let mut g = grant();
        // SQL 形状的 all_objects 不能借用于 Redis。
        g.object_policy_json = r#"{"all_objects":true}"#.into();
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Redis, None).is_err());
        g.object_policy_json = r#"{"key_prefixes":["cache:"]}"#.into();
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Redis, None).is_ok());
    }

    #[test]
    fn grant_rejects_out_of_range_rows() {
        let conn = setup();
        save_server_profile(&conn, 1_000, &profile(), None).expect("profile");
        let mut g = grant();
        g.max_rows = 5000;
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Sql, None).is_err());
    }

    #[test]
    fn grant_requires_existing_profile() {
        let conn = setup();
        let mut g = grant();
        g.profile_id = "missing".into();
        assert!(save_resource_grant(&conn, 1_000, &g, GrantResourceKind::Sql, None).is_err());
    }
}
