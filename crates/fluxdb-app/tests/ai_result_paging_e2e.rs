//! T027 端到端：大结果经生产链路 spill 成 retained 结果行，再用 `result.read_page` 分页读回。
//!
//! 用真实 SQLite 文件与真实驱动（不用 demo MockConnector，它固定只返回 2 行、产生不了 spill），
//! 只替换模型事件来源；schema 校验→审计→授权→执行→投影→落盘全部走生产实现。
//! 不触网、不读生产配置与凭据。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiModelSaveInput, AppCommand,
    AppController, AppEvent, KernelAuditHook, KernelPolicy, MapSqlResolver,
    RESULT_STORE_MEMORY_BYTES, ResultStore, SQL_RESULT_MAX_BYTES, ScriptedGenerationProvider,
    TargetRef, TurnPermissionContext, TurnTargetPin, run_ai_conversation_turn,
    standard_sql_catalog_with_results,
};
use fluxdb_core::{
    CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, FinishReason, ModelCapabilities,
    ModelEvent, ToolCallRequest, ToolExecutionContext,
};
use fluxdb_storage::{FileStorage, Storage as _, ai_platform::sessions};

/// 用**生产常量**而不是测试自拟值：仓库预算若没有小于工具层投影上限，
/// 工具投影早已被截断到 `SQL_RESULT_MAX_BYTES`，溢写分支永远不可达。
const E2E_STORE_BYTES: usize = RESULT_STORE_MEMORY_BYTES;
/// 单行宽度让 200 行超过工具层 128 KiB 上限，并被仓库预算判定为需要溢写。
const ROW_PAD: usize = 900;

/// 溢写在生产里是否真的可达：这一条比任何具体页数据都重要。
#[test]
fn result_store_budget_is_below_tool_projection_cap() {
    assert!(
        RESULT_STORE_MEMORY_BYTES < SQL_RESULT_MAX_BYTES,
        "仓库内存预算（{RESULT_STORE_MEMORY_BYTES}）必须小于工具层回执上限（{SQL_RESULT_MAX_BYTES}），\
         否则工具投影已截断、spill/retained 在生产里永远不会触发"
    );
}

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t027-e2e-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        // 真实 SQLite：demo MockConnector 只返回 2 行，触发不了 spill。
        let path = dir.join("wide.sqlite");
        let database = rusqlite::Connection::open(&path).unwrap();
        database
            .execute_batch(&format!(
                "CREATE TABLE wide(id INTEGER PRIMARY KEY, payload TEXT);
                 WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<200)
                 INSERT INTO wide(id,payload) SELECT n, 'head-' || n || '-' || replace(hex(zeroblob({ROW_PAD})),'00','x') || '-tail-' || n FROM seq;"
            ))
            .unwrap();
        let mut options = std::collections::BTreeMap::new();
        options.insert("t027_e2e".to_string(), "true".to_string());
        let config = ConnectionConfig {
            id: ConnectionId(1),
            name: "t027-wide".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: fluxdb_core::Endpoint::SqliteFile {
                path,
                read_only: true,
            },
            options,
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        };
        storage.save_connections(&[config.clone()]).unwrap();
        Self { dir, storage }
    }

    fn configured(&self) -> (AppController, ConnectionConfig) {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T027 离线模型".into(),
            provider: "openai".into(),
            model: "t027-e2e-offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        assert!(matches!(
            controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
                display_name: "T027 E2E Agent".into(),
                description: "真实 SQLite 大结果".into(),
                system_prompt: "T027 端到端".into(),
                model_profile_id: Some(model),
                enabled: true,
                ..Default::default()
            })),
            AppEvent::AiAgentSaved { .. }
        ));
        let config = self
            .storage
            .load_connections()
            .unwrap()
            .into_iter()
            .next()
            .expect("fixture 连接");
        (controller, config)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        schema: None,
        object: Some("wide".into()),
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: Some(1),
        captured_tab_revision: 1,
    }
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tool_calling: true,
        reasoning_summary: false,
    }
}

fn query_step(call_id: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: "db.query".into(),
            arguments_json: r#"{"target":"conn:1/db:main","sql":"SELECT id,payload FROM wide ORDER BY id","max_rows":200}"#.into(),
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn text_step(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn last_terminal(
    rx: &std::sync::mpsc::Receiver<fluxdb_app::AiConversationUpdate>,
) -> fluxdb_app::AiConversationView {
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = update.view;
        }
    }
    last.expect("终态回包必须携带视图")
}

fn run_read_page(
    catalog: &fluxdb_app::ToolCatalog,
    job: &AiConversationJob,
    arguments: serde_json::Value,
) -> fluxdb_core::ToolReceipt {
    let tool = catalog
        .executable("result.read_page")
        .expect("目录里必须有 result.read_page 的可执行实现");
    let ctx = ToolExecutionContext {
        automatic_approval: false,
        bounded_write: false,
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
        iteration: 1,
        call_id: "call-read-page".into(),
        cancel: CancelFlag::new(),
        progress: None,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时")
        .block_on(tool.execute(ctx, arguments.to_string()))
        .expect("result.read_page 执行")
}

/// 大结果不进上下文、可分页读回、retained 落点可核对：一条链走完。
#[test]
fn wide_query_spills_to_retained_row_and_is_readable_page_by_page() {
    let fixture = Fixture::new();
    let (mut controller, config) = fixture.configured();
    let job = controller
        .prepare_ai_conversation("把所有宽行读出来", &target())
        .expect("准备任务");

    // 与生产装配同构：按 turn 绑定归属 + 受控 spill 根。
    let results = Arc::new(std::sync::Mutex::new(ResultStore::for_turn(
        E2E_STORE_BYTES,
        &job.session_id,
        &job.turn_id,
        Some(fixture.dir.join("ai-results")),
    )));
    let catalog = standard_sql_catalog_with_results(
        Arc::new(MapSqlResolver::new([config.clone()])),
        Arc::clone(&results),
        false,
    )
    .expect("标准只读目录 + 结果分页");
    let permission = TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        disabled_tools: Default::default(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
    };
    let provider = ScriptedGenerationProvider::new(
        capabilities(),
        vec![query_step("call-wide"), text_step("已按真实回执总结")],
    );
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(64);

    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        permission,
    );
    let view = last_terminal(&rx);

    // —— 1. 回执只带样本，完整结果不进上下文 ——
    let call = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_tool_calls_for_turn(conn, &job.turn_id))
        .expect("读取工具调用")
        .into_iter()
        .find(|call| call.tool_id == "db.query")
        .expect("db.query 调用事实");
    assert_eq!(
        call.status.as_str(),
        "succeeded",
        "查询必须真的成功：{:?}",
        call.receipt_json
    );
    let receipt: serde_json::Value =
        serde_json::from_str(call.receipt_json.as_deref().expect("成功回执")).expect("回执 JSON");
    // 回执行了字节上限截断：工具层最多带回 SQL_RESULT_MAX_BYTES，超出只标记 truncated。
    assert_eq!(receipt["truncated"], true, "宽行查询必然触发上限");
    let handle = receipt["result_ref"]["id"]
        .as_str()
        .expect("回执必须带 result_ref");
    // 回执给模型的是进程内仓库句柄，落库行 ID 是 `{turn_id}-result-{call_id}`：两者用途不同，
    // 句柄给 result.read_page 用，行 ID 给持久化事实用。
    let row_id = format!("{}-result-call-wide", job.turn_id);
    let sample = receipt["rows"].as_array().expect("样本行数组");
    assert_eq!(
        sample.len() as u64,
        receipt["row_count"].as_u64().unwrap(),
        "原始回执保留已取得行"
    );
    let row_count = receipt["row_count"].as_u64().expect("总行数");
    assert!(row_count > 100, "大结果必须有足够行数，实际 {row_count}");
    assert_eq!(
        receipt["rows_omitted"].as_u64(),
        Some(0),
        "回执要如实说明省略了多少行"
    );
    assert!(
        call.receipt_json.as_deref().unwrap().len() < SQL_RESULT_MAX_BYTES + 8 * 1024,
        "回执不得接近完整结果大小：{}",
        call.receipt_json.as_deref().unwrap().len()
    );

    // —— 2. 落盘事实：retained + 受控根内相对路径 + 内容哈希 ——
    let row: (String, Option<String>, Option<String>, i64) = fixture
        .storage
        .with_ai_connection(|conn| {
            conn.query_row(
                "SELECT retention_kind,relative_path,content_hash,byte_count FROM ai_results WHERE id=?1",
                [&row_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string()))
        })
        .expect("读取结果行");
    assert_eq!(row.0, "retained", "溢写结果必须按 retained 登记");
    let relative_path = row.1.expect("retained 必须带相对路径");
    let spill_file = fixture.dir.join("ai-results").join(&relative_path);
    assert!(spill_file.is_file(), "受控根内必须有真实 spill 文件");
    assert!(row.2.is_some_and(|hash| !hash.is_empty()), "必须记内容哈希");
    // retained 行的字节数必须是 spill 文件自身大小，而不是回执长度：
    // 否则清理与配额会按回执算，和真实磁盘占用对不上。
    let spilled_bytes = fs::metadata(&spill_file).expect("读取 spill 文件").len() as i64;
    assert_eq!(
        row.3, spilled_bytes,
        "retained 行记录的字节数必须等于 spill 文件大小（{spilled_bytes}），而不是回执长度"
    );
    assert!(
        !serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&spill_file).unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty(),
        "spill 文件里必须有行"
    );
    // 回执与结果卡都不得带上受控根的绝对路径。
    assert!(
        !call
            .receipt_json
            .as_deref()
            .unwrap()
            .contains(&fixture.dir.display().to_string()),
        "回执不得泄露宿主路径"
    );
    let result_card = view
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find(|part| part.kind == "tool_result_ref")
        .expect("工具结果卡必须入库");
    assert!(
        !result_card
            .payload_json
            .contains(&fixture.dir.display().to_string()),
        "结果卡不得泄露宿主路径"
    );

    // —— 3. result.read_page 分页读回全部行，不重不漏、不重跑来源工具 ——
    let mut seen: Vec<u64> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut arguments = serde_json::json!({"result_ref": {"id": handle}, "limit": 32});
        if let Some(cursor) = cursor.as_deref() {
            arguments["cursor"] = serde_json::Value::String(cursor.into());
        }
        let receipt = run_read_page(&catalog, &job, arguments);
        assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
        let payload: serde_json::Value = serde_json::from_str(&receipt.result_json).unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["total_items"].as_u64(), Some(row_count));
        for item in payload["items"].as_array().expect("条目数组") {
            seen.push(item[0].as_u64().expect("id 列"));
        }
        pages += 1;
        assert!(pages <= 32, "分页不应无限循环");
        match payload["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert!(pages > 1, "大结果必须真的分了多页");
    assert_eq!(
        seen,
        (1..=row_count).collect::<Vec<u64>>(),
        "分页应读回全部 id 且不重不漏"
    );

    // —— 4. 跨会话读不到：另一个 turn 的仓库不认本轮句柄 ——
    let foreign = ResultStore::for_turn(
        E2E_STORE_BYTES,
        "s-other",
        "t-other",
        Some(fixture.dir.join("ai-results")),
    );
    assert_eq!(
        foreign
            .read_page(handle, None, 10, 4096)
            .expect_err("跨会话不得读到")
            .code,
        "unknown_result"
    );

    // —— 5. 重启：memory 过期、retained 仍在且可按路径核对 ——
    let mut app = AppController::new();
    app.set_ai_storage(FileStorage::new(&fixture.dir));
    assert!(matches!(
        app.dispatch(AppCommand::AiConversation(
            AiConversationCommand::RecoverOnStartup
        )),
        AppEvent::AiConversationLoaded(_)
    ));
    fixture
        .storage
        .with_ai_connection(|conn| {
            let (kind, status): (String, String) = conn
                .query_row(
                    "SELECT retention_kind,status FROM ai_results WHERE id=?1",
                    [&row_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string())
                })?;
            assert_eq!(kind, "retained");
            assert_eq!(status, "available", "无 TTL 的保留结果不因重启失效");
            Ok(())
        })
        .expect("重启后核对");
    assert!(spill_file.is_file(), "重启不得动未过期的保留结果文件");
}
