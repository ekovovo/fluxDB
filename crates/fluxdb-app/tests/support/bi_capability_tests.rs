// 插件状态改变的是产物能力，不是通用工具的存在性。
fn set_bi_enabled(fixture: &Fixture, enabled: bool) {
    fixture
        .storage
        .with_ai_connection(|conn| {
            let instance = plugins::list_plugin_instances(conn)?
                .into_iter()
                .find(|instance| instance.id == "builtin.bi")
                .unwrap();
            plugins::set_plugin_instance_enabled(conn, 1, &instance.id, enabled, instance.revision)
        })
        .unwrap();
}

struct DisableBiAfterCreate {
    inner: ReceiptDrivenProvider,
    agent_binding: bool,
    disabled: Mutex<bool>,
}

impl GenerationProvider for DisableBiAfterCreate {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream(&self, request: ModelRequest, cancel: CancelFlag) -> AiFuture<'_, Vec<ModelEvent>> {
        Box::pin(async move {
            if request.messages.iter().any(|message| {
                message.role == ModelRole::Tool && message.tool_call_id.as_deref() == Some("create")
            }) {
                let mut disabled = self.disabled.lock().unwrap();
                if !*disabled {
                    self.inner.storage.with_ai_connection(|conn| {
                        if self.agent_binding {
                            let session = sessions::load_session(conn, &self.inner.job.session_id)?;
                            plugins::bind_plugin_system(conn, 1, "bi-live-binding", &session.agent_id, "builtin.bi")?;
                            conn.execute("UPDATE ai_agent_plugin_bindings SET enabled=0 WHERE id='bi-live-binding'", []).unwrap();
                        } else {
                            let instance = plugins::list_plugin_instances(conn)?.into_iter()
                                .find(|instance| instance.id == "builtin.bi").unwrap();
                            plugins::set_plugin_instance_enabled(conn, 1, &instance.id, false, instance.revision)?;
                        }
                        Ok(())
                    }).unwrap();
                    *disabled = true;
                }
            }
            self.inner.stream(request, cancel).await
        })
    }
}

#[test]
fn disabling_bi_after_create_rejects_add_and_finalize_without_changing_draft() {
    for agent_binding in [false, true] {
        let fixture = Fixture::new();
        let job = fixture.job();
        let (mut catalog, results) = fixture.catalog(&job);
        add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
        let provider = DisableBiAfterCreate {
            inner: ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY)),
            agent_binding,
            disabled: Mutex::new(false),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(128);
        run_ai_conversation_turn(
            &fixture.storage,
            &job,
            &CancelFlag::new(),
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
        let view = rx
            .try_iter()
            .filter(|update| update.terminal)
            .filter_map(|update| update.view)
            .last()
            .unwrap();
        assert!(view.error.is_none(), "{:?}", view.error);
        assert!(provider.inner.steps.lock().unwrap().is_empty());
        let created = fixture.receipt(&job, "create", "succeeded");
        for call in ["add", "finalize"] {
            let receipt = fixture.receipt(&job, call, "failed");
            assert_eq!(receipt["code"], "artifact_capability_disabled");
            assert!(receipt["file"].is_null());
        }
        let artifact = fixture.artifacts(&job).remove(0);
        assert_eq!(artifact.status, "draft");
        assert_eq!(artifact.revision, created["revision"].as_i64().unwrap());
        let spec: ArtifactSpec = serde_json::from_str(&artifact.spec_json).unwrap();
        assert_eq!(spec.blocks.len(), 1);
        assert!(artifact.relative_path.is_none());
        provider.inner.assert_sql_unchanged();
    }
}

#[test]
fn reenabled_bi_can_use_already_registered_tools() {
    let fixture = Fixture::new();
    let job = fixture.job();
    set_bi_enabled(&fixture, false);
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    assert!(
        !catalog
            .describe("artifact.create")
            .unwrap()
            .description
            .contains(REPORT)
    );
    set_bi_enabled(&fixture, true);
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    let view = run(&fixture, &job, &catalog, &provider);
    assert_ready(&fixture, &job, &view);
    provider.assert_sql_unchanged();
    let (mut refreshed, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut refreshed, &fixture.storage, &job, results).unwrap();
    assert!(
        refreshed
            .describe("artifact.create")
            .unwrap()
            .description
            .contains(REPORT)
    );
}

#[test]
fn disabling_bi_preserves_ready_report_view_and_export() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    let before = fluxdb_app::load_ai_report(&fixture.storage, &artifact.id).unwrap();
    let export =
        fluxdb_app::render_ai_report_export(&before, fluxdb_app::AiReportExportFormat::Html)
            .unwrap();
    set_bi_enabled(&fixture, false);
    let after = fluxdb_app::load_ai_report(&fixture.storage, &artifact.id).unwrap();
    assert_eq!(after.status, "ready");
    assert!(!after.metadata_only());
    assert_eq!(before.markdown, after.markdown);
    assert_eq!(
        export,
        fluxdb_app::render_ai_report_export(&after, fluxdb_app::AiReportExportFormat::Html)
            .unwrap()
    );
    assert_eq!(fixture.artifacts(&job).remove(0), artifact);
    provider.assert_sql_unchanged();
}
