//! Agent 的模型角色绑定（`ai_agent_model_bindings`，设计 §17/§18/T033）。
//!
//! 模型引用统一落引用列，不藏进插件配置 JSON。每行独立 `revision` 做对象级 CAS；
//! `binding_source=system` 由内置 Jev/默认规则 reconcile 维护，不得覆盖 `user` 行。
//!
//! 角色语义（§42 字段约束补充）：
//! - `main_candidate` / `compaction` / `artifact` 必须指向 `generation` 模型；
//! - `decision` 必须指向 `decision` 模型；
//! - 决策候选不允许 `required` 失败策略（Jev 只能是可选优化）。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

use super::models::{ModelKind, ModelProfileView};

/// 模型角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    MainCandidate,
    Compaction,
    Artifact,
    Decision,
}

impl ModelRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::MainCandidate => "main_candidate",
            ModelRole::Compaction => "compaction",
            ModelRole::Artifact => "artifact",
            ModelRole::Decision => "decision",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "main_candidate" => Ok(ModelRole::MainCandidate),
            "compaction" => Ok(ModelRole::Compaction),
            "artifact" => Ok(ModelRole::Artifact),
            "decision" => Ok(ModelRole::Decision),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知模型角色: {other}"),
            )),
        }
    }

    /// 该角色要求的模型用途。
    pub fn required_kind(self) -> ModelKind {
        match self {
            ModelRole::Decision => ModelKind::Decision,
            _ => ModelKind::Generation,
        }
    }
}

/// 绑定来源：用户明确设置，或内置系统 reconcile 维护。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    User,
    System,
}

impl BindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingSource::User => "user",
            BindingSource::System => "system",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(BindingSource::User),
            "system" => Ok(BindingSource::System),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知绑定来源: {other}"),
            )),
        }
    }
}

/// 绑定行公开视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelBindingView {
    pub id: String,
    pub agent_id: String,
    pub model_profile_id: String,
    pub role: ModelRole,
    pub binding_source: BindingSource,
    pub enabled: bool,
    pub priority: i64,
    pub revision: i64,
}

/// 新建绑定输入。
#[derive(Debug, Clone)]
pub struct ModelBindingEdit {
    pub id: String,
    pub agent_id: String,
    pub model_profile_id: String,
    pub role: ModelRole,
    pub binding_source: BindingSource,
    pub enabled: bool,
    pub priority: i64,
}

/// 校验 Agent、模型存在性以及角色/用途一致性。
///
/// 应用层必须在同一事务内调用，避免"先校验后写入"之间引用被改。
fn validate(
    conn: &Connection,
    agent_id: &str,
    model: &ModelProfileView,
    role: ModelRole,
) -> Result<()> {
    let agent_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_agents WHERE id=?1)",
            [agent_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if !agent_exists {
        return Err(Error::new(ErrorKind::Internal, "Agent 不存在"));
    }
    if model.kind != role.required_kind() {
        return Err(Error::new(
            ErrorKind::Internal,
            format!(
                "模型用途与角色不匹配：角色 {} 需要 {} 模型",
                role.as_str(),
                match role.required_kind() {
                    ModelKind::Generation => "生成",
                    ModelKind::Decision => "决策",
                }
            ),
        ));
    }
    Ok(())
}

/// 新建绑定（对象级 CAS 起点 `revision=1`）。重复 (agent, role, model) 由唯一约束拒绝。
pub fn create_binding(
    conn: &Connection,
    now_ms: i64,
    edit: &ModelBindingEdit,
) -> Result<ModelBindingView> {
    let model = super::models::load_model(conn, &edit.model_profile_id)?;
    validate(conn, &edit.agent_id, &model, edit.role)?;
    conn.execute(
        "INSERT INTO ai_agent_model_bindings
            (id, agent_id, model_profile_id, role, binding_source, enabled, priority,
             options_json, revision, created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'{}',1,?8,?8)",
        params![
            edit.id,
            edit.agent_id,
            edit.model_profile_id,
            edit.role.as_str(),
            edit.binding_source.as_str(),
            edit.enabled as i64,
            edit.priority,
            now_ms,
        ],
    )
    .map_err(sqlite_error)?;
    load_binding(conn, &edit.id)
}

/// 在事务内幂等建立 system 绑定：已存在任意来源的 (agent, role, model) 记录时保持不动。
///
/// 返回是否为本次新建。用户手动绑定/停用优先，reconcile 不得覆盖。
pub fn ensure_system_binding(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    agent_id: &str,
    model_profile_id: &str,
    role: ModelRole,
    priority: i64,
) -> Result<bool> {
    let model = super::models::load_model(conn, model_profile_id)?;
    validate(conn, agent_id, &model, role)?;
    let changed = conn
        .execute(
            "INSERT INTO ai_agent_model_bindings
                (id, agent_id, model_profile_id, role, binding_source, enabled, priority,
                 options_json, revision, created_at_ms, updated_at_ms)
             VALUES (?1,?2,?3,?4,'system',1,?5,'{}',1,?6,?6)
             ON CONFLICT(agent_id, role, model_profile_id) DO NOTHING",
            params![
                id,
                agent_id,
                model_profile_id,
                role.as_str(),
                priority,
                now_ms
            ],
        )
        .map_err(sqlite_error)?;
    Ok(changed == 1)
}

/// 读取单个绑定。
pub fn load_binding(conn: &Connection, id: &str) -> Result<ModelBindingView> {
    let row = conn
        .query_row(
            "SELECT id, agent_id, model_profile_id, role, binding_source, enabled, priority, revision
               FROM ai_agent_model_bindings WHERE id=?1",
            [id],
            raw_binding_row,
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "模型绑定不存在"))?;
    parse_binding_row(row)
}

/// 列出某 Agent 的全部绑定，按角色、优先级、ID 稳定排序。
pub fn list_agent_bindings(conn: &Connection, agent_id: &str) -> Result<Vec<ModelBindingView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, model_profile_id, role, binding_source, enabled, priority, revision
               FROM ai_agent_model_bindings WHERE agent_id=?1
              ORDER BY role, priority, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([agent_id], raw_binding_row)
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    rows.into_iter().map(parse_binding_row).collect()
}

/// 列出某 Agent 某角色的启用绑定，按优先级、ID 排序（路由候选顺序）。
pub fn list_role_bindings(
    conn: &Connection,
    agent_id: &str,
    role: ModelRole,
) -> Result<Vec<ModelBindingView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, model_profile_id, role, binding_source, enabled, priority, revision
               FROM ai_agent_model_bindings
              WHERE agent_id=?1 AND role=?2 AND enabled=1
              ORDER BY priority, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![agent_id, role.as_str()], raw_binding_row)
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    rows.into_iter().map(parse_binding_row).collect()
}

/// 删除某 Agent 某角色下**非保留模型**的 system 绑定行。
///
/// reconcile 换模型/撤销自动选择时用它清理旧行，避免同一角色同时挂着多个 system
/// 候选。用户绑定（`binding_source='user'`）永不删除，保留项按模型 ID 精确匹配。
pub fn prune_system_role_bindings(
    conn: &Connection,
    now_ms: i64,
    agent_id: &str,
    role: ModelRole,
    keep_model_profile_id: Option<&str>,
) -> Result<usize> {
    let changed = conn
        .execute(
            "UPDATE ai_agent_model_bindings
                SET enabled=0, revision=revision+1, updated_at_ms=?3
              WHERE agent_id=?1 AND role=?2 AND binding_source='system' AND enabled=1
                AND COALESCE(?4, '') <> model_profile_id",
            params![agent_id, role.as_str(), now_ms, keep_model_profile_id],
        )
        .map_err(sqlite_error)?;
    Ok(changed)
}

/// 重新启用某角色下指向指定模型的 system 绑定（换模型后把旧的自动选择重新启用）。
///
/// 只操作 `binding_source='system'` 行；用户绑定与其它模型的历史行不动。
pub fn enable_system_role_bindings(
    conn: &Connection,
    now_ms: i64,
    agent_id: &str,
    role: ModelRole,
    model_profile_id: &str,
) -> Result<usize> {
    let changed = conn
        .execute(
            "UPDATE ai_agent_model_bindings
                SET enabled=1, revision=revision+1, updated_at_ms=?4
              WHERE agent_id=?1 AND role=?2 AND binding_source='system'
                AND model_profile_id=?3 AND enabled=0",
            params![agent_id, role.as_str(), model_profile_id, now_ms],
        )
        .map_err(sqlite_error)?;
    Ok(changed)
}

/// 启停绑定（对象级 CAS）。
pub fn set_binding_enabled(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    enabled: bool,
    expected_revision: i64,
) -> Result<ModelBindingView> {
    let changed = conn
        .execute(
            "UPDATE ai_agent_model_bindings SET enabled=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, enabled as i64, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型绑定已变更，请重新加载",
        ));
    }
    load_binding(conn, id)
}

/// 删除绑定（对象级 CAS）。
pub fn delete_binding(conn: &Connection, id: &str, expected_revision: i64) -> Result<()> {
    let changed = conn
        .execute(
            "DELETE FROM ai_agent_model_bindings WHERE id=?1 AND revision=?2",
            params![id, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型绑定不存在或状态已变化，请重新加载",
        ));
    }
    Ok(())
}

/// 设置某 Agent 某角色的**用户**模型绑定。
///
/// `Some(model)` 选中该模型；`None` 表示"跟随该角色的默认"（清除用户绑定）。
/// 只增删 `binding_source='user'` 行，不动 system reconcile 维护的绑定；换模型时按旧行的
/// `expected_revision` 做对象级 CAS，避免另一个窗口的编辑被静默覆盖。
pub fn set_role_model(
    conn: &Connection,
    now_ms: i64,
    new_id: &str,
    agent_id: &str,
    role: ModelRole,
    model_profile_id: Option<&str>,
    expected_revision: Option<i64>,
) -> Result<Option<ModelBindingView>> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let existing: Option<ModelBindingView> = tx
        .query_row(
            "SELECT id, agent_id, model_profile_id, role, binding_source, enabled, priority, revision
               FROM ai_agent_model_bindings
              WHERE agent_id=?1 AND role=?2 AND binding_source='user'
              ORDER BY priority, id
              LIMIT 1",
            params![agent_id, role.as_str()],
            raw_binding_row,
        )
        .optional()
        .map_err(sqlite_error)?
        .map(parse_binding_row)
        .transpose()?;

    let Some(model_profile_id) = model_profile_id else {
        if let Some(existing) = existing {
            check_revision(&existing, expected_revision)?;
            tx.execute(
                "DELETE FROM ai_agent_model_bindings WHERE id=?1",
                [&existing.id],
            )
            .map_err(sqlite_error)?;
        }
        tx.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    let model = super::models::load_model(&tx, model_profile_id)?;
    validate(&tx, agent_id, &model, role)?;
    match existing {
        Some(existing) if existing.model_profile_id == model_profile_id => {
            tx.commit().map_err(sqlite_error)?;
            return Ok(Some(existing));
        }
        Some(existing) => {
            check_revision(&existing, expected_revision)?;
            tx.execute(
                "DELETE FROM ai_agent_model_bindings WHERE id=?1",
                [&existing.id],
            )
            .map_err(sqlite_error)?;
        }
        None => {}
    }
    // 同 (agent, role, model) 已由 system reconcile 建立时保持原行，不制造重复绑定。
    if let Some(existing) = tx
        .query_row(
            "SELECT id, agent_id, model_profile_id, role, binding_source, enabled, priority, revision
               FROM ai_agent_model_bindings
              WHERE agent_id=?1 AND role=?2 AND model_profile_id=?3",
            params![agent_id, role.as_str(), model_profile_id],
            raw_binding_row,
        )
        .optional()
        .map_err(sqlite_error)?
        .map(parse_binding_row)
        .transpose()?
    {
        tx.commit().map_err(sqlite_error)?;
        return Ok(Some(existing));
    }
    tx.execute(
        "INSERT INTO ai_agent_model_bindings
            (id, agent_id, model_profile_id, role, binding_source, enabled, priority,
             options_json, revision, created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,?4,'user',1,100,'{}',1,?5,?5)",
        params![new_id, agent_id, model_profile_id, role.as_str(), now_ms],
    )
    .map_err(sqlite_error)?;
    let binding = load_binding(&tx, new_id)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Some(binding))
}

/// 换模型时的对象级 CAS：旧行 revision 与调用方看到的不符即失败。
fn check_revision(existing: &ModelBindingView, expected_revision: Option<i64>) -> Result<()> {
    if expected_revision.is_some_and(|expected| expected != existing.revision) {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型绑定已变更，请重新加载",
        ));
    }
    Ok(())
}

/// 原始列（字符串/整数），枚举解析放到闭包之外，避免把存储损坏伪装成 SQL 失败。
type RawBindingRow = (String, String, String, String, String, i64, i64, i64);

fn raw_binding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawBindingRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn parse_binding_row(row: RawBindingRow) -> Result<ModelBindingView> {
    let (id, agent_id, model_profile_id, role, source, enabled, priority, revision) = row;
    Ok(ModelBindingView {
        id,
        agent_id,
        model_profile_id,
        role: ModelRole::parse(&role)?,
        binding_source: BindingSource::parse(&source)?,
        enabled: enabled != 0,
        priority,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_platform::ensure_schema;
    use crate::ai_platform::models::{ApiKeyChange, ModelEdit};
    use rusqlite::Connection;

    fn setup() -> (Connection, aes_gcm::Aes256Gcm) {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("外键");
        ensure_schema(&conn).expect("建表");
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-model-bindings-{}-{:016x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let (cipher, _) = crate::secret_crypto::load_cipher(&dir).expect("测试密钥");
        crate::ai_platform::agents::create_agent(
            &conn,
            1_000,
            &crate::ai_platform::agents::AgentEdit {
                max_iterations: 24,
                compression_trigger_percent: 80,
                id: "a1".into(),
                display_name: "Agent".into(),
                description: String::new(),
                enabled: true,
                default_model_profile_id: None,
                system_prompt: String::new(),
                budget_policy_id: None,
                allowed_tools: vec![],
            },
        )
        .expect("建 Agent");
        (conn, cipher)
    }

    fn make_model(conn: &Connection, cipher: &aes_gcm::Aes256Gcm, id: &str, kind: ModelKind) {
        crate::ai_platform::models::save_model(
            conn,
            cipher,
            "test-key-id",
            1_000,
            &ModelEdit {
                id: id.into(),
                display_name: id.into(),
                provider: "openai".into(),
                api_protocol: "openai-compatible".into(),
                kind,
                model: "gpt-test".into(),
                endpoint: None,
                endpoint_is_full_url: false,
                enabled: true,
                auth_mode: crate::ai_platform::models::AuthMode::None,
                egress: crate::ai_platform::models::EgressPermission::Allow,
                context_window_tokens: None,
                max_output_tokens: None,
                input_price_micros_per_million: None,
                output_price_micros_per_million: None,
                cached_input_price_micros_per_million: None,
            },
            None,
            ApiKeyChange::Keep,
        )
        .expect("建模型");
    }

    fn edit(id: &str, model: &str, role: ModelRole, source: BindingSource) -> ModelBindingEdit {
        ModelBindingEdit {
            id: id.into(),
            agent_id: "a1".into(),
            model_profile_id: model.into(),
            role,
            binding_source: source,
            enabled: true,
            priority: 100,
        }
    }

    #[test]
    fn create_list_and_role_order() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        make_model(&conn, &cipher, "gen2", ModelKind::Generation);
        create_binding(
            &conn,
            2_000,
            &edit("b2", "gen2", ModelRole::MainCandidate, BindingSource::User),
        )
        .expect("建绑定");
        let mut first = edit("b1", "gen", ModelRole::MainCandidate, BindingSource::User);
        first.priority = 10;
        create_binding(&conn, 2_000, &first).expect("建绑定");
        let list = list_agent_bindings(&conn, "a1").expect("列表");
        assert_eq!(
            list.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            ["b1", "b2"]
        );
        let role = list_role_bindings(&conn, "a1", ModelRole::MainCandidate).expect("角色");
        assert_eq!(role.len(), 2);
        assert_eq!(role[0].id, "b1");
    }

    #[test]
    fn set_role_model_upserts_replaces_and_clears_with_cas() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        make_model(&conn, &cipher, "gen2", ModelKind::Generation);
        // 首次选择：建立用户绑定。
        let first = set_role_model(
            &conn,
            2_000,
            "b1",
            "a1",
            ModelRole::Compaction,
            Some("gen"),
            None,
        )
        .expect("首次设置")
        .expect("应有绑定");
        assert_eq!(first.model_profile_id, "gen");
        assert_eq!(first.binding_source, BindingSource::User);
        // 重复设置同一模型：幂等，不新增行、不改 revision。
        let again = set_role_model(
            &conn,
            2_100,
            "b-other",
            "a1",
            ModelRole::Compaction,
            Some("gen"),
            None,
        )
        .expect("幂等设置")
        .expect("应有绑定");
        assert_eq!(again.id, "b1");
        assert_eq!(again.revision, first.revision);
        // 换模型：旧行被替换，旧 revision 不能作为期望值复用。
        assert!(
            set_role_model(
                &conn,
                2_200,
                "b2",
                "a1",
                ModelRole::Compaction,
                Some("gen2"),
                Some(first.revision + 9)
            )
            .is_err(),
            "过期 revision 必须拒绝"
        );
        let replaced = set_role_model(
            &conn,
            2_300,
            "b2",
            "a1",
            ModelRole::Compaction,
            Some("gen2"),
            Some(first.revision),
        )
        .expect("换模型")
        .expect("应有绑定");
        assert_eq!(replaced.model_profile_id, "gen2");
        let role = list_role_bindings(&conn, "a1", ModelRole::Compaction).expect("角色绑定");
        assert_eq!(role.len(), 1, "同一角色只保留一条用户绑定");
        // 清除：跟随默认，不能再有用户绑定。
        assert!(
            set_role_model(
                &conn,
                2_400,
                "b9",
                "a1",
                ModelRole::Compaction,
                None,
                Some(replaced.revision),
            )
            .expect("清除")
            .is_none()
        );
        assert!(
            list_role_bindings(&conn, "a1", ModelRole::Compaction)
                .expect("角色绑定")
                .is_empty()
        );
        // 角色与模型用途不一致时拒绝，不留下半条绑定。
        make_model(&conn, &cipher, "dec", ModelKind::Decision);
        assert!(
            set_role_model(
                &conn,
                2_500,
                "b3",
                "a1",
                ModelRole::Compaction,
                Some("dec"),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn set_role_model_keeps_system_binding_instead_of_duplicating() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        ensure_system_binding(&conn, 2_000, "sys", "a1", "gen", ModelRole::Compaction, 100)
            .expect("system 绑定");
        let chosen = set_role_model(
            &conn,
            2_100,
            "b1",
            "a1",
            ModelRole::Compaction,
            Some("gen"),
            None,
        )
        .expect("选择 system 已绑定的模型")
        .expect("应有绑定");
        assert_eq!(chosen.id, "sys", "不制造重复绑定");
        assert_eq!(
            list_role_bindings(&conn, "a1", ModelRole::Compaction)
                .expect("角色绑定")
                .len(),
            1
        );
    }

    #[test]
    fn role_kind_mismatch_is_rejected() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        make_model(&conn, &cipher, "dec", ModelKind::Decision);
        assert!(
            create_binding(
                &conn,
                2_000,
                &edit("bad", "gen", ModelRole::Decision, BindingSource::User)
            )
            .is_err()
        );
        assert!(
            create_binding(
                &conn,
                2_000,
                &edit("bad2", "dec", ModelRole::MainCandidate, BindingSource::User)
            )
            .is_err()
        );
        create_binding(
            &conn,
            2_000,
            &edit("ok", "dec", ModelRole::Decision, BindingSource::System),
        )
        .expect("decision 角色接受 decision 模型");
    }

    #[test]
    fn missing_agent_or_model_is_rejected() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        let mut missing_agent = edit("x", "gen", ModelRole::MainCandidate, BindingSource::User);
        missing_agent.agent_id = "nope".into();
        assert!(create_binding(&conn, 2_000, &missing_agent).is_err());
        assert!(
            create_binding(
                &conn,
                2_000,
                &edit("x2", "nope", ModelRole::MainCandidate, BindingSource::User)
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_role_model_is_rejected() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        create_binding(
            &conn,
            2_000,
            &edit("b1", "gen", ModelRole::MainCandidate, BindingSource::User),
        )
        .expect("建绑定");
        assert!(
            create_binding(
                &conn,
                2_000,
                &edit("b2", "gen", ModelRole::MainCandidate, BindingSource::System)
            )
            .is_err()
        );
    }

    #[test]
    fn system_reconcile_does_not_override_user_binding() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "dec", ModelKind::Decision);
        // 用户手动绑定（停用）之后，reconcile 不得覆盖它的启停状态或来源。
        let mut user = edit("user", "dec", ModelRole::Decision, BindingSource::User);
        user.enabled = false;
        create_binding(&conn, 2_000, &user).expect("用户绑定");
        let created =
            ensure_system_binding(&conn, 3_000, "sys", "a1", "dec", ModelRole::Decision, 50)
                .expect("reconcile");
        assert!(!created, "已存在用户绑定时不应新建 system 行");
        let list = list_role_bindings(&conn, "a1", ModelRole::Decision).expect("列表");
        assert!(list.is_empty(), "用户已停用，角色候选应为空");
        let all = list_agent_bindings(&conn, "a1").expect("全部");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].binding_source, BindingSource::User);
    }

    #[test]
    fn ensure_system_binding_is_idempotent() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "dec", ModelKind::Decision);
        assert!(
            ensure_system_binding(&conn, 2_000, "s1", "a1", "dec", ModelRole::Decision, 100)
                .expect("首次")
        );
        assert!(
            !ensure_system_binding(&conn, 3_000, "s2", "a1", "dec", ModelRole::Decision, 100)
                .expect("重复")
        );
        assert_eq!(list_agent_bindings(&conn, "a1").expect("列表").len(), 1);
    }

    #[test]
    fn toggle_and_delete_use_object_cas() {
        let (conn, cipher) = setup();
        make_model(&conn, &cipher, "gen", ModelKind::Generation);
        let binding = create_binding(
            &conn,
            2_000,
            &edit("b1", "gen", ModelRole::MainCandidate, BindingSource::User),
        )
        .expect("建绑定");
        let disabled =
            set_binding_enabled(&conn, 3_000, "b1", false, binding.revision).expect("停用");
        assert!(!disabled.enabled);
        assert_eq!(disabled.revision, 2);
        assert!(
            set_binding_enabled(&conn, 4_000, "b1", true, 1).is_err(),
            "过期 revision 应拒绝"
        );
        delete_binding(&conn, "b1", 2).expect("删除");
        assert!(load_binding(&conn, "b1").is_err());
    }
}
