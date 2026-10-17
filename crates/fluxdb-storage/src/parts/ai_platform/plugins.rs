//! 内置插件与绑定的管理（设计文档 §23、T012）。
//!
//! 本期只有内置插件：`ai_plugin_packages` 由代码种子写入，外部安装/Wasm 不实现。
//! Agent↔实例绑定区分 user/system 来源，system 绑定由 reconcile 维护且不被用户覆盖。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 常驻插件实例：页面上下文与安全审计，任何调用方都不能关闭。
///
/// 这是宿主固定能力而非用户偏好，因此不放在实例行里，改用固定 ID 白名单，
/// 避免"忘记写 mandatory 列"导致可被绕过。
pub const MANDATORY_INSTANCE_IDS: &[&str] = &["builtin.context-ui", "builtin.security-audit"];

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
///
/// `default_enabled` 只决定首次建行的启停状态；已存在的行不会被覆盖。
pub fn seed_builtin_instance(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    package_id: &str,
    display_name: &str,
    default_enabled: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ai_plugin_instances (id, display_name, package_id, enabled,
            config_schema_version, config_json, granted_permissions_json, revision,
            created_at_ms, updated_at_ms)
         VALUES (?1,?2,?3,?4,1,'{}','[]',1,?5,?5)
         ON CONFLICT(id) DO NOTHING",
        params![
            id,
            display_name,
            package_id,
            i64::from(default_enabled),
            now_ms
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// 内置插件注册表的一行。
pub struct BuiltinPlugin {
    pub instance_id: &'static str,
    pub package_id: &'static str,
    pub package_key: &'static str,
    pub display_name: &'static str,
    /// 首次建行时的启停状态。
    ///
    /// 只在没有实现、仅作目录占位的行上取 `false`；其余内置能力都默认启用。
    pub default_enabled: bool,
}

/// 内置插件注册表。
///
/// 与设计 §23 的内置能力一一对应；常驻能力的 instance_id 必须与
/// [`MANDATORY_INSTANCE_IDS`] 保持一致，否则强制项会被误当作可关闭。
pub const BUILTIN_PLUGINS: &[BuiltinPlugin] = &[
    BuiltinPlugin {
        instance_id: "builtin.context-ui",
        package_id: "pkg.fluxdb.context.ui",
        package_key: "fluxdb.context.ui",
        display_name: "当前界面与 @ 上下文",
        default_enabled: true,
    },
    BuiltinPlugin {
        instance_id: "builtin.files",
        package_id: "pkg.fluxdb.context.files",
        package_key: "fluxdb.context.files",
        display_name: "文件与附件",
        default_enabled: true,
    },
    BuiltinPlugin {
        // 模型路由能力已删除，实例只作目录占位行，不能停留在"已启用"。
        instance_id: "builtin.router",
        package_id: "pkg.fluxdb.model.router",
        package_key: "fluxdb.model.router",
        display_name: "模型路由",
        default_enabled: false,
    },
    BuiltinPlugin {
        instance_id: "builtin.jev",
        package_id: "pkg.fluxdb.decision.jev",
        package_key: "fluxdb.decision.jev",
        display_name: "Jev 决策优化",
        default_enabled: true,
    },
    BuiltinPlugin {
        instance_id: "builtin.compaction",
        package_id: "pkg.fluxdb.context.compaction",
        package_key: "fluxdb.context.compaction",
        display_name: "上下文压缩",
        default_enabled: true,
    },
    BuiltinPlugin {
        instance_id: "builtin.security-audit",
        package_id: "pkg.fluxdb.security.audit",
        package_key: "fluxdb.security.audit",
        display_name: "调用前安全审计",
        default_enabled: true,
    },
    BuiltinPlugin {
        // 用量统计看板尚未实现，实例只作目录占位行。
        instance_id: "builtin.budget",
        package_id: "pkg.fluxdb.usage.budget",
        package_key: "fluxdb.usage.budget",
        display_name: "AI 用量统计",
        default_enabled: false,
    },
    BuiltinPlugin {
        instance_id: "builtin.bi",
        package_id: "pkg.fluxdb.output.bi",
        package_key: "fluxdb.output.bi",
        display_name: "BI 与报表输出",
        default_enabled: true,
    },
    BuiltinPlugin {
        instance_id: "builtin.progress",
        package_id: "pkg.fluxdb.progress.plan",
        package_key: "fluxdb.progress.plan",
        display_name: "计划与执行进度",
        default_enabled: true,
    },
    BuiltinPlugin {
        instance_id: "builtin.telemetry",
        package_id: "pkg.fluxdb.lifecycle.telemetry",
        package_key: "fluxdb.lifecycle.telemetry",
        display_name: "生命周期诊断",
        default_enabled: true,
    },
];

/// 幂等注册全部内置插件包与实例（随应用注册，用户无需安装）。
///
/// 只写入缺失的目录行：已存在的实例保留用户的启停状态与显示名，不被 seed 覆盖。
pub fn seed_builtin_plugins(conn: &Connection, now_ms: i64) -> Result<()> {
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    for plugin in BUILTIN_PLUGINS {
        seed_builtin_package(
            &tx,
            now_ms,
            plugin.package_id,
            plugin.package_key,
            "1",
            "{}",
        )?;
        seed_builtin_instance(
            &tx,
            now_ms,
            plugin.instance_id,
            plugin.package_id,
            plugin.display_name,
            plugin.default_enabled,
        )?;
    }
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
            "当前界面与 @ 上下文、安全审计是常驻能力，不可停用",
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
