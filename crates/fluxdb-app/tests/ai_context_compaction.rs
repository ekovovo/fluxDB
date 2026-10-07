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
    ai_platform::{context_checkpoints, sessions},
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
    let job = controller
        .prepare_ai_conversation(LONG_PROMPT, &target())
        .expect("准备会话");
    let (sender, receiver) = std::sync::mpsc::sync_channel(16);
    let storage = FileStorage::new(&fixture.dir);
    let provider: Arc<dyn fluxdb_core::GenerationProvider> = provider.clone();
    let audit = AllowAudit;
    let resolver = Arc::new(fluxdb_app::MapSqlResolver::new([]));
    let tools = standard_sql_catalog(resolver, false).expect("只读 SQL 工具目录");
    fluxdb_app::run_ai_conversation_turn(
        &storage,
        &job,
        &CancelFlag::new(),
        &sender,
        provider.as_ref(),
        &tools,
        &audit,
        TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            policy: KernelPolicy::default(),
            plugin_digest: None,
        },
    );
    drop(sender);
    let mut text = String::new();
    let mut session_id = job.session_id.clone();
    while let Ok(update) = receiver.recv() {
        session_id = update.job.session_id.clone();
        for message in &update.view.messages {
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
