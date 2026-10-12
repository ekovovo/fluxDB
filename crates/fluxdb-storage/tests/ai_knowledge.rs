//! 业务知识与按需 Skills 存储契约（T038）：scored scope、来源/状态、CAS、大正文有界读取。
use std::{fs, path::PathBuf};

use fluxdb_storage::ai_platform::{ensure_schema, knowledge};
use rusqlite::Connection;

fn entry(
    id: &str,
    title: &str,
    status: &str,
    connection_id: Option<i64>,
    database: Option<&str>,
) -> knowledge::KnowledgeEntryEdit {
    knowledge::KnowledgeEntryEdit {
        id: id.into(),
        title: title.into(),
        kind: "metric".into(),
        agent_id: None,
        connection_id,
        database_name: database.map(str::to_owned),
        namespace: None,
        scope_json: match (connection_id, database) {
            (Some(id), Some(db)) => format!(r#"{{"connections":[{id}], "databases":["{db}"]}}"#),
            (Some(id), None) => format!(r#"{{"connections":[{id}]}}"#),
            (None, Some(db)) => format!(r#"{{"databases":["{db}"]}}"#),
            (None, None) => "{}".to_string(),
        },
        content_json: r#"{"text":"规则"}"#.into(),
        source_json: r#"{"origin":"manual"}"#.into(),
        status: status.into(),
    }
}

struct Fixture {
    dir: PathBuf,
    conn: Option<Connection>,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-knowledge-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&dir).expect("独占临时目录");
        let mut fixture = Self { dir, conn: None };
        fixture.reopen();
        fixture
    }
    fn db(&self) -> &Connection {
        self.conn.as_ref().expect("已打开 SQLite")
    }
    fn reopen(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.close().expect("关闭真实连接");
        }
        let conn = Connection::open(self.dir.join("knowledge.sqlite")).expect("文件 SQLite");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        ensure_schema(&conn).expect("生产 schema");
        self.conn = Some(conn);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.close();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn save_confirmed_entry_then_list_and_survive_reopen() {
    let mut fixture = Fixture::new();
    let edit = entry("k1", "订单金额口径", "confirmed", Some(1), Some("shop"));
    knowledge::save(fixture.db(), 1_000, &edit, None).expect("保存知识条目");
    fixture.reopen();
    let rows =
        knowledge::list_confirmed_scoped(fixture.db(), Some((1, Some("shop"))), "", 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "订单金额口径");
    assert_eq!(rows[0].kind, "metric");
    assert_eq!(rows[0].database_name.as_deref(), Some("shop"));
    // 直接按_id读回完整条目，检查作用域与来源。
    let loaded = knowledge::load(fixture.db(), "k1").unwrap();
    assert_eq!(loaded.status, "confirmed");
    assert_eq!(loaded.source_json, r#"{"origin":"manual"}"#);
}

#[test]
fn list_only_returns_confirmed_and_filters_by_query() {
    let fixture = Fixture::new();
    for (id, title, status) in [
        ("k1", "订单金额口径", "confirmed"),
        ("k2", "退款规则", "proposed"),
        ("k3", "订单退款比例", "confirmed"),
    ] {
        let edit = entry(id, title, status, Some(1), Some("shop"));
        knowledge::save(fixture.db(), 1_000, &edit, None).unwrap();
    }
    let all =
        knowledge::list_confirmed_scoped(fixture.db(), Some((1, Some("shop"))), "", 50).unwrap();
    assert_eq!(all.len(), 2, "proposed 不应出现在 confirmed 列表");
    let filtered =
        knowledge::list_confirmed_scoped(fixture.db(), Some((1, Some("shop"))), "退款", 50)
            .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].title, "订单退款比例");
}

#[test]
fn scope_filters_prevent_cross_database_mixing() {
    let fixture = Fixture::new();
    // scoped to connection 1 / db shop, without freeing to other dbs.
    for (connection, database) in [(1i64, Some("shop")), (2i64, Some("analytics"))] {
        let edit = entry("k", "销售额口径", "confirmed", Some(connection), database);
        let mut edit2 = edit.clone();
        edit2.id = format!("k-{connection}");
        knowledge::save(fixture.db(), 1_000, &edit2, None).unwrap();
    }
    // 无目标不返回任何作用域条目，避免旧库口径进入新会话。
    let none = knowledge::list_confirmed_scoped(fixture.db(), None, "", 50).unwrap();
    assert!(none.is_empty(), "有作用域的条目未指定匹配目标不能进候选");
    let match_1 =
        knowledge::list_confirmed_scoped(fixture.db(), Some((1, Some("shop"))), "", 50).unwrap();
    assert_eq!(match_1.len(), 1, "只有一个条目本作用域可匹配");
    assert_eq!(match_1[0].id, "k-1");
    // same connection different DB is rejected by scoped filter.
    let wrong_db =
        knowledge::list_confirmed_scoped(fixture.db(), Some((1, Some("analytics"))), "", 50)
            .unwrap();
    assert!(wrong_db.is_empty());
}

#[test]
fn proposed_confirm_retire_lifecycle_cas() {
    let fixture = Fixture::new();
    let edit = entry("k", "退款规则", "proposed", None, None);
    knowledge::save(fixture.db(), 1_000, &edit, None).unwrap();
    // proposed entries do not enter confirmed candidates.
    assert!(
        knowledge::list_confirmed_scoped(fixture.db(), None, "", 50)
            .unwrap()
            .is_empty()
    );
    // stale revision confirm should fail and leave proposed unchanged.
    let err = knowledge::confirm(fixture.db(), 2_000, "k", 99)
        .err()
        .unwrap();
    assert!(err.message.contains("重新加载"));
    let view = knowledge::load(fixture.db(), "k").unwrap();
    assert_eq!(view.status, "proposed");
    // correct revision confirm works and bumps revision.
    let revision = knowledge::confirm(fixture.db(), 2_001, "k", view.revision).unwrap();
    assert_eq!(revision, 2);
    // retire is allowed from confirmed, returning next revision.
    let revision = knowledge::retire(fixture.db(), 2_002, "k", revision).unwrap();
    assert_eq!(revision, 3);
    // retired is not in confirmed results and cannot be re-confirmed.
    assert!(
        knowledge::list_confirmed_scoped(fixture.db(), None, "", 50)
            .unwrap()
            .is_empty()
    );
    let err = knowledge::confirm(fixture.db(), 2_003, "k", revision)
        .err()
        .unwrap();
    assert!(err.message.contains("可变更状态"));
}

#[test]
fn save_validates_title_status_body_and_cas() {
    let fixture = Fixture::new();
    let mut edit = entry("k1", "  ", "confirmed", None, None);
    assert!(knowledge::save(fixture.db(), 1, &edit, None).is_err());
    edit = entry("k1", "标题", "unknown", None, None);
    assert!(knowledge::save(fixture.db(), 1, &edit, None).is_err());
    edit = entry("k1", "标题", "confirmed", None, None);
    edit.content_json = "not-json".into();
    assert!(knowledge::save(fixture.db(), 1, &edit, None).is_err());
    edit = entry("k1", "标题", "confirmed", None, None);
    let revision = knowledge::save(fixture.db(), 1, &edit, None).unwrap();
    assert_eq!(revision, 1);
    // 过期 revision 必须拒绝，不能覆盖。
    let mut next = entry("k1", "改标题", "confirmed", None, None);
    next.content_json = r#"{"text":"新规则"}"#.into();
    assert!(knowledge::save(fixture.db(), 2, &next, Some(99)).is_err());
    let newer = knowledge::save(fixture.db(), 2, &next, Some(revision)).unwrap();
    assert_eq!(newer, 2);
    // retired entries are terminal; save cannot resurrect.
    let revision = knowledge::retire(fixture.db(), 3, "k1", newer).unwrap();
    edit = entry("k1", "改标题", "confirmed", None, None);
    assert!(knowledge::save(fixture.db(), 4, &edit, Some(revision)).is_err());
}

#[test]
fn oversized_body_is_rejected() {
    let fixture = Fixture::new();
    let mut edit = entry("big", "规则", "confirmed", None, None);
    edit.content_json = format!(r#"{{"text":"{}"}}"#, "a".repeat(64 * 1024));
    assert!(knowledge::save(fixture.db(), 1, &edit, None).is_err());
}

#[test]
fn confirmed_excerpt_is_bounded() {
    let fixture = Fixture::new();
    let mut edit = entry("big", "大规则", "confirmed", None, None);
    edit.content_json = format!(r#"{{"text":"{}"}}"#, "b".repeat(4_000));
    knowledge::save(fixture.db(), 1, &edit, None).unwrap();
    let excerpt = knowledge::confirmed_excerpt(fixture.db(), "big", 1_024).unwrap();
    assert!(excerpt.len() <= 1_100);
    assert!(excerpt.contains("截断"));
    // proposed entries do not feed confirmed context.
    edit = entry("prop", "提议规则", "proposed", None, None);
    knowledge::save(fixture.db(), 2, &edit, None).unwrap();
    assert!(knowledge::confirmed_excerpt(fixture.db(), "prop", 1024).is_err());
}
