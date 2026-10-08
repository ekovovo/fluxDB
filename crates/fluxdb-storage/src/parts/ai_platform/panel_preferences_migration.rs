use fluxdb_core::{Error, ErrorKind, Result};
use rusqlite::Connection;

use super::schema;
use crate::sqlite::sqlite_error;

pub(super) fn ensure_tab_mode(conn: &Connection) -> Result<()> {
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='ai_panel_preferences'",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    let normalize = |sql: &str| -> String {
        sql.to_lowercase()
            .chars()
            .filter(|ch| !ch.is_whitespace() && *ch != ';')
            .collect::<String>()
            .replace("ifnotexists", "")
    };
    let normalized = normalize(&sql);
    if normalized.contains("preferred_modein('drawer','embedded','tab')") {
        return Ok(());
    }
    let legacy = schema::PANEL_PREFERENCES_SQL
        .replace("('drawer','embedded','tab')", "('drawer','embedded')");
    if normalized != normalize(&legacy) {
        return Err(Error::new(
            ErrorKind::Internal,
            "AI 面板偏好约束无法识别，未自动迁移",
        ));
    }
    // SQLite 不能修改 CHECK；事务内重建已知旧表，完整保留草稿、会话引用与 CAS 版本。
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    tx.execute_batch(&schema::PANEL_PREFERENCES_SQL.replace(
        "IF NOT EXISTS ai_panel_preferences",
        "ai_panel_preferences_upgrade",
    ))
    .map_err(sqlite_error)?;
    tx.execute_batch(
        "INSERT INTO ai_panel_preferences_upgrade
            (id, preferred_mode, drawer_width_dp, embedded_width_dp, last_session_id,
             pending_draft_json, revision, updated_at_ms)
         SELECT id, preferred_mode, drawer_width_dp, embedded_width_dp, last_session_id,
                pending_draft_json, revision, updated_at_ms FROM ai_panel_preferences;
         DROP TABLE ai_panel_preferences;
         ALTER TABLE ai_panel_preferences_upgrade RENAME TO ai_panel_preferences;",
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_platform::{agents, ensure_schema, sessions, settings};

    #[test]
    fn ai_panel_tab_mode_migrates_legacy_preferences_without_data_loss() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute_batch("DROP TABLE ai_panel_preferences")
            .unwrap();
        conn.execute_batch(
            &schema::PANEL_PREFERENCES_SQL
                .replace("('drawer','embedded','tab')", "('drawer','embedded')"),
        )
        .unwrap();
        sessions::create_session(
            &conn,
            100,
            "retained",
            agents::BUILTIN_AGENT_ID,
            "retained session",
        )
        .unwrap();
        let mut prefs = settings::PanelPreferences::default();
        prefs.preferred_mode = settings::PanelMode::Embedded;
        prefs.drawer_width_dp = 531.;
        prefs.embedded_width_dp = 379.;
        prefs.pending_draft_json = Some(r#"{"text":"keep draft"}"#.into());
        prefs.last_session_id = Some("retained".into());
        let saved = settings::save_panel_preferences(&conn, 123, &prefs, 0).unwrap();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        assert_eq!(settings::load_panel_preferences(&conn).unwrap(), saved);
        prefs = saved.clone();
        prefs.preferred_mode = settings::PanelMode::Tab;
        let tab = settings::save_panel_preferences(&conn, 124, &prefs, saved.revision).unwrap();
        assert_eq!(settings::load_panel_preferences(&conn).unwrap(), tab);
        assert!(
            conn.execute("DELETE FROM ai_sessions WHERE id='retained'", [])
                .is_err()
        );
        assert!(settings::save_panel_preferences(&conn, 125, &prefs, saved.revision).is_err());
        assert_eq!(
            settings::resolve_panel_width(settings::PanelMode::Tab, 420., 900.),
            900.
        );
        assert!(
            conn.execute(
                "UPDATE ai_panel_preferences SET preferred_mode='invalid'",
                []
            )
            .is_err()
        );
    }

    #[test]
    fn ai_panel_tab_mode_refuses_unknown_legacy_structure() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute_batch("DROP TABLE ai_panel_preferences")
            .unwrap();
        let legacy = schema::PANEL_PREFERENCES_SQL
            .replace("('drawer','embedded','tab')", "('drawer','embedded')")
            .replace(
                "updated_at_ms INTEGER NOT NULL",
                "updated_at_ms INTEGER NOT NULL, unknown_value TEXT",
            );
        conn.execute_batch(&legacy).unwrap();
        conn.execute("INSERT INTO ai_panel_preferences (id, updated_at_ms, unknown_value) VALUES (1, 1, 'preserve')", []).unwrap();
        assert!(ensure_tab_mode(&conn).is_err());
        assert_eq!(
            conn.query_row(
                "SELECT unknown_value FROM ai_panel_preferences",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "preserve"
        );
    }
}
