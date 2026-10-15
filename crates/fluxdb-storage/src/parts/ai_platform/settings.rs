//! 单例运行设置与「询问 AI」面板偏好（设计文档 §30.1/§31.6、T010/T016）。

use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;
use fluxdb_core::{Error, ErrorKind, Result};

/// 「询问 AI」展现形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    Drawer,
    Embedded,
    Tab,
}

impl PanelMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelMode::Drawer => "drawer",
            PanelMode::Embedded => "embedded",
            PanelMode::Tab => "tab",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "drawer" => Ok(PanelMode::Drawer),
            "embedded" => Ok(PanelMode::Embedded),
            "tab" => Ok(PanelMode::Tab),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知面板展现形式: {other}"),
            )),
        }
    }
    /// 该形式相对主窗口可用宽度的硬上限比例（§31.4）。
    pub fn max_ratio(self) -> f64 {
        match self {
            PanelMode::Drawer => 0.60,
            PanelMode::Embedded => 0.40,
            PanelMode::Tab => 1.0,
        }
    }
}

/// 计算实际展示宽度：比例硬上限与最小宽度同时生效。
///
/// 窗口过窄时有效最小值退让到比例上限；独立页签使用整个内容区。
pub fn resolve_panel_width(mode: PanelMode, desired_dp: f64, viewport_dp: f64) -> f64 {
    if mode == PanelMode::Tab {
        return viewport_dp.max(0.0);
    }
    let max_dp = (viewport_dp.max(0.0) * mode.max_ratio()).floor();
    let min_dp = 320.0_f64.min(max_dp);
    desired_dp.clamp(min_dp, max_dp)
}

/// 应用级运行设置（单例）。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSettings {
    pub default_agent_id: Option<String>,
    pub default_model_profile_id: Option<String>,
    pub default_decision_model_profile_id: Option<String>,
    /// 压缩决策槽位：压缩前判断历史取舍可选的 Jev 模型，与 [`Self::default_decision_model_profile_id`]
    /// 各自独立（在压缩插件管理里选，不连带改动 Jev 决策优化的选择）。
    pub compaction_decision_model_profile_id: Option<String>,
    pub max_active_turns: i64,
    pub revision: i64,
}

/// 隐私策略（`ai_runtime_settings.privacy_policy_json` 的受校验投影，设计 §29）。
///
/// 只放路由真正会用到的字段；模型自己的出站许可在 `ai_model_profiles.provider_options_json`
/// 的 `privacy_egress`（见 `models::EgressPermission`），两者是"全局应急闸门 + 单模型许可"的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivacyPolicy {
    /// 全局出站闸门：false 时任何模型都不再接收会话数据（怀疑外泄时的应急开关）。
    /// 缺失或无法识别的历史值按 `true` 处理，避免升级后静默停用全部模型。
    pub model_egress_enabled: bool,
}

impl Default for PrivacyPolicy {
    fn default() -> Self {
        Self {
            model_egress_enabled: true,
        }
    }
}

/// 读取隐私策略；行缺失或 `privacy_policy_json` 为空时返回默认（允许出站）。
pub fn load_privacy_policy(conn: &Connection) -> Result<PrivacyPolicy> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT privacy_policy_json FROM ai_runtime_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(raw) = raw else {
        return Ok(PrivacyPolicy::default());
    };
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| Error::new(ErrorKind::Internal, "隐私策略不是合法 JSON"))?;
    Ok(PrivacyPolicy {
        model_egress_enabled: value
            .get("model_egress_enabled")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(true),
    })
}

/// 保存隐私策略（对象级 CAS 复用 `ai_runtime_settings.revision`）。
pub fn save_privacy_policy(
    conn: &Connection,
    now_ms: i64,
    policy: PrivacyPolicy,
    expected_revision: i64,
) -> Result<PrivacyPolicy> {
    let json =
        serde_json::json!({ "model_egress_enabled": policy.model_egress_enabled }).to_string();
    let changed = conn
        .execute(
            "INSERT INTO ai_runtime_settings (id, max_active_turns, privacy_policy_json, revision, updated_at_ms)
             VALUES (1, 2, ?1, 1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                privacy_policy_json=excluded.privacy_policy_json,
                revision=ai_runtime_settings.revision+1, updated_at_ms=excluded.updated_at_ms
              WHERE ai_runtime_settings.revision=?3",
            params![json, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "运行设置已被其他窗口修改，请重新加载",
        ));
    }
    load_privacy_policy(conn)
}

/// 读取运行设置；不存在时返回默认值（revision=0 表示尚未落库）。
pub fn load_runtime_settings(conn: &Connection) -> Result<RuntimeSettings> {
    let row = conn
        .query_row(
            "SELECT default_agent_id, default_model_profile_id, default_decision_model_profile_id,
                    compaction_decision_model_profile_id, max_active_turns, revision
               FROM ai_runtime_settings WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(match row {
        Some(r) => RuntimeSettings {
            default_agent_id: r.0,
            default_model_profile_id: r.1,
            default_decision_model_profile_id: r.2,
            compaction_decision_model_profile_id: r.3,
            max_active_turns: r.4,
            revision: r.5,
        },
        None => RuntimeSettings {
            default_agent_id: None,
            default_model_profile_id: None,
            default_decision_model_profile_id: None,
            compaction_decision_model_profile_id: None,
            max_active_turns: 2,
            revision: 0,
        },
    })
}

/// 保存运行设置（单例 upsert + CAS；`expected_revision=0` 表示首次创建）。
pub fn save_runtime_settings(
    conn: &Connection,
    now_ms: i64,
    settings: &RuntimeSettings,
    expected_revision: i64,
) -> Result<RuntimeSettings> {
    if settings.max_active_turns <= 0 {
        return Err(Error::new(ErrorKind::Internal, "最大活动任务数必须为正"));
    }
    let changed = conn
        .execute(
            "INSERT INTO ai_runtime_settings (id, default_agent_id, default_model_profile_id,
                default_decision_model_profile_id, compaction_decision_model_profile_id,
                max_active_turns, revision, updated_at_ms)
             VALUES (1,?1,?2,?3,?4,?5,1,?6)
             ON CONFLICT(id) DO UPDATE SET
                default_agent_id=excluded.default_agent_id,
                default_model_profile_id=excluded.default_model_profile_id,
                default_decision_model_profile_id=excluded.default_decision_model_profile_id,
                compaction_decision_model_profile_id=excluded.compaction_decision_model_profile_id,
                max_active_turns=excluded.max_active_turns,
                revision=ai_runtime_settings.revision+1, updated_at_ms=excluded.updated_at_ms
              WHERE ai_runtime_settings.revision=?7",
            params![
                settings.default_agent_id,
                settings.default_model_profile_id,
                settings.default_decision_model_profile_id,
                settings.compaction_decision_model_profile_id,
                settings.max_active_turns,
                now_ms,
                expected_revision,
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "运行设置已被其他窗口修改，请重新加载",
        ));
    }
    load_runtime_settings(conn)
}

/// 「询问 AI」面板偏好（单例）。
#[derive(Debug, Clone, PartialEq)]
pub struct PanelPreferences {
    pub preferred_mode: PanelMode,
    pub drawer_width_dp: f64,
    pub embedded_width_dp: f64,
    pub last_session_id: Option<String>,
    /// 尚未成为消息的新草稿（面板关闭/重启后恢复），`None` 表示无草稿。
    pub pending_draft_json: Option<String>,
    pub revision: i64,
}

impl Default for PanelPreferences {
    fn default() -> Self {
        Self {
            // 首次默认抽屉；之后沿用上次形式（§31.4）。
            preferred_mode: PanelMode::Drawer,
            drawer_width_dp: 460.0,
            embedded_width_dp: 420.0,
            last_session_id: None,
            pending_draft_json: None,
            revision: 0,
        }
    }
}

/// 读取面板偏好；不存在时返回首次默认值。
pub fn load_panel_preferences(conn: &Connection) -> Result<PanelPreferences> {
    let row = conn
        .query_row(
            "SELECT preferred_mode, drawer_width_dp, embedded_width_dp, last_session_id,
                    pending_draft_json, revision
               FROM ai_panel_preferences WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(match row {
        Some(r) => PanelPreferences {
            preferred_mode: PanelMode::parse(&r.0)?,
            drawer_width_dp: r.1,
            embedded_width_dp: r.2,
            last_session_id: r.3,
            pending_draft_json: r.4,
            revision: r.5,
        },
        None => PanelPreferences::default(),
    })
}

/// 保存面板偏好（单例 upsert + CAS）。
pub fn save_panel_preferences(
    conn: &Connection,
    now_ms: i64,
    prefs: &PanelPreferences,
    expected_revision: i64,
) -> Result<PanelPreferences> {
    for width in [prefs.drawer_width_dp, prefs.embedded_width_dp] {
        if !(width.is_finite() && width > 0.0) {
            return Err(Error::new(ErrorKind::Internal, "面板宽度必须为正数"));
        }
    }
    if let Some(draft) = prefs.pending_draft_json.as_deref() {
        serde_json::from_str::<serde_json::Value>(draft)
            .map_err(|e| Error::new(ErrorKind::Internal, format!("草稿不是合法 JSON: {e}")))?;
    }
    let changed = conn
        .execute(
            "INSERT INTO ai_panel_preferences (id, preferred_mode, drawer_width_dp,
                embedded_width_dp, last_session_id, pending_draft_json, revision, updated_at_ms)
             VALUES (1,?1,?2,?3,?4,?5,1,?6)
             ON CONFLICT(id) DO UPDATE SET
                preferred_mode=excluded.preferred_mode,
                drawer_width_dp=excluded.drawer_width_dp,
                embedded_width_dp=excluded.embedded_width_dp,
                last_session_id=excluded.last_session_id,
                pending_draft_json=excluded.pending_draft_json,
                revision=ai_panel_preferences.revision+1, updated_at_ms=excluded.updated_at_ms
              WHERE ai_panel_preferences.revision=?7",
            params![
                prefs.preferred_mode.as_str(),
                prefs.drawer_width_dp,
                prefs.embedded_width_dp,
                prefs.last_session_id,
                prefs.pending_draft_json,
                now_ms,
                expected_revision,
            ],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "面板偏好已被其他窗口修改，请重试",
        ));
    }
    load_panel_preferences(conn)
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
    fn mode_and_widths_persist_independently() {
        let conn = setup();
        let mut prefs = load_panel_preferences(&conn).unwrap();
        prefs.preferred_mode = PanelMode::Embedded;
        prefs.drawer_width_dp = 500.0;
        let saved = save_panel_preferences(&conn, 1_000, &prefs, prefs.revision).expect("保存");
        assert_eq!(saved.preferred_mode, PanelMode::Embedded);
        assert_eq!(saved.drawer_width_dp, 500.0);
        assert_eq!(saved.embedded_width_dp, 420.0, "两种宽度互不影响");
    }

    #[test]
    fn panel_width_respects_ratio_caps() {
        assert_eq!(resolve_panel_width(PanelMode::Drawer, 460.0, 1200.0), 460.0);
        assert_eq!(
            resolve_panel_width(PanelMode::Drawer, 5000.0, 1200.0),
            720.0
        );
        assert_eq!(
            resolve_panel_width(PanelMode::Embedded, 5000.0, 1200.0),
            480.0
        );
    }

    #[test]
    fn narrow_window_keeps_mode_and_respects_cap() {
        // 窗口过窄：有效最小退让到 40%，但不切第三种形式
        let w = resolve_panel_width(PanelMode::Embedded, 420.0, 700.0);
        assert_eq!(w, 280.0);
        assert!(w <= 700.0 * 0.40);
    }

    #[test]
    fn stale_revision_is_rejected() {
        let conn = setup();
        let prefs = load_panel_preferences(&conn).unwrap();
        save_panel_preferences(&conn, 1_000, &prefs, prefs.revision).expect("首次保存");
        assert!(save_panel_preferences(&conn, 2_000, &prefs, prefs.revision).is_err());
    }

    #[test]
    fn runtime_settings_upsert_with_cas() {
        let conn = setup();
        let first = load_runtime_settings(&conn).unwrap();
        assert_eq!(first.revision, 0, "未落库时 revision=0");
        let mut settings = first.clone();
        settings.max_active_turns = 3;
        let saved = save_runtime_settings(&conn, 1_000, &settings, 0).expect("首次保存");
        assert_eq!(saved.max_active_turns, 3);
        assert_eq!(saved.revision, 1);
        assert!(save_runtime_settings(&conn, 2_000, &settings, 0).is_err());
    }

    #[test]
    fn privacy_policy_defaults_to_allowing_egress_and_uses_cas() {
        let conn = setup();
        assert!(
            load_privacy_policy(&conn).unwrap().model_egress_enabled,
            "未配置或缺失时默认允许出站，升级不改变现有行为"
        );
        let saved = save_privacy_policy(
            &conn,
            1_000,
            PrivacyPolicy {
                model_egress_enabled: false,
            },
            0,
        )
        .expect("首次保存");
        assert!(!saved.model_egress_enabled);
        assert_eq!(
            load_runtime_settings(&conn).unwrap().revision,
            1,
            "与运行设置共享行 revision"
        );
        assert!(
            save_privacy_policy(&conn, 2_000, PrivacyPolicy::default(), 0).is_err(),
            "过期 revision 必须拒绝"
        );
        // 保存隐私策略不得清掉同行的默认模型字段。
        let mut settings = load_runtime_settings(&conn).unwrap();
        settings.default_model_profile_id = None;
        save_runtime_settings(&conn, 3_000, &settings, settings.revision).expect("保存运行设置");
        assert!(!load_privacy_policy(&conn).unwrap().model_egress_enabled);
    }

    #[test]
    fn unparsable_privacy_value_falls_back_to_allowed() {
        let conn = setup();
        save_privacy_policy(
            &conn,
            1_000,
            PrivacyPolicy {
                model_egress_enabled: false,
            },
            0,
        )
        .unwrap();
        conn.execute(
            "UPDATE ai_runtime_settings SET privacy_policy_json='{\"model_egress_enabled\":\"nope\"}' WHERE id=1",
            [],
        )
        .unwrap();
        assert!(load_privacy_policy(&conn).unwrap().model_egress_enabled);
    }
}
