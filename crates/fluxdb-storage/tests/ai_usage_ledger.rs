//! Offline ledger contracts: isolated file SQLite, real foreign keys, no user data or Provider.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use aes_gcm::{Aes256Gcm, KeyInit};
use fluxdb_core::Usage;
use fluxdb_storage::ai_platform::{
    agents, ensure_schema, mcp, models, sessions,
    usage_ledger::{self, UsageRequest, UsageTotals},
};
use rusqlite::Connection;

const DAY: i64 = 86_400_000;
const NOW: i64 = 10 * DAY + 1_000;
const SESSION: &str = "usage-session";
const TURN: &str = "usage-turn";
const MODEL: &str = "usage-model";
const AGENT: &str = "usage-agent";
const PRICES: [Option<i64>; 3] = [Some(1_000_000), Some(2_000_000), Some(500_000)];

fn policy(concurrency: i64) -> mcp::BudgetPolicyView {
    mcp::BudgetPolicyView {
        id: "usage-policy".into(),
        display_name: "Offline policy".into(),
        enabled: true,
        max_iterations: 40,
        max_wall_time_ms: 600_000,
        max_model_concurrency: concurrency,
        revision: 1,
    }
}

fn open(path: &Path) -> Connection {
    let conn = Connection::open(path).expect("open isolated SQLite");
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert!(
        conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))
            .unwrap()
    );
    conn
}

struct Fixture {
    dir: PathBuf,
    conn: Option<Connection>,
}

impl Fixture {
    fn new(prices: [Option<i64>; 3], budget: Option<mcp::BudgetPolicyView>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-usage-ledger-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&dir).expect("create exclusive fixture directory");
        let fixture = Self {
            conn: Some(open(&dir.join("usage.sqlite"))),
            dir,
        };
        ensure_schema(fixture.db()).unwrap();
        fixture.save_prices(prices);
        if let Some(ref p) = budget {
            mcp::save_budget_policy(fixture.db(), NOW, p, None).unwrap();
        }
        agents::create_agent(
            fixture.db(),
            NOW,
            &agents::AgentEdit {
                id: AGENT.into(),
                display_name: "Offline ledger agent".into(),
                description: String::new(),
                enabled: true,
                default_model_profile_id: Some(MODEL.into()),
                system_prompt: "No external calls".into(),
                budget_policy_id: budget.map(|p| p.id),
                allowed_tools: vec![],
                compression_trigger_percent: 80,
            },
        )
        .unwrap();
        fixture.add_session(SESSION, TURN);
        fixture
    }

    fn db(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }

    fn path(&self) -> PathBuf {
        self.dir.join("usage.sqlite")
    }

    fn reopen(&mut self) {
        self.conn.take().unwrap().close().unwrap();
        let conn = open(&self.path());
        ensure_schema(&conn).unwrap();
        self.conn = Some(conn);
    }

    fn save_prices(&self, prices: [Option<i64>; 3]) {
        let revision = models::load_model(self.db(), MODEL)
            .ok()
            .map(|m| m.revision);
        let cipher = Aes256Gcm::new_from_slice(&[0; 32]).unwrap();
        models::save_model(
            self.db(),
            &cipher,
            "fixture-only",
            NOW,
            &models::ModelEdit {
                id: MODEL.into(),
                display_name: "Offline model".into(),
                provider: "openai-compatible".into(),
                api_protocol: "openai-compatible".into(),
                kind: models::ModelKind::Generation,
                model: "offline".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                endpoint_is_full_url: false,
                enabled: true,
                egress: models::EgressPermission::Allow,
                auth_mode: models::AuthMode::None,
                context_window_tokens: Some(8_192),
                max_output_tokens: Some(1_024),
                input_price_micros_per_million: prices[0],
                output_price_micros_per_million: prices[1],
                cached_input_price_micros_per_million: prices[2],
            },
            revision,
            models::ApiKeyChange::Keep,
        )
        .unwrap();
    }

    fn add_session(&self, session: &str, turn: &str) {
        sessions::create_session(self.db(), NOW, session, AGENT, "Offline session").unwrap();
        sessions::start_turn(self.db(), NOW, session, turn, "{}", "{}").unwrap();
    }

    fn set_status(&self, status: sessions::TurnStatus) {
        let revision = sessions::load_turn(self.db(), TURN).unwrap().revision;
        sessions::update_turn_status(self.db(), NOW + 1, TURN, status, None, revision).unwrap();
    }

    fn request(&self, id: &str) -> UsageRequest {
        UsageRequest {
            request_id: id.into(),
            session_id: SESSION.into(),
            turn_id: TURN.into(),
            model_profile_id: MODEL.into(),
            purpose: "main".into(),
            title: None,
        }
    }

    fn start(&self, id: &str) {
        assert!(usage_ledger::start_request(self.db(), NOW, &self.request(id)).unwrap());
    }

    fn totals(&self) -> UsageTotals {
        usage_ledger::totals(self.db(), SESSION).unwrap()
    }

    fn count(&self) -> i64 {
        self.db()
            .query_row("SELECT COUNT(*) FROM ai_usage_ledger", [], |r| r.get(0))
            .unwrap()
    }

    fn ledger(&self, id: &str) -> serde_json::Value {
        self.db()
            .query_row(
                "SELECT input_tokens, output_tokens, cached_input_tokens, total_tokens,
                        measurement, purpose, attempt, created_at_ms, updated_at_ms,
                        model_profile_id, budget_policy_id
                   FROM ai_usage_ledger WHERE request_id=?1",
                [id],
                |r| {
                    Ok(serde_json::json!({
                        "input": r.get::<_, Option<i64>>(0)?,
                        "output": r.get::<_, Option<i64>>(1)?,
                        "cache": r.get::<_, Option<i64>>(2)?,
                        "total": r.get::<_, Option<i64>>(3)?,
                        "measurement": r.get::<_, String>(4)?,
                        "purpose": r.get::<_, String>(5)?,
                        "attempt": r.get::<_, i64>(6)?,
                        "created": r.get::<_, i64>(7)?,
                        "updated": r.get::<_, i64>(8)?,
                        "model": r.get::<_, String>(9)?,
                        "policy": r.get::<_, Option<String>>(10)?,
                    }))
                },
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.conn.take());
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn usage(input: i64, output: i64, cached: i64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
    }
}

#[test]
fn actual_usage_counts_input_plus_output_without_double_counting_cache() {
    let f = Fixture::new(PRICES, Some(policy(2)));
    f.start("normal");
    assert_eq!(f.ledger("normal")["measurement"], "pending");
    assert_eq!(f.totals().unknown_requests, 1);
    assert!(usage_ledger::finish_request(f.db(), NOW + 5, "normal", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.ledger("normal")["total"], 12);
    assert_eq!(
        f.totals(),
        UsageTotals {
            requests: 1,
            input_tokens: 8,
            output_tokens: 4,
            cached_input_tokens: 2,
            total_tokens: 12,
            unknown_requests: 0,
        }
    );
}

#[test]
fn all_purposes_share_totals_but_sessions_do_not() {
    let f = Fixture::new([None; 3], None);
    for purpose in ["main", "decision", "compaction", "artifact", "title"] {
        let mut r = f.request(purpose);
        r.purpose = purpose.into();
        assert!(usage_ledger::start_request(f.db(), NOW, &r).unwrap());
        assert!(
            usage_ledger::finish_request(f.db(), NOW + 1, purpose, Some(usage(8, 4, 2))).unwrap()
        );
    }
    assert_eq!(f.totals().requests, 5);
    assert_eq!(f.totals().total_tokens, 60);
    assert_eq!(f.totals().cached_input_tokens, 10);
    f.add_session("other-session", "other-turn");
    let mut r = f.request("other");
    r.session_id = "other-session".into();
    r.turn_id = "other-turn".into();
    assert!(usage_ledger::start_request(f.db(), NOW, &r).unwrap());
    assert!(
        usage_ledger::finish_request(f.db(), NOW + 1, "other", Some(usage(100, 200, 10))).unwrap()
    );
    assert_eq!(f.totals().total_tokens, 60);
    assert_eq!(
        usage_ledger::totals(f.db(), "other-session")
            .unwrap()
            .total_tokens,
        300
    );
    assert_eq!(
        usage_ledger::totals(f.db(), "missing").unwrap(),
        UsageTotals::default()
    );
}

#[test]
fn duplicate_start_never_mutates_or_counts_twice() {
    let f = Fixture::new(PRICES, Some(policy(1)));
    f.start("same");
    let before = f.ledger("same");
    assert!(!usage_ledger::start_request(f.db(), NOW + 1, &f.request("same")).unwrap());
    assert_eq!(f.ledger("same"), before);
    assert!(usage_ledger::finish_request(f.db(), NOW + 2, "same", Some(usage(8, 4, 2))).unwrap());
    assert!(!usage_ledger::start_request(f.db(), NOW + 3, &f.request("same")).unwrap());
    assert_eq!(f.count(), 1);
}

#[test]
fn duplicate_finish_does_not_overwrite_actual_or_unknown() {
    let f = Fixture::new(PRICES, None);
    for (id, first) in [("actual", Some(usage(8, 4, 2))), ("unknown", None)] {
        f.start(id);
        assert!(usage_ledger::finish_request(f.db(), NOW + 1, id, first).unwrap());
        let before = f.ledger(id);
        for second in [None, Some(usage(100, 200, 0))] {
            assert!(!usage_ledger::finish_request(f.db(), NOW + 2, id, second).unwrap());
            assert_eq!(f.ledger(id), before);
        }
    }
    assert_eq!(f.count(), 2);
    assert_eq!(f.totals().total_tokens, 12);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn invalid_identity_and_purpose_create_no_rows() {
    let f = Fixture::new(PRICES, None);
    for id in ["", " ", "\n\t"] {
        assert!(usage_ledger::start_request(f.db(), NOW, &f.request(id)).is_err());
    }
    let mut r = f.request("bad");
    r.purpose = "not-a-purpose".into();
    assert!(usage_ledger::start_request(f.db(), NOW, &r).is_err());
    assert_eq!(f.count(), 0);
}

#[test]
fn missing_references_and_wrong_ownership_are_rejected_atomically() {
    let f = Fixture::new(PRICES, None);
    f.add_session("other-session", "other-turn");
    for (session, turn, model) in [
        ("missing-session", TURN, MODEL),
        (SESSION, "missing-turn", MODEL),
        (SESSION, TURN, "missing-model"),
        (SESSION, "other-turn", MODEL),
    ] {
        let mut r = f.request("bad-reference");
        r.session_id = session.into();
        r.turn_id = turn.into();
        r.model_profile_id = model.into();
        assert!(usage_ledger::start_request(f.db(), NOW, &r).is_err());
        assert_eq!(f.count(), 0);
    }
}

#[test]
fn invalid_or_overflowing_usage_preserves_pending_and_allows_retry() {
    let f = Fixture::new(PRICES, None);
    f.start("invalid");
    let before = f.ledger("invalid");
    for u in [
        usage(-1, 0, 0),
        usage(1, -1, 0),
        usage(1, 0, -1),
        usage(1, 0, 2),
        usage(i64::MAX, 1, 0),
    ] {
        assert!(usage_ledger::finish_request(f.db(), NOW + 1, "invalid", Some(u)).is_err());
        assert_eq!(f.ledger("invalid"), before);
    }
    assert!(
        usage_ledger::finish_request(f.db(), NOW + 2, "invalid", Some(usage(8, 4, 2))).unwrap()
    );
}

#[test]
fn finishing_unregistered_request_is_an_error() {
    let f = Fixture::new(PRICES, None);
    for u in [None, Some(Usage::default())] {
        assert!(usage_ledger::finish_request(f.db(), NOW, "missing", u).is_err());
    }
    assert_eq!(f.count(), 0);
}

#[test]
fn insert_failure_rolls_back_and_reopen_can_retry() {
    let mut f = Fixture::new(PRICES, None);
    f.db()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_insert BEFORE INSERT ON ai_usage_ledger
        BEGIN SELECT RAISE(ABORT, 'fixture insert failure'); END;",
        )
        .unwrap();
    assert!(usage_ledger::start_request(f.db(), NOW, &f.request("retry")).is_err());
    assert_eq!(f.count(), 0);
    assert!(f.db().is_autocommit());
    f.reopen();
    assert_eq!(f.count(), 0);
    f.start("retry");
}

#[test]
fn finish_failure_rolls_back_and_reopen_can_retry() {
    let mut f = Fixture::new(PRICES, None);
    f.start("retry");
    let before = f.ledger("retry");
    f.db()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_finish BEFORE UPDATE OF measurement ON ai_usage_ledger
        WHEN NEW.measurement='actual' BEGIN SELECT RAISE(ABORT, 'fixture finish failure'); END;",
        )
        .unwrap();
    assert!(usage_ledger::finish_request(f.db(), NOW + 1, "retry", Some(usage(8, 4, 2))).is_err());
    assert_eq!(f.ledger("retry"), before);
    assert!(f.db().is_autocommit());
    f.reopen();
    assert_eq!(f.ledger("retry"), before);
    assert!(usage_ledger::finish_request(f.db(), NOW + 2, "retry", Some(usage(8, 4, 2))).unwrap());
}

#[test]
fn missing_usage_is_unknown_not_zero_and_releases_concurrency() {
    let f = Fixture::new([None; 3], Some(policy(1)));
    f.start("timeout");
    assert!(usage_ledger::finish_request(f.db(), NOW + 1, "timeout", None).unwrap());
    let row = f.ledger("timeout");
    assert_eq!(row["measurement"], "unknown");
    for field in ["input", "output", "cache", "total"] {
        assert!(row[field].is_null());
    }
    f.start("next");
    assert!(usage_ledger::finish_request(f.db(), NOW + 2, "next", Some(Usage::default())).unwrap());
    assert_eq!(f.ledger("next")["total"], 0);
    assert_eq!(f.totals().requests, 2);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn prices_and_price_edits_never_affect_token_totals_or_request_eligibility() {
    for prices in [
        [None; 3],
        [Some(0); 3],
        PRICES,
        [Some(i64::MAX); 3],
        [Some(1), Some(2), None],
    ] {
        let f = Fixture::new(prices, Some(policy(1)));
        f.start("before-price-edit");
        f.save_prices(PRICES);
        assert!(
            usage_ledger::finish_request(
                f.db(),
                NOW + 1,
                "before-price-edit",
                Some(usage(8, 4, 2))
            )
            .unwrap()
        );
        assert_eq!(f.totals().total_tokens, 12);
        assert_eq!(f.totals().unknown_requests, 0);
        f.start("after-price-edit");
    }
}

#[test]
fn schema_has_token_totals_and_no_cost_limits_or_reservations() {
    let f = Fixture::new(PRICES, None);
    for (table, forbidden) in [
        (
            "ai_usage_ledger",
            vec!["cost_micros", "pricing_snapshot_json"],
        ),
        (
            "ai_budget_policies",
            vec![
                "session_limit_micros",
                "daily_limit_micros",
                "request_limit_micros",
            ],
        ),
    ] {
        let names: Vec<String> = f
            .db()
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for name in forbidden {
            assert!(!names.iter().any(|n| n == name));
        }
        if table == "ai_usage_ledger" {
            assert!(names.iter().any(|n| n == "total_tokens"));
        }
    }
    let exists: bool = f
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='ai_budget_reservations')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!exists);
}

#[test]
fn policy_concurrency_is_shared_across_sessions_and_actual_finish_frees_slot() {
    let f = Fixture::new(PRICES, Some(policy(1)));
    f.add_session("other-session", "other-turn");
    f.start("first");
    let mut r = f.request("second");
    r.session_id = "other-session".into();
    r.turn_id = "other-turn".into();
    assert!(usage_ledger::start_request(f.db(), NOW, &r).is_err());
    assert!(usage_ledger::finish_request(f.db(), NOW + 1, "first", Some(usage(8, 4, 2))).unwrap());
    assert!(usage_ledger::start_request(f.db(), NOW + 2, &r).unwrap());
}

#[test]
fn disabled_policy_does_not_limit_concurrency() {
    let mut p = policy(1);
    p.enabled = false;
    let f = Fixture::new([None; 3], Some(p));
    f.start("first");
    f.start("second");
    assert_eq!(f.count(), 2);
}

#[test]
fn terminal_and_cancel_requested_turns_reject_new_requests() {
    for status in [
        sessions::TurnStatus::Cancelled,
        sessions::TurnStatus::Completed,
        sessions::TurnStatus::Failed,
        sessions::TurnStatus::Interrupted,
        sessions::TurnStatus::OutcomeUnknown,
        sessions::TurnStatus::CancelRequested,
    ] {
        let f = Fixture::new(PRICES, None);
        f.set_status(status);
        assert!(
            usage_ledger::start_request(f.db(), NOW + 2, &f.request("late")).is_err(),
            "{status:?}"
        );
        assert_eq!(f.count(), 0);
    }
}

#[test]
fn active_turns_accept_requests() {
    for status in [
        sessions::TurnStatus::Queued,
        sessions::TurnStatus::Preparing,
        sessions::TurnStatus::Generating,
        sessions::TurnStatus::ExecutingTools,
        sessions::TurnStatus::WaitingApproval,
        sessions::TurnStatus::WaitingUser,
    ] {
        let f = Fixture::new(PRICES, None);
        f.set_status(status);
        assert!(usage_ledger::start_request(f.db(), NOW + 2, &f.request("active")).unwrap());
    }
}

#[test]
fn cancellation_still_records_inflight_actual_or_unknown_tokens() {
    for result in [Some(usage(8, 4, 2)), None] {
        let f = Fixture::new(PRICES, None);
        f.start("inflight");
        f.set_status(sessions::TurnStatus::Cancelled);
        assert!(usage_ledger::finish_request(f.db(), NOW + 2, "inflight", result).unwrap());
        assert_eq!(
            f.totals().total_tokens,
            if result.is_some() { 12 } else { 0 }
        );
        assert_eq!(
            f.totals().unknown_requests,
            if result.is_some() { 0 } else { 1 }
        );
        assert_eq!(
            sessions::load_turn(f.db(), TURN).unwrap().status,
            sessions::TurnStatus::Cancelled
        );
    }
}

#[test]
fn records_survive_reopen_with_idempotency_intact() {
    let mut f = Fixture::new(PRICES, None);
    for id in ["pending", "actual", "unknown"] {
        f.start(id);
    }
    assert!(usage_ledger::finish_request(f.db(), NOW + 1, "actual", Some(usage(8, 4, 2))).unwrap());
    assert!(usage_ledger::finish_request(f.db(), NOW + 1, "unknown", None).unwrap());
    let before = f.totals();
    f.reopen();
    assert_eq!(f.totals(), before);
    for id in ["pending", "actual", "unknown"] {
        assert!(!usage_ledger::start_request(f.db(), NOW + 2, &f.request(id)).unwrap());
    }
    assert!(!usage_ledger::finish_request(f.db(), NOW + 2, "actual", None).unwrap());
    assert!(
        !usage_ledger::finish_request(f.db(), NOW + 2, "unknown", Some(usage(1, 0, 0))).unwrap()
    );
    assert!(
        usage_ledger::finish_request(f.db(), NOW + 2, "pending", Some(usage(8, 4, 2))).unwrap()
    );
    assert_eq!(f.totals().total_tokens, 24);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn startup_marks_pending_unknown_and_frees_slots_without_replaying() {
    let mut f = Fixture::new(PRICES, Some(policy(1)));
    f.start("interrupted");
    fluxdb_storage::ai_platform::recovery::recover_on_startup(f.conn.as_mut().unwrap(), NOW + 1)
        .unwrap();
    assert_eq!(f.ledger("interrupted")["measurement"], "unknown");
    assert_eq!(f.totals().unknown_requests, 1);
    assert!(
        !usage_ledger::finish_request(f.db(), NOW + 2, "interrupted", Some(usage(8, 4, 2)))
            .unwrap()
    );
    f.add_session("new-session", "new-turn");
    let mut r = f.request("new");
    r.session_id = "new-session".into();
    r.turn_id = "new-turn".into();
    assert!(usage_ledger::start_request(f.db(), NOW + 2, &r).unwrap());
}

fn race_start(f: &Fixture, requests: Vec<UsageRequest>) -> Vec<Result<bool, String>> {
    let barrier = Arc::new(Barrier::new(requests.len()));
    let handles: Vec<_> = requests
        .into_iter()
        .map(|request| {
            let barrier = Arc::clone(&barrier);
            let path = f.path();
            thread::spawn(move || {
                let conn = open(&path);
                barrier.wait();
                usage_ledger::start_request(&conn, NOW, &request).map_err(|e| e.to_string())
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

#[test]
fn concurrent_starts_enforce_concurrency_and_deduplicate_request_id() {
    let f = Fixture::new(PRICES, Some(policy(2)));
    let results = race_start(
        &f,
        (0..8).map(|i| f.request(&format!("race-{i}"))).collect(),
    );
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(true))).count(),
        2,
        "{results:?}"
    );
    for error in results.into_iter().filter_map(Result::err) {
        assert!(error.contains("并发"), "{error}");
    }
    assert_eq!(f.count(), 2);
    let f = Fixture::new(PRICES, Some(policy(1)));
    let results = race_start(&f, vec![f.request("same"); 8]);
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(true))).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(false))).count(),
        7,
        "{results:?}"
    );
    assert_eq!(f.count(), 1);
}

#[test]
fn concurrent_finish_has_one_winner_without_double_counting() {
    let f = Fixture::new(PRICES, None);
    f.start("same");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = f.path();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let conn = open(&path);
                barrier.wait();
                usage_ledger::finish_request(&conn, NOW + 1, "same", Some(usage(8, 4, 2)))
                    .map_err(|e| e.to_string())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(true))).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(false))).count(),
        7,
        "{results:?}"
    );
    assert_eq!(f.totals().total_tokens, 12);
    assert_eq!(f.totals().unknown_requests, 0);
}
