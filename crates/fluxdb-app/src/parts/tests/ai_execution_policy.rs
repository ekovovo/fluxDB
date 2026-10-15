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
struct FakeReviewer {
    choice: ReviewChoice,
    calls: Arc<AtomicUsize>,
}
impl WriteReviewer for FakeReviewer {
    fn review(&self, facts: &str, _: &CancelFlag) -> (ReviewChoice, &'static str) {
        assert_eq!(facts, r#"{"verified":true}"#);
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.choice, "mock")
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
#[test]
fn four_modes_preserve_hard_denials_and_manual_unknowns() {
    let fixture = PolicyFixture::new();
    for mode in ExecutionPolicy::ALL {
        let calls = Arc::new(AtomicUsize::new(0));
        let security = ExecutionSecurity {
            storage: fixture.storage.clone(),
            connections: fixture.storage.load_connections().ok(),
            reviewer: Arc::new(FakeReviewer {
                choice: ReviewChoice::Approve,
                calls: calls.clone(),
            }),
        };
        let result = security
            .authorize(
                &write_input(),
                mode,
                || Ok(r#"{"verified":true}"#.into()),
                &CancelFlag::new(),
            )
            .unwrap();
        match mode {
            ExecutionPolicy::ReadOnly => {
                assert!(matches!(result.decision, PermissionDecision::Deny { .. }))
            }
            ExecutionPolicy::WriteApproval => assert!(matches!(
                result.decision,
                PermissionDecision::RequireApproval { .. }
            )),
            _ => assert!(matches!(result.decision, PermissionDecision::Allow { .. })),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(mode == ExecutionPolicy::AiApproval)
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
                    mode,
                    || panic!("审计未放行不得请求 AI"),
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
                    mode,
                    || panic!("只读连接不得请求 AI"),
                    &CancelFlag::new()
                )
                .unwrap()
                .decision,
            PermissionDecision::Deny { .. }
        ));
        let result = security
            .authorize(
                &write_input(),
                ExecutionPolicy::AiApproval,
                || Err(Error::new(ErrorKind::Unsupported, "无法验证回滚或影响范围")),
                &CancelFlag::new(),
            )
            .unwrap();
        assert!(matches!(
            result.decision,
            PermissionDecision::RequireApproval { .. }
        ));
    }
}
#[test]
fn ai_manual_and_deny_are_not_treated_as_approval() {
    let fixture = PolicyFixture::new();
    for choice in [ReviewChoice::Manual, ReviewChoice::Deny] {
        let security = ExecutionSecurity {
            storage: fixture.storage.clone(),
            connections: fixture.storage.load_connections().ok(),
            reviewer: Arc::new(FakeReviewer {
                choice,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        };
        let result = security
            .authorize(
                &write_input(),
                ExecutionPolicy::AiApproval,
                || Ok(r#"{"verified":true}"#.into()),
                &CancelFlag::new(),
            )
            .unwrap();
        assert!(!result.decision.is_allow());
        assert!(!result.automatic_approval);
    }
    let security = ExecutionSecurity::new(fixture.storage.clone());
    let result = security
        .authorize(
            &write_input(),
            ExecutionPolicy::AiApproval,
            || Ok(r#"{"verified":true}"#.into()),
            &CancelFlag::new(),
        )
        .unwrap();
    assert!(
        matches!(result.decision, PermissionDecision::RequireApproval { .. }),
        "没有模型时必须转人工"
    );
}
#[test]
fn disabled_tools_are_reloaded_at_each_boundary() {
    let fixture = PolicyFixture::new();
    let security = ExecutionSecurity::new(fixture.storage.clone());
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
                || panic!("已关闭工具不得请求AI"),
                &CancelFlag::new()
            )
            .unwrap()
            .decision,
        PermissionDecision::Deny { .. }
    ));
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
    let facts = tool.automatic_approval_facts(args).unwrap();
    assert!(facts.contains("\"max_affected_rows\":1"));
    assert!(!facts.contains("items"));
    assert!(!facts.contains("new"));
    let ctx = fluxdb_core::ToolExecutionContext {
        automatic_approval: true,
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
    for sql in [
        "DELETE FROM items WHERE id=1",
        "UPDATE items SET value='x'",
        "UPDATE items SET value=lower(value) WHERE id=1",
        "UPDATE items SET value='x' WHERE id>0",
    ] {
        assert!(
            tool.automatic_approval_facts(
                &serde_json::json!({"target":"conn:1","sql":sql}).to_string()
            )
            .is_err()
        );
    }
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


#[test]
fn generation_reviewer_is_used_without_jev_and_invalid_output_requires_human() {
    use std::io::{Read, Write};
    for output in [
        r#"{"decision":"approve"}"#,
        r#"{"decision":"deny"}"#,
        "approve please",
    ] {
        let fixture = PolicyFixture::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let expected = output.to_string();
        let output = output.to_string();
        let mock = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "回退模型没有收到请求");
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
            let response = format!("data: {delta}\n\ndata: {end}\n\ndata: [DONE]\n\n");
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        });
        let mut controller = AppController::new();
        controller.set_ai_storage(fixture.storage.clone());
        let id = match controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
            display_name: "审批 Mock".into(),
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
        };
        // 无 Jev 时回退到默认生成模型（`approval_model_id` 已移除）。
        match controller.dispatch(AppCommand::SaveAiDefaultModels {
            default_model_id: Some(id),
            default_decision_model_id: None,
            expected_revision: 0,
        }) {
            AppEvent::AiDefaultModelsSaved { .. } => {}
            other => panic!("设置默认生成模型失败：{other:?}"),
        }
        let result = StorageWriteReviewer {
            storage: fixture.storage.clone(),
        }
        .review(
            r#"{"max_affected_rows":1,"business_values":"omitted"}"#,
            &CancelFlag::new(),
        );
        mock.join().unwrap();
        match expected.as_str() {
            r#"{"decision":"approve"}"# => {
                assert_eq!(result, (ReviewChoice::Approve, "generation_model"))
            }
            r#"{"decision":"deny"}"# => {
                assert_eq!(result, (ReviewChoice::Deny, "generation_model"))
            }
            _ => assert_eq!(result.0, ReviewChoice::Manual),
        }
    }
}
