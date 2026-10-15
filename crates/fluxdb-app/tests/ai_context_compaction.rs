//! T069 运行时压缩闭环：模型窗口百分比触发 → 摘要 → 校验 → 原子 checkpoint。
//!
//! 用 ScriptedGenerationProvider 替代网络，但走真实存储、真实消息组装与真实 checkpoint 表。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent, AuditInput,
    AuditOutcome, KernelPolicy, SecurityAuditHook, TargetRef, TurnPermissionContext, TurnTargetPin,
    standard_sql_catalog,
};
use fluxdb_core::{CancelFlag, FinishReason, ModelCapabilities, ModelEvent, ModelRequest};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{context_checkpoints, model_bindings, sessions},
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t069-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    /// 窗口故意设小（4096），便于用少量历史触发压缩。传 `None` 表示模型未配置窗口。
    fn configured(&self, trigger_percent: u8, context_window_tokens: Option<i64>) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "T069 离线模型".into(),
            provider: "openai".into(),
            model: "t069-offline".into(),
            endpoint: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            context_window_tokens,
            max_output_tokens: Some(512),
            ..Default::default()
        })) {
            AppEvent::AiModelSaved { id, .. } => id,
            other => panic!("配置模型失败: {other:?}"),
        };
        match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "T069 Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "T069 压缩测试".into(),
            model_profile_id: Some(model),
            enabled: true,
            compression_trigger_percent: trigger_percent,
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { .. } => {}
            other => panic!("配置 Agent 失败: {other:?}"),
        }
        controller.dispatch(AppCommand::ReplaceConnections(vec![
            fluxdb_core::ConnectionConfig {
                id: fluxdb_core::ConnectionId(1),
                name: "compaction-sqlite".into(),
                kind: fluxdb_core::DatabaseKind::Sqlite,
                endpoint: fluxdb_core::Endpoint::SqliteFile {
                    path: "demo.db".into(),
                    read_only: true,
                },
                options: Default::default(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
            },
        ]));
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct AllowAudit;

impl SecurityAuditHook for AllowAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
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

/// 记录每次请求，并按"摘要请求/正常请求"返回不同内容。
/// `raw_summary` 为真时摘要请求原样返回 `summary`（用于构造不合规摘要）。
struct CompactionScript {
    requests: std::sync::Mutex<Vec<ModelRequest>>,
    summary: std::sync::Mutex<String>,
    raw_summary: bool,
}

impl CompactionScript {
    fn new(summary: &str) -> Arc<Self> {
        Arc::new(Self {
            requests: std::sync::Mutex::new(Vec::new()),
            summary: std::sync::Mutex::new(summary.to_string()),
            raw_summary: false,
        })
    }

    /// 摘要请求原样返回给定文本，用于验证"摘要校验失败不落盘"。
    fn raw(summary: &str) -> Arc<Self> {
        Arc::new(Self {
            requests: std::sync::Mutex::new(Vec::new()),
            summary: std::sync::Mutex::new(summary.to_string()),
            raw_summary: true,
        })
    }
}

impl fluxdb_core::GenerationProvider for CompactionScript {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        }
    }

    fn stream(
        &self,
        request: ModelRequest,
        _cancel: CancelFlag,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = fluxdb_core::Result<Vec<ModelEvent>>> + Send + '_>,
    > {
        let is_summary = request
            .messages
            .iter()
            .any(|m| m.text.contains("压缩历史上下文"));
        let raw = self.raw_summary;
        let text = if is_summary {
            if raw {
                self.summary.lock().unwrap().clone()
            } else {
                format!(
                    "{{\"goal\":\"压缩目标\",\"constraints\":\"无\",\"verified_facts\":\"{}\",\"decisions\":\"继续\",\"next_steps\":\"下一步\"}}",
                    self.summary.lock().unwrap()
                )
            }
        } else {
            "完成".to_string()
        };
        self.requests.lock().unwrap().push(request);
        Box::pin(async move {
            Ok(vec![
                ModelEvent::TextDelta(text),
                ModelEvent::Finished(FinishReason::Stop),
            ])
        })
    }
}

/// 走真实生产链路发一轮消息并等待终态。
/// 每轮 prompt 故意较长（约 500 个 CJK 字符 ≈ 500 token 估算），便于跨过窗口阈值。
const LONG_PROMPT: &str = "请基于当前数据库结构继续分析这张订单表的索引与查询计划，\
并说明哪些字段适合建立联合索引、哪些查询可能触发全表扫描。同时请解释上一次执行的\
SQL 为什么会产生临时表，以及可以怎样改写以减少排序和回表次数。请把结论按目标、约束、\
已验证事实、决策和下一步组织，并在需要更多信息时明确指出要读取的表结构或执行计划。";

fn run_turn(
    fixture: &Fixture,
    controller: &mut AppController,
    provider: &Arc<CompactionScript>,
) -> String {
    run_turn_with_summary(fixture, controller, provider, None)
}

/// 同上，但可显式注入摘要 Provider（用于验证独立摘要模型确实被使用）。
fn run_turn_with_summary(
    fixture: &Fixture,
    controller: &mut AppController,
    provider: &Arc<CompactionScript>,
    summary: Option<&Arc<CompactionScript>>,
) -> String {
    let job = controller
        .prepare_ai_conversation(LONG_PROMPT, &target())
        .expect("准备会话");
    let (sender, receiver) = std::sync::mpsc::sync_channel(16);
    let storage = FileStorage::new(&fixture.dir);
    let provider: Arc<dyn fluxdb_core::GenerationProvider> = provider.clone();
    let summary_provider: Option<Arc<dyn fluxdb_core::GenerationProvider>> =
        summary.map(|provider| provider.clone() as Arc<dyn fluxdb_core::GenerationProvider>);
    let audit = AllowAudit;
    let resolver = Arc::new(fluxdb_app::MapSqlResolver::new([]));
    let tools = standard_sql_catalog(resolver, false).expect("只读 SQL 工具目录");
    fluxdb_app::run_ai_conversation_turn_with_summary(
        &storage,
        &job,
        &CancelFlag::new(),
        &sender,
        provider.as_ref(),
        summary_provider.as_deref(),
        &tools,
        &audit,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );
    drop(sender);
    let mut text = String::new();
    let mut session_id = job.session_id.clone();
    while let Ok(update) = receiver.recv() {
        session_id = update.job.session_id.clone();
        let Some(view) = update.view.as_ref() else {
            continue; // 纯增量回包不携带视图（正文由 checkpoint 重现）。
        };
        for message in &view.messages {
            for part in &message.parts {
                if part.kind == "text" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&part.payload_json)
                    {
                        if let Some(value) = value.get("text").and_then(|v| v.as_str()) {
                            text.push_str(value);
                        }
                    }
                }
            }
        }
        if update.terminal {
            break;
        }
    }
    let _ = session_id;
    text
}

/// 建一个指向本地 mock 的 System One 决策模型，并把它写进压缩决策槽位。
///
/// 走「上下文压缩 · 管理」的同一条落库口径：槽位是宿主设置，模型引用不在插件 JSON 里。
fn configure_compaction_decision_model(
    fixture: &Fixture,
    controller: &mut AppController,
    endpoint: &str,
) -> String {
    let model_id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: "T069 压缩决策模型".into(),
        provider: "decisions".into(),
        model: "t069-compaction-decision".into(),
        endpoint: Some(endpoint.to_string()),
        enabled: true,
        api_key: Some("sk-fixture-not-real".into()),
        context_window_tokens: Some(8_192),
        max_output_tokens: Some(512),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("配置压缩决策模型失败: {other:?}"),
    };
    let event = controller.dispatch(AppCommand::SaveAiPluginConfig {
        instance_id: "builtin.compaction".into(),
        selections: vec![fluxdb_app::AiPluginFieldSelection {
            key: fluxdb_app::COMPACTION_DECISION_MODEL_FIELD.into(),
            resource_id: Some(model_id.clone()),
        }],
        expected_revision: controller.state().ai_runtime_revision,
    });
    assert!(
        matches!(event, AppEvent::AiPluginConfigSaved { .. }),
        "保存压缩决策模型失败: {event:?}"
    );
    // 槽位必须落在压缩决策列，而不是 Jev 决策列。
    assert_eq!(controller.state().ai_default_decision_model_id, None);
    fixture
        .storage
        .with_ai_connection(|conn| {
            let runtime = fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?;
            assert_eq!(
                runtime.compaction_decision_model_profile_id.as_deref(),
                Some(model_id.as_str())
            );
            Ok(())
        })
        .unwrap();
    model_id
}

/// 决策模型的取舍：在宿主给出的合法切点里选哪一个（数字最大 = 压缩最多）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CutPick {
    Most,
    Least,
}

/// 本地 mock System One 服务：记录收到的候选 ID，并按 [`CutPick`] 返回一个候选。
fn spawn_compaction_mock(pick: CutPick) -> (String, Arc<std::sync::Mutex<Vec<Vec<String>>>>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地 mock 端口");
    listener.set_nonblocking(true).expect("非阻塞");
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let seen: Arc<std::sync::Mutex<Vec<Vec<String>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handle = Arc::clone(&seen);
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let handle = Arc::clone(&handle);
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut request = Vec::new();
                        let mut chunk = [0u8; 4096];
                        while let Ok(read) = stream.read(&mut chunk) {
                            request.extend_from_slice(&chunk[..read]);
                            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let head = String::from_utf8_lossy(&request).to_string();
                        let length = head
                            .split("\r\n")
                            .find_map(|line| {
                                line.strip_prefix("Content-Length: ")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|index| index + 4)
                            .unwrap_or(request.len());
                        let mut body = request[header_end..].to_vec();
                        while body.len() < length {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(read) => body.extend_from_slice(&chunk[..read]),
                            }
                        }
                        let parsed: serde_json::Value =
                            serde_json::from_slice(&body).unwrap_or_default();
                        let ids: Vec<String> = parsed["questions"]["decision"]["criteria"]
                            .as_object()
                            .map(|map| map.keys().cloned().collect())
                            .unwrap_or_default();
                        handle.lock().unwrap().push(ids.clone());
                        let seq = |id: &String| {
                            id.strip_prefix("cut:")
                                .and_then(|value| value.parse::<i64>().ok())
                                .unwrap_or_default()
                        };
                        let chosen = match pick {
                            CutPick::Most => ids.iter().max_by_key(|id| seq(id)).cloned(),
                            CutPick::Least => ids.iter().min_by_key(|id| seq(id)).cloned(),
                        }
                        .unwrap_or_default();
                        let payload = serde_json::json!({
                            "answers": {"decision": {
                                "type": "choice",
                                "choice": chosen,
                                "probabilities": ids.iter()
                                    .map(|id| (
                                        id.clone(),
                                        serde_json::json!(if *id == chosen { 0.82 } else { 0.09 }),
                                    ))
                                    .collect::<serde_json::Map<_, _>>(),
                            }},
                            "usage": {"input_tokens": 21, "output_tokens": 3}
                        })
                        .to_string()
                        .into_bytes();
                        let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        )
                        .as_bytes(),
                    );
                        let _ = stream.write_all(&payload);
                        let _ = stream.flush();
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });
    (endpoint, seen)
}

/// 再建一个已启用的生成模型（摘要模型候选）。
fn add_generation_model(controller: &mut AppController, display_name: &str) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: display_name.into(),
        provider: "openai".into(),
        model: format!("t035-{}", display_name),
        endpoint: Some("http://127.0.0.1:9/v1".into()),
        enabled: true,
        context_window_tokens: Some(4096),
        max_output_tokens: Some(512),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("配置摘要模型失败: {other:?}"),
    }
}

/// 给 Agent 建立 `compaction` 角色绑定（用户来源）。
fn bind_compaction_model(fixture: &Fixture, agent_id: &str, model_id: &str, enabled: bool) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            model_bindings::create_binding(
                conn,
                1,
                &model_bindings::ModelBindingEdit {
                    id: format!("binding-{model_id}"),
                    agent_id: agent_id.to_string(),
                    model_profile_id: model_id.to_string(),
                    role: model_bindings::ModelRole::Compaction,
                    binding_source: model_bindings::BindingSource::User,
                    enabled,
                    priority: 100,
                },
            )?;
            Ok(())
        })
        .expect("建立 compaction 绑定");
}

/// 读回该会话的计量行（用途，模型档案）。
fn ledger_rows(fixture: &Fixture, session_id: &str) -> Vec<(String, String)> {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT purpose, model_profile_id FROM ai_usage_ledger WHERE session_id=?1 ORDER BY created_at_ms, id")
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            let rows = stmt
                .query_map([session_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(rows)
        })
        .expect("读取计量行")
}

/// 会话里可见的 notice 片段（id、状态、文本），用于断言"信息性降级不标失败"等观感契约。
fn notice_rows(fixture: &Fixture, session_id: &str) -> Vec<(String, String, String)> {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT p.id, p.status, p.payload_json FROM ai_message_parts p JOIN ai_messages m ON m.id=p.message_id WHERE m.session_id=?1 AND p.kind='notice' ORDER BY p.created_at_ms")
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            let rows = stmt
                .query_map([session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(rows)
        })
        .expect("读取 notice")
        .into_iter()
        .map(|(id, status, payload)| {
            let text = serde_json::from_str::<serde_json::Value>(&payload)
                .ok()
                .and_then(|value| value.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                .unwrap_or_default();
            (id, status, text)
        })
        .collect()
}

/// 会话里可见的压缩相关 notice 文本。
fn compaction_notices(fixture: &Fixture, session_id: &str) -> Vec<String> {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT payload_json FROM ai_message_parts p JOIN ai_messages m ON m.id=p.message_id WHERE m.session_id=?1 AND p.kind='notice' ORDER BY p.created_at_ms")
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            let rows = stmt
                .query_map([session_id], |row| row.get::<_, String>(0))
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(rows)
        })
        .expect("读取 notice")
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter_map(|value| value.get("text").and_then(|text| text.as_str()).map(str::to_owned))
        .collect()
}

#[test]
fn percent_trigger_creates_checkpoint_and_keeps_original_messages() {
    let fixture = Fixture::new();
    // 4096 窗口、35% 阈值（≈1433 token）；最大输出 512 与安全余量 512 后可用 ≈3072，
    // 因此这里命中"百分比"分支而不是输出预留分支。
    let mut controller = fixture.configured(35, Some(4096));
    let provider = CompactionScript::new("已验证事实");

    // 先灌入足够长的历史，使 4096 窗口的 50% 阈值被跨过。
    for _ in 0..10 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }

    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    let checkpoint = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("达到阈值后应产生 checkpoint");
    assert!(
        checkpoint.summary_text.contains("已验证事实"),
        "checkpoint 摘要应来自摘要模型的真实输出：{}",
        checkpoint.summary_text
    );
    assert!(checkpoint.last_message_seq >= 1);
    let requests = provider.requests.lock().unwrap();
    for request in requests.iter().filter(|request| {
        request
            .messages
            .iter()
            .any(|message| message.text.contains("当前执行目标"))
    }) {
        assert!(
            request.messages[0]
                .text
                .contains(r#""database_kind":"sqlite""#),
            "历史压缩前后都必须保留真实引擎"
        );
        assert!(
            request.messages[0]
                .text
                .contains(r#""sql_dialect":"sqlite""#)
        );
    }
    drop(requests);

    // 原始消息绝不删除：checkpoint 只记录切点。
    let messages = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_messages(conn, &session_id))
        .unwrap();
    assert!(
        messages.len() > checkpoint.last_message_seq as usize,
        "原始历史应完整保留：messages={}, checkpoint.last={}",
        messages.len(),
        checkpoint.last_message_seq
    );
}

#[test]
fn invalid_summary_never_commits_checkpoint() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(40, Some(4096));
    // 摘要缺少保护项 → 校验失败，不得落盘。
    let provider = CompactionScript::raw("这是一段缺少保护项字段的自由文本");
    for _ in 0..12 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    assert!(
        fixture
            .storage
            .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
            .unwrap()
            .is_none(),
        "摘要校验失败时绝不能提交 checkpoint"
    );
    // 失败不影响原始历史与本轮继续运行。
    let messages = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_messages(conn, &session_id))
        .unwrap();
    assert!(messages.len() > 1, "原始历史必须保留且继续增长");
}

#[test]
fn no_checkpoint_and_no_blocking_when_window_is_unknown() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(50, None);
    // 窗口未知（未配置 Jev/模型容量）时，不触发压缩，但对话必须完整跑完。
    let provider = CompactionScript::new("摘要");
    for _ in 0..8 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("会话应正常创建");
    assert!(
        fixture
            .storage
            .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
            .unwrap()
            .is_none(),
        "窗口未知时不得触发压缩"
    );
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.messages.iter().any(|m| m.text.contains("压缩历史上下文"))),
        "窗口未知时不应发出摘要请求"
    );
    assert!(
        !fixture
            .storage
            .with_ai_connection(|conn| sessions::list_messages(conn, &session_id))
            .unwrap()
            .is_empty(),
        "对话必须正常完成并持久化"
    );
}

#[test]
fn checkpoint_carries_previous_summary_incrementally() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(40, Some(4096));
    let provider = CompactionScript::new("增量事实");
    for _ in 0..14 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    let all = fixture
        .storage
        .with_ai_connection(|conn| {
            let sql = "SELECT generation, summary_text FROM ai_context_checkpoints WHERE session_id=?1 ORDER BY generation";
            let mut stmt = conn
                .prepare(sql)
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            let rows = stmt
                .query_map([&session_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(rows)
        })
        .unwrap();
    assert!(!all.is_empty(), "应产生至少一个 checkpoint");
    // generation 必须从 1 起单调递增，不能覆盖旧 generation。
    for (index, (generation, _)) in all.iter().enumerate() {
        assert_eq!(*generation, index as i64 + 1, "generation 必须单调递增");
    }
    // 第二次压缩时，摘要请求里必须带上上一次摘要（增量合并）。
    let requests = provider.requests.lock().unwrap();
    let later_summary_requests: Vec<&ModelRequest> = requests
        .iter()
        .filter(|r| r.messages.iter().any(|m| m.text.contains("压缩历史上下文")))
        .collect();
    if later_summary_requests.len() >= 2 {
        assert!(
            later_summary_requests[1]
                .messages
                .iter()
                .any(|m| m.text.contains("已有摘要")),
            "后续压缩必须携带旧摘要做增量"
        );
    }
}

#[test]
fn bound_compaction_model_is_used_and_metered_under_its_own_profile() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(35, Some(4096));
    let agent_id = controller.state().ai_agents[0].id.clone();
    let summary_model = add_generation_model(&mut controller, "T035 摘要模型");
    bind_compaction_model(&fixture, &agent_id, &summary_model, true);

    // 主模型与摘要模型用两个脚本：只有摘要脚本会命中独立模型那条链路。
    let main = CompactionScript::new("主模型");
    let summary = CompactionScript::new("独立摘要模型事实");
    for _ in 0..10 {
        let _ = run_turn_with_summary(&fixture, &mut controller, &main, Some(&summary));
    }

    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    // 摘要请求必须落在摘要 Provider 上，主 Provider 不能收到摘要请求。
    assert!(
        summary
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.text.contains("压缩历史上下文"))),
        "摘要请求必须发给绑定的摘要模型"
    );
    assert!(
        main.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.messages.iter().any(|m| m.text.contains("压缩历史上下文"))),
        "配置了独立摘要模型后，主模型不应再收到摘要请求"
    );
    let request = summary
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.messages.iter().any(|m| m.text.contains("压缩历史上下文")))
        .cloned()
        .expect("摘要请求");
    assert_eq!(request.model, summary_model, "请求必须声明摘要模型档案");

    // 计量：compaction 行必须记在摘要模型上，而不是主模型。
    let rows = ledger_rows(&fixture, &session_id);
    let compaction_models: Vec<&String> = rows
        .iter()
        .filter(|(purpose, _)| purpose == "compaction")
        .map(|(_, model)| model)
        .collect();
    assert!(!compaction_models.is_empty(), "摘要请求必须进入计量账本");
    assert!(
        compaction_models
            .iter()
            .all(|model| **model == summary_model),
        "compaction 计量必须记为实际使用的摘要模型：{compaction_models:?}"
    );

    // checkpoint 记录所用模型，便于"原始记录可查"。
    let checkpoint = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("应产生 checkpoint");
    assert_eq!(
        checkpoint.model_profile_id.as_deref(),
        Some(summary_model.as_str()),
        "checkpoint 必须记录实际摘要模型"
    );
}

#[test]
fn unavailable_compaction_model_is_visible_and_never_silently_replaced() {
    let fixture = Fixture::new();
    // 10% 阈值 + 小窗口：很快超出可用输入预算，能验证"仍超预算则可见暂停"。
    let mut controller = fixture.configured(10, Some(2048));
    let agent_id = controller.state().ai_agents[0].id.clone();
    // 摘要绑定启用，但其模型被停用 → 候选不合格。
    let summary_model = add_generation_model(&mut controller, "T035 停用摘要模型");
    bind_compaction_model(&fixture, &agent_id, &summary_model, true);
    controller.dispatch(AppCommand::SetAiModelEnabled {
        id: summary_model.clone(),
        enabled: false,
        expected_revision: 1,
    });

    let provider = CompactionScript::new("主模型");
    for _ in 0..4 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }

    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    // 不静默改用主模型：主模型不应收到任何摘要请求。
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.messages.iter().any(|m| m.text.contains("压缩历史上下文"))),
        "摘要模型不可用时绝不能静默改用主模型"
    );
    assert!(
        fixture
            .storage
            .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
            .unwrap()
            .is_none(),
        "摘要失败不得提交 checkpoint"
    );
    let notices = compaction_notices(&fixture, &session_id);
    assert!(
        notices.iter().any(|text| text.contains("摘要模型")
            && (text.contains("不可用") || text.contains("已停用"))),
        "摘要模型不可用必须有可见说明：{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|text| text.contains("已暂停") || text.contains("超出模型窗口")),
        "超预算时必须可见暂停：{notices:?}"
    );
}

#[test]
fn truncated_summary_keeps_previous_checkpoint_and_original_history() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(35, Some(4096));
    let good = CompactionScript::new("第一次有效摘要");
    for _ in 0..10 {
        let _ = run_turn(&fixture, &mut controller, &good);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    let first = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("首次压缩应提交 checkpoint");

    // 之后摘要一律返回被截断的自由文本 → 校验失败，旧 checkpoint 必须原样保留。
    let truncated = CompactionScript::raw("goal: 目标被截断…[truncated]");
    for _ in 0..6 {
        let _ = run_turn(&fixture, &mut controller, &truncated);
    }

    let after = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("旧 checkpoint 必须保留");
    assert_eq!(after.id, first.id, "摘要校验失败不得覆盖旧 checkpoint");
    assert_eq!(after.generation, first.generation);
    assert_eq!(after.summary_text, first.summary_text);
    assert!(
        compaction_notices(&fixture, &session_id)
            .iter()
            .any(|text| text.contains("上下文压缩未完成")),
        "截断摘要必须给出可见失败说明"
    );
    // 原始记录可查：消息没有因为压缩或失败被删除。
    let messages = fixture
        .storage
        .with_ai_connection(|conn| sessions::list_messages(conn, &session_id))
        .unwrap();
    assert!(messages.len() > first.last_message_seq as usize);
}

#[test]
fn informational_compaction_notice_is_not_marked_as_failure() {
    let fixture = Fixture::new();
    // 窗口 4096、阈值 10%（409）：本条提问已过阈值，但历史只有一条、切不出安全切点
    // → 本轮不压缩、对话照常继续（与真实窗口里看到的现象一致），notice 不该标失败。
    let mut controller = fixture.configured(10, Some(4_096));
    let provider = CompactionScript::new("主模型");
    let _ = run_turn(&fixture, &mut controller, &provider);
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");

    let rows = notice_rows(&fixture, &session_id);
    assert_eq!(rows.len(), 1, "只应有一条压缩 notice：{rows:?}");
    let (id, status, text) = &rows[0];
    assert!(
        text.contains("暂无可安全压缩的完整历史"),
        "实际文案：{text}"
    );
    assert_ne!(
        *status, "error",
        "本轮只是不压缩、对话继续，不能把 notice 标成失败：{id} {status}"
    );
    assert!(
        rows.iter()
            .all(|(id, _, _)| !id.ends_with("-notice-compaction-paused")),
        "没有暂停时不应出现暂停片段"
    );
}

#[test]
fn paused_compaction_keeps_two_visible_copies_and_never_marks_success() {
    let fixture = Fixture::new();
    // 窗口 1024 / 输出 512：可用输入预算为 0，第一条就会判定超预算 → 直接走可见暂停。
    let mut controller = fixture.configured(10, Some(1_024));
    let agent_id = controller.state().ai_agents[0].id.clone();
    let summary_model = add_generation_model(&mut controller, "T035 停用摘要模型");
    bind_compaction_model(&fixture, &agent_id, &summary_model, true);
    controller.dispatch(AppCommand::SetAiModelEnabled {
        id: summary_model,
        enabled: false,
        expected_revision: 1,
    });
    let provider = CompactionScript::new("主模型");
    let _ = run_turn(&fixture, &mut controller, &provider);
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");

    let rows = notice_rows(&fixture, &session_id);
    let compaction_rows: Vec<&(String, String, String)> = rows
        .iter()
        .filter(|(id, _, _)| id.ends_with("-notice-compaction"))
        .collect();
    assert_eq!(
        compaction_rows.len(),
        1,
        "暂停轮只应有一条压缩 notice：{rows:?}"
    );
    assert_eq!(compaction_rows[0].1, "error", "暂停必须可见失败态");
    assert!(compaction_rows[0].2.contains("超出模型窗口"));
    assert!(
        rows.iter()
            .all(|(id, _, _)| !id.ends_with("-notice-compaction-paused")),
        "暂停原因已由压缩 notice 承载，不应再存第三份副本：{rows:?}"
    );
    // 助手正文保留一句可见的暂停状态，方便折叠 notice 时也能看到。
    let texts: Vec<String> = fixture
        .storage
        .with_ai_connection(|conn| {
            let sql = "SELECT p.payload_json FROM ai_message_parts p JOIN ai_messages m ON m.id=p.message_id WHERE m.session_id=?1 AND p.kind='text' ORDER BY p.created_at_ms";
            let mut stmt = conn.prepare(sql).map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            let rows = stmt.query_map([session_id.as_str()], |row| row.get::<_, String>(0))
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(rows)
        })
        .unwrap()
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter_map(|value| value.get("text").and_then(|t| t.as_str()).map(str::to_owned))
        .collect();
    assert!(
        texts.iter().any(|text| text.starts_with("本轮已暂停")),
        "暂停轮应有可见正文：{texts:?}"
    );
}

/// 用给定取舍跑满 10 轮，返回（会话 id、每次决策请求的候选、最终 checkpoint 切点）。
fn run_with_decision_pick(fixture: &Fixture, pick: CutPick) -> (String, Vec<Vec<String>>, i64) {
    let mut controller = fixture.configured(35, Some(4_096));
    let (endpoint, seen) = spawn_compaction_mock(pick);
    configure_compaction_decision_model(fixture, &mut controller, &endpoint);
    let provider = CompactionScript::new("压缩决策已验证事实");
    for _ in 0..10 {
        let _ = run_turn(fixture, &mut controller, &provider);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    let checkpoint = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("触发后应提交 checkpoint");
    // 空候选只可能来自 mock 没读全请求体；宿主在候选不足时根本不会发请求。
    let requests: Vec<Vec<String>> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|ids| !ids.is_empty())
        .cloned()
        .collect();
    (session_id, requests, checkpoint.last_message_seq)
}

/// 压缩决策模型真的参与压缩：它选的切点决定 checkpoint 覆盖到哪一条历史。
///
/// 走真实链路：真实 SQLite + 真实 System One HTTP（本地 mock）+ 真实 checkpoint 表。
/// 两次运行只有"决策模型选哪个切点"不同，最终切点必须随之不同——否则说明建议没被采用。
#[test]
fn compaction_decision_model_choice_selects_the_cut() {
    let fixture = Fixture::new();
    let (session_id, requests, most_cut) = run_with_decision_pick(&fixture, CutPick::Most);
    assert!(
        !requests.is_empty(),
        "配置了压缩决策模型后必须真的发出决策请求"
    );
    assert!(
        requests.iter().all(|ids| ids.len() >= 2),
        "候选必须是宿主算好的多个合法切点：{requests:?}"
    );
    // 提交的切点必须来自决策模型给出的候选，而不是宿主自己挑的。
    let chosen: Vec<i64> = requests
        .iter()
        .filter_map(|ids| ids.iter().max_by_key(|id| seq_of_candidate(id)).cloned())
        .filter_map(|id| seq_of_candidate(&id))
        .collect();
    assert!(
        chosen.contains(&most_cut),
        "checkpoint 切点 {most_cut} 必须来自决策模型的候选：{chosen:?}"
    );
    // 决策请求仍必须进入账本（不吞掉用量）。
    let rows = ledger_rows(&fixture, &session_id);
    assert!(
        rows.iter().any(|(purpose, _)| purpose == "decision"),
        "压缩决策请求必须进入计量账本：{rows:?}"
    );

    // 同样历史、只把决策模型换成"压缩最少"，最终切点必须不同。
    let other = Fixture::new();
    let (_, other_requests, least_cut) = run_with_decision_pick(&other, CutPick::Least);
    assert!(!other_requests.is_empty(), "另一组也必须真的发出决策请求");
    assert_ne!(
        most_cut, least_cut,
        "决策模型选了不同的切点，压缩结果必须随之不同（否则建议没有被采用）"
    );
}

/// 从候选 ID 解析切点 seq（宿主用 `cut:<seq>` 编码）。
fn seq_of_candidate(id: &str) -> Option<i64> {
    id.strip_prefix("cut:")
        .and_then(|value| value.parse::<i64>().ok())
}

/// 压缩决策模型不可用（未选择）时：不发决策请求，压缩仍按确定性切点正常完成。
#[test]
fn compaction_runs_on_deterministic_cut_without_decision_model() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured(35, Some(4_096));
    let provider = CompactionScript::new("无决策模型的确定性压缩");
    for _ in 0..10 {
        let _ = run_turn(&fixture, &mut controller, &provider);
    }
    let session_id = controller
        .state()
        .ai_conversation
        .session_id
        .clone()
        .expect("已有会话");
    let checkpoint = fixture
        .storage
        .with_ai_connection(|conn| context_checkpoints::load_latest(conn, &session_id))
        .unwrap()
        .expect("未配置压缩决策模型也必须正常压缩");
    assert!(checkpoint.summary_text.contains("无决策模型的确定性压缩"));
    let rows = ledger_rows(&fixture, &session_id);
    assert!(
        rows.iter().all(|(purpose, _)| purpose != "decision"),
        "没有配置压缩决策模型时不得发出决策请求：{rows:?}"
    );
}
