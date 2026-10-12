use super::*;
use crate::ai_platform::conversation::upsert_artifact_part;
use crate::ai_platform::{agents, ensure_schema, sessions, tool_calls};

fn setup() -> Connection {
    let conn = Connection::open_in_memory().expect("内存库");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("开启外键");
    ensure_schema(&conn).expect("建表");
    conn
}

/// 造出同任务的 session/turn/tool_call/result 行。
fn seed_task(conn: &Connection, session_id: &str, turn_id: &str, call_id: &str, result_id: &str) {
    sessions::create_session(conn, 1_000, session_id, agents::BUILTIN_AGENT_ID, "会话")
        .expect("建会话");
    sessions::start_turn(conn, 1_000, session_id, turn_id, "{}", "{}").expect("建 Turn");
    seed_result(conn, session_id, turn_id, call_id, result_id, 0);
}

fn seed_result(
    conn: &Connection,
    session_id: &str,
    turn_id: &str,
    call_id: &str,
    result_id: &str,
    ordinal: i64,
) {
    tool_calls::create_tool_call(
        conn,
        1_000,
        call_id,
        session_id,
        turn_id,
        1,
        ordinal,
        &format!("pc-{call_id}"),
        "db.query",
        1,
        None,
        "{}",
        "{}",
        "plan-hash",
        "succeeded",
    )
    .expect("建工具调用");
    tool_calls::insert_result(
        conn,
        1_000,
        result_id,
        session_id,
        turn_id,
        call_id,
        "{}",
        "available",
        0,
        &tool_calls::ResultRetention::Memory,
    )
    .expect("登记结果");
}

fn spec() -> fluxdb_core::ArtifactSpec {
    fluxdb_core::ArtifactSpec {
        schema_version: fluxdb_core::ARTIFACT_SCHEMA_VERSION,
        kind: fluxdb_core::ArtifactKind::Report,
        title: "销售额分析".into(),
        renderer: "fluxdb.bi.report".into(),
        metric: None,
        sources: vec![fluxdb_core::ArtifactSourceRef {
            result_ref: Some("result-1".into()),
            tool_call_ref: None,
        }],
        blocks: vec![fluxdb_core::ArtifactBlock::Note {
            text: "统计范围与数据限制".into(),
        }],
        ..Default::default()
    }
}

fn source(result_id: &str) -> ArtifactSourceInput {
    ArtifactSourceInput {
        declared_ref: Some("result-1".into()),
        result_id: Some(result_id.into()),
        tool_call_id: None,
    }
}

fn draft(conn: &Connection, id: &str, session: &str, turn: &str, result: &str) -> ArtifactView {
    create_artifact(
        conn,
        2_000,
        id,
        session,
        turn,
        "1.0.0",
        &spec(),
        &[source(result)],
    )
    .expect("创建测试草稿")
}

fn artifacts_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM ai_artifacts", [], |r| r.get(0))
        .expect("统计产物")
}

fn rows(conn: &Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let columns = stmt.column_count();
    stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

fn snapshot(conn: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "ai_artifacts",
        "ai_artifact_sources",
        "ai_results",
        "ai_tool_calls",
        "ai_messages",
        "ai_message_parts",
        "ai_sessions",
    ]
    .into_iter()
    .map(|table| rows(conn, &format!("SELECT * FROM {table} ORDER BY rowid")))
    .collect()
}

fn assert_rejected_sources(
    conn: &Connection,
    spec: &fluxdb_core::ArtifactSpec,
    sources: &[ArtifactSourceInput],
    expected_error: &str,
) {
    let before = snapshot(conn);
    let error = create_artifact(conn, 2_000, "rejected", "s1", "t1", "1", spec, sources)
        .expect_err("拒绝无效来源");
    assert!(error.message.contains(expected_error), "{}", error.message);
    assert_eq!(snapshot(conn), before);
}

#[test]
fn create_and_load_roundtrip() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    let view = draft(&conn, "art1", "s1", "t1", "r1");
    assert_eq!(view.kind, "report");
    assert_eq!(view.title, "销售额分析");
    assert_eq!(view.schema_version, 1);
    assert_eq!(view.renderer_id, "fluxdb.bi.report");
    assert_eq!(view.renderer_version, "1.0.0");
    assert_eq!(view.status, "draft");
    assert_eq!(view.byte_count, 0);
    assert_eq!(view.relative_path, None);
    assert_eq!(view.revision, 1);
    assert_eq!(
        serde_json::from_str::<fluxdb_core::ArtifactSpec>(&view.spec_json).expect("spec 可解析"),
        spec()
    );

    let sources = list_artifact_sources(&conn, "art1").expect("读来源");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].ordinal, 0);
    assert_eq!(sources[0].result_id.as_deref(), Some("r1"));
    // 只给了结果句柄时，产出结果的工具调用也要一并落库。
    assert_eq!(sources[0].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sources[0].metadata_json).unwrap(),
        serde_json::json!({"result_ref": "result-1"})
    );

    let listed = list_artifacts_for_turn(&conn, "t1").expect("按 Turn 列出");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "art1");
}

#[test]
fn source_from_other_task_is_rejected_without_residue() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    // 另一个会话/ Turn 的结果不属于本任务。
    seed_task(&conn, "s2", "t2", "c2", "r2");

    assert_rejected_sources(&conn, &spec(), &[source("r2")], "不属于本任务");
    assert_eq!(artifacts_count(&conn), 0, "失败后不得残留产物行");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_artifact_sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "失败后不得残留来源行");
}

#[test]
fn resolve_count_must_match_declared_sources() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    // 声明 1 个来源，但解析出 0 个。
    assert_rejected_sources(&conn, &spec(), &[], "来源");
    assert_eq!(artifacts_count(&conn), 0);

    // 声明 1 个来源，但解析出 2 个。
    assert_rejected_sources(&conn, &spec(), &[source("r1"), source("r1")], "数量不一致");
    assert_eq!(artifacts_count(&conn), 0);
}

#[test]
fn replace_rewrites_spec_and_sources_with_cas() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    // 同任务里再登记一个结果，用于验证来源全量重写。
    seed_result(&conn, "s1", "t1", "c2", "r2", 1);

    let created = draft(&conn, "art1", "s1", "t1", "r1");

    let mut next = spec();
    next.title = "新标题".into();
    next.sources = vec![
        fluxdb_core::ArtifactSourceRef {
            result_ref: Some("result-1".into()),
            tool_call_ref: None,
        },
        fluxdb_core::ArtifactSourceRef {
            result_ref: Some("result-2".into()),
            tool_call_ref: None,
        },
    ];
    let mut second = source("r2");
    second.declared_ref = Some("result-2".into());

    // revision 不匹配时应拒绝且 spec 未变。
    let err = replace_artifact_spec(
        &conn,
        2_100,
        "art1",
        &next,
        &[source("r1"), second.clone()],
        created.revision + 5,
    )
    .expect_err("过期 revision 应拒绝");
    assert!(err.message.contains("重新加载"), "实际: {}", err.message);
    let unchanged = load_artifact(&conn, "art1").unwrap();
    assert_eq!(unchanged.title, "销售额分析");
    assert_eq!(unchanged.revision, created.revision);
    assert_eq!(list_artifact_sources(&conn, "art1").unwrap().len(), 1);

    // 匹配时全量重写来源。
    let replaced = replace_artifact_spec(
        &conn,
        2_200,
        "art1",
        &next,
        &[source("r1"), second],
        created.revision,
    )
    .expect("覆盖声明");
    assert_eq!(replaced.revision, created.revision + 1);
    assert_eq!(replaced.title, "新标题");
    let sources = list_artifact_sources(&conn, "art1").unwrap();
    assert_eq!(sources.len(), 2, "旧来源应被删除");
    assert_eq!(sources[0].result_id.as_deref(), Some("r1"));
    assert_eq!(sources[1].result_id.as_deref(), Some("r2"));
    assert_eq!(sources[1].ordinal, 1);
}

#[test]
fn status_machine_and_ready_writeback() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    let created = draft(&conn, "art1", "s1", "t1", "r1");

    let staged =
        set_artifact_status(&conn, 2_100, "art1", "staged", created.revision).expect("草稿→暂存");
    assert_eq!(staged.status, "staged");
    let before = snapshot(&conn);
    assert!(set_artifact_status(&conn, 2_150, "art1", "failed", created.revision).is_err());
    for (bytes, hash, revision) in [
        (10, "hash", created.revision),
        (-1, "hash", staged.revision),
        (10, "", staged.revision),
    ] {
        assert!(
            mark_artifact_ready(
                &conn,
                2_150,
                "art1",
                "artifacts/art1.html",
                bytes,
                hash,
                revision,
            )
            .is_err()
        );
        assert_eq!(snapshot(&conn), before);
    }

    // 非法路径不得写任何列。
    for bad in [
        "/etc/passwd",
        "../x",
        "",
        ".",
        "./x",
        "x/.",
        "x/..",
        "x/../y",
        "x//y",
        "x/",
        "//host/share",
        "\\\\host\\share",
        "\\root",
        "..\\x",
        "x\\..\\y",
        "x\\y",
        "C:\\x",
        "C:/x",
        "C:x",
        "x:stream",
        "x/\0y",
        "x/.. /y",
        "x/y.",
        "x/y ",
    ] {
        let err = mark_artifact_ready(&conn, 2_200, "art1", bad, 10, "hash", staged.revision)
            .expect_err("非法路径应拒绝");
        assert!(err.message.contains("相对路径"), "实际: {}", err.message);
        assert_eq!(load_artifact(&conn, "art1").unwrap(), staged);
    }
    let ready = mark_artifact_ready(
        &conn,
        2_300,
        "art1",
        "artifacts/art1.html",
        4_096,
        "sha256:abcd",
        staged.revision,
    )
    .expect("写回并置就绪");
    assert_eq!(ready.status, "ready");
    assert_eq!(ready.relative_path.as_deref(), Some("artifacts/art1.html"));
    assert_eq!(ready.byte_count, 4_096);
    assert_eq!(ready.content_hash.as_deref(), Some("sha256:abcd"));
    assert_eq!(ready.revision, staged.revision + 1);

    // ready 是终态：再改状态必须失败且行值不变。
    let err = set_artifact_status(&conn, 2_400, "art1", "failed", ready.revision)
        .expect_err("终态不应再改");
    assert!(err.message.contains("终态"), "实际: {}", err.message);
    let still_ready = load_artifact(&conn, "art1").unwrap();
    assert_eq!(still_ready.status, "ready");
    assert_eq!(still_ready.revision, ready.revision);
}

#[test]
fn failed_artifact_keeps_sources_and_sql_result_untouched() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    let created = draft(&conn, "art1", "s1", "t1", "r1");

    let before = rows(&conn, "SELECT * FROM ai_results WHERE id='r1'");

    let failed =
        set_artifact_status(&conn, 2_100, "art1", "failed", created.revision).expect("置失败");
    assert_eq!(failed.status, "failed");
    assert_eq!(
        list_artifact_sources(&conn, "art1").unwrap().len(),
        1,
        "失败产物仍需保留来源，才能解释它引用了哪些结果"
    );

    // renderer 失败不改写 SQL 结果：结果行事实前后完全不变。
    let after = rows(&conn, "SELECT * FROM ai_results WHERE id='r1'");
    assert_eq!(before, after, "产物失败不得修改结果行");
}

#[test]
fn upsert_artifact_part_requires_same_turn_artifact() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    seed_task(&conn, "s2", "t2", "c2", "r2");
    draft(&conn, "art1", "s1", "t1", "r1");
    draft(&conn, "art2", "s2", "t2", "r2");

    // 正常写入：片段落成 artifact_ref 并指向产物。
    upsert_artifact_part(
        &conn,
        2_100,
        "s1",
        "t1",
        "m1",
        "p1",
        sessions::PartStatus::Complete,
        "{}",
        "art1",
    )
    .expect("追加产物卡");
    let part = sessions::load_part(&conn, "p1").expect("读片段");
    assert_eq!(part.kind, sessions::PartKind::ArtifactRef);
    assert_eq!(part.artifact_id.as_deref(), Some("art1"));

    // 别的 Turn 的产物不能挂到当前消息上。
    let err = upsert_artifact_part(
        &conn,
        2_200,
        "s1",
        "t1",
        "m1",
        "p2",
        sessions::PartStatus::Complete,
        "{}",
        "art2",
    )
    .expect_err("跨 Turn 产物应拒绝");
    assert!(
        err.message.contains("不属于本任务"),
        "实际: {}",
        err.message
    );
    assert!(
        sessions::load_part(&conn, "p2").is_err(),
        "拒绝时不得写片段"
    );
}

#[test]
fn source_handles_must_match_each_declaration_in_order() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    for declared_ref in [None, Some("other".into()), Some(" result-1 ".into())] {
        let mut input = source("r1");
        input.declared_ref = declared_ref;
        assert_rejected_sources(&conn, &spec(), &[input], "句柄");
    }
    let mut next = spec();
    next.sources.push(fluxdb_core::ArtifactSourceRef {
        result_ref: Some("result-2".into()),
        tool_call_ref: None,
    });
    let mut second = source("r1");
    second.declared_ref = Some("result-2".into());
    assert_rejected_sources(&conn, &next, &[second.clone(), source("r1")], "句柄");
    assert_rejected_sources(&conn, &next, &[source("r1"), source("r1")], "句柄");
    draft(&conn, "art1", "s1", "t1", "r1");
    let before = load_artifact(&conn, "art1").unwrap();
    let before_sources = list_artifact_sources(&conn, "art1").unwrap();
    assert!(
        replace_artifact_spec(
            &conn,
            2_100,
            "art1",
            &next,
            &[source("r1"), source("r1")],
            1,
        )
        .is_err()
    );
    assert_eq!(load_artifact(&conn, "art1").unwrap(), before);
    assert_eq!(
        list_artifact_sources(&conn, "art1").unwrap(),
        before_sources
    );
}

#[test]
fn result_availability_and_actual_call_success_are_required() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    for status in ["expired", "deleted", "failed"] {
        conn.execute("UPDATE ai_results SET status=?1 WHERE id='r1'", [status])
            .unwrap();
        assert_rejected_sources(&conn, &spec(), &[source("r1")], "不可用");
    }
    conn.execute("UPDATE ai_results SET status='available' WHERE id='r1'", [])
        .unwrap();
    for status in [
        "prepared",
        "waiting_approval",
        "running",
        "failed",
        "denied",
        "cancelled",
        "unknown",
    ] {
        conn.execute("UPDATE ai_tool_calls SET status=?1 WHERE id='c1'", [status])
            .unwrap();
        for explicit in [false, true] {
            let mut input = source("r1");
            input.tool_call_id = explicit.then(|| "c1".into());
            assert_rejected_sources(&conn, &spec(), &[input], "未成功");
        }
    }
}

#[test]
fn unrelated_explicit_and_invalid_implicit_calls_are_rejected() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    seed_task(&conn, "s2", "t2", "c2", "r2");
    tool_calls::create_tool_call(
        &conn,
        1_000,
        "c3",
        "s1",
        "t1",
        1,
        1,
        "pc-c3",
        "db.query",
        1,
        None,
        "{}",
        "{}",
        "hash",
        "succeeded",
    )
    .unwrap();
    for call in ["c2", "c3", "missing"] {
        let mut input = source("r1");
        input.tool_call_id = Some(call.into());
        assert_rejected_sources(&conn, &spec(), &[input], "实际来源不一致");
    }
    conn.execute(
        "UPDATE ai_results SET source_call_id='c2' WHERE id='r1'",
        [],
    )
    .unwrap();
    assert_rejected_sources(&conn, &spec(), &[source("r1")], "不属于本任务");
    let mut explicit = source("r1");
    explicit.tool_call_id = Some("c2".into());
    assert_rejected_sources(&conn, &spec(), &[explicit], "不属于本任务");
    conn.execute(
        "UPDATE ai_results SET source_call_id=NULL WHERE id='r1'",
        [],
    )
    .unwrap();
    assert_rejected_sources(&conn, &spec(), &[source("r1")], "缺少实际工具调用");
    // 兼容历史坏行时也不能跳过实际调用检查，关闭外键仅限隔离 fixture。
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute(
        "UPDATE ai_results SET source_call_id='missing' WHERE id='r1'",
        [],
    )
    .unwrap();
    assert_rejected_sources(&conn, &spec(), &[source("r1")], "不存在");
}

#[test]
fn tool_call_only_source_is_traceable_but_does_not_invent_results() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    let mut next = spec();
    next.sources = vec![fluxdb_core::ArtifactSourceRef {
        result_ref: None,
        tool_call_ref: Some("pc-c1".into()),
    }];
    let input = ArtifactSourceInput {
        declared_ref: None,
        result_id: None,
        tool_call_id: Some("c1".into()),
    };
    let results = rows(&conn, "SELECT * FROM ai_results ORDER BY id");
    create_artifact(
        &conn,
        2_000,
        "art1",
        "s1",
        "t1",
        "1",
        &next,
        &[input.clone()],
    )
    .unwrap();
    let stored = list_artifact_sources(&conn, "art1").unwrap();
    assert_eq!(stored[0].result_id, None);
    assert_eq!(stored[0].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(rows(&conn, "SELECT * FROM ai_results ORDER BY id"), results);
    let mut invalid = input.clone();
    invalid.result_id = Some("r1".into());
    assert_rejected_sources(&conn, &next, &[invalid], "未声明的结果");
    let mut result_missing = source("r1");
    result_missing.result_id = None;
    result_missing.tool_call_id = Some("c1".into());
    assert_rejected_sources(&conn, &spec(), &[result_missing], "必须解析到结果行");
    conn.execute("UPDATE ai_tool_calls SET status='failed' WHERE id='c1'", [])
        .unwrap();
    assert_rejected_sources(&conn, &next, &[input], "未成功");
}

#[test]
fn create_and_replace_reject_terminal_or_wrong_owner_turns() {
    for status in [
        "completed",
        "cancelled",
        "failed",
        "interrupted",
        "outcome_unknown",
    ] {
        let conn = setup();
        seed_task(&conn, "s1", "t1", "c1", "r1");
        draft(&conn, "art1", "s1", "t1", "r1");
        conn.execute("UPDATE ai_turns SET status=?1 WHERE id='t1'", [status])
            .unwrap();
        let before = load_artifact(&conn, "art1").unwrap();
        let sources = list_artifact_sources(&conn, "art1").unwrap();
        assert_rejected_sources(&conn, &spec(), &[source("r1")], "迟到");
        assert!(replace_artifact_spec(&conn, 2_100, "art1", &spec(), &[source("r1")], 1).is_err());
        assert_eq!(load_artifact(&conn, "art1").unwrap(), before);
        assert_eq!(list_artifact_sources(&conn, "art1").unwrap(), sources);
        assert!(
            upsert_artifact_part(
                &conn,
                2_100,
                "s1",
                "t1",
                "m1",
                "p1",
                sessions::PartStatus::Complete,
                "{}",
                "art1",
            )
            .is_err()
        );
        assert!(rows(&conn, "SELECT * FROM ai_messages").is_empty());
        assert!(rows(&conn, "SELECT * FROM ai_message_parts").is_empty());
    }
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    seed_task(&conn, "s2", "t2", "c2", "r2");
    let error = create_artifact(
        &conn,
        2_000,
        "bad",
        "s2",
        "t1",
        "1",
        &spec(),
        &[source("r1")],
    )
    .unwrap_err();
    assert!(error.message.contains("归属"));
    assert_eq!(artifacts_count(&conn), 0);
    draft(&conn, "art1", "s1", "t1", "r1");
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute(
        "UPDATE ai_artifacts SET session_id='s2' WHERE id='art1'",
        [],
    )
    .unwrap();
    let before = load_artifact(&conn, "art1").unwrap();
    let error =
        replace_artifact_spec(&conn, 2_100, "art1", &spec(), &[source("r1")], 1).unwrap_err();
    assert!(error.message.contains("归属"));
    assert_eq!(load_artifact(&conn, "art1").unwrap(), before);
}

#[test]
fn existing_artifact_part_cannot_be_repurposed_or_moved() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    seed_task(&conn, "s2", "t2", "c2", "r2");
    for id in ["art1", "art2"] {
        draft(&conn, id, "s1", "t1", "r1");
    }
    draft(&conn, "foreign", "s2", "t2", "r2");
    upsert_artifact_part(
        &conn,
        2_100,
        "s1",
        "t1",
        "m1",
        "p1",
        sessions::PartStatus::Pending,
        "{}",
        "art1",
    )
    .unwrap();
    upsert_artifact_part(
        &conn,
        2_100,
        "s2",
        "t2",
        "m2",
        "p2",
        sessions::PartStatus::Pending,
        "{}",
        "foreign",
    )
    .unwrap();
    sessions::append_part(
        &conn,
        2_100,
        "text",
        "m1",
        sessions::PartKind::Text,
        sessions::PartStatus::Complete,
        "{\"text\":\"unchanged\"}",
        None,
        None,
        None,
    )
    .unwrap();
    for (message, part, artifact) in [
        ("m1", "p1", "art2"),
        ("new-message", "p1", "art1"),
        ("m1", "p2", "art1"),
        ("m1", "text", "art1"),
        ("m2", "new-part", "art1"),
    ] {
        let before = snapshot(&conn);
        assert!(
            upsert_artifact_part(
                &conn,
                2_200,
                "s1",
                "t1",
                message,
                part,
                sessions::PartStatus::Complete,
                "{\"text\":\"replacement\"}",
                artifact,
            )
            .is_err()
        );
        assert_eq!(snapshot(&conn), before);
    }
    let before = sessions::load_part(&conn, "p1").unwrap();
    upsert_artifact_part(
        &conn,
        2_300,
        "s1",
        "t1",
        "m1",
        "p1",
        sessions::PartStatus::Complete,
        "{\"text\":\"complete\"}",
        "art1",
    )
    .unwrap();
    let updated = sessions::load_part(&conn, "p1").unwrap();
    assert_eq!(updated.revision, before.revision + 1);
    assert_eq!(updated.status, sessions::PartStatus::Complete);
    assert_eq!(updated.payload_json, "{\"text\":\"complete\"}");
    assert_eq!(updated.artifact_id, before.artifact_id);
}

#[test]
fn write_failures_roll_back_artifacts_sources_messages_and_revisions() {
    let conn = setup();
    seed_task(&conn, "s1", "t1", "c1", "r1");
    draft(&conn, "art1", "s1", "t1", "r1");
    let mut next = spec();
    next.title = "不得部分保存".into();
    next.sources.push(fluxdb_core::ArtifactSourceRef {
        result_ref: Some("result-2".into()),
        tool_call_ref: None,
    });
    let mut second = source("r1");
    second.declared_ref = Some("result-2".into());
    let sources = [source("r1"), second];
    // 故障发生在第二条来源插入，第一条与产物行更新都已执行，仍须整体回滚。
    conn.execute_batch(
        "CREATE TEMP TRIGGER fail_second_source BEFORE INSERT ON ai_artifact_sources
         WHEN NEW.ordinal=1 BEGIN SELECT RAISE(ABORT, 'fixture source failure'); END;",
    )
    .unwrap();
    let before = snapshot(&conn);
    assert_rejected_sources(&conn, &next, &sources, "fixture source failure");
    let error = replace_artifact_spec(&conn, 2_100, "art1", &next, &sources, 1).unwrap_err();
    assert!(error.message.contains("fixture source failure"));
    assert_eq!(snapshot(&conn), before);
    conn.execute_batch("DROP TRIGGER fail_second_source")
        .unwrap();
    let updated = replace_artifact_spec(&conn, 2_200, "art1", &next, &sources, 1).unwrap();
    assert_eq!(updated.revision, 2);
    let before = snapshot(&conn);
    assert!(
        upsert_artifact_part(
            &conn,
            2_300,
            "s1",
            "t1",
            "m1",
            "p1",
            sessions::PartStatus::Complete,
            "{",
            "art1",
        )
        .is_err()
    );
    assert_eq!(
        snapshot(&conn),
        before,
        "片段失败不能留下消息或消耗消息序号"
    );
}
