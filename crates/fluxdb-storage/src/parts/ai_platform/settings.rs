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
    pub max_active_turns: i64,
    pub revision: i64,
}

/// 读取运行设置；不存在时返回默认值（revision=0 表示尚未落库）。
pub fn load_runtime_settings(conn: &Connection) -> Result<RuntimeSettings> {
    let row = conn
        .query_row(
            "SELECT default_agent_id, default_model_profile_id, default_decision_model_profile_id,
                    max_active_turns, revision
               FROM ai_runtime_settings WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
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
            max_active_turns: r.3,
            revision: r.4,
        },
        None => RuntimeSettings {
            default_agent_id: None,
            default_model_profile_id: None,
            default_decision_model_profile_id: None,
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
                default_decision_model_profile_id, max_active_turns, revision, updated_at_ms)
             VALUES (1,?1,?2,?3,?4,1,?5)
             ON CONFLICT(id) DO UPDATE SET
                default_agent_id=excluded.default_agent_id,
                default_model_profile_id=excluded.default_model_profile_id,
                default_decision_model_profile_id=excluded.default_decision_model_profile_id,
                max_active_turns=excluded.max_active_turns,
                revision=ai_runtime_settings.revision+1, updated_at_ms=excluded.updated_at_ms
              WHERE ai_runtime_settings.revision=?6",
            params![
                settings.default_agent_id,
                settings.default_model_profile_id,
                settings.default_decision_model_profile_id,
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
    fn first_load_defaults_to_drawer() {
        let conn = setup();
        let prefs = load_panel_preferences(&conn).expect("读取");
        assert_eq!(prefs.preferred_mode, PanelMode::Drawer);
        assert_eq!(prefs.drawer_width_dp, 460.0);
        assert_eq!(prefs.embedded_width_dp, 420.0);
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
}
