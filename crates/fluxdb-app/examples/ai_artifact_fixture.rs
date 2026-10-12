//! T039 真实窗口验收夹具：跑通真实工具链并产出真实产物，不预置成功回执。
//!
//! 与 ai_panel_fixture 的区别：这里复用集成测试的受控 Provider 驱动**生产装配链**
//! （schema 校验 → 审计 → 授权 → 执行 → 投影 → 落盘 → renderer → 快照提交），
//! 产物文件与来源行都是真实写入的，不手写 artifact_ref 片段。
//!
//! 离线：不联网、不读用户配置或凭据，只接受新建的空目录。
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluxdb_app::*;
use fluxdb_core::{
    AiFuture, CancelFlag, FinishReason, GenerationProvider, ModelCapabilities, ModelEvent,
    ModelMessage, ModelRequest, ModelRole, ToolCallRequest,
};
use fluxdb_storage::{FileStorage, Storage as _};

/// 已知结果：web=12、store=34。模型只声明来源句柄与字段名，数字由宿主从回执解析。
const SALES_SQL: &str = "SELECT channel,revenue FROM sales ORDER BY revenue";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("必须传入新建 fixture root")?;
    let dir = PathBuf::from(root);
    let storage = FileStorage::new(&dir);
    storage.with_ai_connection(|_| Ok(()))?;

    // 隔离 SQLite：产物数字必须能当场核对。
    let path = dir.join("sales.sqlite");
    rusqlite::Connection::open(&path)?.execute_batch(
        "CREATE TABLE sales(channel TEXT,revenue INTEGER);
         INSERT INTO sales VALUES ('web',12),('store',34);",
    )?;

    let mut settings = fluxdb_core::Settings::default();
    settings.log_path = dir.join("logs").to_string_lossy().into_owned();
    storage.save_settings(&settings)?;
    let config = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "T039 sales(夹具)".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path,
            read_only: true,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    storage.save_connections(&[config.clone()])?;

    let mut app = AppController::new();
    app.set_ai_storage(storage.clone());
    let model_id = match app.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "T039 离线模型(不联网)".into(),
        provider: "openai".into(),
        model: "t039-fixture".into(),
        endpoint: Some("http://127.0.0.1:1/v1".into()),
        endpoint_is_full_url: false,
        enabled: true,
        egress: true,
        context_window_tokens: Some(32768),
        max_output_tokens: Some(1024),
        api_key: None,
        clear_key: false,
        expected_revision: None,
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => return Err(format!("模型保存失败 {other:?}").into()),
    };
    let _agent_id = match app.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
        compression_trigger_percent: 80,
        id: None,
        display_name: "T039 销售分析".into(),
        description: "生成图表与表格报告".into(),
        system_prompt: "先查询真实数据，再用 artifact 工具产出报告；数字不得自行编造。".into(),
        model_profile_id: Some(model_id),
        allowed_tools: Vec::new(),
        enabled: true,
        expected_revision: 0,
    })) {
        AppEvent::AiAgentSaved { id, .. } => id,
        other => return Err(format!("Agent 保存失败 {other:?}").into()),
    };

    let target = TargetRef {
        connection_id: Some(1),
        database: Some("main".into()),
        schema: None,
        object: Some("sales".into()),
        redis_db: None,
    };
    let job = app.prepare_ai_conversation(
        "查询各渠道销售额并生成报告",
        &TurnTargetPin {
            target_json: target.to_json(),
            target,
            captured_tab_id: Some(1),
            captured_tab_revision: 1,
        },
    )?;

    // 与生产装配同构：按 turn 绑定归属 + 受控 spill 根。
    let results = Arc::new(Mutex::new(ResultStore::for_turn(
        RESULT_STORE_MEMORY_BYTES,
        &job.session_id,
        &job.turn_id,
        Some(dir.join("ai-results")),
    )));
    let mut catalog = standard_sql_catalog_with_results(
        Arc::new(MapSqlResolver::new([config])),
        Arc::clone(&results),
        false,
    )?;
    add_ai_artifact_tools(&mut catalog, &storage, &job, Arc::clone(&results))?;

    let provider = FixtureProvider::new();
    let cancel = CancelFlag::new();
    let (tx, _rx) = std::sync::mpsc::sync_channel(256);
    run_ai_conversation_turn(
        &storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &KernelAuditHook,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );

    storage.with_ai_connection(|conn| {
        use fluxdb_storage::ai_platform::{artifacts, sessions};
        let rows = artifacts::list_artifacts_for_turn(conn, &job.turn_id)?;
        for row in &rows {
            println!(
                "artifact id={} status={} kind={} sources={} path={:?} bytes={} hash={:?}",
                row.id,
                row.status,
                row.kind,
                artifacts::list_artifact_sources(conn, &row.id)?.len(),
                row.relative_path,
                row.byte_count,
                row.content_hash,
            );
        }
        if rows.is_empty() || rows.iter().any(|row| row.status != "ready") {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "产物未全部 ready，请检查上面的状态",
            ));
        }
        for call in sessions::list_tool_calls_for_turn(conn, &job.turn_id)? {
            println!("tool {} -> {}", call.tool_id, call.status);
        }
        Ok(())
    })?;

    println!(
        "session={} 已完成；产物快照目录 {}",
        job.session_id,
        dir.join("ai-artifacts").display()
    );
    Ok(())
}

/// 受控 Provider：只决定**调用哪个工具**，不伪造任何工具回执。
///
/// artifact_id 由宿主分配，模型无从预知，所以后续调用必须从上一轮真实回执里读。
struct FixtureProvider {
    step: Mutex<usize>,
}

impl FixtureProvider {
    fn new() -> Self {
        Self {
            step: Mutex::new(0),
        }
    }

    /// 从已回放的 tool 消息里取真实 artifact ID。
    fn artifact_id_from(messages: &[ModelMessage], tool: &str) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|message| {
                message.role == ModelRole::Tool && message.tool_name.as_deref() == Some(tool)
            })
            .and_then(|message| serde_json::from_str::<serde_json::Value>(&message.text).ok())
            .and_then(|value| value["result"]["artifact_id"].as_str().map(str::to_owned))
    }

    fn tool_result_status(messages: &[ModelMessage], tool: &str) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|message| {
                message.role == ModelRole::Tool && message.tool_name.as_deref() == Some(tool)
            })
            .and_then(|message| serde_json::from_str::<serde_json::Value>(&message.text).ok())
            .and_then(|value| value["result"]["status"].as_str().map(str::to_owned))
    }
}

impl GenerationProvider for FixtureProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        }
    }

    fn stream(&self, request: ModelRequest, _cancel: CancelFlag) -> AiFuture<'_, Vec<ModelEvent>> {
        let mut step = self.step.lock().expect("step 锁");
        let index = *step;
        *step += 1;
        let call = |id: &str, tool: &str, arguments: serde_json::Value| {
            vec![
                ModelEvent::ToolCallRequested(ToolCallRequest {
                    provider_call_id: id.into(),
                    tool_id: tool.into(),
                    arguments_json: arguments.to_string(),
                }),
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        let events = match index {
            0 => call(
                "fixture-query",
                "db.query",
                serde_json::json!({
                    "target": "conn:1/db:main",
                    "sql": SALES_SQL,
                    "max_rows": 200
                }),
            ),
            1 => {
                // 句柄由宿主在上一轮回执里给出，模型照抄，不自造。
                let handle = request
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == ModelRole::Tool)
                    .and_then(|message| {
                        serde_json::from_str::<serde_json::Value>(&message.text).ok()
                    })
                    .and_then(|value| {
                        value["result"]["result_ref"]["id"]
                            .as_str()
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "result-1".into());
                call(
                    "fixture-create",
                    "artifact.create",
                    serde_json::json!({
                        "title": "各渠道销售额",
                        "kind": "report",
                        "renderer": "fluxdb.bi.report",
                        "metric": {"name": "Revenue", "timezone": "UTC",
                                   "assumption": "夹具口径：revenue 列原样汇总"},
                        "sources": [{"result_ref": handle}],
                        "blocks": [{"type": "chart", "result_ref": handle, "chart": "bar",
                                    "x": "channel", "y": "revenue"}]
                    }),
                )
            }
            2 => {
                let id = Self::artifact_id_from(&request.messages, "artifact.create")
                    .expect("上一轮必须真实创建产物");
                let status = Self::tool_result_status(&request.messages, "artifact.create")
                    .unwrap_or_default();
                assert_eq!(status, "draft", "create 必须成功，真实回执：{status}");
                call(
                    "fixture-add",
                    "artifact.add_block",
                    serde_json::json!({
                        "artifact_id": id,
                        "block": {"type": "table", "result_ref": "result-1",
                                  "columns": ["channel", "revenue"]}
                    }),
                )
            }
            _ => {
                if index == 3 {
                    let id = Self::artifact_id_from(&request.messages, "artifact.create")
                        .expect("产物 ID 必须来自真实回执");
                    call(
                        "fixture-finalize",
                        "artifact.finalize",
                        serde_json::json!({"artifact_id": id}),
                    )
                } else {
                    // 完成后再被要求一次时给文本收尾，不重复提交。
                    vec![
                        ModelEvent::TextDelta("报告已完成：图表与表格均来自真实查询结果。".into()),
                        ModelEvent::Finished(FinishReason::Stop),
                    ]
                }
            }
        };
        Box::pin(async move { Ok(events) })
    }
}
