//! T036 决策用量记账：`MeteredDecisionProvider` 走真实 SQLite 账本，
//! 把 decision 请求登记/完成（actual 或 unknown），不伪造零消耗、不碰真实凭据。
use std::{fs, path::PathBuf};

use fluxdb_app::{MeteredDecisionProvider, ScriptedDecisionProvider};
use fluxdb_core::{CancelFlag, DecisionProvider, DecisionRequest, DecisionResponse, Usage};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{agents, models, sessions, usage_ledger},
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t036-usage-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn ctx(&self) -> (String, String) {
        self.storage
            .with_ai_connection(|conn| {
                let (cipher, _) =
                    fluxdb_storage::ai_platform::load_ai_cipher(self.storage.root_dir())?;
                let model = models::save_model(
                    conn,
                    &cipher,
                    "fixture-key",
                    1_000,
                    &models::ModelEdit {
                        id: "decision-model".into(),
                        display_name: "决策模型".into(),
                        provider: "decisions".into(),
                        api_protocol: "system-one".into(),
                        kind: models::ModelKind::Decision,
                        model: "jev".into(),
                        endpoint: Some("https://decision.example.invalid/v1".into()),
                        endpoint_is_full_url: false,
                        enabled: true,
                        egress: models::EgressPermission::Allow,
                        auth_mode: models::AuthMode::ApiKey,
                        context_window_tokens: Some(8_192),
                        max_output_tokens: Some(1_024),
                        input_price_micros_per_million: None,
                        output_price_micros_per_million: None,
                        cached_input_price_micros_per_million: None,
                    },
                    None,
                    models::ApiKeyChange::Set(
                        fluxdb_storage::ai_platform::model_secret::SecretInput::new(
                            "sk-fixture-not-real".to_string(),
                        )
                        .unwrap(),
                    ),
                )?;
                sessions::create_session(
                    conn,
                    2_000,
                    "t036-session",
                    agents::BUILTIN_AGENT_ID,
                    "T036",
                )?;
                sessions::start_turn(conn, 2_000, "t036-session", "t036-turn", "{}", "{}")?;
                Ok((model.id, "t036-turn".to_string()))
            })
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn metered_decision_records_actual_usage_in_ledger() {
    let fixture = Fixture::new();
    let (model_id, turn_id) = fixture.ctx();
    let inner = ScriptedDecisionProvider::new(DecisionResponse {
        chosen: Some("model-a".into()),
        scores: vec![("model-a".into(), 0.9)],
        reason: None,
        confidence: Some(0.9),
        usage: Some(Usage {
            input_tokens: 40,
            output_tokens: 6,
            cached_input_tokens: 0,
        }),
    });
    let metered = MeteredDecisionProvider::new(
        &inner,
        &fixture.storage,
        "t036-session",
        &turn_id,
        &model_id,
    );
    let response = block_on(metered.decide(
        DecisionRequest {
            model: model_id.clone(),
            candidates: vec!["model-a".into()],
            prompt: "选择".into(),
        },
        CancelFlag::new(),
    ))
    .expect("决策成功");
    assert_eq!(response.chosen.as_deref(), Some("model-a"));

    fixture
        .storage
        .with_ai_connection(|conn| {
            let totals = usage_ledger::totals(conn, "t036-session")?;
            assert_eq!(totals.requests, 1, "决策请求应登记一条");
            assert_eq!(totals.input_tokens, 40);
            assert_eq!(totals.output_tokens, 6);
            assert_eq!(totals.total_tokens, 46);
            assert_eq!(totals.unknown_requests, 0, "有实际 usage 不应记为 unknown");
            let (purpose, measurement, profile): (String, String, String) = conn.query_row(
                "SELECT purpose, measurement, model_profile_id FROM ai_usage_ledger WHERE session_id='t036-session'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
            assert_eq!(purpose, "decision");
            assert_eq!(measurement, "actual");
            assert_eq!(profile, model_id);
            Ok(())
        })
        .unwrap();
}

#[test]
fn metered_decision_without_usage_is_recorded_unknown() {
    let fixture = Fixture::new();
    let (model_id, turn_id) = fixture.ctx();
    let inner = ScriptedDecisionProvider::new(DecisionResponse {
        chosen: Some("model-a".into()),
        scores: Vec::new(),
        reason: None,
        confidence: None,
        usage: None,
    });
    let metered = MeteredDecisionProvider::new(
        &inner,
        &fixture.storage,
        "t036-session",
        &turn_id,
        &model_id,
    );
    block_on(metered.decide(
        DecisionRequest {
            model: model_id.clone(),
            candidates: vec!["model-a".into()],
            prompt: "选择".into(),
        },
        CancelFlag::new(),
    ))
    .expect("决策成功");

    fixture
        .storage
        .with_ai_connection(|conn| {
            let totals = usage_ledger::totals(conn, "t036-session")?;
            assert_eq!(totals.requests, 1);
            assert_eq!(totals.total_tokens, 0, "未返回 usage 不得伪造 token");
            assert_eq!(totals.unknown_requests, 1, "缺 usage 必须显式记 unknown");
            Ok(())
        })
        .unwrap();
}
