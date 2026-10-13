//! 工具失败提示回归：失败的工具卡写在发生位置；只有"没有再成功过"的失败才在回合末尾
//! 兜底一条聚合提示，已自愈的失败不重复出现。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiModelSaveInput, AppCommand, AppController, AppEvent, AuditInput,
    AuditOutcome, KernelPolicy, SecurityAuditHook, TargetRef, ToolCatalog, TurnPermissionContext,
    TurnTargetPin, run_ai_conversation_turn,
};
use fluxdb_core::{
    AiFuture, AiTool, CancelFlag, FinishReason, ModelCapabilities, ModelEvent, ToolCallRequest,
    ToolDescriptor, ToolExecutionContext, ToolReceipt, ToolReceiptStatus, ToolRisk,
};
use fluxdb_storage::FileStorage;

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-tool-notice-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn configured(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        let model = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "失败提示离线模型".into(),
            provider: "openai".into(),
            model: "tool-notice-offline".into(),
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
            display_name: "失败提示 Agent".into(),
            description: "独立 fixture".into(),
            system_prompt: "工具失败提示测试".into(),
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

fn permission() -> TurnPermissionContext {
    TurnPermissionContext {
        agent_allowed_tools: Vec::new(),
        disabled_tools: Default::default(),
        policy: KernelPolicy::default(),
        plugin_digest: None,
    }
}

/// 前 `fail_calls` 次返回受控失败，之后成功；用于区分"自愈"与"未自愈"。
struct FlakyTool {
    descriptor: ToolDescriptor,
    fail_calls: usize,
    calls: std::sync::atomic::AtomicUsize,
}

impl FlakyTool {
    fn new(id: &str, fail_calls: usize) -> Self {
        Self {
            descriptor: ToolDescriptor {
                id: id.to_string(),
                version: 1,
                description: format!("flaky tool {id}"),
                risk: ToolRisk::Read,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            },
            fail_calls,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl AiTool for FlakyTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: ToolExecutionContext,
        _arguments_json: String,
    ) -> AiFuture<'_, ToolReceipt> {
        Box::pin(async move {
            let attempt = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let failed = attempt < self.fail_calls;
            Ok(ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status: if failed {
                    ToolReceiptStatus::Failed
                } else {
                    ToolReceiptStatus::Succeeded
                },
                result_json: if failed {
                    r#"{"ok":false,"code":"non_numeric_field","message":"指标不是数值列"}"#
                        .to_string()
                } else {
                    r#"{"ok":true}"#.to_string()
                },
                error_code: failed.then(|| "non_numeric_field".to_string()),
                affected_rows: None,
                started_at_ms: 0,
                finished_at_ms: 1,
            })
        })
    }
}

fn tool_call_stop(call_id: &str, tool: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallRequested(ToolCallRequest {
            provider_call_id: call_id.into(),
            tool_id: tool.into(),
            arguments_json: "{}".into(),
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

fn text_stop(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::Finished(FinishReason::Stop),
    ]
}

/// 跑一轮并返回终态视图里的全部片段 `(id, kind, status, payload)`。
fn run_turn(
    fixture: &Fixture,
    controller: &mut AppController,
    step: &str,
    tool: Arc<FlakyTool>,
    provider: fluxdb_app::ScriptedGenerationProvider,
) -> Vec<(String, String, String, String)> {
    let job = controller
        .prepare_ai_conversation(step, &target())
        .expect("准备任务");
    let mut catalog = ToolCatalog::new(Vec::new());
    catalog
        .add_tool(Box::new(SharedTool(Arc::clone(&tool))))
        .expect("注册工具");
    let cancel = CancelFlag::new();
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    run_ai_conversation_turn(
        &fixture.storage,
        &job,
        &cancel,
        &tx,
        &provider,
        &catalog,
        &AllowAudit,
        permission(),
    );
    let mut last = None;
    while let Ok(update) = rx.try_recv() {
        if update.terminal {
            last = update.view;
        }
    }
    let view = last.expect("终态回包必须携带视图");
    view.messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .map(|part| {
            (
                part.id.clone(),
                part.kind.clone(),
                part.status.clone(),
                part.payload_json.clone(),
            )
        })
        .collect()
}

/// 让 `Arc<FlakyTool>` 作为可注册实现；测试仍持有 `Arc` 观察真实调用次数。
struct SharedTool(Arc<FlakyTool>);

impl AiTool for SharedTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.0.descriptor()
    }

    fn execute(
        &self,
        ctx: ToolExecutionContext,
        arguments_json: String,
    ) -> AiFuture<'_, ToolReceipt> {
        self.0.execute(ctx, arguments_json)
    }
}

fn scripted(steps: Vec<Vec<ModelEvent>>) -> fluxdb_app::ScriptedGenerationProvider {
    fluxdb_app::ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        steps,
    )
}

fn notice_parts(
    parts: &[(String, String, String, String)],
) -> Vec<&(String, String, String, String)> {
    parts
        .iter()
        .filter(|(id, _, _, _)| id.ends_with("-notice-tool-failure"))
        .collect()
}

#[test]
fn recovered_tool_failure_is_not_repeated_at_turn_end() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = Arc::new(FlakyTool::new("artifact.create", 1));
    let provider = scripted(vec![
        tool_call_stop("call-1", "artifact.create"),
        tool_call_stop("call-2", "artifact.create"),
        text_stop("已按修正后的结果重建产物"),
    ]);

    let parts = run_turn(
        &fixture,
        &mut controller,
        "重建产物",
        Arc::clone(&tool),
        provider,
    );

    assert_eq!(tool.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        failed_result_part(&parts).is_some(),
        "失败的工具结果卡必须留在发生位置"
    );
    assert!(
        notice_parts(&parts).is_empty(),
        "同工具后续已成功，回合末尾不应重复失败提示：{parts:?}"
    );
}

#[test]
fn unrecovered_tool_failure_is_reported_once_with_code() {
    let fixture = Fixture::new();
    let mut controller = fixture.configured();
    let tool = Arc::new(FlakyTool::new("artifact.create", 1));
    let provider = scripted(vec![
        tool_call_stop("call-1", "artifact.create"),
        text_stop("无法完成，已说明原因"),
    ]);

    let parts = run_turn(
        &fixture,
        &mut controller,
        "重建产物",
        Arc::clone(&tool),
        provider,
    );

    let notices = notice_parts(&parts);
    assert_eq!(notices.len(), 1, "未自愈的失败应聚合为一条：{parts:?}");
    let payload: serde_json::Value = serde_json::from_str(&notices[0].3).expect("提示载荷");
    assert_eq!(
        payload["text"].as_str(),
        Some("工具 artifact.create 未成功：non_numeric_field")
    );
    let failed = failed_result_part(&parts).expect("失败的工具结果卡必须留在发生位置");
    let failed_payload: serde_json::Value =
        serde_json::from_str(&failed.3).expect("工具结果卡载荷");
    assert_eq!(
        failed_payload["error_code"].as_str(),
        Some("non_numeric_field"),
        "内联失败行要带受控错误码"
    );
}

/// 失败回执落成 `tool_result_ref` 的 error 片段（`tool_call_ref` 只记录调用开始）。
fn failed_result_part<'a>(
    parts: &'a [(String, String, String, String)],
) -> Option<&'a (String, String, String, String)> {
    parts
        .iter()
        .find(|(_, kind, status, _)| kind == "tool_result_ref" && status == "error")
}
