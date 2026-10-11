//! 上下文 checkpoint 存储契约：独立 SQLite、真实 FK、原子写入与"失败不覆盖"。
//! 不读取用户配置，不调用模型。
use std::{fs, path::PathBuf};

use fluxdb_storage::ai_platform::{
    agents, context_checkpoints, conversation, ensure_schema, load_ai_cipher, models, sessions,
};
use rusqlite::Connection;

const SESSION: &str = "ctx-session";
const TURN: &str = "ctx-turn";

struct Fixture {
    dir: PathBuf,
    conn: Option<Connection>,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-ctx-checkpoints-{}-{:016x}",
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
        let conn = Connection::open(self.dir.join("ctx.sqlite")).expect("文件 SQLite");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        ensure_schema(&conn).expect("生产 schema 与种子");
        self.conn = Some(conn);
    }

    fn seed(&mut self) -> String {
        let (cipher, _) = load_ai_cipher(&self.dir).expect("仅读取独占 fixture 配置");
        let model_id = models::save_model(
            self.db(),
            &cipher,
            "fixture-key",
            1_000,
            &models::ModelEdit {
                id: "ctx-model".into(),
                display_name: "离线模型".into(),
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
                input_price_micros_per_million: None,
                output_price_micros_per_million: None,
                cached_input_price_micros_per_million: None,
            },
            None,
            models::ApiKeyChange::Keep,
        )
        .expect("保存模型")
        .id;
        let agent_id = agents::BUILTIN_AGENT_ID.to_string();
        conversation::send(
            self.db(),
            2_000,
            true,
            SESSION,
            &agent_id,
            TURN,
            "ctx-msg",
            "历史正文",
            r#"{"kind":"sql"}"#,
            r#"{"model_profile_id":"ctx-model"}"#,
        )
        .expect("发送事务");
        model_id
    }

    fn edit(
        &self,
        generation: i64,
        last_seq: i64,
        summary: &str,
    ) -> context_checkpoints::ContextCheckpointEdit {
        context_checkpoints::ContextCheckpointEdit {
            id: format!("ctx-cp-{generation}"),
            session_id: SESSION.into(),
            turn_id: TURN.into(),
            generation,
            first_message_seq: 1,
            last_message_seq: last_seq,
            summary_text: summary.into(),
            protected_items_json: r#"{"items":["goal"]}"#.into(),
            source_hash: format!("hash-{generation}"),
            model_profile_id: Some("ctx-model".into()),
        }
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
fn insert_then_load_latest_survives_reopen() {
    let mut fixture = Fixture::new();
    let _model = fixture.seed();
    context_checkpoints::insert(fixture.db(), 3_000, &fixture.edit(1, 4, "第一版摘要")).unwrap();
    fixture.reopen();
    let latest = context_checkpoints::load_latest(fixture.db(), SESSION)
        .unwrap()
        .expect("重开后仍有 checkpoint");
    assert_eq!(latest.generation, 1);
    assert_eq!(latest.summary_text, "第一版摘要");
    assert_eq!(latest.last_message_seq, 4);
}

#[test]
fn latest_is_highest_generation_and_incremental_generation_increases() {
    let mut fixture = Fixture::new();
    let _model = fixture.seed();
    context_checkpoints::insert(fixture.db(), 3_000, &fixture.edit(1, 2, "第一版")).unwrap();
    context_checkpoints::insert(fixture.db(), 3_100, &fixture.edit(2, 6, "第二版增量")).unwrap();
    let latest = context_checkpoints::load_latest(fixture.db(), SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(latest.generation, 2);
    assert_eq!(latest.summary_text, "第二版增量");
}

#[test]
fn duplicate_turn_generation_is_rejected_without_overwriting_old_summary() {
    let mut fixture = Fixture::new();
    let _model = fixture.seed();
    context_checkpoints::insert(fixture.db(), 3_000, &fixture.edit(1, 2, "保留的旧摘要")).unwrap();
    let mut conflicting = fixture.edit(1, 6, "不应覆盖的新摘要");
    conflicting.id = "ctx-cp-conflict".into();
    assert!(
        context_checkpoints::insert(fixture.db(), 3_100, &conflicting).is_err(),
        "同 turn/generation 必须冲突报错"
    );
    let latest = context_checkpoints::load_latest(fixture.db(), SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(latest.summary_text, "保留的旧摘要");
}

#[test]
fn empty_summary_and_backwards_cut_are_rejected() {
    let mut fixture = Fixture::new();
    let _model = fixture.seed();
    assert!(context_checkpoints::insert(fixture.db(), 3_000, &fixture.edit(1, 2, "   ")).is_err());
    let mut backwards = fixture.edit(2, 1, "合法摘要");
    backwards.first_message_seq = 5;
    assert!(context_checkpoints::insert(fixture.db(), 3_100, &backwards).is_err());
    assert!(
        context_checkpoints::load_latest(fixture.db(), SESSION)
            .unwrap()
            .is_none(),
        "被拒绝的写入不能留下 checkpoint"
    );
}

#[test]
fn latest_is_none_for_other_session_and_original_messages_survive() {
    let mut fixture = Fixture::new();
    let _model = fixture.seed();
    context_checkpoints::insert(fixture.db(), 3_000, &fixture.edit(1, 2, "摘要")).unwrap();
    assert!(
        context_checkpoints::load_latest(fixture.db(), "other-session")
            .unwrap()
            .is_none()
    );
    // 压缩只写 checkpoint，原始消息/审计必须原样保留。
    let messages = sessions::list_messages(fixture.db(), SESSION).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].seq, 1);
}
