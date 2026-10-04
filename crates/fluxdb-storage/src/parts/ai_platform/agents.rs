//! Agent 配置的读取与保存（设计文档 §31.8、T011）。
//!
//! 提示词直接存在 `ai_agents.system_prompt`，随 Agent 一起做对象级 CAS；
//! 不建提示词模板/版本表，也没有发布流程。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 提示词长度上限（UTF-8 字节）。
pub const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;

/// Agent 配置（公开视图；不含任何凭据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
    pub default_model_profile_id: Option<String>,
    pub system_prompt: String,
    pub budget_policy_id: Option<String>,
    pub allowed_tools: Vec<String>,
    pub revision: i64,
}

/// Agent 编辑输入。
#[derive(Debug, Clone)]
pub struct AgentEdit {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
    pub default_model_profile_id: Option<String>,
    pub system_prompt: String,
    pub budget_policy_id: Option<String>,
    pub allowed_tools: Vec<String>,
}

fn validate(edit: &AgentEdit) -> Result<()> {
    if edit.id.trim().is_empty() || edit.display_name.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "Agent ID 与名称不能为空"));
    }
    if edit.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        return Err(Error::new(ErrorKind::Internal, "提示词超过 64 KiB 上限"));
    }
    Ok(())
}

fn tools_json(tools: &[String]) -> Result<String> {
    serde_json::to_string(tools)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("工具列表序列化失败: {e}")))
}

/// 新建 Agent（`expected_revision = None`）。
pub fn create_agent(conn: &Connection, now_ms: i64, edit: &AgentEdit) -> Result<AgentView> {
    validate(edit)?;
    if load_agent(conn, &edit.id).is_ok() {
        return Err(Error::new(ErrorKind::Internal, "Agent 已存在"));
    }
    conn.execute(
        "INSERT INTO ai_agents (id, display_name, description, enabled,
            default_model_profile_id, system_prompt, budget_policy_id,
            allowed_tools_json, revision, created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?9)",
        params![
            edit.id,
            edit.display_name,
            edit.description,
            edit.enabled as i64,
            edit.default_model_profile_id,
            edit.system_prompt,
            edit.budget_policy_id,
            tools_json(&edit.allowed_tools)?,
            now_ms,
        ],
    )
    .map_err(sqlite_error)?;
    load_agent(conn, &edit.id)
}

/// 更新 Agent（对象级 CAS）。保存只影响下一个 Turn。
pub fn save_agent(
    conn: &Connection,
    now_ms: i64,
    edit: &AgentEdit,
    expected_revision: i64,
) -> Result<AgentView> {
    validate(edit)?;
    let current = load_agent(conn, &edit.id)?;
    if current.revision != expected_revision {
        return Err(Error::new(
            ErrorKind::Internal,
            "Agent 已被其他窗口修改，请重新加载",
        ));
    }
    let changed = conn
        .execute(
            "UPDATE ai_agents SET display_name=?2, description=?3, enabled=?4,
                default_model_profile_id=?5, system_prompt=?6, budget_policy_id=?7,
                allowed_tools_json=?8, revision=revision+1, updated_at_ms=?9
              WHERE id=?1 AND revision=?10",
            params![
                edit.id,
                edit.display_name,
                edit.description,
                edit.enabled as i64,
                edit.default_model_profile_id,
                edit.system_prompt,
                edit.budget_policy_id,
                tools_json(&edit.allowed_tools)?,
                now_ms,
                expected_revision,
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "Agent 已被其他窗口修改，请重新加载",
        ));
    }
    load_agent(conn, &edit.id)
}

/// 读取单个 Agent。
pub fn load_agent(conn: &Connection, id: &str) -> Result<AgentView> {
    let row = conn
        .query_row(
            "SELECT id, display_name, description, enabled, default_model_profile_id,
                    system_prompt, budget_policy_id, allowed_tools_json, revision
               FROM ai_agents WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "Agent 不存在"))?;
    let allowed_tools: Vec<String> = serde_json::from_str(&row.7).unwrap_or_default();
    Ok(AgentView {
        id: row.0,
        display_name: row.1,
        description: row.2,
        enabled: row.3 != 0,
        default_model_profile_id: row.4,
        system_prompt: row.5,
        budget_policy_id: row.6,
        allowed_tools,
        revision: row.8,
    })
}

/// 按更新时间倒序列出全部 Agent（本期数量少，不分页）。
pub fn list_agents(conn: &Connection) -> Result<Vec<AgentView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_agents ORDER BY updated_at_ms DESC, id")
        .map_err(sqlite_error)?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_agent(conn, &id)).collect()
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

    fn edit(id: &str) -> AgentEdit {
        AgentEdit {
            id: id.into(),
            display_name: "通用数据库 Agent".into(),
            description: "探索并交付可追溯结果".into(),
            enabled: true,
            default_model_profile_id: None,
            system_prompt: "先确认真实结构再行动。".into(),
            budget_policy_id: None,
            allowed_tools: vec!["db.query".into(), "db.search_objects".into()],
        }
    }

    #[test]
    fn create_and_load_roundtrip() {
        let conn = setup();
        let view = create_agent(&conn, 1_000, &edit("a1")).expect("新建");
        assert_eq!(view.revision, 1);
        assert_eq!(view.system_prompt, "先确认真实结构再行动。");
        assert_eq!(view.allowed_tools, vec!["db.query", "db.search_objects"]);
    }

    #[test]
    fn prompt_is_edited_in_place_without_version_table() {
        let conn = setup();
        let view = create_agent(&conn, 1_000, &edit("a1")).expect("新建");
        let mut e = edit("a1");
        e.system_prompt = "新的提示词内容。".into();
        let saved = save_agent(&conn, 2_000, &e, view.revision).expect("保存");
        assert_eq!(saved.system_prompt, "新的提示词内容。");
        assert_eq!(saved.revision, 2, "revision 只做并发控制");
    }

    #[test]
    fn stale_revision_is_rejected() {
        let conn = setup();
        let view = create_agent(&conn, 1_000, &edit("a1")).expect("新建");
        let mut e = edit("a1");
        e.display_name = "并发".into();
        let err = save_agent(&conn, 2_000, &e, view.revision + 5).expect_err("应拒绝");
        assert!(err.message.contains("重新加载"));
    }

    #[test]
    fn oversized_prompt_is_rejected() {
        let conn = setup();
        let mut e = edit("a1");
        e.system_prompt = "x".repeat(MAX_SYSTEM_PROMPT_BYTES + 1);
        let err = create_agent(&conn, 1_000, &e).expect_err("超长提示词应拒绝");
        assert!(err.message.contains("64 KiB"));
    }

    #[test]
    fn duplicate_create_is_rejected() {
        let conn = setup();
        create_agent(&conn, 1_000, &edit("a1")).expect("新建");
        assert!(create_agent(&conn, 2_000, &edit("a1")).is_err());
    }

    #[test]
    fn default_model_must_exist() {
        let conn = setup();
        let mut e = edit("a1");
        e.default_model_profile_id = Some("missing".into());
        assert!(
            create_agent(&conn, 1_000, &e).is_err(),
            "外键应拒绝悬空模型引用"
        );
    }

    #[test]
    fn list_returns_all_agents() {
        let conn = setup();
        create_agent(&conn, 1_000, &edit("a1")).expect("a1");
        create_agent(&conn, 2_000, &edit("a2")).expect("a2");
        let list = list_agents(&conn).expect("列出");
        assert_eq!(list.len(), 2);
    }
}
