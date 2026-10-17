use super::*;
use fluxdb_core::AiTool as _;
use std::sync::atomic::{AtomicUsize, Ordering};

struct PolicyFixture {
    root: std::path::PathBuf,
    storage: fluxdb_storage::FileStorage,
}
impl PolicyFixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("fluxdb-execution-policy-{}", uuid::Uuid::new_v4()));
        let storage = fluxdb_storage::FileStorage::new(&root);
        storage.with_ai_connection(|_| Ok(())).unwrap();
        storage
            .save_connections(&[sqlite_config(&root.join("policy.sqlite"))])
            .unwrap();
        Self { root, storage }
    }
}
impl Drop for PolicyFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 固定评分器：记录是否被问到，并回放固定分数/不可用。
struct FakeReviewer {
    outcome: WriteReviewOutcome,
    calls: Arc<AtomicUsize>,
}
impl WriteRiskReviewer for FakeReviewer {
    fn review(&self, _: &WriteRiskFacts, _: &CancelFlag) -> WriteReviewOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome.clone()
    }
}

fn write_input() -> AuthorizeInput {
    AuthorizeInput {
        tool_id: "db.execute".into(),
        tool_version: 1,
        risk: fluxdb_core::ToolRisk::Write,
        target_json: "conn:1".into(),
        normalized_arguments_json: "{}".into(),
        plugin_digest: None,
        globally_disabled_tools: Default::default(),
        agent_allowed_tools: vec![],
        policy: KernelPolicy {
            read_allowed: true,
            write_allowed: true,
            admin_allowed: false,
        },
        audit: Some(AuditOutcome::Allow),
    }
}

/// 只有软信号的事实：形状确定但不确定度可被评审模型背书。
fn soft_facts(score: f64) -> WriteRiskFacts {
    let mut facts = WriteRiskFacts::new(r#"{"operation":"update"}"#);
    facts.soft("determined_shape", score, "形状确定");
    facts
}

/// 带硬信号的事实：模型无权下穿硬下界。
fn hard_facts(soft: f64, hard: f64) -> WriteRiskFacts {
    let mut facts = WriteRiskFacts::new(r#"{"operation":"update"}"#);
    facts.soft("determined_shape", soft, "形状确定");
    facts.hard("no_predicate", hard, "无谓词的整表更新");
    facts
}

fn security_with(fixture: &PolicyFixture, outcome: WriteReviewOutcome) -> (ExecutionSecurity, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let reviewer = Arc::new(FakeReviewer {
        outcome,
        calls: calls.clone(),
    });
    let security = ExecutionSecurity {
        storage: fixture.storage.clone(),
        connections: fixture.storage.load_connections().unwrap_or_default(),
        revalidate_from_storage: true,
        reviewer,
    };
    (security, calls)
}

#[test]
fn four_modes_preserve_hard_denials_and_manual_unknowns() {
    let fixture = PolicyFixture::new();
    for mode in ExecutionPolicy::ALL {
        let (security, calls) = security_with(
            &fixture,
            WriteReviewOutcome::Scored(ModelRisk::new(0.05, "mock")),
        );
        let result = security
            .authorize(&write_input(), mode, || Ok(soft_facts(0.10)), &CancelFlag::new())
            .unwrap();
        match mode {
            ExecutionPolicy::ReadOnly => {
                assert!(matches!(result.decision, PermissionDecision::Deny { .. }))
            }
            ExecutionPolicy::WriteApproval => assert!(matches!(
                result.decision,
                PermissionDecision::RequireApproval { .. }
            )),
            // 低风险 + 模型背书 → 自动放行；自动执行本来就不评审。
            _ => assert!(matches!(result.decision, PermissionDecision::Allow { .. })),
        }
        // 「写审批」与「只读」不评审；「自动执行」也不问模型（语义是不逐次确认）。
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(mode == ExecutionPolicy::AiApproval)
        );
        // 阈值以上的形状：即使模型给极低分、即使策略是自动执行之外的任何模式，都不放行。
        let high = security
            .authorize(
                &write_input(),
                ExecutionPolicy::AiApproval,
                || Ok(soft_facts(0.80)),
                &CancelFlag::new(),
            )
            .unwrap();
        assert!(!high.decision.is_allow());
    }
}

#[test]
fn audit_and_connection_boundaries_never_reach_the_reviewer() {
    let fixture = PolicyFixture::new();
    let (security, calls) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.0, "mock")),
    );
    for audit in [
        None,
        Some(AuditOutcome::RequireApproval),
        Some(AuditOutcome::Deny),
    ] {
        let mut input = write_input();
        input.audit = audit;
        let result = security
            .authorize(
                &input,
                ExecutionPolicy::AiApproval,
                || panic!("审计未放行不得请求评审"),
                &CancelFlag::new(),
            )
            .unwrap();
        assert!(!result.decision.is_allow());
    }
    let mut input = write_input();
    input.policy.write_allowed = false;
    assert!(matches!(
        security
            .authorize(
                &input,
                ExecutionPolicy::AiApproval,
                || panic!("只读连接不得请求评审"),
                &CancelFlag::new()
            )
            .unwrap()
            .decision,
        PermissionDecision::Deny { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unscoreable_write_stays_manual_instead_of_guessing() {
    let fixture = PolicyFixture::new();
    let (security, calls) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.0, "mock")),
    );
    let result = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Err(Error::new(ErrorKind::Unsupported, "无法核实影响范围")),
            &CancelFlag::new(),
        )
        .unwrap();
    assert!(
        matches!(result.decision, PermissionDecision::RequireApproval { .. }),
        "拿不到证据时必须转人工"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn reviewer_approval_cannot_push_risk_below_the_hard_floor() {
    let fixture = PolicyFixture::new();
    // 形状分 0.45、硬信号 0.60：模型给 0.0 也不放行。
    let (security, calls) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.0, "mock")),
    );
    let result = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(hard_facts(0.45, 0.60)),
            &CancelFlag::new(),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "模型应被问到");
    assert!(
        !result.decision.is_allow(),
        "评审模型不能下穿确定性边界"
    );
    let risk = result.risk.expect("写入应有合成结果");
    assert!((risk.hard_floor - 0.60).abs() < 1e-9);
    assert!(risk.final_score >= 0.60);
}

#[test]
fn reviewer_can_lower_soft_uncertainty_below_the_threshold() {
    let fixture = PolicyFixture::new();
    let (security, _) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.10, "mock")),
    );
    // 软信号 0.40 高于默认阈值 0.35；权重 0.5 → 0.5*0.40+0.5*0.10 = 0.25 ≤ 0.35。
    let result = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(soft_facts(0.40)),
            &CancelFlag::new(),
        )
        .unwrap();
    assert!(result.decision.is_allow(), "软不确定度应能被评审背书降下来");
    assert!(result.automatic_approval);
}

#[test]
fn unavailable_reviewer_falls_back_to_rule_score_only() {
    let fixture = PolicyFixture::new();
    let (security, calls) = security_with(
        &fixture,
        WriteReviewOutcome::Unavailable {
            reason: "no_model".into(),
        },
    );
    // 规则分 0.10 本身在阈值下：没有模型也放行。
    let allow = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(soft_facts(0.10)),
            &CancelFlag::new(),
        )
        .unwrap();
    assert!(allow.decision.is_allow());
    // 规则分 0.40 在阈值上：没有模型时停在人工，不把"没有评审"当作低风险。
    let manual = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(soft_facts(0.40)),
            &CancelFlag::new(),
        )
        .unwrap();
    assert!(matches!(
        manual.decision,
        PermissionDecision::RequireApproval { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn bounded_commit_is_used_only_when_the_connector_declares_it() {
    let fixture = PolicyFixture::new();
    let (security, _) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.0, "mock")),
    );
    let mut facts = soft_facts(0.10);
    facts.bounded_capable = true;
    let result = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(facts),
            &CancelFlag::new(),
        )
        .unwrap();
    assert_eq!(result.write_commit, WriteCommit::Bounded);

    let plain = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(soft_facts(0.10)),
            &CancelFlag::new(),
        )
        .unwrap();
    assert_eq!(plain.write_commit, WriteCommit::Ordinary);
}

#[test]
fn disabled_tools_are_reloaded_at_each_boundary() {
    let fixture = PolicyFixture::new();
    let (security, calls) = security_with(
        &fixture,
        WriteReviewOutcome::Scored(ModelRisk::new(0.0, "mock")),
    );
    fixture
        .storage
        .with_ai_connection(|conn| {
            tool_settings::save_tool_setting(conn, 1, "db.execute", false, 0).map(|_| ())
        })
        .unwrap();
    assert!(matches!(
        security
            .authorize(
                &write_input(),
                ExecutionPolicy::Automatic,
                || panic!("已关闭工具不得请求评审"),
                &CancelFlag::new()
            )
            .unwrap()
            .decision,
        PermissionDecision::Deny { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn sqlite_config(path: &std::path::Path) -> fluxdb_core::ConnectionConfig {
    fluxdb_core::ConnectionConfig {
        id: ConnectionId(1),
        name: "隔离策略库".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: path.into(),
            read_only: false,
        },
        options: Default::default(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}
#[test]
fn sqlite_auto_approval_commits_only_a_reverified_bounded_update() {
    let fixture = PolicyFixture::new();
    let path = fixture.root.join("business.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,value TEXT);INSERT INTO items VALUES(1,'old'),(2,'old');").unwrap();
    let config = sqlite_config(&path);
    let resolver = Arc::new(MapSqlResolver::new([config]));
    let tool = AiSqlExecuteTool::new(resolver).unwrap();
    let args = r#"{"target":"conn:1","sql":"UPDATE items SET value='new' WHERE id=1"}"#;
    let facts = tool.write_risk_facts(args).unwrap();
    assert!(facts.bounded_capable, "SQLite 声明了有界提交");
    assert_eq!(tool.descriptor().risk, fluxdb_core::ToolRisk::Write);
    // 结构事实不含对象名与业务值。
    assert!(!facts.evidence_json.contains("items"));
    assert!(!facts.evidence_json.contains("new"));
    // 唯一键等值 + 有界提交：规则分在默认阈值下，没有模型也能自动放行。
    assert!(facts.rule_score() <= fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD);
    let ctx = fluxdb_core::ToolExecutionContext {
        automatic_approval: true,
        // 有界提交路径：执行时必须在事务内重新复核影响。
        bounded_write: true,
        session_id: "s".into(),
        turn_id: "t".into(),
        iteration: 1,
        call_id: "c".into(),
        cancel: CancelFlag::new(),
        progress: None,
    };
    let receipt = block_on_ready(tool.execute(ctx.clone(), args.into()))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, fluxdb_core::ToolReceiptStatus::Succeeded);
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT value FROM items WHERE id=1", [], |r| r.get(0))
            .unwrap(),
        "new"
    );
    // 检查后增加触发器：执行时必须重新核实，不能复用刚才的证据。
    conn.execute_batch("CREATE TRIGGER unsafe AFTER UPDATE ON items BEGIN UPDATE items SET value='bad' WHERE id=2; END;").unwrap();
    let result = block_on_ready(tool.execute(ctx, args.replace("'new'", "'unapproved'")))
        .unwrap()
        .unwrap();
    assert_eq!(result.status, fluxdb_core::ToolReceiptStatus::Failed);
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT value FROM items WHERE id=2", [], |r| r.get(0))
            .unwrap(),
        "old"
    );
    // 形状评分：无谓词整表更新、表达式赋值、无谓词删除都必须落在硬信号上，
    // 有界提交也不再是"自动放行"的理由——执行时会重新核实并拒绝。
    for sql in [
        "UPDATE items SET value='x'",
        "UPDATE items SET value=lower(value) WHERE id=1",
    ] {
        let facts = tool
            .write_risk_facts(&serde_json::json!({"target":"conn:1","sql":sql}).to_string())
            .unwrap();
        assert!(facts.rule_score() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, "{sql}");
        assert!(
            facts.codes().iter().any(|code| {
                facts
                    .signals
                    .iter()
                    .any(|signal| signal.code == *code && signal.class == fluxdb_core::WriteRiskClass::Hard)
            }),
            "无谓词/表达式赋值必须是硬信号：{sql}"
        );
    }
    // DELETE 不走有界提交（该能力只覆盖 UPDATE），即使连接声明了能力也按不确定处理。
    let delete_facts = tool
        .write_risk_facts(&serde_json::json!({"target":"conn:1","sql":"DELETE FROM items WHERE id=1"}).to_string())
        .unwrap();
    assert!(!delete_facts.bounded_capable);
    assert!(delete_facts.rule_score() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD);
}

fn mcp_service(fixture: &PolicyFixture, allows_write: bool) -> (McpExecutionService, McpCallIdentity) {
    use fluxdb_storage::ai_platform::{mcp_server::*, mcp_server_runtime::*};
    let config = sqlite_config(&fixture.root.join("mcp-business.sqlite"));
    rusqlite::Connection::open(match &config.endpoint {
        Endpoint::SqliteFile { path, .. } => path,
        _ => unreachable!(),
    })
    .unwrap()
    .execute_batch(
        "CREATE TABLE items(id INTEGER PRIMARY KEY,value TEXT);INSERT INTO items VALUES(1,'old');",
    )
    .unwrap();
    fixture.storage.save_connections(&[config.clone()]).unwrap();
    let (profile, datasources) = fixture
        .storage
        .with_ai_connection(|conn| {
            let edit = ServerProfileEdit {
                id: "p".into(),
                display_name: "p".into(),
                enabled: true,
                options_json: "{}".into(),
            };
            let created = save_server_profile(conn, 1, &edit, None)?;
            let profile = save_server_profile(conn, 2, &edit, Some(created.revision))?;
            let policy = if allows_write {
                DatasourceWritePolicy::ReadWrite
            } else {
                DatasourceWritePolicy::ReadOnly
            };
            save_datasource(conn, 1, "p", 1, policy, None)?;
            let datasources = list_datasources(conn, "p")?;
            start_server_session(conn, 1, "s", "p", "2025-06-18", "{}")?;
            start_server_request(
                conn,
                1,
                "r",
                "s",
                "p",
                "\"1\"",
                "fluxdb_execute",
                1,
                Some(1),
                "{}",
                "{}",
                "prepared",
            )?;
            Ok((profile, datasources))
        })
        .unwrap();
    // 全局默认只读；数据源自身策略已在 `save_datasource` 上写入并解析。
    let service = McpExecutionService::new(
        vec![config],
        profile,
        datasources,
        fluxdb_core::McpWritePolicy::ReadOnly,
        std::collections::BTreeSet::new(),
        Arc::new(Mutex::new(ResultStore::new(SQL_RESULT_MAX_BYTES))),
    )
    .unwrap()
    .with_execution_security(fixture.storage.clone());
    (
        service,
        McpCallIdentity {
            session_id: "s".into(),
            request_id: "r".into(),
        },
    )
}

#[test]
fn mcp_datasource_read_write_policy_hides_writes_when_readonly() {
    let fixture = PolicyFixture::new();
    // 数据源只读：写工具不列出，直接调用被拒，数据库不变。
    let (service, identity) = mcp_service(&fixture, false);
    assert!(
        !service
            .tool_views()
            .unwrap()
            .iter()
            .any(|t| t.name == "fluxdb_execute")
    );
    let args = r#"{"target":"conn:1/db:main","sql":"UPDATE items SET value='changed' WHERE id=1"}"#;
    let outcome = service.call(&identity, "fluxdb_execute", args, &CancelFlag::new());
    assert!(!outcome.ok, "只读数据源上的写必须被拒绝");
    let conn = rusqlite::Connection::open(fixture.root.join("mcp-business.sqlite")).unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT value FROM items", [], |r| r.get(0))
            .unwrap(),
        "old"
    );
}

#[test]
fn mcp_datasource_read_write_policy_executes_write_when_allowed() {
    let fixture = PolicyFixture::new();
    let (service, identity) = mcp_service(&fixture, true);
    assert!(
        service
            .tool_views()
            .unwrap()
            .iter()
            .any(|t| t.name == "fluxdb_execute"),
        "允许读写的数据源应列出写工具"
    );
    let args = r#"{"target":"conn:1/db:main","sql":"UPDATE items SET value='changed' WHERE id=1"}"#;
    let outcome = service.call(&identity, "fluxdb_execute", args, &CancelFlag::new());
    assert!(outcome.ok, "允许读写的写应成功: {:?}", outcome.error);
    let conn = rusqlite::Connection::open(fixture.root.join("mcp-business.sqlite")).unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>("SELECT value FROM items", [], |r| r.get(0))
            .unwrap(),
        "changed"
    );
}


/// 一个审批 Mock：记录是否真的被问到，并回放固定结论。
struct ReviewMock {
    endpoint: String,
    hit: Arc<AtomicUsize>,
    handle: std::thread::JoinHandle<()>,
}

impl ReviewMock {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn assert_asked_and_join(self) {
        self.handle.join().unwrap();
        assert_eq!(
            self.hit.load(Ordering::SeqCst),
            1,
            "审批请求没有发到这个模型"
        );
    }
}

/// 起一个只回放固定审批结论的 OpenAI 兼容 Mock。
///
/// `expected_hits` 是 5 秒窗口内期望收到的请求数：为 1 时收不到即判失败（证明请求确实
/// 发到了这个模型），为 0 时收不到才是正确结果（证明不该被问到）。线程内同时校验
/// "只有一个 system 消息"和"证据里没有业务值"，审批请求的出站边界由这里守住。
fn spawn_review_mock(output: &str, expected_hits: usize) -> ReviewMock {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let output = output.to_string();
    let hit = Arc::new(AtomicUsize::new(0));
    let handle = {
        let hit = hit.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut served = 0usize;
            while served < expected_hits {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "审批模型收到的请求数不符预期：期望 {expected_hits}，实际 {served}"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("Mock 监听失败：{error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let header_end = loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                assert_eq!(body["messages"].as_array().unwrap().len(), 1);
                assert!(!body.to_string().contains("business-secret"));
                let delta = serde_json::json!({"choices":[{"index":0,"delta":{"content":output},"finish_reason":null}]});
                let end =
                    serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                let response =
                    format!("data: {delta}\n\ndata: {end}\n\ndata: [DONE]\n\n");
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
                served += 1;
                hit.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    ReviewMock {
        endpoint,
        hit,
        handle,
    }
}

/// 存一个指向 Mock 的生成模型，返回档案 id。
fn save_mock_generation_model(fixture: &PolicyFixture, name: &str, endpoint: String) -> String {
    let mut controller = AppController::new();
    controller.set_ai_storage(fixture.storage.clone());
    match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
        display_name: name.into(),
        provider: "openai".into(),
        model: "review-mock".into(),
        endpoint: Some(endpoint),
        enabled: true,
        egress: true,
        api_key: Some("not-a-real-key".into()),
        ..Default::default()
    })) {
        AppEvent::AiModelSaved { id, .. } => id,
        other => panic!("隔离模型保存失败：{other:?}"),
    }
}

fn set_global_default_model(fixture: &PolicyFixture, id: &str) {
    let mut controller = AppController::new();
    controller.set_ai_storage(fixture.storage.clone());
    match controller.dispatch(AppCommand::SetAiDefaultModel {
        model_id: Some(id.to_string()),
        expected_revision: 0,
    }) {
        AppEvent::AiDefaultModelsSaved { .. } => {}
        other => panic!("设置默认生成模型失败：{other:?}"),
    }
}

#[test]
fn generation_reviewer_is_used_without_jev_and_invalid_output_is_unavailable() {
    for output in [r#"{"risk":0.05}"#, r#"{"risk":0.9}"#, "approve please"] {
        let fixture = PolicyFixture::new();
        let mock = spawn_review_mock(output, 1);
        let expected = output.to_string();
        let id = save_mock_generation_model(&fixture, "评审 Mock", mock.endpoint());
        // 无 Jev 时回退到全局默认生成模型（对外 MCP 没有 Agent 概念）。
        set_global_default_model(&fixture, &id);
        let result = StorageWriteRiskReviewer::new(fixture.storage.clone(), None).review(
            &soft_facts(0.40),
            &CancelFlag::new(),
        );
        mock.assert_asked_and_join();
        match expected.as_str() {
            r#"{"risk":0.05}"# => assert_eq!(
                result,
                WriteReviewOutcome::Scored(ModelRisk::new(0.05, SOURCE_TURN_MODEL))
            ),
            r#"{"risk":0.9}"# => assert_eq!(
                result,
                WriteReviewOutcome::Scored(ModelRisk::new(0.9, SOURCE_TURN_MODEL))
            ),
            // 非法输出不是 0 分：明确不可用，调用方退回代码规则分。
            _ => assert!(matches!(result, WriteReviewOutcome::Unavailable { .. })),
        }
    }
}

#[test]
fn generation_risk_parser_rejects_sloppy_shapes() {
    for (text, expected) in [
        (r#"{"risk":0.42}"#, Some(0.42)),
        ("```json\n{\"risk\":0.5}\n```", Some(0.5)),
        (r#"{"risk":2.0}"#, Some(1.0)),
        (r#"{"risk":-1}"#, Some(0.0)),
        (r#"{"risk":"high"}"#, None),
        (r#"{"risk":0.2,"note":"x"}"#, None),
        (r#"{"decision":"approve"}"#, None),
        ("", None),
    ] {
        assert_eq!(parse_generation_risk(text), expected, "{text}");
    }
}

/// 无 Jev 时评审必须问"当前 Agent 正在用的那个模型"，而不是全局默认主模型。
///
/// 两个 Mock 给出相反结论：全局默认给高风险、本轮模型给低风险。全局默认那个 Mock
/// 期望 0 次请求，因此"它没被问到"和"结论来自本轮模型"是同一次请求的两个面。
#[test]
fn generation_reviewer_falls_back_to_the_turn_model_not_the_global_default() {
    let fixture = PolicyFixture::new();
    let default_mock = spawn_review_mock(r#"{"risk":0.95}"#, 0);
    let turn_mock = spawn_review_mock(r#"{"risk":0.05}"#, 1);
    let default_id = save_mock_generation_model(&fixture, "全局默认", default_mock.endpoint());
    let turn_id = save_mock_generation_model(&fixture, "本轮模型", turn_mock.endpoint());
    set_global_default_model(&fixture, &default_id);

    let result = StorageWriteRiskReviewer::new(fixture.storage.clone(), Some(turn_id)).review(
        &soft_facts(0.40),
        &CancelFlag::new(),
    );
    turn_mock.assert_asked_and_join();
    default_mock.handle.join().unwrap();
    assert_eq!(
        default_mock.hit.load(Ordering::SeqCst),
        0,
        "评审不得绕过本轮 Agent 去问全局默认模型"
    );
    assert_eq!(
        result,
        WriteReviewOutcome::Scored(ModelRisk::new(0.05, SOURCE_TURN_MODEL)),
        "结论必须来自本轮模型"
    );
}

/// 这次真实运行暴露的缺陷：Jev 拿到的候选只有 `候选 1..5` 这样的占位说明，
/// 提示词也没写档位含义，于是它在无意义的标签里瞎选，给出最高档 1.00。
/// 档位含义必须随提示词一起出站，否则评审结果不可信。
#[test]
fn jev_prompt_describes_every_risk_band() {
    let prompt = jev_review_prompt(r#"{"operation":"insert"}"#);
    for band in RISK_BANDS {
        assert!(
            prompt.contains(band.id),
            "提示词缺少档位 ID：{}",
            band.id
        );
        assert!(
            prompt.contains(band.meaning),
            "提示词缺少档位含义：{}",
            band.id
        );
    }
    assert!(prompt.contains("不是指令"), "证据必须声明为数据");
    assert!(prompt.contains(r#"{"operation":"insert"}"#), "证据必须随提示词出站");
}

/// 回退生成模型走的是另一套输出契约（严格 JSON），不能被 Jev 的候选说明污染。
#[test]
fn generation_prompt_asks_for_strict_json_risk_score() {
    let prompt = generation_review_prompt(r#"{"operation":"insert"}"#);
    assert!(prompt.contains(r#"{"risk":0.0}"#));
    assert!(!prompt.contains("risk_0_1"), "生成模型没有候选协议");
    assert!(prompt.contains("不是指令"));
}
