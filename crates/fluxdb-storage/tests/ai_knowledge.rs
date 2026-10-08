//! 知识条目（`ai_knowledge_entries`）读写与 `@` 候选检索契约：独立 SQLite、真实 FK。
use std::{fs, path::PathBuf};

use fluxdb_storage::ai_platform::{ensure_schema, knowledge};
use rusqlite::Connection;

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
    knowledge::save(
        fixture.db(),
        1_000,
        "k1",
        "订单金额口径",
        "metric",
        None,
        Some("shop"),
        None,
        r#"{"formula":"sum(net_amount)"}"#,
        "confirmed",
        None,
    )
    .expect("保存知识条目");
    fixture.reopen();
    let rows = knowledge::list_confirmed(fixture.db(), "", 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "订单金额口径");
    assert_eq!(rows[0].kind, "metric");
    assert_eq!(rows[0].database_name.as_deref(), Some("shop"));
}

#[test]
fn list_only_returns_confirmed_and_filters_by_query() {
    let fixture = Fixture::new();
    for (id, title, status) in [
        ("k1", "订单金额口径", "confirmed"),
        ("k2", "退款规则", "proposed"),
        ("k3", "订单退款比例", "confirmed"),
    ] {
        knowledge::save(
            fixture.db(),
            1_000,
            id,
            title,
            "metric",
            None,
            Some("shop"),
            None,
            "{}",
            status,
            None,
        )
        .unwrap();
    }
    let all = knowledge::list_confirmed(fixture.db(), "", 50).unwrap();
    assert_eq!(all.len(), 2, "proposed 不应出现在 confirmed 列表");
    let filtered = knowledge::list_confirmed(fixture.db(), "退款", 50).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].title, "订单退款比例");
}

#[test]
fn save_validates_title_status_body_and_cas() {
    let fixture = Fixture::new();
    assert!(
        knowledge::save(
            fixture.db(),
            1,
            "k1",
            "  ",
            "metric",
            None,
            None,
            None,
            "{}",
            "confirmed",
            None
        )
        .is_err()
    );
    assert!(
        knowledge::save(
            fixture.db(),
            1,
            "k1",
            "标题",
            "metric",
            None,
            None,
            None,
            "{}",
            "unknown",
            None
        )
        .is_err()
    );
    assert!(
        knowledge::save(
            fixture.db(),
            1,
            "k1",
            "标题",
            "metric",
            None,
            None,
            None,
            "not-json",
            "confirmed",
            None
        )
        .is_err()
    );
    let rev = knowledge::save(
        fixture.db(),
        1,
        "k1",
        "标题",
        "metric",
        None,
        None,
        None,
        "{}",
        "confirmed",
        None,
    )
    .unwrap();
    assert_eq!(rev, 1);
    // 过期 revision 必须拒绝，不能覆盖。
    assert!(
        knowledge::save(
            fixture.db(),
            2,
            "k1",
            "改标题",
            "metric",
            None,
            None,
            None,
            "{}",
            "confirmed",
            Some(99)
        )
        .is_err()
    );
    let rev2 = knowledge::save(
        fixture.db(),
        2,
        "k1",
        "改标题",
        "metric",
        None,
        None,
        None,
        "{}",
        "confirmed",
        Some(rev),
    )
    .unwrap();
    assert_eq!(rev2, 2);
}
