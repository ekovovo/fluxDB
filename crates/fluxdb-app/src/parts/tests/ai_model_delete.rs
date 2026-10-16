#[test]
fn ai_model_delete_removes_profile_and_encrypted_key() {
    let (mut controller, dir) = controller_with_ai_storage("delete");
    let id = create_model(&mut controller, "openai", "待删除", Some("delete-test-key"));
    assert!(matches!(
        controller.dispatch(AppCommand::DeleteAiModel {
            id: id.clone(),
            expected_revision: 1
        }),
        AppEvent::AiModelDeleted { .. }
    ));
    assert!(controller.state().ai_models.is_empty());
    let conn = fluxdb_storage::FileStorage::new(dir.clone())
        .open_ai_connection()
        .unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM ai_model_profiles WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert!(matches!(
        controller.dispatch(AppCommand::DeleteAiModel {
            id,
            expected_revision: 1
        }),
        AppEvent::AiSettingsError(_)
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ai_model_delete_stale_revision_leaves_key_and_state_unchanged() {
    let (mut controller, dir) = controller_with_ai_storage("delete-stale");
    let id = create_model(&mut controller, "openai", "保留", Some("delete-test-key"));
    assert!(matches!(
        controller.dispatch(AppCommand::DeleteAiModel {
            id: id.clone(),
            expected_revision: 2
        }),
        AppEvent::AiSettingsError(_)
    ));
    assert_eq!(controller.state().ai_models.len(), 1);
    assert_eq!(
        controller.reveal_ai_model_secret(&id).unwrap(),
        "delete-test-key"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ai_model_delete_protects_defaults_agents_and_bindings() {
    for reference in ["main", "decision", "agent", "binding"] {
        let (mut controller, dir) = controller_with_ai_storage(reference);
        let provider = if reference == "decision" {
            "decisions"
        } else {
            "openai"
        };
        let id = create_model(&mut controller, provider, "被引用", Some("delete-test-key"));
        let conn = fluxdb_storage::FileStorage::new(dir.clone())
            .open_ai_connection()
            .unwrap();
        if reference == "main" {
            controller.dispatch(AppCommand::SetAiDefaultModel {
                model_id: Some(id.clone()),
                expected_revision: controller.state().ai_runtime_revision,
            });
        } else if reference == "decision" {
            // 决策槽位只由 Jev 插件管理弹框写入，这里直接落该行以构造"被引用"前置状态。
            conn.execute(
                "INSERT INTO ai_runtime_settings (id, default_decision_model_profile_id, revision, updated_at_ms)
                 VALUES (1, ?1, 1, 1)
                 ON CONFLICT(id) DO UPDATE SET default_decision_model_profile_id=excluded.default_decision_model_profile_id",
                [&id],
            )
            .unwrap();
        } else {
            let agent = create_agent(&mut controller, "引用 Agent", "测试");
            if reference == "agent" {
                conn.execute(
                    "UPDATE ai_agents SET default_model_profile_id=?1 WHERE id=?2",
                    [&id, &agent],
                )
                .unwrap();
            } else {
                conn.execute("INSERT INTO ai_agent_model_bindings(id,agent_id,model_profile_id,role,created_at_ms,updated_at_ms) VALUES('binding',?1,?2,'main_candidate',1,1)", [&agent, &id]).unwrap();
            }
        }
        let AppEvent::AiSettingsError(message) = controller.dispatch(AppCommand::DeleteAiModel {
            id: id.clone(),
            expected_revision: 1,
        }) else {
            panic!("被引用模型不应删除: {reference}")
        };
        assert!(message.contains("引用"));
        assert_eq!(controller.state().ai_models.len(), 1);
        assert_eq!(
            controller.reveal_ai_model_secret(&id).unwrap(),
            "delete-test-key"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn ai_model_delete_completion_only_removes_target_and_preserves_other_state() {
    let (mut controller, dir) = controller_with_ai_storage("delete-complete");
    let id = create_model(&mut controller, "openai", "待删除", None);
    let keep = create_model(&mut controller, "openai", "并发新增", None);
    controller.dispatch(AppCommand::CompleteAiModelDelete {
        id: id.clone(),
        result: Err("被引用".into()),
    });
    assert_eq!(controller.state().ai_models.len(), 2);
    controller.dispatch(AppCommand::CompleteAiModelDelete { id, result: Ok(()) });
    assert_eq!(controller.state().ai_models[0].id, keep);
    let _ = std::fs::remove_dir_all(dir);
}
