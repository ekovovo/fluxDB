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

fn policy(session: Option<i64>, daily: Option<i64>, concurrency: i64) -> mcp::BudgetPolicyView {
    mcp::BudgetPolicyView {
        id: "usage-policy".into(),
        display_name: "Offline policy".into(),
        enabled: true,
        session_limit_micros: session,
        daily_limit_micros: daily,
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
            estimated_input_tokens: 10,
            max_output_tokens: 20,
            title: None,
        }
    }

    fn reserve(&self, id: &str) {
        assert!(usage_ledger::reserve(self.db(), NOW, &self.request(id)).unwrap());
    }

    fn totals(&self) -> UsageTotals {
        usage_ledger::totals(self.db(), SESSION).unwrap()
    }

    fn counts(&self) -> (i64, i64) {
        self.db()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM ai_usage_ledger),
                        (SELECT COUNT(*) FROM ai_budget_reservations)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn reservation(&self, id: &str) -> (String, i64, i64) {
        self.db()
            .query_row(
                "SELECT status, amount_micros, updated_at_ms
                   FROM ai_budget_reservations WHERE request_id=?1 AND attempt=1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    fn ledger(&self, id: &str) -> serde_json::Value {
        self.db()
            .query_row(
                "SELECT input_tokens, output_tokens, cached_input_tokens, cost_micros,
                        measurement, pricing_snapshot_json, purpose, attempt, created_at_ms,
                        model_profile_id, budget_policy_id
                   FROM ai_usage_ledger WHERE request_id=?1",
                [id],
                |r| {
                    Ok(serde_json::json!({
                        "input": r.get::<_, Option<i64>>(0)?,
                        "output": r.get::<_, Option<i64>>(1)?,
                        "cache": r.get::<_, Option<i64>>(2)?,
                        "cost": r.get::<_, Option<i64>>(3)?,
                        "measurement": r.get::<_, String>(4)?,
                        "pricing": r.get::<_, String>(5)?,
                        "purpose": r.get::<_, String>(6)?,
                        "attempt": r.get::<_, i64>(7)?,
                        "created": r.get::<_, i64>(8)?,
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
fn reserve_then_settle_counts_actual_cached_tokens_and_cost_once() {
    let f = Fixture::new(PRICES, Some(policy(Some(100), None, 10)));
    f.reserve("normal");
    assert_eq!(f.reservation("normal"), ("held".into(), 50, NOW));
    assert_eq!(f.counts(), (1, 1));
    assert_eq!(f.totals().unknown_requests, 1);
    assert!(usage_ledger::settle(f.db(), NOW + 5, "normal", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.reservation("normal"), ("settled".into(), 15, NOW + 5));
    assert_eq!(
        f.totals(),
        UsageTotals {
            requests: 1,
            input_tokens: 8,
            output_tokens: 4,
            cost_micros: 15,
            unknown_requests: 0
        }
    );
    let row = f.ledger("normal");
    assert_eq!(row["cache"], 2);
    assert_eq!(row["measurement"], "actual");
    assert_eq!(row["attempt"], 1);
    assert_eq!(row["model"], MODEL);
    assert_eq!(row["policy"], "usage-policy");
    assert_eq!(row["created"], NOW);
}

#[test]
fn all_four_purposes_share_ledger_and_totals_are_session_scoped() {
    let f = Fixture::new(PRICES, None);
    for purpose in ["main", "decision", "compaction", "artifact"] {
        let mut request = f.request(purpose);
        request.purpose = purpose.into();
        assert!(usage_ledger::reserve(f.db(), NOW, &request).unwrap());
        assert!(usage_ledger::settle(f.db(), NOW + 1, purpose, Some(usage(8, 4, 2))).unwrap());
        assert_eq!(f.ledger(purpose)["purpose"], purpose);
    }
    f.add_session("other-session", "other-turn");
    let mut other = f.request("other");
    other.session_id = "other-session".into();
    other.turn_id = "other-turn".into();
    assert!(usage_ledger::reserve(f.db(), NOW, &other).unwrap());
    assert_eq!(
        f.totals(),
        UsageTotals {
            requests: 4,
            input_tokens: 32,
            output_tokens: 16,
            cost_micros: 60,
            unknown_requests: 0
        }
    );
    assert_eq!(
        usage_ledger::totals(f.db(), "missing-session").unwrap(),
        UsageTotals::default()
    );
}

#[test]
fn repeated_reserve_is_false_and_does_not_mutate_or_recharge() {
    let f = Fixture::new(PRICES, Some(policy(Some(50), None, 1)));
    f.reserve("same");
    let before = f.ledger("same");
    let mut repeated = f.request("same");
    repeated.estimated_input_tokens = 1_000;
    assert!(!usage_ledger::reserve(f.db(), NOW + 10, &repeated).unwrap());
    assert_eq!(f.ledger("same"), before);
    assert_eq!(f.reservation("same"), ("held".into(), 50, NOW));
    assert_eq!(f.counts(), (1, 1));
    assert!(usage_ledger::settle(f.db(), NOW + 20, "same", Some(usage(8, 4, 2))).unwrap());
    assert!(!usage_ledger::reserve(f.db(), NOW + 30, &repeated).unwrap());
    assert_eq!(f.reservation("same").1, 15);
}

#[test]
fn repeated_settlement_does_not_overwrite_actual_or_unknown() {
    let f = Fixture::new(PRICES, None);
    for (id, first) in [("actual", Some(usage(8, 4, 2))), ("unknown", None)] {
        f.reserve(id);
        assert!(usage_ledger::settle(f.db(), NOW + 1, id, first).unwrap());
        let row = f.ledger(id);
        let reservation = f.reservation(id);
        for replacement in [None, Some(usage(99, 99, 0))] {
            assert!(!usage_ledger::settle(f.db(), NOW + 2, id, replacement).unwrap());
            assert_eq!(f.ledger(id), row);
            assert_eq!(f.reservation(id), reservation);
        }
        assert!(!usage_ledger::reserve(f.db(), NOW + 3, &f.request(id)).unwrap());
    }
    assert_eq!(f.counts(), (2, 2));
    assert_eq!(f.totals().cost_micros, 15);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn invalid_request_parameters_leave_both_tables_empty() {
    let f = Fixture::new(PRICES, None);
    let mut requests = vec![];
    for purpose in ["", "MAIN", "tool"] {
        let mut r = f.request("invalid");
        r.purpose = purpose.into();
        requests.push(r);
    }
    let mut r = f.request("negative-input");
    r.estimated_input_tokens = -1;
    requests.push(r);
    let mut r = f.request("negative-output");
    r.max_output_tokens = -1;
    requests.push(r);
    for r in requests {
        assert!(usage_ledger::reserve(f.db(), NOW, &r).is_err(), "{r:?}");
        assert_eq!(f.counts(), (0, 0));
    }
}

#[test]
fn empty_or_whitespace_request_identity_is_rejected_without_rows() {
    let f = Fixture::new(PRICES, None);
    let accepted: Vec<_> = ["", " ", "\t\n"]
        .into_iter()
        .filter(|id| usage_ledger::reserve(f.db(), NOW, &f.request(id)).is_ok())
        .collect();
    assert!(
        accepted.is_empty(),
        "accepted blank request identities: {accepted:?}"
    );
    assert_eq!(f.counts(), (0, 0));
}

#[test]
fn missing_references_and_wrong_turn_ownership_are_rejected_atomically() {
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
        assert!(usage_ledger::reserve(f.db(), NOW, &r).is_err());
        assert_eq!(f.counts(), (0, 0));
    }
}

#[test]
fn invalid_usage_preserves_held_reservation_and_allows_valid_retry() {
    let f = Fixture::new(PRICES, None);
    f.reserve("invalid-usage");
    let before = f.ledger("invalid-usage");
    for u in [
        usage(-1, 0, 0),
        usage(1, -1, 0),
        usage(1, 0, -1),
        usage(1, 0, 2),
    ] {
        assert!(usage_ledger::settle(f.db(), NOW + 1, "invalid-usage", Some(u)).is_err());
        assert_eq!(f.ledger("invalid-usage"), before);
        assert_eq!(f.reservation("invalid-usage"), ("held".into(), 50, NOW));
    }
    assert!(usage_ledger::settle(f.db(), NOW + 2, "invalid-usage", Some(usage(8, 4, 2))).unwrap());
}

#[test]
fn settling_unreserved_request_is_an_error_without_creating_rows() {
    let f = Fixture::new(PRICES, None);
    assert!(usage_ledger::settle(f.db(), NOW, "missing", None).is_err());
    assert!(usage_ledger::settle(f.db(), NOW, "missing", Some(Usage::default())).is_err());
    assert_eq!(f.counts(), (0, 0));
}

#[test]
fn ledger_insert_failure_rolls_back_reservation_and_is_retryable() {
    let f = Fixture::new(PRICES, None);
    f.db()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_ledger BEFORE INSERT ON ai_usage_ledger
         BEGIN SELECT RAISE(ABORT, 'fixture insert failure'); END;",
        )
        .unwrap();
    assert!(usage_ledger::reserve(f.db(), NOW, &f.request("rollback")).is_err());
    assert_eq!(f.counts(), (0, 0));
    assert!(f.db().is_autocommit());
    f.db().execute_batch("DROP TRIGGER fail_ledger;").unwrap();
    f.reserve("rollback");
}

#[test]
fn settlement_write_failure_rolls_back_actual_usage_and_is_retryable() {
    let f = Fixture::new(PRICES, None);
    f.reserve("rollback");
    let before = f.ledger("rollback");
    f.db()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_settle BEFORE UPDATE OF status ON ai_budget_reservations
         WHEN NEW.status='settled'
         BEGIN SELECT RAISE(ABORT, 'fixture settlement failure'); END;",
        )
        .unwrap();
    assert!(usage_ledger::settle(f.db(), NOW + 1, "rollback", Some(usage(8, 4, 2))).is_err());
    assert_eq!(f.ledger("rollback"), before);
    assert_eq!(f.reservation("rollback"), ("held".into(), 50, NOW));
    assert!(f.db().is_autocommit());
    f.db().execute_batch("DROP TRIGGER fail_settle;").unwrap();
    assert!(usage_ledger::settle(f.db(), NOW + 2, "rollback", Some(usage(8, 4, 2))).unwrap());
}

#[test]
fn settlement_uses_only_the_pricing_snapshot_not_current_model_prices() {
    let f = Fixture::new([None; 3], None);
    f.reserve("originally-unpriced");
    let before = f.ledger("originally-unpriced")["pricing"].clone();
    f.save_prices(PRICES);
    assert!(
        usage_ledger::settle(f.db(), NOW + 1, "originally-unpriced", Some(usage(8, 4, 0))).unwrap()
    );
    assert_eq!(f.ledger("originally-unpriced")["pricing"], before);
    assert!(f.ledger("originally-unpriced")["cost"].is_null());
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn missing_usage_retains_amount_and_consumes_session_budget() {
    let f = Fixture::new(PRICES, Some(policy(Some(75), None, 10)));
    f.reserve("timeout");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "timeout", None).unwrap());
    assert_eq!(f.reservation("timeout"), ("unknown".into(), 50, NOW + 1));
    let row = f.ledger("timeout");
    assert!(row["input"].is_null());
    assert!(row["cost"].is_null());
    assert_eq!(row["measurement"], "unknown");
    assert_eq!(f.totals().unknown_requests, 1);
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &f.request("next")).is_err());
    assert_eq!(f.counts(), (1, 1));
}

#[test]
fn settled_actual_plus_held_plus_unknown_are_counted_without_double_charge() {
    let f = Fixture::new(PRICES, Some(policy(Some(150), None, 10)));
    f.reserve("actual");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "actual", Some(usage(8, 4, 2))).unwrap());
    f.reserve("held");
    f.reserve("unknown");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "unknown", None).unwrap());
    let mut r = f.request("boundary");
    r.estimated_input_tokens = 15;
    r.max_output_tokens = 10;
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &r).unwrap());
    assert!(usage_ledger::reserve(f.db(), NOW + 3, &f.request("over")).is_err());
    assert_eq!(f.counts(), (4, 4));
}

#[test]
fn actual_cost_can_exceed_reservation_and_blocks_next_request() {
    let f = Fixture::new(PRICES, Some(policy(Some(75), None, 10)));
    f.reserve("over-estimate");
    assert!(
        usage_ledger::settle(f.db(), NOW + 1, "over-estimate", Some(usage(80, 20, 0))).unwrap()
    );
    assert_eq!(f.reservation("over-estimate").1, 120);
    assert_eq!(f.totals().cost_micros, 120);
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &f.request("next")).is_err());
}

#[test]
fn policy_concurrency_is_shared_across_sessions_and_settlement_frees_slot() {
    let f = Fixture::new(PRICES, Some(policy(None, None, 1)));
    f.add_session("other-session", "other-turn");
    f.reserve("first");
    let mut r = f.request("second");
    r.session_id = "other-session".into();
    r.turn_id = "other-turn".into();
    assert!(usage_ledger::reserve(f.db(), NOW, &r).is_err());
    assert!(usage_ledger::settle(f.db(), NOW + 1, "first", Some(usage(8, 4, 2))).unwrap());
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &r).unwrap());
}

#[test]
fn daily_budget_is_policy_scoped_while_session_limit_is_session_scoped() {
    let f = Fixture::new(PRICES, Some(policy(Some(50), Some(75), 10)));
    f.add_session("other-session", "other-turn");
    f.reserve("first");
    let mut r = f.request("other");
    r.session_id = "other-session".into();
    r.turn_id = "other-turn".into();
    assert!(usage_ledger::reserve(f.db(), NOW, &r).is_err());
    assert_eq!(f.counts(), (1, 1));
    assert!(usage_ledger::reserve(f.db(), 11 * DAY, &r).unwrap());
    assert!(usage_ledger::reserve(f.db(), 11 * DAY, &f.request("same-session")).is_err());
}

#[test]
fn unknown_reservation_also_consumes_daily_budget() {
    let f = Fixture::new(PRICES, Some(policy(None, Some(75), 10)));
    f.reserve("unknown");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "unknown", None).unwrap());
    f.add_session("other-session", "other-turn");
    let mut r = f.request("other");
    r.session_id = "other-session".into();
    r.turn_id = "other-turn".into();
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &r).is_err());
    assert_eq!(f.counts(), (1, 1));
}

#[test]
fn no_policy_or_unlimited_policy_allows_missing_prices_but_not_free_cost() {
    for p in [None, Some(policy(None, None, 10))] {
        let f = Fixture::new([None; 3], p);
        f.reserve("unpriced");
        assert!(usage_ledger::settle(f.db(), NOW + 1, "unpriced", Some(usage(8, 4, 0))).unwrap());
        assert!(f.ledger("unpriced")["cost"].is_null());
        assert_eq!(f.ledger("unpriced")["measurement"], "actual");
        assert_eq!(f.totals().input_tokens, 8);
        assert_eq!(f.totals().output_tokens, 4);
        assert_eq!(f.totals().unknown_requests, 1);
    }
}

#[test]
fn any_enabled_amount_limit_requires_prices_and_failure_is_atomic() {
    for p in [
        policy(Some(100), None, 10),
        policy(None, Some(100), 10),
        policy(Some(0), None, 10),
    ] {
        for prices in [
            [None; 3],
            [None, PRICES[1], PRICES[2]],
            [PRICES[0], None, PRICES[2]],
        ] {
            let f = Fixture::new(prices, Some(p.clone()));
            let error = usage_ledger::reserve(f.db(), NOW, &f.request("strict")).unwrap_err();
            assert!(error.to_string().contains("价格"), "{error}");
            assert_eq!(f.counts(), (0, 0));
        }
    }
}

#[test]
fn disabled_policy_does_not_apply_amount_or_concurrency_limits() {
    let mut p = policy(Some(0), Some(0), 1);
    p.enabled = false;
    let f = Fixture::new([None; 3], Some(p));
    f.reserve("first");
    f.reserve("second");
    assert_eq!(f.counts(), (2, 2));
    assert_eq!(f.totals().unknown_requests, 2);
}

#[test]
fn missing_cache_price_is_unknown_only_when_cached_tokens_are_used() {
    let f = Fixture::new(
        [PRICES[0], PRICES[1], None],
        Some(policy(Some(100), None, 10)),
    );
    f.reserve("cached");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "cached", Some(usage(8, 4, 2))).unwrap());
    assert!(f.ledger("cached")["cost"].is_null());
    assert_eq!(f.reservation("cached").1, 50);
    f.reserve("uncached");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "uncached", Some(usage(8, 4, 0))).unwrap());
    assert_eq!(f.ledger("uncached")["cost"], 16);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn price_snapshot_survives_model_edits_and_reopen() {
    let mut f = Fixture::new(PRICES, None);
    f.reserve("old-price");
    let before = f.ledger("old-price")["pricing"].clone();
    let snapshot: serde_json::Value = serde_json::from_str(before.as_str().unwrap()).unwrap();
    assert_eq!(snapshot["input"], PRICES[0].unwrap());
    assert_eq!(snapshot["output"], PRICES[1].unwrap());
    assert_eq!(snapshot["cache"], PRICES[2].unwrap());
    assert_eq!(
        snapshot["model_revision"],
        models::load_model(f.db(), MODEL).unwrap().revision
    );
    f.save_prices([Some(10_000_000); 3]);
    f.reopen();
    assert!(usage_ledger::settle(f.db(), NOW + 1, "old-price", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.ledger("old-price")["pricing"], before);
    assert_eq!(f.ledger("old-price")["cost"], 15);
    f.reserve("new-price");
    assert!(usage_ledger::settle(f.db(), NOW + 2, "new-price", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.ledger("new-price")["cost"], 120);
}

#[test]
fn pricing_rounds_up_combined_components_not_each_component() {
    let f = Fixture::new([Some(1); 3], None);
    f.reserve("fraction");
    assert_eq!(f.reservation("fraction").1, 1);
    assert!(usage_ledger::settle(f.db(), NOW + 1, "fraction", Some(usage(2, 1, 1))).unwrap());
    assert_eq!(f.ledger("fraction")["cost"], 1);
}

#[test]
fn zero_usage_is_known_zero_even_without_prices_and_explicit_zero_prices_are_valid() {
    for prices in [[None; 3], [Some(0); 3]] {
        let f = Fixture::new(prices, None);
        f.reserve("zero");
        assert!(usage_ledger::settle(f.db(), NOW + 1, "zero", Some(Usage::default())).unwrap());
        assert_eq!(f.ledger("zero")["cost"], 0);
        assert_eq!(f.reservation("zero").1, 0);
        assert_eq!(f.totals().unknown_requests, 0);
    }
    let f = Fixture::new([Some(0); 3], Some(policy(Some(0), Some(0), 10)));
    f.reserve("free-priced");
    assert!(usage_ledger::settle(f.db(), NOW + 1, "free-priced", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.totals().cost_micros, 0);
    assert_eq!(f.totals().unknown_requests, 0);
}

#[test]
fn unrepresentable_cost_is_unknown_not_wrapped_and_strict_reserve_rejects_it() {
    let f = Fixture::new([Some(i64::MAX); 3], Some(policy(Some(i64::MAX), None, 10)));
    let mut r = f.request("overflow");
    r.estimated_input_tokens = i64::MAX;
    r.max_output_tokens = i64::MAX;
    assert!(usage_ledger::reserve(f.db(), NOW, &r).is_err());
    assert_eq!(f.counts(), (0, 0));
    let f = Fixture::new([Some(i64::MAX); 3], None);
    f.reserve("overflow");
    let held = f.reservation("overflow").1;
    assert!(
        usage_ledger::settle(
            f.db(),
            NOW + 1,
            "overflow",
            Some(usage(i64::MAX, i64::MAX, 0))
        )
        .unwrap()
    );
    assert!(f.ledger("overflow")["cost"].is_null());
    assert_eq!(f.reservation("overflow").1, held);
    assert_eq!(f.totals().unknown_requests, 1);
}

#[test]
fn terminal_turns_reject_new_reservations() {
    for status in [
        sessions::TurnStatus::Cancelled,
        sessions::TurnStatus::Completed,
        sessions::TurnStatus::Failed,
        sessions::TurnStatus::Interrupted,
        sessions::TurnStatus::OutcomeUnknown,
    ] {
        let f = Fixture::new(PRICES, None);
        f.set_status(status);
        assert!(
            usage_ledger::reserve(f.db(), NOW + 2, &f.request("late")).is_err(),
            "{status:?}"
        );
        assert_eq!(f.counts(), (0, 0));
    }
}

#[test]
fn noncancelled_active_turns_accept_reservations() {
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
        assert!(usage_ledger::reserve(f.db(), NOW + 2, &f.request("active")).unwrap());
        assert_eq!(f.counts(), (1, 1));
    }
}

#[test]
fn cancel_requested_turn_must_not_start_another_supplier_request() {
    let f = Fixture::new(PRICES, None);
    f.set_status(sessions::TurnStatus::CancelRequested);
    assert!(
        usage_ledger::reserve(f.db(), NOW + 2, &f.request("after-cancel")).is_err(),
        "cancel_requested must prevent new supplier requests"
    );
    assert_eq!(f.counts(), (0, 0));
}

#[test]
fn cancelled_turn_still_accounts_for_inflight_actual_or_unknown_cost() {
    for result in [Some(usage(8, 4, 2)), None] {
        let f = Fixture::new(PRICES, None);
        f.reserve("inflight");
        f.set_status(sessions::TurnStatus::Cancelled);
        assert!(usage_ledger::settle(f.db(), NOW + 2, "inflight", result).unwrap());
        assert_eq!(
            f.reservation("inflight").1,
            if result.is_some() { 15 } else { 50 }
        );
        assert_eq!(
            f.reservation("inflight").0,
            if result.is_some() {
                "settled"
            } else {
                "unknown"
            }
        );
        assert_eq!(
            sessions::load_turn(f.db(), TURN).unwrap().status,
            sessions::TurnStatus::Cancelled
        );
        assert!(!usage_ledger::reserve(f.db(), NOW + 3, &f.request("inflight")).unwrap());
    }
}

#[test]
fn held_actual_and_unknown_survive_reopen_with_idempotency_and_budget_intact() {
    let mut f = Fixture::new(PRICES, Some(policy(Some(150), None, 10)));
    for id in ["held", "actual", "unknown"] {
        f.reserve(id);
    }
    assert!(usage_ledger::settle(f.db(), NOW + 1, "actual", Some(usage(8, 4, 2))).unwrap());
    assert!(usage_ledger::settle(f.db(), NOW + 1, "unknown", None).unwrap());
    let totals = f.totals();
    let rows: Vec<_> = ["held", "actual", "unknown"]
        .map(|id| (f.ledger(id), f.reservation(id)))
        .into();
    f.reopen();
    assert_eq!(f.totals(), totals);
    for (id, before) in ["held", "actual", "unknown"].into_iter().zip(rows) {
        assert_eq!((f.ledger(id), f.reservation(id)), before);
        assert!(!usage_ledger::reserve(f.db(), NOW + 2, &f.request(id)).unwrap());
    }
    assert!(!usage_ledger::settle(f.db(), NOW + 2, "actual", None).unwrap());
    assert!(!usage_ledger::settle(f.db(), NOW + 2, "unknown", Some(usage(1, 0, 0))).unwrap());
    assert!(usage_ledger::reserve(f.db(), NOW + 2, &f.request("over")).is_err());
    assert!(usage_ledger::settle(f.db(), NOW + 3, "held", Some(usage(8, 4, 2))).unwrap());
    assert_eq!(f.totals().cost_micros, 30);
    assert_eq!(f.totals().unknown_requests, 1);
}

fn race_reserve(f: &Fixture, requests: Vec<UsageRequest>) -> Vec<Result<bool, String>> {
    let barrier = Arc::new(Barrier::new(requests.len()));
    // 每个线程使用真实独立连接，屏障对齐起跑；不靠串行调用或 sleep 模拟竞争。
    let handles: Vec<_> = requests
        .into_iter()
        .map(|request| {
            let barrier = Arc::clone(&barrier);
            let path = f.path();
            thread::spawn(move || {
                let conn = open(&path);
                barrier.wait();
                usage_ledger::reserve(&conn, NOW, &request).map_err(|e| e.to_string())
            })
        })
        .collect();
    handles
        .into_iter()
        .map(|h| h.join().expect("worker did not panic"))
        .collect()
}

#[test]
fn concurrent_session_budget_reservations_never_overspend() {
    let f = Fixture::new(PRICES, Some(policy(Some(75), None, 10)));
    let results = race_reserve(
        &f,
        (0..8).map(|i| f.request(&format!("race-{i}"))).collect(),
    );
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(true))).count(),
        1,
        "{results:?}"
    );
    for error in results.into_iter().filter_map(Result::err) {
        assert!(
            error.contains("超出预算"),
            "must be budget denial, not SQLite lock failure: {error}"
        );
    }
    assert_eq!(f.counts(), (1, 1));
}

#[test]
fn concurrent_daily_budget_across_sessions_never_overspends() {
    let f = Fixture::new(PRICES, Some(policy(None, Some(75), 10)));
    let requests = (0..8)
        .map(|i| {
            let session = format!("race-session-{i}");
            let turn = format!("race-turn-{i}");
            f.add_session(&session, &turn);
            let mut r = f.request(&format!("race-{i}"));
            r.session_id = session;
            r.turn_id = turn;
            r
        })
        .collect();
    let results = race_reserve(&f, requests);
    assert_eq!(
        results.iter().filter(|r| matches!(r, Ok(true))).count(),
        1,
        "{results:?}"
    );
    for error in results.into_iter().filter_map(Result::err) {
        assert!(error.contains("超出预算"), "{error}");
    }
    assert_eq!(f.counts(), (1, 1));
}

#[test]
fn concurrent_concurrency_limit_and_same_request_id_are_atomic() {
    let f = Fixture::new(PRICES, Some(policy(None, None, 2)));
    let results = race_reserve(
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
    assert_eq!(f.counts(), (2, 2));
    let f = Fixture::new(PRICES, Some(policy(Some(50), None, 1)));
    let results = race_reserve(&f, vec![f.request("same"); 8]);
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
    assert_eq!(f.counts(), (1, 1));
}

#[test]
fn concurrent_settlement_has_one_winner_and_no_duplicate_charge() {
    let f = Fixture::new(PRICES, None);
    f.reserve("same");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = f.path();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let conn = open(&path);
                barrier.wait();
                usage_ledger::settle(&conn, NOW + 1, "same", Some(usage(8, 4, 2)))
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
    assert_eq!(f.counts(), (1, 1));
    assert_eq!(f.totals().cost_micros, 15);
    assert_eq!(f.reservation("same"), ("settled".into(), 15, NOW + 1));
}
