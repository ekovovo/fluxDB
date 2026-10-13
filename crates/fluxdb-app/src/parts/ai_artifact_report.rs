// 产物报告读取与安全导出（设计 §27，T040）。
//
// 契约：
// - 报告**只读已生成快照**：打开时读取 `ai-artifacts/<uuid>.json` 与 `ai_artifacts` 行，
//   不重新查询数据库、不重新调用模型、不刷新来源结果版本。
// - 快照缺失/篡改/未就绪时给出可见解释，仍展示可解释的元数据（标题/类型/renderer/口径/来源），
//   绝不用空数据伪装成正常报告。
// - 导出由宿主完成：目标路径来自用户显式选择（路径授权），写入前校验、写临时文件后改名、
//   回读核对内容指纹；失败不返回成功、不留下半截文件、不伪造下载链接。
// - HTML 导出只做转义后的静态文档，禁止脚本与远程资源；PDF/XLSX 本期不实现。

/// 单个来源的可展示快照：来自产物文件，不重查数据库。
#[derive(Clone, Debug, PartialEq)]
pub struct AiReportSource {
    /// 模型声明的结果句柄（可追溯）。
    pub declared_ref: Option<String>,
    pub result_id: Option<String>,
    /// 产出该结果的工具调用行 ID。
    pub tool_call_id: Option<String>,
    pub snapshot_at_ms: i64,
    pub rows: usize,
    pub truncated: bool,
    /// 真实列结构（零行结果仍保留列名）。
    pub columns: Vec<String>,
    /// 有界真实行（对象形状，键为列名）。
    pub items: Vec<serde_json::Value>,
    /// 来源 SQL：从产出调用的脱敏参数里取，只读、不重新执行。
    pub sql: Option<String>,
}

/// 报告视图模型：弹框只消费这里的数据，不再访库。
#[derive(Clone, Debug, PartialEq)]
pub struct AiReportView {
    pub artifact_id: String,
    pub title: String,
    pub kind: String,
    pub renderer: String,
    pub renderer_version: String,
    pub status: String,
    pub revision: i64,
    pub spec: fluxdb_core::ArtifactSpec,
    pub sources: Vec<AiReportSource>,
    /// renderer 生成的 Markdown 正文；快照不可读时为 `None`。
    pub markdown: Option<String>,
    pub content_hash: Option<String>,
    pub byte_count: i64,
    pub truncated: bool,
    /// 快照缺失/损坏/未就绪时的可见解释；此时只展示元数据，不展示伪造正文。
    pub data_issue: Option<String>,
}

impl AiReportView {
    /// 报告是否只有元数据（没有可展示的渲染正文）。
    pub fn metadata_only(&self) -> bool {
        self.markdown.is_none()
    }
}

/// 导出格式；PDF/XLSX 本期不实现，不在此列出，避免伪造可用格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiReportExportFormat {
    Markdown,
    Html,
    Json,
}

impl AiReportExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            AiReportExportFormat::Markdown => "markdown",
            AiReportExportFormat::Html => "html",
            AiReportExportFormat::Json => "json",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            AiReportExportFormat::Markdown => "md",
            AiReportExportFormat::Html => "html",
            AiReportExportFormat::Json => "json",
        }
    }
}

/// 导出结果事实：真实写入的路径、字节数与内容指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiReportExportFact {
    pub path: String,
    pub byte_count: i64,
    pub content_hash: String,
}

fn report_error(code: &'static str, message: impl Into<String>) -> fluxdb_core::Error {
    fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        format!("{code}: {}", message.into()),
    )
}

/// 读取产物报告：产物行 + 快照文件，快照缺失/损坏时给出解释但不丢元数据。
pub fn load_ai_report(
    storage: &fluxdb_storage::FileStorage,
    artifact_id: &str,
) -> fluxdb_core::Result<AiReportView> {
    let view = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::artifacts::load_artifact(conn, artifact_id)
    })?;
    let spec = serde_json::from_str::<fluxdb_core::ArtifactSpec>(&view.spec_json)
        .map_err(|error| report_error("artifact_spec_damaged", format!("产物声明损坏: {error}")))?;

    let stored_sources = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::artifacts::list_artifact_sources(conn, artifact_id)
    })
    .unwrap_or_default();
    let sources = stored_sources
        .iter()
        .map(|source| {
            let declared_ref = serde_json::from_str::<serde_json::Value>(&source.metadata_json)
                .ok()
                .and_then(|value| value.get("result_ref")?.as_str().map(str::to_owned));
            let sql = source
                .tool_call_id
                .as_deref()
                .and_then(|call_id| load_source_sql(storage, call_id));
            AiReportSource {
                declared_ref,
                result_id: source.result_id.clone(),
                tool_call_id: source.tool_call_id.clone(),
                snapshot_at_ms: 0,
                rows: 0,
                truncated: false,
                columns: Vec::new(),
                items: Vec::new(),
                sql,
            }
        })
        .collect::<Vec<_>>();

    let mut report = AiReportView {
        artifact_id: view.id.clone(),
        title: spec.title.clone(),
        kind: spec.kind.as_str().to_string(),
        renderer: spec.renderer.clone(),
        renderer_version: view.renderer_version.clone(),
        status: view.status.clone(),
        revision: view.revision,
        spec,
        sources,
        markdown: None,
        content_hash: view.content_hash.clone(),
        byte_count: view.byte_count,
        truncated: false,
        data_issue: None,
    };

    if view.status != "ready" {
        report.data_issue = Some(match view.status.as_str() {
            "failed" => "产物渲染失败：只显示声明的来源与口径，正文未生成。".to_string(),
            "draft" => "产物仍是草稿，尚未渲染快照。".to_string(),
            "staged" => "产物快照尚未提交完成（staged）。".to_string(),
            "expired" => "产物已过期，快照不再可用。".to_string(),
            other => format!("产物状态为 {other}，没有可展示的渲染正文。"),
        });
        return Ok(report);
    }

    let Some(relative_path) = view.relative_path.clone() else {
        report.data_issue = Some("产物标记为就绪但缺少快照路径。".to_string());
        return Ok(report);
    };
    if let Err(error) =
        fluxdb_storage::ai_platform::artifacts::validate_relative_path(&relative_path)
    {
        report.data_issue = Some(format!("产物快照路径非法：{}", error.message));
        return Ok(report);
    }
    let path = storage.root_dir().join("ai-artifacts").join(&relative_path);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            report.data_issue = Some(format!("产物快照无法读取：{error}"));
            return Ok(report);
        }
    };
    if content.len() as i64 != view.byte_count
        || view
            .content_hash
            .as_deref()
            .is_some_and(|hash| crate::source_hash(&[content.as_str()]) != hash)
    {
        report.data_issue = Some("产物快照内容与记录不一致，拒绝展示可能被篡改的数据。".to_string());
        return Ok(report);
    }
    let snapshot: serde_json::Value = match serde_json::from_str(&content) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            report.data_issue = Some(format!("产物快照不是合法 JSON：{error}"));
            return Ok(report);
        }
    };
    report.markdown = snapshot
        .get("markdown")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    report.truncated = snapshot
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if let Some(items) = snapshot.get("sources").and_then(serde_json::Value::as_array) {
        for (index, item) in items.iter().enumerate() {
            let Some(target) = report.sources.get_mut(index) else {
                break;
            };
            target.snapshot_at_ms = item
                .get("snapshot_at_ms")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            target.rows = item
                .get("row_count")
                .and_then(serde_json::Value::as_u64)
                .map(|rows| rows as usize)
                .unwrap_or(0);
            target.truncated = item
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            target.columns = item
                .get("columns")
                .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
                .unwrap_or_default();
            target.items = item
                .get("items")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            if target.declared_ref.is_none() {
                target.declared_ref = item
                    .get("result_ref")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
        }
    }
    Ok(report)
}

/// 从产出调用的脱敏参数里取 SQL 文本（只读展示，不重新执行）。
fn load_source_sql(storage: &fluxdb_storage::FileStorage, call_id: &str) -> Option<String> {
    let call = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::load_tool_call(conn, call_id)
    })
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(&call.arguments_redacted_json).ok()?;
    ["sql", "original_sql", "statements", "command"]
        .iter()
        .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

/// 生成导出内容；HTML 只输出转义后的静态文档，不含脚本与远程资源。
pub fn render_ai_report_export(
    report: &AiReportView,
    format: AiReportExportFormat,
) -> fluxdb_core::Result<String> {
    let markdown = report.markdown.as_deref().ok_or_else(|| {
        report_error(
            "export_unavailable",
            "产物没有可导出的渲染正文（可能渲染失败或快照不可读）",
        )
    })?;
    match format {
        AiReportExportFormat::Markdown => Ok(markdown.to_string()),
        AiReportExportFormat::Html => Ok(report_to_html(report, markdown)),
        AiReportExportFormat::Json => {
            let sources = report
                .sources
                .iter()
                .map(|source| {
                    serde_json::json!({
                        "result_ref": source.declared_ref,
                        "result_id": source.result_id,
                        "tool_call_id": source.tool_call_id,
                        "snapshot_at_ms": source.snapshot_at_ms,
                        "row_count": source.rows,
                        "truncated": source.truncated,
                        "columns": source.columns,
                        "items": source.items,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string_pretty(&serde_json::json!({
                "artifact_id": report.artifact_id,
                "title": report.title,
                "kind": report.kind,
                "renderer": report.renderer,
                "renderer_version": report.renderer_version,
                "revision": report.revision,
                "status": report.status,
                "truncated": report.truncated,
                "spec": report.spec,
                "sources": sources,
            }))
            .map_err(|error| report_error("export_failed", format!("序列化导出失败: {error}")))
        }
    }
}

/// HTML 转义：模型/数据库文本一律按文本输出，不能构造标签或脚本。
fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn report_to_html(report: &AiReportView, markdown: &str) -> String {
    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", html_escape(&report.title)));
    body.push_str(&format!(
        "<p class=\"meta\">类型 {} · renderer {} · revision {} · {}{}</p>\n",
        html_escape(&report.kind),
        html_escape(&report.renderer),
        report.revision,
        if report.status == "ready" { "快照 · 不自动刷新" } else { "" },
        if report.truncated { " · 抽样/截断" } else { "" },
    ));
    body.push_str(&format!("<pre class=\"report\">{}</pre>\n", html_escape(markdown)));
    for source in &report.sources {
        body.push_str(&format!(
            "<section class=\"source\"><h2>来源 {}</h2><p>{} 行{}{}</p>",
            html_escape(source.declared_ref.as_deref().unwrap_or("-")),
            source.rows,
            if source.truncated { "（抽样）" } else { "" },
            match &source.sql {
                Some(sql) => format!("<pre class=\"sql\">{}</pre>", html_escape(sql)),
                None => String::new(),
            },
        ));
        body.push_str("</section>\n");
    }
    // 静态文档：无脚本、无远程资源、无外链样式，仅内联最小样式。
    format!(
        "<!doctype html>\n<html lang=\"zh\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>body{{font-family:sans-serif;margin:24px;line-height:1.6;color:#111}}.meta{{color:#555}}.report,.sql{{white-space:pre-wrap;background:#f5f5f5;padding:10px;border-radius:6px}}</style>\n</head>\n<body>\n{}\n<p class=\"disclaimer\">本文件由 fluxDB 静态导出，仅含生成时快照；打开不会重新查询数据库。</p>\n</body>\n</html>\n",
        html_escape(&report.title),
        body,
    )
}

/// 安全写出导出文件：目标来自用户选择的授权路径；拒绝符号链接与非文件覆盖，临时文件改名。
pub fn write_ai_report_export(
    destination: &std::path::Path,
    content: &str,
) -> fluxdb_core::Result<AiReportExportFact> {
    if content.len() as i64 > crate::BI_SNAPSHOT_MAX_BYTES as i64 {
        return Err(report_error("export_too_large", "导出内容超过单文件上限"));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() {
            return Err(report_error("export_denied", "导出目标不能是符号链接"));
        }
        if metadata.is_dir() {
            return Err(report_error("export_denied", "导出目标不能是目录"));
        }
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| report_error("export_denied", "导出目标缺少有效父目录"))?;
    if !parent.is_dir() {
        return Err(report_error("export_denied", "导出目录不存在或不是目录"));
    }
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| report_error("export_denied", "导出文件名非法"))?;
    let temp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let write = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(report_error("export_failed", format!("写入导出文件失败: {error}")));
    }
    if let Err(error) = std::fs::rename(&temp, destination) {
        let _ = std::fs::remove_file(&temp);
        return Err(report_error("export_failed", format!("提交导出文件失败: {error}")));
    }
    // 回读核对：失败不返回成功事实。
    let verified = std::fs::read_to_string(destination)
        .map_err(|error| report_error("export_verify_failed", format!("导出文件无法回读: {error}")))?;
    if verified != content {
        return Err(report_error("export_verify_failed", "导出内容核对不一致"));
    }
    Ok(AiReportExportFact {
        path: destination.to_string_lossy().into_owned(),
        byte_count: content.len() as i64,
        content_hash: crate::source_hash(&[content]),
    })
}
