//! 会话模型选择：生产命令/发送/审批恢复 + 独立临时 SQLite + 离线 Provider。
use std::{fs, path::PathBuf, sync::Arc};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationCommand, AiConversationJob, AiConversationView,
    AiModelSaveInput, AppCommand, AppController, AppEvent, AuditInput, AuditOutcome, KernelPolicy,
    RecordingTool, ScriptedGenerationProvider, SecurityAuditHook, TargetRef, ToolCatalog, ToolSpec,
    TurnPermissionContext, TurnTargetPin, run_ai_conversation_resume_approval,
    run_ai_conversation_turn,
};
use fluxdb_core::{
    AiFuture, AiTool, CancelFlag, FinishReason, ModelCapabilities, ModelEvent, ToolCallRequest,
    ToolDescriptor, ToolExecutionContext, ToolReceipt, ToolRisk,
};
use fluxdb_storage::{
    FileStorage,
    ai_platform::{agents, audits, model_selection, models, sessions, settings},
};

struct Fixture {
    dir: PathBuf,
    storage: FileStorage,
}

impl Fixture {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("fluxdb-model-select-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let storage = FileStorage::new(&dir);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        Self { dir, storage }
    }

    fn controller(&self) -> AppController {
        let mut controller = AppController::new();
        controller.set_ai_storage(FileStorage::new(&self.dir));
        load(&mut controller, None);
        controller
    }

    fn configured(&self) -> (AppController, String, String) {
        let mut controller = self.controller();
        let agent_model = create_model(&mut controller, "agent", "openai", true);
        let selected_model = create_model(&mut controller, "selected", "openai", true);
        let agent_id = match controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            display_name: "selection-fixture".into(),
            system_prompt: "selection-fixture-system".into(),
            model_profile_id: Some(agent_model.clone()),
            ..Default::default()
        })) {
            AppEvent::AiAgentSaved { id, .. } => id,
            other => panic!("save agent: {other:?}"),
        };
        // 固定 fixture 的 Agent，避免测试依赖内置助手/默认选择的兜底顺序。
        self.storage
            .with_ai_connection(|conn| {
                let mut runtime = settings::load_runtime_settings(conn)?;
                let revision = runtime.revision;
                runtime.default_agent_id = Some(agent_id);
                settings::save_runtime_settings(conn, 1, &runtime, revision)?;
                Ok(())
            })
            .unwrap();
        (controller, agent_model, selected_model)
    }

    fn selection(&self, session: Option<&str>) -> (Option<String>, i64) {
        self.storage
            .with_ai_connection(|conn| model_selection::load(conn, session))
            .unwrap()
    }

    fn snapshot_model(&self, job: &AiConversationJob) -> String {
        self.storage
            .with_ai_connection(|conn| {
                let turn = sessions::load_turn(conn, &job.turn_id)?;
                let snapshot: serde_json::Value =
                    serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
                Ok(snapshot["model_profile_id"].as_str().unwrap().to_owned())
            })
            .unwrap()
    }

    fn session_count(&self) -> usize {
        self.storage
            .with_ai_connection(|conn| Ok(sessions::list_sessions(conn, 100)?.len()))
            .unwrap()
    }

    fn set_status(&self, job: &AiConversationJob, status: sessions::TurnStatus) {
        self.storage
            .with_ai_connection(|conn| {
                let turn = sessions::load_turn(conn, &job.turn_id)?;
                sessions::update_turn_status(conn, 1, &job.turn_id, status, None, turn.revision)?;
                Ok(())
            })
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn create_model(
    controller: &mut AppController,
    name: &str,
    provider: &str,
    enabled: bool,
) -> String {
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: provider.into(),
        model: format!("offline-{name}"),
        endpoint: Some("http://127.0.0.1:9/v1".into()),
        enabled,
        context_window_tokens: Some(32_768),
        max_output_tokens: Some(1_024),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("save model: {other:?}"),
    }
}

fn edit_model(
    fixture: &Fixture,
    controller: &mut AppController,
    id: &str,
    provider: &str,
    enabled: bool,
) {
    let old = fixture
        .storage
        .with_ai_connection(|conn| models::load_model(conn, id))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            id: Some(id.into()),
            expected_revision: Some(old.revision),
            display_name: old.display_name,
            provider: provider.into(),
            model: old.model,
            endpoint: old.endpoint,
            enabled,
            context_window_tokens: old.context_window_tokens,
            max_output_tokens: old.max_output_tokens,
            ..Default::default()
        })),
        AppEvent::AiModelSaved { .. }
    ));
}

fn command(controller: &mut AppController, command: AiConversationCommand) -> AiConversationView {
    match controller.dispatch(AppCommand::AiConversation(command)) {
        AppEvent::AiConversationLoaded(view) => {
            assert_eq!(&controller.state().ai_conversation, &view);
            view
        }
        other => panic!("conversation command: {other:?}"),
    }
}

fn load(controller: &mut AppController, session: Option<&str>) -> AiConversationView {
    command(
        controller,
        AiConversationCommand::Load {
            session_id: session.map(str::to_owned),
        },
    )
}

fn select(controller: &mut AppController, id: Option<&str>) -> AiConversationView {
    let expected_revision = controller.state().ai_conversation.model_selection_revision;
    command(
        controller,
        AiConversationCommand::SelectModel {
            model_id: id.map(str::to_owned),
            expected_revision,
        },
    )
}

fn assert_selection_rejected(controller: &mut AppController, id: Option<&str>, revision: i64) {
    let before = controller.state().ai_conversation.clone();
    let event = controller.dispatch(AppCommand::AiConversation(
        AiConversationCommand::SelectModel {
            model_id: id.map(str::to_owned),
            expected_revision: revision,
        },
    ));
    assert!(
        matches!(event, AppEvent::AiSettingsError(ref message) if !message.is_empty()),
        "{event:?}"
    );
    assert_eq!(&controller.state().ai_conversation, &before);
}

fn target() -> TurnTargetPin {
    let target = TargetRef {
        connection_id: Some(1),
        database: Some("fixture".into()),
        schema: None,
        object: None,
        redis_db: None,
    };
    TurnTargetPin {
        target_json: target.to_json(),
        target,
        captured_tab_id: None,
        captured_tab_revision: 0,
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

struct Audit(bool);

impl SecurityAuditHook for Audit {
    fn audit(&self, _: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(if self.0 {
            AuditOutcome::RequireApproval
        } else {
            AuditOutcome::Allow
        })
    }
}

fn run(
    fixture: &Fixture,
    job: &AiConversationJob,
    provider: &ScriptedGenerationProvider,
    tools: &ToolCatalog,
    approval: bool,
) -> AiConversationView {
    let (tx, rx) = std::sync::mpsc::sync_channel(32);
    run_ai_conversation_turn(
        &fixture.storage,
        job,
        &CancelFlag::new(),
        &tx,
        provider,
        tools,
        &Audit(approval),
        permission(),
    );
    drop(tx);
    rx.try_iter()
        .filter(|update| update.terminal)
        .last()
        .expect("terminal update")
        .view
        .expect("终态回包必须携带视图")
}

fn finish(fixture: &Fixture, controller: &mut AppController, job: &AiConversationJob, model: &str) {
    let provider = ScriptedGenerationProvider::text_only("offline-completed");
    let view = run(
        fixture,
        job,
        &provider,
        &ToolCatalog::new(Vec::new()),
        false,
    );
    assert!(view.error.is_none(), "{:?}", view.error);
    // 首轮成功后会用同一 Provider 发独立的 title 请求；第 0 次才是会话生成请求。
    assert_eq!(provider.call_count(), 2);
    assert_eq!(provider.request_at(0).unwrap().model, model);
    let title_request = provider.request_at(1).unwrap();
    assert_eq!(title_request.model, model);
    assert!(title_request.messages[0].text.contains("生成一个不超过"));
    assert_eq!(fixture.snapshot_model(job), model);
    assert_eq!(
        fixture
            .storage
            .with_ai_connection(|conn| Ok(sessions::load_turn(conn, &job.turn_id)?.status))
            .unwrap(),
        sessions::TurnStatus::Completed
    );
    command(controller, AiConversationCommand::Apply { view });
}

#[test]
fn draft_selection_transfers_to_session_and_matches_send_snapshot_and_provider() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    let view = select(&mut controller, Some(&selected));
    assert_eq!(view.selected_model_id.as_deref(), Some(selected.as_str()));
    assert_eq!(view.model_selection_revision, 1);
    assert_eq!(
        fixture.session_count(),
        0,
        "selection must not create history"
    );
    assert_eq!(fixture.selection(None), (Some(selected.clone()), 1));
    let job = controller
        .prepare_ai_conversation("selected model request", &target())
        .unwrap();
    assert_eq!(job.model_profile_id, selected);
    assert_eq!(fixture.selection(None), (None, 0));
    assert_eq!(
        fixture.selection(Some(&job.session_id)),
        (Some(selected.clone()), 1)
    );
    assert_eq!(
        controller
            .state()
            .ai_conversation
            .selected_model_id
            .as_deref(),
        Some(selected.as_str())
    );
    finish(&fixture, &mut controller, &job, &selected);
}

#[test]
fn draft_and_session_selection_survive_reopen_without_requests_or_empty_history() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    select(&mut controller, Some(&selected));
    drop(controller);
    let mut reopened = fixture.controller();
    assert_eq!(
        reopened
            .state()
            .ai_conversation
            .selected_model_id
            .as_deref(),
        Some(selected.as_str())
    );
    assert_eq!(fixture.session_count(), 0);
    let job = reopened
        .prepare_ai_conversation("persist selection", &target())
        .unwrap();
    finish(&fixture, &mut reopened, &job, &selected);
    drop(reopened);
    let mut reopened = fixture.controller();
    assert_eq!(
        reopened.state().ai_conversation.session_id.as_deref(),
        Some(job.session_id.as_str())
    );
    assert_eq!(
        reopened
            .state()
            .ai_conversation
            .selected_model_id
            .as_deref(),
        Some(selected.as_str())
    );
    assert_eq!(reopened.state().ai_conversation.model_selection_revision, 1);
    let next = reopened
        .prepare_ai_conversation("next request", &target())
        .unwrap();
    assert_eq!(next.session_id, job.session_id);
    assert_eq!(next.model_profile_id, selected);
    finish(&fixture, &mut reopened, &next, &selected);
}

#[test]
fn new_clears_draft_selection_but_preserves_existing_session_selection() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    select(&mut controller, Some(&selected));
    let view = command(&mut controller, AiConversationCommand::New);
    assert_eq!(
        (view.selected_model_id, view.model_selection_revision),
        (None, 0)
    );
    assert_eq!(fixture.selection(None), (None, 0));
    assert_eq!(fixture.session_count(), 0);
    select(&mut controller, Some(&selected));
    let job = controller
        .prepare_ai_conversation("existing history", &target())
        .unwrap();
    finish(&fixture, &mut controller, &job, &selected);
    command(&mut controller, AiConversationCommand::New);
    select(&mut controller, Some(&selected));
    command(&mut controller, AiConversationCommand::New);
    assert_eq!(fixture.selection(None), (None, 0));
    assert_eq!(
        fixture.selection(Some(&job.session_id)),
        (Some(selected.clone()), 1)
    );
    let restored = load(&mut controller, Some(&job.session_id));
    assert_eq!(
        restored.selected_model_id.as_deref(),
        Some(selected.as_str())
    );
    assert_eq!(fixture.session_count(), 1);
}

#[test]
fn clear_session_override_falls_back_to_agent_not_global_and_keeps_old_snapshot() {
    let fixture = Fixture::new();
    let (mut controller, agent, selected) = fixture.configured();
    let global = create_model(&mut controller, "global", "openai", true);
    let revision = fixture
        .storage
        .with_ai_connection(|conn| Ok(settings::load_runtime_settings(conn)?.revision))
        .unwrap();
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(global),
            default_decision_model_id: None,
            expected_revision: revision,
        }),
        AppEvent::AiDefaultModelsSaved { .. }
    ));
    select(&mut controller, Some(&selected));
    let first = controller
        .prepare_ai_conversation("explicit wins", &target())
        .unwrap();
    finish(&fixture, &mut controller, &first, &selected);
    let cleared = select(&mut controller, None);
    assert_eq!(
        (cleared.selected_model_id, cleared.model_selection_revision),
        (None, 2)
    );
    assert_eq!(fixture.selection(Some(&first.session_id)), (None, 2));
    let second = controller
        .prepare_ai_conversation("agent fallback", &target())
        .unwrap();
    assert_eq!(second.model_profile_id, agent);
    assert_eq!(fixture.snapshot_model(&first), selected);
    finish(&fixture, &mut controller, &second, &agent);
}

#[test]
fn failed_first_send_rolls_back_draft_transfer_and_history() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    select(&mut controller, Some(&selected));
    // 故障发生在事务内的会话创建/草稿转移之后，验证 transfer 不是部分提交。
    fixture.storage.with_ai_connection(|conn| {
        conn.execute_batch("CREATE TRIGGER fail_fixture_turn BEFORE INSERT ON ai_turns BEGIN SELECT RAISE(ABORT,'fixture turn failure'); END;").unwrap();
        Ok(())
    }).unwrap();
    let before = controller.state().ai_conversation.clone();
    assert!(
        controller
            .prepare_ai_conversation("failure injection", &target())
            .is_err()
    );
    assert_eq!(&controller.state().ai_conversation, &before);
    assert_eq!(fixture.selection(None), (Some(selected.clone()), 1));
    assert_eq!(fixture.session_count(), 0);
    let reopened = fixture.controller();
    assert_eq!(
        reopened
            .state()
            .ai_conversation
            .selected_model_id
            .as_deref(),
        Some(selected.as_str())
    );
}

#[test]
fn invalid_disabled_and_decision_candidates_never_change_selection_or_revision() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    let disabled = create_model(&mut controller, "disabled", "openai", false);
    let decision = create_model(&mut controller, "decision", "decisions", true);
    select(&mut controller, Some(&selected));
    for id in ["missing-model", disabled.as_str(), decision.as_str()] {
        assert_selection_rejected(&mut controller, Some(id), 1);
        assert_eq!(fixture.selection(None), (Some(selected.clone()), 1));
        assert_eq!(fixture.session_count(), 0);
    }
    let job = controller
        .prepare_ai_conversation("unchanged model", &target())
        .unwrap();
    finish(&fixture, &mut controller, &job, &selected);
    for id in ["missing-model", disabled.as_str(), decision.as_str()] {
        assert_selection_rejected(&mut controller, Some(id), 1);
        assert_eq!(
            fixture.selection(Some(&job.session_id)),
            (Some(selected.clone()), 1)
        );
        assert_eq!(fixture.snapshot_model(&job), selected);
    }
}

#[test]
fn selected_model_becoming_unavailable_fails_send_without_fallback_or_transfer() {
    for established_session in [false, true] {
        for (provider, enabled) in [("openai", false), ("decisions", true)] {
            let fixture = Fixture::new();
            let (mut controller, _, selected) = fixture.configured();
            select(&mut controller, Some(&selected));
            let original = established_session.then(|| {
                let job = controller
                    .prepare_ai_conversation("before disable", &target())
                    .unwrap();
                finish(&fixture, &mut controller, &job, &selected);
                job
            });
            let scope = original.as_ref().map(|job| job.session_id.as_str());
            edit_model(&fixture, &mut controller, &selected, provider, enabled);
            assert!(
                controller
                    .prepare_ai_conversation("must not fallback", &target())
                    .is_err()
            );
            assert_eq!(fixture.selection(scope), (Some(selected.clone()), 1));
            assert_eq!(fixture.session_count(), usize::from(established_session));
            if let Some(job) = &original {
                assert_eq!(fixture.snapshot_model(job), selected);
                assert_eq!(
                    fixture
                        .storage
                        .with_ai_connection(|conn| Ok(
                            sessions::list_turns(conn, &job.session_id)?.len()
                        ))
                        .unwrap(),
                    1
                );
            }
            let mut reopened = fixture.controller();
            assert_eq!(
                reopened
                    .state()
                    .ai_conversation
                    .selected_model_id
                    .as_deref(),
                Some(selected.as_str())
            );
            assert!(
                reopened
                    .prepare_ai_conversation("still unavailable", &target())
                    .is_err()
            );
        }
    }
}

#[test]
fn explicit_selection_can_send_without_agent_or_global_default() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let selected = create_model(&mut controller, "only-explicit", "openai", true);
    select(&mut controller, Some(&selected));
    let job = controller
        .prepare_ai_conversation("no defaults required", &target())
        .unwrap();
    assert_eq!(job.model_profile_id, selected);
    finish(&fixture, &mut controller, &job, &selected);
}

#[test]
fn selection_cas_rejects_stale_controllers_for_draft_and_session() {
    let fixture = Fixture::new();
    let (mut first, agent, selected) = fixture.configured();
    let mut stale = fixture.controller();
    select(&mut first, Some(&selected));
    assert_selection_rejected(&mut stale, Some(&agent), 0);
    assert_eq!(fixture.selection(None), (Some(selected.clone()), 1));
    load(&mut stale, None);
    select(&mut stale, Some(&agent));
    assert_selection_rejected(&mut first, None, 1);
    load(&mut first, None);
    let job = first
        .prepare_ai_conversation("session cas", &target())
        .unwrap();
    finish(&fixture, &mut first, &job, &agent);
    load(&mut stale, Some(&job.session_id));
    select(&mut first, Some(&selected));
    assert_selection_rejected(&mut stale, None, 2);
    assert_eq!(
        fixture.selection(Some(&job.session_id)),
        (Some(selected.clone()), 3)
    );
    load(&mut stale, Some(&job.session_id));
    let next = stale
        .prepare_ai_conversation("winner preserved", &target())
        .unwrap();
    finish(&fixture, &mut stale, &next, &selected);
}

#[test]
fn active_and_unknown_turns_block_both_switch_and_clear_without_changing_snapshot() {
    for status in [
        sessions::TurnStatus::Queued,
        sessions::TurnStatus::Preparing,
        sessions::TurnStatus::Generating,
        sessions::TurnStatus::ExecutingTools,
        sessions::TurnStatus::WaitingApproval,
        sessions::TurnStatus::WaitingUser,
        sessions::TurnStatus::CancelRequested,
        sessions::TurnStatus::OutcomeUnknown,
    ] {
        let fixture = Fixture::new();
        let (mut controller, agent, selected) = fixture.configured();
        select(&mut controller, Some(&selected));
        let job = controller
            .prepare_ai_conversation("locked selection", &target())
            .unwrap();
        fixture.set_status(&job, status);
        load(&mut controller, Some(&job.session_id));
        assert_selection_rejected(&mut controller, Some(&agent), 1);
        assert_selection_rejected(&mut controller, None, 1);
        assert_eq!(
            fixture.selection(Some(&job.session_id)),
            (Some(selected.clone()), 1),
            "{status:?}"
        );
        assert_eq!(fixture.snapshot_model(&job), selected);
    }
}

struct SharedTool(Arc<RecordingTool>);

impl AiTool for SharedTool {
    fn descriptor(&self) -> &ToolDescriptor {
        self.0.descriptor()
    }
    fn execute(
        &self,
        context: ToolExecutionContext,
        arguments: String,
    ) -> AiFuture<'_, ToolReceipt> {
        self.0.execute(context, arguments)
    }
}

fn catalog(tool: &Arc<RecordingTool>) -> ToolCatalog {
    let mut catalog = ToolCatalog::new(vec![ToolSpec::from_descriptor(tool.descriptor()).unwrap()]);
    catalog
        .register(Box::new(SharedTool(tool.clone())))
        .unwrap();
    catalog
}

fn pause(
    fixture: &Fixture,
    controller: &mut AppController,
    tool: &Arc<RecordingTool>,
) -> (AiConversationJob, audits::ApprovalView) {
    let job = controller
        .prepare_ai_conversation("approval model pin", &target())
        .unwrap();
    let provider = ScriptedGenerationProvider::new(
        ModelCapabilities {
            streaming: true,
            tool_calling: true,
            reasoning_summary: false,
        },
        vec![vec![
            ModelEvent::ToolCallRequested(ToolCallRequest {
                provider_call_id: "selection-call".into(),
                tool_id: tool.descriptor().id.clone(),
                arguments_json: "{}".into(),
            }),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ]],
    );
    let view = run(fixture, &job, &provider, &catalog(tool), true);
    assert_eq!(tool.call_count(), 0);
    assert_eq!(provider.request_at(0).unwrap().model, job.model_profile_id);
    command(controller, AiConversationCommand::Apply { view });
    let approval = fixture
        .storage
        .with_ai_connection(|conn| {
            let calls = sessions::list_tool_calls_for_turn(conn, &job.turn_id)?;
            assert_eq!(calls.len(), 1);
            assert_eq!(
                sessions::load_turn(conn, &job.turn_id)?.status,
                sessions::TurnStatus::WaitingApproval
            );
            Ok(audits::list_approvals_for_call(conn, &calls[0].id)?.remove(0))
        })
        .unwrap();
    (job, approval)
}

fn approve(controller: &mut AppController, approval: &audits::ApprovalView) {
    command(
        controller,
        AiConversationCommand::DecideApproval {
            approval_id: approval.id.clone(),
            approved: true,
            expected_revision: approval.revision,
        },
    );
}

#[test]
fn approval_resume_preserves_selected_turn_model_when_agent_default_changes() {
    let fixture = Fixture::new();
    let (mut controller, agent_model, selected) = fixture.configured();
    select(&mut controller, Some(&selected));
    let tool = Arc::new(RecordingTool::new("fixture.read", ToolRisk::Read));
    let (job, approval) = pause(&fixture, &mut controller, &tool);
    assert_selection_rejected(&mut controller, Some(&agent_model), 1);
    let agent = fixture
        .storage
        .with_ai_connection(|conn| {
            let session = sessions::load_session(conn, &job.session_id)?;
            agents::load_agent(conn, &session.agent_id)
        })
        .unwrap();
    let new_default = create_model(&mut controller, "new-agent-default", "openai", true);
    assert!(matches!(
        controller.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput {
            id: Some(agent.id),
            expected_revision: agent.revision,
            display_name: agent.display_name,
            description: agent.description,
            system_prompt: agent.system_prompt,
            model_profile_id: Some(new_default),
            allowed_tools: agent.allowed_tools,
            enabled: agent.enabled,
            compression_trigger_percent: agent.compression_trigger_percent,
            max_iterations: agent.max_iterations,
        })),
        AppEvent::AiAgentSaved { .. }
    ));
    drop(controller);
    let mut reopened = fixture.controller();
    command(&mut reopened, AiConversationCommand::RecoverOnStartup);
    load(&mut reopened, Some(&job.session_id));
    let approval = fixture
        .storage
        .with_ai_connection(|conn| audits::load_approval(conn, &approval.id))
        .unwrap();
    approve(&mut reopened, &approval);
    let resumed = reopened.prepare_ai_resume_job(&approval.id).unwrap();
    assert_eq!(resumed.model_profile_id, selected);
    assert_eq!(resumed.turn_id, job.turn_id);
    assert_eq!(resumed.session_id, job.session_id);
    assert_eq!(resumed.assistant_message_id, job.assistant_message_id);
    let provider = ScriptedGenerationProvider::text_only("approved result");
    let (tx, rx) = std::sync::mpsc::sync_channel(32);
    run_ai_conversation_resume_approval(
        &fixture.storage,
        &resumed,
        &CancelFlag::new(),
        &tx,
        &provider,
        &catalog(&tool),
        &Audit(true),
        permission(),
        &approval.id,
    );
    drop(tx);
    let view = rx
        .try_iter()
        .filter(|u| u.terminal)
        .last()
        .unwrap()
        .view
        .expect("终态回包必须携带视图");
    assert!(view.error.is_none(), "{:?}", view.error);
    assert_eq!(tool.call_count(), 1);
    assert_eq!(provider.call_count(), 2);
    assert_eq!(provider.request_at(0).unwrap().model, selected);
    assert_eq!(provider.request_at(1).unwrap().model, selected);
    assert_eq!(fixture.snapshot_model(&job), selected);
}

#[test]
fn approval_resume_rejects_original_model_changed_to_decision_without_fallback() {
    let fixture = Fixture::new();
    let (mut controller, _, selected) = fixture.configured();
    select(&mut controller, Some(&selected));
    let tool = Arc::new(RecordingTool::new("fixture.read", ToolRisk::Read));
    let (job, approval) = pause(&fixture, &mut controller, &tool);
    edit_model(&fixture, &mut controller, &selected, "decisions", true);
    approve(&mut controller, &approval);
    let before = fixture
        .storage
        .with_ai_connection(|conn| sessions::session_snapshot(conn, &job.session_id))
        .unwrap();
    let result = controller.prepare_ai_resume_job(&approval.id);
    assert!(
        result.is_err(),
        "decision model must not become generation on resume: {result:?}"
    );
    let after = fixture
        .storage
        .with_ai_connection(|conn| sessions::session_snapshot(conn, &job.session_id))
        .unwrap();
    assert_eq!(before.session, after.session);
    assert_eq!(before.turns, after.turns);
    assert_eq!(before.messages, after.messages);
    assert_eq!(before.parts, after.parts);
    assert_eq!(before.run_events, after.run_events);
    assert_eq!(before.ui_state, after.ui_state);
    assert_eq!(fixture.snapshot_model(&job), selected);
    assert_eq!(
        fixture.selection(Some(&job.session_id)),
        (Some(selected), 1)
    );
    assert_eq!(tool.call_count(), 0);
}

mod file_references {
    include!("parts/file_references.rs");
}
