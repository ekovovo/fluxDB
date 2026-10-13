// 「会话历史」管理命令端到端：真实 AppCommand → 真实 SQLite。
// 覆盖 RefreshAiSettings 装载列表、重命名/归档/恢复的 CAS 与错误语义。

fn session_manage_storage(label: &str) -> (AppController, fluxdb_storage::FileStorage, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-ai-sessions-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = fluxdb_storage::FileStorage::new(dir.clone());
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    (controller, storage, dir)
}

fn seed_session(
    storage: &fluxdb_storage::FileStorage,
    session_id: &str,
    user_message: &str,
) -> String {
    let agent = storage
        .with_ai_connection(|conn| {
            Ok(fluxdb_storage::ai_platform::agents::list_agents(conn)?
                .into_iter()
                .next()
                .map(|agent| agent.id))
        })
        .unwrap()
        .expect("内置 Agent 必须存在");
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::conversation::send(
                conn,
                1_000,
                true,
                session_id,
                &agent,
                &format!("{session_id}-turn"),
                &format!("{session_id}-user"),
                user_message,
                "{}",
                "{}",
            )
        })
        .unwrap();
    agent
}

#[test]
fn refresh_lists_sessions_and_rename_archive_restore_round_trip() {
    let (mut controller, storage, dir) = session_manage_storage("list");
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    assert!(controller.state().ai_sessions.is_empty(), "新库无会话");

    let agent = seed_session(&storage, "session-a", "解释这个表");
    let event = controller.dispatch(AppCommand::RefreshAiSettings);
    assert!(matches!(event, AppEvent::AiSettingsLoaded));
    assert_eq!(controller.state().ai_sessions.len(), 1);
    let row = &controller.state().ai_sessions[0];
    assert_eq!(row.id, "session-a");
    assert_eq!(row.title, "解释这个表");
    assert_eq!(row.agent_id, agent);
    assert_eq!(row.status, "active");
    assert_eq!(row.message_count, 1);
    assert_eq!(row.turn_count, 1);
    assert_eq!(row.last_turn_status.as_deref(), Some("queued"));
    let revision = row.revision;

    // 重命名：对象级 CAS，回写列表；空标题被拒且不改库。
    let event = controller.dispatch(AppCommand::RenameAiSession {
        id: "session-a".into(),
        title: "  会话 A  ".into(),
        expected_revision: revision,
    });
    let AppEvent::AiSessionRenamed {
        title, revision: new_rev, ..
    } = event
    else {
        panic!("重命名应返回 AiSessionRenamed，实际: {event:?}");
    };
    assert_eq!(title, "会话 A");
    assert_eq!(new_rev, revision + 1);
    assert_eq!(controller.state().ai_sessions[0].title, "会话 A");

    let event = controller.dispatch(AppCommand::RenameAiSession {
        id: "session-a".into(),
        title: "   ".into(),
        expected_revision: new_rev,
    });
    assert!(
        matches!(event, AppEvent::AiSettingsError(_)),
        "空白标题必须被拒绝，实际: {event:?}"
    );

    // 过期 revision 被拒，列表不被静默改写。
    let event = controller.dispatch(AppCommand::RenameAiSession {
        id: "session-a".into(),
        title: "过期写入".into(),
        expected_revision: revision,
    });
    assert!(matches!(event, AppEvent::AiSettingsError(_)));
    assert_eq!(controller.state().ai_sessions[0].title, "会话 A");

    // 归档 → 恢复：状态切换但消息事实保留。
    let event = controller.dispatch(AppCommand::ArchiveAiSession {
        id: "session-a".into(),
        expected_revision: new_rev,
    });
    let AppEvent::AiSessionArchived {
        revision: archived_rev,
        ..
    } = event
    else {
        panic!("归档应返回 AiSessionArchived，实际: {event:?}");
    };
    assert_eq!(controller.state().ai_sessions[0].status, "archived");

    // 重复归档（已归档）必须失败，不能把 CAS 当成幂等写。
    let event = controller.dispatch(AppCommand::ArchiveAiSession {
        id: "session-a".into(),
        expected_revision: archived_rev,
    });
    assert!(matches!(event, AppEvent::AiSettingsError(_)));

    let event = controller.dispatch(AppCommand::RestoreAiSession {
        id: "session-a".into(),
        expected_revision: archived_rev,
    });
    let AppEvent::AiSessionRestored {
        revision: restored_rev,
        ..
    } = event
    else {
        panic!("恢复应返回 AiSessionRestored，实际: {event:?}");
    };
    assert_eq!(controller.state().ai_sessions[0].status, "active");
    assert!(restored_rev > archived_rev);

    let (messages, turns, status): (i64, i64, String) = storage
        .with_ai_connection(|conn| {
            let count = |table: &str| {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE session_id='session-a'"),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string())
                })
            };
            let status = conn
                .query_row(
                    "SELECT status FROM ai_sessions WHERE id='session-a'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string())
                })?;
            Ok((count("ai_messages")?, count("ai_turns")?, status))
        })
        .unwrap();
    assert_eq!((messages, turns), (1, 1), "重命名/归档/恢复不得丢消息或轮次");
    assert_eq!(status, "active");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_commands_without_ai_storage_report_error_instead_of_silent_noop() {
    let mut controller = AppController::new();
    let event = controller.dispatch(AppCommand::ArchiveAiSession {
        id: "missing".into(),
        expected_revision: 1,
    });
    assert!(matches!(event, AppEvent::AiSettingsError(_)));
}

#[test]
fn load_more_pages_session_history_without_duplicates_or_gaps() {
    let (mut controller, storage, dir) = session_manage_storage("paging");
    // 超过一页：第一页只加载 AI_SESSION_PAGE_SIZE 条，续读补齐剩余。
    let total = crate::AI_SESSION_PAGE_SIZE + 7;
    for index in 0..total {
        // 全部使用同一 updated_at，逼出“同时间按 id 升序”的稳定游标分支。
        seed_session(&storage, &format!("session-{index:03}"), "分页会话");
    }
    assert!(matches!(
        controller.dispatch(AppCommand::RefreshAiSettings),
        AppEvent::AiSettingsLoaded
    ));
    {
        let state = controller.state();
        assert_eq!(state.ai_sessions.len(), crate::AI_SESSION_PAGE_SIZE as usize);
        assert_eq!(state.ai_sessions_total, total);
        assert!(state.ai_sessions_has_more);
        assert!(!state.ai_sessions_loading_more);
    }

    let event = controller.dispatch(AppCommand::LoadMoreAiSessions);
    let AppEvent::AiSessionsAppended { appended, has_more } = event else {
        panic!("续读应返回 AiSessionsAppended，实际: {event:?}");
    };
    assert_eq!(appended, 7);
    assert!(!has_more);
    {
        let state = controller.state();
        assert_eq!(state.ai_sessions.len(), total as usize);
        let mut ids: Vec<_> = state
            .ai_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let loaded = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), loaded, "分页不得重复加载同一条会话");
        assert_eq!(ids.first().unwrap(), "session-000");
        assert_eq!(ids.last().unwrap(), &format!("session-{:03}", total - 1));
    }

    // 已到底后再续读是显式 no-op：不报错、不重复追加。
    let event = controller.dispatch(AppCommand::LoadMoreAiSessions);
    assert!(matches!(
        event,
        AppEvent::AiSessionsAppended {
            appended: 0,
            has_more: false
        }
    ));
    assert_eq!(controller.state().ai_sessions.len(), total as usize);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn purge_archived_session_deletes_facts_files_and_rejects_active() {
    let (mut controller, storage, dir) = session_manage_storage("purge");
    let _ = controller.dispatch(AppCommand::RefreshAiSettings);
    let agent = seed_session(&storage, "session-purge", "待删除会话");

    // 制造真实落点文件：spill 结果与产物快照。
    let results_dir = storage.root_dir().join("ai-results").join("results");
    std::fs::create_dir_all(&results_dir).unwrap();
    let result_file = results_dir.join("deadbeef-0000-0000-0000-000000000001.json");
    std::fs::write(&result_file, b"{\"rows\":[]}").unwrap();
    let artifacts_dir = storage.root_dir().join("ai-artifacts");
    std::fs::create_dir_all(&artifacts_dir).unwrap();
    let artifact_file = artifacts_dir.join("cafebabe-0000-0000-0000-000000000002.json");
    std::fs::write(&artifact_file, b"{\"markdown\":\"x\"}").unwrap();

    // 用真实表写入一条带落点的结果行与产物行（同 session/turn）。
    storage
        .with_ai_connection(|conn| {
            let turn_id = "session-purge-turn";
            let call_id = format!("{turn_id}-toolcall-1");
            fluxdb_storage::ai_platform::sessions::create_tool_call(
                conn, 2_000, &call_id, "session-purge", turn_id, 1, 0, "provider-purge",
                "db.query", 1, None, r#"{"sql":"SELECT 1"}"#, "{}", "plan-purge", "succeeded",
            )?;
            fluxdb_storage::ai_platform::sessions::insert_result(
                conn, 2_010, &format!("{turn_id}-result-1"), "session-purge", turn_id, &call_id,
                r#"{"tool":"db.query"}"#, "available", 9,
                &fluxdb_storage::ai_platform::sessions::ResultRetention::Retained {
                    relative_path: "results/deadbeef-0000-0000-0000-000000000001.json".into(),
                    byte_count: 9,
                    content_hash: "hash-r".into(),
                },
            )?;
            conn.execute(
                "INSERT INTO ai_artifacts(id,session_id,turn_id,kind,schema_version,title,renderer_id,renderer_version,spec_json,status,relative_path,byte_count,content_hash,revision,created_at_ms,updated_at_ms)
                 VALUES('artifact-purge','session-purge',?1,'report',1,'标题','r','1','{}','ready','cafebabe-0000-0000-0000-000000000002.json',10,'hash-a',1,2011,2011)",
                [turn_id],
            ).map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))?;
            Ok(())
        })
        .unwrap();

    // 未归档即删除：拒绝，行与文件保留。
    let event = controller.dispatch(AppCommand::PurgeAiSession {
        id: "session-purge".into(),
        expected_revision: 1,
    });
    assert!(matches!(event, AppEvent::AiSettingsError(_)), "未归档会话不得删除");
    assert!(result_file.exists() && artifact_file.exists());

    // 活跃 Turn 时即使已归档也拒绝删除：先造出「归档 + 活跃 Turn」再改回终态。
    // 把该会话的 Turn 置为终态（模拟任务已完成），使删除具备条件。
    storage
        .with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::sessions::update_turn_status(
                conn,
                2_020,
                "session-purge-turn",
                fluxdb_storage::ai_platform::sessions::TurnStatus::Completed,
                None,
                1,
            )
            .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))
        })
        .unwrap();

    // 归档后再删除：成功并清理依赖事实与受控文件。
    let event = controller.dispatch(AppCommand::ArchiveAiSession {
        id: "session-purge".into(),
        expected_revision: 1,
    });
    let AppEvent::AiSessionArchived { revision, .. } = event else {
        panic!("归档应成功: {event:?}");
    };
    let event = controller.dispatch(AppCommand::PurgeAiSession {
        id: "session-purge".into(),
        expected_revision: revision,
    });
    assert!(
        matches!(event, AppEvent::AiSessionPurged { .. }),
        "删除应成功: {event:?}"
    );
    assert!(
        !controller.state().ai_sessions.iter().any(|s| s.id == "session-purge"),
        "删除后列表不得再有该会话"
    );
    assert!(!result_file.exists(), "spill 结果文件必须删除");
    assert!(!artifact_file.exists(), "产物快照文件必须删除");
    let remaining = storage
        .with_ai_connection(|conn| {
            let count = |sql: &str| -> fluxdb_core::Result<i64> {
                conn.query_row(sql, [], |row| row.get(0))
                    .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, e.to_string()))
            };
            Ok((
                count("SELECT COUNT(*) FROM ai_sessions WHERE id='session-purge'")?,
                count("SELECT COUNT(*) FROM ai_tool_calls WHERE session_id='session-purge'")?,
                count("SELECT COUNT(*) FROM ai_results WHERE session_id='session-purge'")?,
                count("SELECT COUNT(*) FROM ai_artifacts WHERE session_id='session-purge'")?,
            ))
        })
        .unwrap();
    assert_eq!(remaining, (0, 0, 0, 0), "依赖事实必须一并删除");
    // 对照：其它会话信息（agent）不受影响。
    assert!(storage
        .with_ai_connection(|conn| Ok(fluxdb_storage::ai_platform::agents::load_agent(conn, &agent)?))
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}
