//! T040：会话产物报告读取与安全导出（只读快照，不重查数据库）。
//! 复用 T039 的真实 SQLite + 受控 Provider 产物夹具，先产出真实 ready 产物，再验证报告与导出。
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fluxdb_app::{
    AiAgentSaveInput, AiConversationJob, AiConversationView, AiModelSaveInput,
    AiReportExportFormat, AppCommand, AppController, AppEvent, BuiltinBiProducer, KernelAuditHook,
    KernelPolicy, MapSqlResolver, RESULT_STORE_MEMORY_BYTES, ResultStore, TargetRef, ToolCatalog,
    TurnPermissionContext, TurnTargetPin, add_ai_artifact_tools, load_ai_report,
    render_ai_report_export, run_ai_conversation_turn, source_hash,
    standard_sql_catalog_with_results, write_ai_report_export,
};
use fluxdb_core::{
    AiFuture, CancelFlag, ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, FinishReason,
    GenerationProvider, ModelCapabilities, ModelEvent, ModelRequest, ModelRole, ToolCallRequest,
};
use fluxdb_storage::{
    FileStorage, Storage as _,
    ai_platform::{artifacts, sessions},
};
use serde_json::{Value, json};

include!("support/bi_artifacts_fixture.rs");

/// 跑通 create/add/finalize 得到真实 ready 产物后返回夹具与产物 ID。
fn ready_fixture() -> (Fixture, AiConversationJob, String) {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "ready");
    (fixture, job, artifact.id)
}

#[test]
fn report_reads_real_snapshot_with_values_and_sql_without_requery() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    assert_eq!(report.status, "ready");
    assert!(!report.metadata_only(), "ready 产物必须带渲染正文");
    assert!(report.markdown.as_deref().unwrap().contains("图表"));
    assert_eq!(report.sources.len(), 1);
    let source = &report.sources[0];
    assert_eq!(source.rows, 2);
    assert_eq!(
        source.columns,
        vec!["channel".to_string(), "revenue".to_string()]
    );
    assert_eq!(
        source.items,
        vec![
            json!({"channel":"web","revenue":12}),
            json!({"channel":"store","revenue":34})
        ]
    );
    // 来源 SQL 来自真实调用参数，只读展示。
    assert!(
        source
            .sql
            .as_deref()
            .unwrap_or("")
            .contains("SELECT channel,revenue FROM sales")
    );
}

#[test]
fn report_missing_snapshot_keeps_metadata_and_shows_visible_reason() {
    let (fixture, _job, artifact_id) = ready_fixture();
    // 删除快照文件：报告必须明确解释，而不是伪装成空报告。
    let artifacts_dir = fixture.dir.join("ai-artifacts");
    for entry in fs::read_dir(&artifacts_dir).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    assert!(report.metadata_only());
    assert!(report.data_issue.is_some());
    assert_eq!(report.sources.len(), 1);
    // 元数据仍在：来源句柄/口径仍可解释。
    assert_eq!(report.spec.sources.len(), 1);
    assert!(render_ai_report_export(&report, AiReportExportFormat::Markdown).is_err());
}

#[test]
fn report_tampered_snapshot_is_rejected_not_rendered() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let artifacts_dir = fixture.dir.join("ai-artifacts");
    let entry = fs::read_dir(&artifacts_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let path = entry.path();
    let mut content = fs::read_to_string(&path).unwrap();
    // 保持字节数不变地篡改内容，验证哈希核对而非仅长度核对。
    content = content.replace("web", "WEB");
    fs::write(&path, &content).unwrap();
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    assert!(report.metadata_only());
    assert!(report.data_issue.as_ref().unwrap().contains("不一致"));
}

#[test]
fn markdown_and_html_exports_escape_and_hash_real_content() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    let markdown = render_ai_report_export(&report, AiReportExportFormat::Markdown).unwrap();
    assert!(markdown.contains("web"));
    let html = render_ai_report_export(&report, AiReportExportFormat::Html).unwrap();
    // 静态 HTML：无脚本、无远程资源。
    assert!(html.starts_with("<!doctype html>"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("http://") && !html.contains("https://"));

    let out_md = fixture.dir.join("export.md");
    let fact = write_ai_report_export(&out_md, &markdown).unwrap();
    assert_eq!(fs::read_to_string(&out_md).unwrap(), markdown);
    assert_eq!(fact.byte_count, markdown.len() as i64);
    assert_eq!(
        fluxdb_app::source_hash(&[markdown.as_str()]),
        fact.content_hash
    );
}

#[test]
fn html_export_escapes_database_text_to_avoid_injection() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let mut report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    // 模拟数据库文本/模型说明里的标签：必须按文本转义，不能构造 HTML。
    report.markdown = Some("<img src=x onerror=alert(1)> & <b>bold</b>".to_string());
    let html = render_ai_report_export(&report, AiReportExportFormat::Html).unwrap();
    assert!(!html.contains("<img"));
    assert!(!html.contains("<b>bold"));
    assert!(html.contains("&lt;img"));
    assert!(html.contains("&amp;"));
}

#[test]
fn export_denies_symlink_and_directory_targets_without_writing() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    let content = render_ai_report_export(&report, AiReportExportFormat::Markdown).unwrap();
    // 目录拒绝。
    let dir_target = fixture.dir.join("export-dir");
    fs::create_dir(&dir_target).unwrap();
    assert!(write_ai_report_export(&dir_target, &content).is_err());
    // 符号链接拒绝（若平台支持）。
    #[cfg(unix)]
    {
        let real = fixture.dir.join("real.md");
        fs::write(&real, "original").unwrap();
        let link = fixture.dir.join("link.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(write_ai_report_export(&link, &content).is_err());
        assert_eq!(fs::read_to_string(&real).unwrap(), "original");
    }
    // 不存在的父目录拒绝。
    assert!(write_ai_report_export(&fixture.dir.join("nope/child.md"), &content).is_err());
}

#[test]
fn report_of_unfinalized_draft_shows_metadata_only() {
    let fixture = Fixture::new();
    let job = fixture.job();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    // 只跑到 create，不 finalize：产物保持 draft，报告必须只给元数据。
    let provider = ReceiptDrivenProvider::new(
        &fixture,
        &job,
        vec![
            query(QUERY),
            call("create", "artifact.create", create_args()),
            Step::Done,
        ],
    );
    run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "draft");
    let report = load_ai_report(&fixture.storage, &artifact.id).unwrap();
    assert_eq!(report.status, "draft");
    assert!(report.metadata_only());
    assert!(report.data_issue.is_some());
}

#[test]
fn json_export_contains_real_source_rows() {
    let (fixture, _job, artifact_id) = ready_fixture();
    let report = load_ai_report(&fixture.storage, &artifact_id).unwrap();
    let json = render_ai_report_export(&report, AiReportExportFormat::Json).unwrap();
    let value: Value = serde_json::from_str(&json).unwrap();
    let items = &value["sources"][0]["items"];
    assert_eq!(items[0]["channel"], "web");
    assert_eq!(items[1]["revenue"], 34);
}

#[test]
fn report_of_failed_artifact_explains_metadata_only_without_fake_body() {
    let fixture = Fixture::new();
    let job = fixture.job();
    // renderer/提交失败时产物置 failed 并留可见卡片；报告必须给出解释、不展示伪造正文。
    fixture
        .storage
        .with_ai_connection(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER reject_artifact_ready BEFORE UPDATE OF status ON ai_artifacts
            WHEN NEW.status='ready' BEGIN SELECT RAISE(ABORT, 'controlled ready rejection'); END;",
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    let (mut catalog, results) = fixture.catalog(&job);
    add_ai_artifact_tools(&mut catalog, &fixture.storage, &job, results).unwrap();
    let provider = ReceiptDrivenProvider::new(&fixture, &job, report_steps(QUERY));
    run(&fixture, &job, &catalog, &provider);
    let artifact = fixture.artifacts(&job).remove(0);
    assert_eq!(artifact.status, "failed");
    let report = load_ai_report(&fixture.storage, &artifact.id).unwrap();
    assert_eq!(report.status, "failed");
    assert!(report.metadata_only());
    assert!(report.data_issue.as_ref().unwrap().contains("失败"));
    // 元数据仍可解释来源，不伪造正文与下载。
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.spec.sources.len(), 1);
    assert!(render_ai_report_export(&report, AiReportExportFormat::Markdown).is_err());
}

// 显式忽略未用告警
