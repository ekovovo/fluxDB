//! 工具回合持久化与数据链路保护测试。
//!
//! 本文件只验证生产链路：`prepare_ai_conversation` → `run_ai_conversation_turn` /
//! `run_ai_conversation_failure` → `FileStorage`，并对 SQLite 事实表做真实查询。
//! 不直接 INSERT 业务数据，不读取用户真实配置。

use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    run_ai_conversation_failure, run_ai_conversation_turn, standard_sql_catalog, AiAgentSaveInput,
    AiConversationJob, AiConversationUpdate, AiModelSaveInput, AppCommand, AppController, AppEvent,
    AuditInput, AuditOutcome, KernelPolicy, MapSqlResolver, ScriptedGenerationProvider,
    SecurityAuditHook, TargetRef, ToolCatalog, TurnPermissionContext, TurnTargetPin,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest,
};
use fluxdb_storage::{
    ai_platform::{conversation, sessions},
    FileStorage,
};
use rusqlite::OptionalExtension;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-ai-conversation-tools-guard-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("创建隔离 fixture 目录");
        let storage = FileStorage::new(&dir);
        storage
            .with_ai_connection(|_| Ok(()))
            .expect("初始化 AI schema");
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        controller
    }

    fn configured(&self) -> AppController {
        let mut controller = self.controller();
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "工具链路 guard 模型".into(),
            provider: "openai".into(),
            model: "guard-offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "工具链路 guard Agent".into(),
            description: "独立测试 fixture".into(),
            system_prompt: "只依据工具真实回执陈述结果。".into(),
            model_profile_id: Some(model),
            enabled: true,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { .. } => {}
            other => panic!("配置 Agent 失败: {other:?}"),
        }
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct AllowAudit(std::sync::atomic::AtomicUsize);

impl SecurityAuditHook for AllowAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(AuditOutcome::Allow)
    }
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        schema: None,
        object: Some("product".into()),
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: Some(1),
        captured_tab_revision: 1,
    }
}

fn demo_config() -> ConnectionConfig {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    ConnectionConfig {
        id: ConnectionId(1),
        name: "guard demo".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: "demo.db".into(),
            read_only: true,
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

fn permission() -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
    }
}

fn read_catalog() -> ToolCatalog {
    standard_sql_catalog(Arc::new(MapSqlResolver::new([demo_config()])), false)
        .expect("标准只读工具目录")
}

fn text_call(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn tool_call(call_id: &str, args: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: "db.query".into(),
            arguments_json: args.into(),
        }),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]
}

fn run_turn(
    fixture: &Fixture,
    job: &AiConversationJob,
    provider: &ScriptedGenerationProvider,
    catalog: &ToolCatalog,
) -> AiConversationUpdate {
    let audit = AllowAudit(std::sync::atomic::AtomicUsize::new(0));
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(32);
    run_ai_conversation_turn(
        &fixture.storage,
        job,
        &cancel,
        &tx,
        provider,
        catalog,
        &audit,
        permission(),
    );
    let mut terminal = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            terminal = Some(update);
        }
    }
    terminal.expect("任务必须发送终态回包")
}

fn sql_error(error: rusqlite::Error) -> fluxdb_core::Error {
    fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        format!("SQLite 查询失败: {error}"),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct ToolFactRow {
    id: String,
    status: String,
    receipt_json: Option<String>,
    plan_hash: String,
    arguments_redacted_json: String,
}

fn tool_facts(storage: &FileStorage, session_id: &str) -> Vec<ToolFactRow> {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<Vec<ToolFactRow>> {
            let mut stmt = conn
                .prepare(
                    "SELECT id,status,receipt_json,plan_hash,arguments_redacted_json
                       FROM ai_tool_calls
                      WHERE session_id=?1
                      ORDER BY id",
                )
                .map_err(sql_error)?;
            let rows = stmt
                .query_map([session_id], |row| {
                    Ok(ToolFactRow {
                        id: row.get(0)?,
                        status: row.get(1)?,
                        receipt_json: row.get(2)?,
                        plan_hash: row.get(3)?,
                        arguments_redacted_json: row.get(4)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(rows)
        })
        .expect("查询工具事实行")
}

#[derive(Debug, PartialEq, Eq)]
struct ResultFactRow {
    id: String,
    source_call_id: Option<String>,
    status: String,
}

fn result_facts(storage: &FileStorage, session_id: &str) -> Vec<ResultFactRow> {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<Vec<ResultFactRow>> {
            let mut stmt = conn
                .prepare(
                    "SELECT id,source_call_id,status
                       FROM ai_results
                      WHERE session_id=?1
                      ORDER BY id",
                )
                .map_err(sql_error)?;
            let rows = stmt
                .query_map([session_id], |row| {
                    Ok(ResultFactRow {
                        id: row.get(0)?,
                        source_call_id: row.get(1)?,
                        status: row.get(2)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(rows)
        })
        .expect("查询结果事实行")
}

#[derive(Debug, PartialEq, Eq)]
struct PartRow {
    kind: String,
    status: String,
    payload_json: String,
    tool_call_id: Option<String>,
    result_id: Option<String>,
}

fn message_parts(storage: &FileStorage, message_id: &str) -> Vec<PartRow> {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<Vec<PartRow>> {
            let mut stmt = conn
                .prepare(
                    "SELECT kind,status,payload_json,tool_call_id,result_id
                       FROM ai_message_parts
                      WHERE message_id=?1
                      ORDER BY ordinal",
                )
                .map_err(sql_error)?;
            let rows = stmt
                .query_map([message_id], |row| {
                    Ok(PartRow {
                        kind: row.get(0)?,
                        status: row.get(1)?,
                        payload_json: row.get(2)?,
                        tool_call_id: row.get(3)?,
                        result_id: row.get(4)?,
                    })
                })
                .map_err(sql_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            Ok(rows)
        })
        .expect("查询消息片段")
}

fn count_rows(storage: &FileStorage, sql: &str, session_id: &str) -> i64 {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<i64> {
            conn.query_row(sql, [session_id], |row| row.get(0))
                .map_err(sql_error)
        })
        .expect("查询计数")
}

fn turn_status(storage: &FileStorage, turn_id: &str) -> String {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<String> {
            conn.query_row(
                "SELECT status FROM ai_turns WHERE id=?1",
                [turn_id],
                |row| row.get(0),
            )
            .map_err(sql_error)
        })
        .expect("查询 Turn 状态")
}

fn text_part_payload(storage: &FileStorage, message_id: &str) -> Option<String> {
    storage
        .with_ai_connection(|conn| -> fluxdb_core::Result<Option<String>> {
            conn.query_row(
                "SELECT payload_json FROM ai_message_parts
                  WHERE message_id=?1 AND kind='text'
                  ORDER BY ordinal LIMIT 1",
                [message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)
        })
        .expect("查询文本片段")
}

fn is_empty_fk(part: &PartRow) -> bool {
    part.tool_call_id.is_none() && part.result_id.is_none()
}

#[test]
fn tool_facts_and_parts_form_closed_loop_and_survive_reopen() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("查询产品", &target())
        .expect("准备工具回合");

    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            tool_call(
                "guard-call-1",
                r#"{"target":"conn:1/db:main","sql":"SELECT * FROM product","max_rows":5}"#,
            ),
            text_call("已依据真实回执完成查询。"),
        ],
    );
    let update = run_turn(&fixture, &job, &provider, &read_catalog());
    assert!(update.terminal, "必须返回终态");

    let calls = tool_facts(&fixture.storage, &job.session_id);
    assert_eq!(calls.len(), 1, "一次真实工具执行应产生一行 ai_tool_calls");
    let call = &calls[0];
    assert_eq!(call.status, "succeeded");
    assert!(
        call.receipt_json
            .as_deref()
            .is_some_and(|raw| !raw.trim().is_empty()),
        "成功工具必须保存非空 receipt_json"
    );
    assert!(!call.plan_hash.is_empty(), "工具事实必须有 plan_hash");
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments_redacted_json).expect("脱敏参数是合法 JSON");
    assert_eq!(args["sql"].as_str(), Some("SELECT * FROM product"));
    assert_eq!(args["target"].as_str(), Some("conn:1/db:main"));

    let results = result_facts(&fixture.storage, &job.session_id);
    assert_eq!(results.len(), 1, "成功工具应登记一行 ai_results");
    let result = &results[0];
    assert_eq!(result.status, "available");
    assert_eq!(
        result.source_call_id.as_deref(),
        Some(call.id.as_str()),
        "ai_results.source_call_id 必须指向真实工具调用"
    );

    let parts = message_parts(&fixture.storage, &job.assistant_message_id);
    let tool_call_part = parts
        .iter()
        .find(|part| part.kind == "tool_call_ref")
        .expect("必须有 tool_call_ref 片段");
    assert_eq!(
        tool_call_part.tool_call_id.as_deref(),
        Some(call.id.as_str())
    );
    assert_eq!(tool_call_part.status, "complete");

    let tool_result_part = parts
        .iter()
        .find(|part| part.kind == "tool_result_ref")
        .expect("必须有 tool_result_ref 片段");
    assert_eq!(
        tool_result_part.result_id.as_deref(),
        Some(result.id.as_str()),
        "tool_result_ref 必须反向引用真实结果"
    );

    // 用新 FileStorage 重新打开同一目录，确认不是内存快照。
    let reopened = FileStorage::new(&fixture.dir);
    let reopened_calls = tool_facts(&reopened, &job.session_id);
    let reopened_results = result_facts(&reopened, &job.session_id);
    let reopened_parts = message_parts(&reopened, &job.assistant_message_id);
    assert_eq!(reopened_calls.len(), 1);
    assert_eq!(reopened_results.len(), 1);
    assert!(reopened_parts.iter().any(|part| {
        part.kind == "tool_result_ref"
            && part.result_id.as_deref() == Some(reopened_results[0].id.as_str())
    }));
}

#[test]
fn arguments_redaction_removes_nested_credentials() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("带凭据参数的查询", &target())
        .expect("准备参数脱敏回合");

    let plaintext_password = "guard-password-should-not-persist";
    let plaintext_api_key = "guard-api-key-should-not-persist";
    let plaintext_token = "guard-token-should-not-persist";
    // `parameters` 的元素 schema 未限制字段；敏感字段放这里仍会通过工具 schema，
    // 因此这是验证 `arguments_redacted_json` 是否递归脱敏的有效生产路径。
    let args = serde_json::json!({
        "target": "conn:1/db:main",
        "sql": "SELECT * FROM product",
        "max_rows": 5,
        "parameters": [{
            "password": plaintext_password,
            "api_key": plaintext_api_key,
            "token": plaintext_token,
        }]
    })
    .to_string();

    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![tool_call("guard-call-secret", &args), text_call("已处理。")],
    );
    let _ = run_turn(&fixture, &job, &provider, &read_catalog());

    let calls = tool_facts(&fixture.storage, &job.session_id);
    assert_eq!(calls.len(), 1, "敏感参数在工具事实中留下可审计行");
    let call = &calls[0];
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments_redacted_json).expect("脱敏参数是合法 JSON");
    assert_eq!(args["sql"].as_str(), Some("SELECT * FROM product"));
    assert_eq!(args["target"].as_str(), Some("conn:1/db:main"));

    let receipt = call.receipt_json.as_deref().unwrap_or("{}");
    for forbidden in [plaintext_password, plaintext_api_key, plaintext_token] {
        assert!(
            !receipt.contains(forbidden),
            "receipt_json 不应包含明文凭据: {forbidden}"
        );
    }

    let serialized = call.arguments_redacted_json.as_str();
    for forbidden in [
        "password",
        "api_key",
        "token",
        plaintext_password,
        plaintext_api_key,
        plaintext_token,
    ] {
        assert!(
            !serialized.contains(forbidden),
            "arguments_redacted_json 不应包含凭据字段或值: {forbidden}; actual={serialized}"
        );
    }
}

#[test]
fn late_or_foreign_events_do_not_append_parts() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("先完成一个纯文本回合", &target())
        .expect("准备终态回合");

    let provider = ScriptedGenerationProvider::text_only("已完成");
    let _ = run_turn(&fixture, &job, &provider, &read_catalog());
    assert_eq!(turn_status(&fixture.storage, &job.turn_id), "completed");

    let before = message_parts(&fixture.storage, &job.assistant_message_id).len();

    let late = fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::upsert_event_part(
                conn,
                9_999_999_999,
                &job.session_id,
                &job.turn_id,
                &job.assistant_message_id,
                "late-notice",
                sessions::PartKind::Notice,
                sessions::PartStatus::Complete,
                r#"{"text":"迟到事件"}"#,
                None,
                None,
            )
        })
        .expect_err("终态 Turn 必须拒绝迟到事件");
    assert!(
        late.message.contains("迟到") || late.message.contains("归属"),
        "拒绝原因应可诊断: {}",
        late.message
    );
    assert_eq!(
        message_parts(&fixture.storage, &job.assistant_message_id).len(),
        before,
        "迟到事件不能新增片段"
    );

    let wrong_turn = fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::upsert_event_part(
                conn,
                9_999_999_999,
                &job.session_id,
                "not-the-job-turn",
                &job.assistant_message_id,
                "wrong-turn-notice",
                sessions::PartKind::Notice,
                sessions::PartStatus::Complete,
                r#"{"text":"错误 Turn"}"#,
                None,
                None,
            )
        })
        .expect_err("不存在的 Turn 必须拒绝");
    assert!(
        wrong_turn.message.contains("Turn") || wrong_turn.message.contains("不存在"),
        "错误 Turn 拒绝原因应可诊断: {}",
        wrong_turn.message
    );

    let wrong_session = fixture
        .storage
        .with_ai_connection(|conn| {
            conversation::upsert_event_part(
                conn,
                9_999_999_999,
                "not-the-job-session",
                &job.turn_id,
                &job.assistant_message_id,
                "wrong-session-notice",
                sessions::PartKind::Notice,
                sessions::PartStatus::Complete,
                r#"{"text":"错误会话"}"#,
                None,
                None,
            )
        })
        .expect_err("会话归属不符必须拒绝");
    assert!(
        wrong_session.message.contains("归属") || wrong_session.message.contains("拒绝"),
        "错误会话拒绝原因应可诊断: {}",
        wrong_session.message
    );

    assert_eq!(
        message_parts(&fixture.storage, &job.assistant_message_id).len(),
        before,
        "错误归属事件不能新增片段"
    );
}

#[test]
fn unknown_tool_creates_notice_without_tool_facts() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("尝试未知工具", &target())
        .expect("准备未知工具回合");

    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![
            vec![
                ModelEvent::ToolCallRequested(ToolCallRequest {
                    provider_call_id: "guard-unknown".into(),
                    tool_id: "db.execute".into(),
                    arguments_json: r#"{"target":"conn:1/db:main","sql":"DELETE FROM product"}"#
                        .into(),
                }),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            text_call("未知工具未执行。"),
        ],
    );
    let _ = run_turn(&fixture, &job, &provider, &read_catalog());

    assert_eq!(
        count_rows(
            &fixture.storage,
            "SELECT COUNT(*) FROM ai_tool_calls WHERE session_id=?1",
            &job.session_id,
        ),
        0
    );
    assert_eq!(
        count_rows(
            &fixture.storage,
            "SELECT COUNT(*) FROM ai_results WHERE session_id=?1",
            &job.session_id,
        ),
        0
    );

    let parts = message_parts(&fixture.storage, &job.assistant_message_id);
    assert_eq!(
        parts
            .iter()
            .filter(|part| part.kind == "tool_result_ref")
            .count(),
        0,
        "未知工具不得产生结果引用片段"
    );
    let notices: Vec<&PartRow> = parts.iter().filter(|part| part.kind == "notice").collect();
    assert!(!notices.is_empty(), "未知工具必须留下可见 notice 片段");
    assert!(
        notices.iter().all(|part| is_empty_fk(part)),
        "notice 片段不能伪造工具/结果外键"
    );
    assert!(notices.iter().any(|part| {
        part.payload_json.contains("未注册") || part.payload_json.contains("未执行")
    }));
}

#[test]
fn text_turn_remains_text_only() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("只返回文本", &target())
        .expect("准备文本回合");

    let provider = ScriptedGenerationProvider::text_only("这是纯文本回答");
    let _ = run_turn(&fixture, &job, &provider, &read_catalog());

    assert_eq!(turn_status(&fixture.storage, &job.turn_id), "completed");
    let payload = text_part_payload(&fixture.storage, &job.assistant_message_id)
        .expect("assistant 文本片段必须入库");
    let payload: serde_json::Value =
        serde_json::from_str(&payload).expect("assistant 文本载荷是合法 JSON");
    assert_eq!(payload["text"].as_str(), Some("这是纯文本回答"));
    assert_eq!(
        count_rows(
            &fixture.storage,
            "SELECT COUNT(*) FROM ai_tool_calls WHERE session_id=?1",
            &job.session_id,
        ),
        0
    );
}

#[test]
fn failure_is_visible_and_marks_turn_failed() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let job = controller
        .prepare_ai_conversation("模拟启动失败", &target())
        .expect("准备失败回合");

    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    run_ai_conversation_failure(
        &fixture.storage,
        &job,
        &tx,
        "Provider 初始化失败".to_string(),
    );
    let update = rx.recv().expect("失败也必须回桥");
    assert!(update.terminal);
    assert!(
        update
            .view
            .error
            .as_deref()
            .is_some_and(|message| { message.contains("Provider 初始化失败") }),
        "回桥必须包含明确失败原因"
    );

    assert_eq!(turn_status(&fixture.storage, &job.turn_id), "failed");
    let payload = text_part_payload(&fixture.storage, &job.assistant_message_id)
        .expect("失败原因必须可见地写入文本片段");
    let payload: serde_json::Value =
        serde_json::from_str(&payload).expect("失败文本载荷是合法 JSON");
    let text = payload["text"].as_str().unwrap_or_default();
    assert!(text.contains("本轮未能开始"));
    assert!(text.contains("Provider 初始化失败"));
    assert_eq!(
        count_rows(
            &fixture.storage,
            "SELECT COUNT(*) FROM ai_tool_calls WHERE session_id=?1",
            &job.session_id,
        ),
        0
    );
}
