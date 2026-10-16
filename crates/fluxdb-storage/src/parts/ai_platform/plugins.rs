//! 内置插件与绑定的管理（设计文档 §23、T012）。
//!
//! 本期只有内置插件：`ai_plugin_packages` 由代码种子写入，外部安装/Wasm 不实现。
//! Agent↔实例绑定区分 user/system 来源，system 绑定由 reconcile 维护且不被用户覆盖。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 常驻插件实例：页面上下文、安全审计与执行护栏，任何调用方都不能关闭。
///
/// 这是宿主固定能力而非用户偏好，因此不放在实例行里，改用固定 ID 白名单，
/// 避免"忘记写 mandatory 列"导致可被绕过。
pub const MANDATORY_INSTANCE_IDS: &[&str] = &[
    "builtin.context-ui",
    "builtin.security-audit",
    "builtin.budget",
];

/// 该实例是否为常驻能力（不可停用）。
pub fn is_mandatory(instance_id: &str) -> bool {
    MANDATORY_INSTANCE_IDS.contains(&instance_id)
}

/// 插件实例视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceView {
    pub id: String,
    pub display_name: String,
    pub package_id: String,
    pub package_key: String,
    pub runtime_kind: String,
    pub enabled: bool,
    pub revision: i64,
}

/// Agent↔插件绑定视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginBindingView {
    pub id: String,
    pub agent_id: String,
    pub plugin_instance_id: String,
    pub binding_source: String,
    pub enabled: bool,
    pub priority: i64,
    pub revision: i64,
}

/// 插件实例的声明来源：`runtime_kind` 决定配置声明读代码注册表还是读 manifest。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceSource {
    pub runtime_kind: String,
    pub manifest_json: String,
}

/// 读取实例的运行来源与 manifest（外置插件配置声明的接入点）。
pub fn load_plugin_instance_source(
    conn: &Connection,
    id: &str,
) -> Result<Option<PluginInstanceSource>> {
    conn.query_row(
        "SELECT p.runtime_kind, p.manifest_json
           FROM ai_plugin_instances i
           JOIN ai_plugin_packages p ON p.id = i.package_id
          WHERE i.id=?1",
        [id],
        |row| {
            Ok(PluginInstanceSource {
                runtime_kind: row.get(0)?,
                manifest_json: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// 幂等写入内置插件包与实例（无密钥、无外部代码）。
///
/// 已有记录不覆盖：用户的启停状态与绑定由后续任务维护。
pub fn seed_builtin_package(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    package_key: &str,
    version: &str,
    manifest_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_plugin_packages (id, package_key, version, runtime_kind, digest,
            host_api_min, host_api_max, manifest_json, status, revision, created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,'builtin',?4,1,1,?5,'active',1,?6,?6)
         ON CONFLICT(package_key, version, digest) DO NOTHING",
        params![id, package_key, version, format!("builtin:{package_key}:{version}"), manifest_json, now_ms],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// 幂等写入内置插件实例（system 来源）。
pub fn seed_builtin_instance(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    package_id: &str,
    display_name: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_plugin_instances (id, display_name, package_id, enabled,
            config_schema_version, config_json, granted_permissions_json, revision,
            created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,1,1,'{}','[]',1,?4,?4)
         ON CONFLICT(id) DO NOTHING",
        params![id, display_name, package_id, now_ms],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// 内置插件注册表：(instance_id, package_id, package_key, 显示名)。
///
/// 与设计 §23 的内置能力一一对应；常驻能力的 instance_id 必须与
/// [`MANDATORY_INSTANCE_IDS`] 保持一致，否则强制项会被误当作可关闭。
pub const BUILTIN_PLUGINS: &[(&str, &str, &str, &str)] = &[
    (
        "builtin.context-ui",
        "pkg.fluxdb.context.ui",
        "fluxdb.context.ui",
        "当前界面与 @ 上下文",
    ),
    (
        "builtin.files",
        "pkg.fluxdb.context.files",
        "fluxdb.context.files",
        "文件与附件",
    ),
    (
        "builtin.router",
        "pkg.fluxdb.model.router",
        "fluxdb.model.router",
        "模型路由",
    ),
    (
        "builtin.jev",
        "pkg.fluxdb.decision.jev",
        "fluxdb.decision.jev",
        "Jev 决策优化",
    ),
    (
        "builtin.compaction",
        "pkg.fluxdb.context.compaction",
        "fluxdb.context.compaction",
        "上下文压缩",
    ),
    (
        "builtin.security-audit",
        "pkg.fluxdb.security.audit",
        "fluxdb.security.audit",
        "调用前安全审计",
    ),
    (
        "builtin.budget",
        "pkg.fluxdb.usage.budget",
        "fluxdb.usage.budget",
        "Token 用量与执行保护",
    ),
    (
        "builtin.knowledge",
        "pkg.fluxdb.knowledge.skills",
        "fluxdb.knowledge.skills",
        "业务知识与 Skills",
    ),
    (
        "builtin.bi",
        "pkg.fluxdb.output.bi",
        "fluxdb.output.bi",
        "BI 与报表输出",
    ),
    (
        "builtin.progress",
        "pkg.fluxdb.progress.plan",
        "fluxdb.progress.plan",
        "计划与执行进度",
    ),
    (
        "builtin.telemetry",
        "pkg.fluxdb.lifecycle.telemetry",
        "fluxdb.lifecycle.telemetry",
        "生命周期诊断",
    ),
];

/// 幂等注册全部内置插件包与实例（随应用注册，用户无需安装）。
///
/// 保留可选实例的启停状态；修正常驻上下文旧状态，并移除已归入工具目录的旧实例。
pub fn seed_builtin_plugins(conn: &Connection, now_ms: i64) -> Result<()> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    for (instance_id, package_id, package_key, display_name) in BUILTIN_PLUGINS {
        seed_builtin_package(&tx, now_ms, package_id, package_key, "1", "{}")?;
        seed_builtin_instance(&tx, now_ms, instance_id, package_id, display_name)?;
    }
    // 旧版允许关闭上下文；升级后恢复常驻状态，且不反复递增 revision。
    tx.execute(
        "UPDATE ai_plugin_instances SET enabled=1, revision=revision+1, updated_at_ms=?1
         WHERE id='builtin.context-ui' AND enabled=0",
        [now_ms],
    )
    .map_err(sqlite_error)?;
    // 模型路由能力已删除，实例只作目录占位行；不允许停留在"已启用"。
    tx.execute(
        "UPDATE ai_plugin_instances SET enabled=0, revision=revision+1, updated_at_ms=?1
         WHERE id='builtin.router' AND enabled=1",
        [now_ms],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "DELETE FROM ai_agent_plugin_bindings WHERE plugin_instance_id='builtin.database'",
        [],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "DELETE FROM ai_plugin_instances WHERE id='builtin.database'",
        [],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "DELETE FROM ai_plugin_packages
         WHERE package_key='fluxdb.tools.database' AND runtime_kind='builtin'
         AND NOT EXISTS (SELECT 1 FROM ai_plugin_instances WHERE package_id=ai_plugin_packages.id)",
        [],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(())
}

/// 建立 Agent↔实例绑定；已存在则不覆盖（reconcile 幂等）。
pub fn bind_plugin_system(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    agent_id: &str,
    instance_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_agent_plugin_bindings (id, agent_id, plugin_instance_id, binding_source,
            enabled, priority, capabilities_json, failure_policy, options_json, revision,
            created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,'system',1,100,'[]','best_effort','{}',1,?4,?4)
         ON CONFLICT(agent_id, plugin_instance_id) DO NOTHING",
        params![id, agent_id, instance_id, now_ms],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// 列出全部插件实例（含包信息）。
pub fn list_plugin_instances(conn: &Connection) -> Result<Vec<PluginInstanceView>> {
    let mut stmt = conn
        .prepare(
            "SELECT i.id, i.display_name, i.package_id, p.package_key, p.runtime_kind,
                    i.enabled, i.revision
               FROM ai_plugin_instances i
               JOIN ai_plugin_packages p ON p.id = i.package_id
              ORDER BY i.display_name, i.id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(PluginInstanceView {
                id: row.get(0)?,
                display_name: row.get(1)?,
                package_id: row.get(2)?,
                package_key: row.get(3)?,
                runtime_kind: row.get(4)?,
                enabled: row.get::<_, i64>(5)? != 0,
                revision: row.get(6)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// 切换插件实例启停（对象级 CAS），在存储边界拒绝停用常驻能力。
pub fn set_plugin_instance_enabled(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    enabled: bool,
    expected_revision: i64,
) -> Result<()> {
    if enabled == false && is_mandatory(id) {
        return Err(Error::new(
            ErrorKind::Internal,
            "当前界面与 @ 上下文、安全审计与执行护栏是常驻能力，不可停用",
        ));
    }
    let changed = conn
        .execute(
            "UPDATE ai_plugin_instances SET enabled=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, enabled as i64, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "插件配置已被其他窗口修改，请重新加载",
        ));
    }
    Ok(())
}

/// 插件实例的标量配置（当前值与对象级 revision）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstanceConfig {
    /// 实例行当前 revision；写入按它做 CAS。
    pub revision: i64,
    /// `config_json` 解析后的对象；不是 JSON 对象时按空对象处理（DDL 只保证 json_valid）。
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// 读取插件实例的标量配置。
///
/// 资源字段（模型槽位）不在这里：它们落在运行设置的槽位列，本函数只返回标量参数。
pub fn load_plugin_instance_config(conn: &Connection, id: &str) -> Result<PluginInstanceConfig> {
    let (config_json, revision): (String, i64) = conn
        .query_row(
            "SELECT config_json, revision FROM ai_plugin_instances WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "插件实例不存在"))?;
    let values = serde_json::from_str::<serde_json::Value>(&config_json)
        .ok()
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    Ok(PluginInstanceConfig { revision, values })
}

/// 覆盖写入插件实例的标量配置（对象级 CAS）。
///
/// 整份 `config_json` 一次替换：调用方先把两类字段合并成完整对象再写入，避免
/// 「一半新值、一半旧值」的半套配置。
pub fn save_plugin_instance_config(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    values: &serde_json::Map<String, serde_json::Value>,
    expected_revision: i64,
) -> Result<i64> {
    let config_json = serde_json::Value::Object(values.clone()).to_string();
    let changed = conn
        .execute(
            "UPDATE ai_plugin_instances SET config_json=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, config_json, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "插件配置已被其他窗口修改，请重新加载",
        ));
    }
    Ok(expected_revision + 1)
}

/// 列出某 Agent 的插件绑定，按优先级、ID 稳定排序。
pub fn list_agent_plugin_bindings(
    conn: &Connection,
    agent_id: &str,
) -> Result<Vec<PluginBindingView>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, plugin_instance_id, binding_source, enabled, priority, revision
               FROM ai_agent_plugin_bindings WHERE agent_id=?1 ORDER BY priority, id",
        )
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([agent_id], |row| {
            Ok(PluginBindingView {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                plugin_instance_id: row.get(2)?,
                binding_source: row.get(3)?,
                enabled: row.get::<_, i64>(4)? != 0,
                priority: row.get(5)?,
                revision: row.get(6)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
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
        seed_builtin_package(
            &conn,
            1_000,
            "pkg-jev",
            "fluxdb.decision.jev",
            "1.0.0",
            "{}",
        )
        .expect("种子包");
        seed_builtin_instance(&conn, 1_000, "inst-jev", "pkg-jev", "Jev 决策").expect("种子实例");
        conn
    }

    #[test]
    fn seeding_is_idempotent() {
        let conn = setup();
        seed_builtin_package(
            &conn,
            2_000,
            "pkg-jev",
            "fluxdb.decision.jev",
            "1.0.0",
            "{}",
        )
        .expect("重复种子");
        seed_builtin_instance(&conn, 2_000, "inst-jev", "pkg-jev", "Jev 决策").expect("重复种子");
        // ensure_schema 会注册内置实例；这里只断言自定义实例没有重复。
        let list = list_plugin_instances(&conn).unwrap();
        assert_eq!(list.iter().filter(|p| p.id == "inst-jev").count(), 1);
        assert_eq!(list.len(), BUILTIN_PLUGINS.len() + 1);
    }
    #[test]
    fn toggle_uses_object_cas() {
        let conn = setup();
        set_plugin_instance_enabled(&conn, 2_000, "inst-jev", false, 1).expect("关闭");
        let jev = list_plugin_instances(&conn)
            .unwrap()
            .into_iter()
            .find(|p| p.id == "inst-jev")
            .expect("实例存在");
        assert!(!jev.enabled);
        let err = set_plugin_instance_enabled(&conn, 3_000, "inst-jev", true, 1)
            .expect_err("过期 revision 应拒绝");
        assert!(err.message.contains("重新加载"));
    }

    #[test]
    fn mandatory_instances_cannot_be_disabled() {
        let conn = setup();
        for id in MANDATORY_INSTANCE_IDS {
            let err = set_plugin_instance_enabled(&conn, 2_000, id, false, 1)
                .expect_err("常驻实例不可停用");
            assert!(err.message.contains("不可停用"), "实际: {}", err.message);
        }
        // 反向：普通实例仍可停用。
        set_plugin_instance_enabled(&conn, 3_000, "inst-jev", false, 1).expect("普通实例可停用");
    }

    #[test]
    fn seeding_upgrades_legacy_catalog_without_resetting_optional_plugins() {
        let conn = setup();
        seed_builtin_package(
            &conn,
            1_000,
            "pkg.fluxdb.tools.database",
            "fluxdb.tools.database",
            "1",
            "{}",
        )
        .unwrap();
        seed_builtin_instance(
            &conn,
            1_000,
            "builtin.database",
            "pkg.fluxdb.tools.database",
            "数据库与 Redis 工具",
        )
        .unwrap();
        bind_plugin_system(&conn, 1_000, "legacy-binding", "a1", "builtin.database").unwrap();
        conn.execute(
            "UPDATE ai_plugin_instances SET enabled=0 WHERE id='builtin.context-ui'",
            [],
        )
        .unwrap();
        set_plugin_instance_enabled(&conn, 1_001, "builtin.files", false, 1).unwrap();

        seed_builtin_plugins(&conn, 2_000).unwrap();
        let first = list_plugin_instances(&conn).unwrap();
        let context = first.iter().find(|p| p.id == "builtin.context-ui").unwrap();
        assert!(context.enabled);
        assert_eq!(context.revision, 2);
        assert!(
            !first
                .iter()
                .find(|p| p.id == "builtin.files")
                .unwrap()
                .enabled
        );
        assert!(!first.iter().any(|p| p.id == "builtin.database"));
        assert!(list_agent_plugin_bindings(&conn, "a1").unwrap().is_empty());
        let packages: i64 = conn
            .query_row(
                "SELECT count(*) FROM ai_plugin_packages WHERE package_key='fluxdb.tools.database'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(packages, 0);
        assert!(
            !conn
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
        seed_builtin_plugins(&conn, 3_000).unwrap();
        assert_eq!(first, list_plugin_instances(&conn).unwrap());
    }

    #[test]
    fn binding_requires_existing_agent_and_instance() {
        let conn = setup();
        assert!(bind_plugin_system(&conn, 1_000, "b", "missing", "inst-jev").is_err());
        assert!(bind_plugin_system(&conn, 1_000, "b", "a1", "missing").is_err());
    }
}
