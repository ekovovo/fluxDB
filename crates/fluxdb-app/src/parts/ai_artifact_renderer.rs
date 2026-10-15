// BI 产物插件：产物声明、来源溯源与内置 renderer（设计 §27、§42.7，T039）。
//
// 契约：
// - 产物是**声明式**的：块只引用已声明来源的字段名，数字一律从真实结果快照里取；
//   模型没有填写数据值的入口，来源句柄由宿主分配，不能凭空构造。
// - 来源必须能解析到**本任务**的结果行（`ai_results`），来源表正规化记录句柄与行 ID，
//   产物失败/删除都不影响来源结果本身。
// - renderer 是宿主可信实现：缺失或渲染失败时产物置 `failed` 并给出可见原因，
//   绝不伪造下载链接或改写 SQL 结果。
// - 插件可替换：工具只依赖 `ArtifactProducer` 边界，换一个 producer 即换掉整套产物规则。

/// 内置 BI 插件实例 ID（与 `BuiltinRegistry` 的 `fluxdb.output.bi` 对应）。
pub const BI_INSTANCE_ID: &str = "builtin.bi";
/// 报告 renderer（默认：Markdown 报告）。
pub const BI_REPORT_RENDERER_ID: &str = "fluxdb.bi.report";
/// 图表 renderer。
pub const BI_CHART_RENDERER_ID: &str = "fluxdb.bi.chart";
/// 表格 renderer。
pub const BI_TABLE_RENDERER_ID: &str = "fluxdb.bi.table";
/// 渲染快照单文件上限；超过直接报错，不做静默截断（截断必须由 renderer 显式标记）。
pub const BI_SNAPSHOT_MAX_BYTES: usize = 1024 * 1024;
/// 快照里最多写出的数据行数。
const BI_SNAPSHOT_MAX_ROWS: usize = 200;
/// 单元长度上限：产物正文是宿主生成的展示文本，仍要有界。
const BI_SNAPSHOT_MAX_CELL_CHARS: usize = 256;
/// 工具回执里最多回给模型的数据点。
const BI_RECEIPT_MAX_POINTS: usize = 50;

/// 来源快照：renderer 只能读这里给出的真实数据，不能再去查库。
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactSourceData {
    pub summary: ArtifactSourceSummary,
    /// 来自 SQL 回执的真实列结构，零行结果仍保留列名。
    pub columns: Vec<String>,
    /// 内存中的有界快照（受结果仓库内存预算约束）。
    pub sample: Vec<serde_json::Value>,
}

/// 宿主 renderer 的渲染结果：正文 + 从真实数据取出的点 + 是否抽样/截断。
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedArtifact {
    pub content: String,
    pub points: Vec<serde_json::Value>,
    pub truncated: bool,
}

/// 可信宿主 renderer（设计 §27「没有 renderer 时返回可见不支持」）。
pub trait ArtifactRenderer: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    /// 把声明 + 真实结果渲染成有界展示正文；失败必须返回错误，不得返回半成品。
    fn render(
        &self,
        spec: &fluxdb_core::ArtifactSpec,
        sources: &[ArtifactSourceData],
    ) -> Result<RenderedArtifact, ArtifactError>;
}

fn source_by_ref<'a>(
    sources: &'a [ArtifactSourceData],
    declared: &str,
) -> Result<&'a ArtifactSourceData, ArtifactError> {
    sources
        .iter()
        .find(|source| source.summary.declared_ref.as_deref() == Some(declared))
        .ok_or_else(|| {
            ArtifactError::new(
                "undeclared_source",
                format!("块引用了未声明的来源 {declared}；请先用 artifact.create 声明来源"),
            )
        })
}

/// 图表块取点：只读取真实行里的字段，`y` 必须是数值列。
fn chart_points(
    source: &ArtifactSourceData,
    x: &str,
    y: &str,
    max_points: usize,
) -> (Vec<serde_json::Value>, bool) {
    let mut points = Vec::new();
    let mut truncated = source.summary.truncated;
    for row in &source.sample {
        let Some(object) = row.as_object() else {
            truncated = true;
            continue;
        };
        let (Some(x_value), Some(y_value)) = (object.get(x), object.get(y)) else {
            truncated = true;
            continue;
        };
        if x_value.is_null() || !y_value.is_number() {
            truncated = true;
            continue;
        }
        if points.len() >= max_points {
            truncated = true;
            break;
        }
        points.push(serde_json::json!({ "x": x_value, "y": y_value }));
    }
    (points, truncated)
}

/// 模型说明与数据库文本只能是展示文字，不能生成 HTML 或远程 Markdown 资源。
fn markdown_text(raw: &str) -> String {
    let mut escaped = String::new();
    for ch in raw.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '|' | '!' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn cell_text(value: &serde_json::Value) -> String {
    if value.is_null() {
        return String::new();
    }
    let raw = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    // 展示正文里换行与表格分隔符都要转义，避免真实数据把报告结构冲掉。
    let bounded: String = raw.chars().take(BI_SNAPSHOT_MAX_CELL_CHARS).collect();
    markdown_text(&bounded)
}

fn markdown_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut text = String::new();
    text.push_str("| ");
    text.push_str(&headers.iter().map(|header| markdown_text(header)).collect::<Vec<_>>().join(" | "));
    text.push_str(" |\n|");
    for _ in headers {
        text.push_str(" --- |");
    }
    text.push('\n');
    for row in rows {
        text.push_str("| ");
        text.push_str(&row.join(" | "));
        text.push_str(" |\n");
    }
    text
}

/// 正文去重键：同一来源 + 同一组字段名 + 同样行数视为同一份数据（字段声明顺序不参与判定）。
///
/// Markdown 没有原生图表，图表块只能降级成数据表；因此「图表 + 表格同源同字段」
/// 或重复声明同一图表，会在正文里把同一份数字写多遍。行数参与判定，避免把点数被
/// 截短或跳空值的图表，与仍保留完整行的表格误判成同一份数据。
fn markdown_data_key<'a>(
    result_ref: &str,
    columns: impl IntoIterator<Item = &'a str>,
    row_count: usize,
) -> String {
    let mut columns: Vec<&str> = columns.into_iter().collect();
    columns.sort_unstable();
    format!("{result_ref}\u{1}{row_count}\u{1}{}", columns.join("\u{1}"))
}

/// 重复数据块的正文提示（宿主文案，不是模型文本）。
const DUPLICATE_DATA_NOTE: &str = "（本块与上文同源同字段的数据相同，未重复输出。）\n";

/// 报告头部：类型/口径/时区/来源与抽样标记。
fn render_header(spec: &fluxdb_core::ArtifactSpec, sources: &[ArtifactSourceData]) -> String {
    let mut text = format!(
        "# {}\n\n- 类型：{}\n- renderer：{}\n",
        markdown_text(&spec.title),
        spec.kind.as_str(),
        markdown_text(&spec.renderer)
    );
    if let Some(metric) = &spec.metric {
        text.push_str(&format!("- 指标口径：{}", markdown_text(&metric.name)));
        if let Some(reference) = &metric.definition_ref {
            text.push_str(&format!("（定义引用 {}）", markdown_text(reference)));
        }
        text.push_str(&format!("，时区 {}\n", metric.timezone));
        if let Some(assumption) = &metric.assumption {
            text.push_str(&format!("- 未确认假设：{}\n", markdown_text(assumption)));
        }
    } else {
        text.push_str("- 指标口径：未声明（结论按来源结果的原始字段说明）\n");
    }
    text.push_str(&format!("- 模板版本：{}\n- 刷新语义：生成时静态快照，不重新查询\n- 可访问性说明：{}\n",
        markdown_text(&spec.template_version), markdown_text(&spec.accessibility_description)));
    if let Some(range) = &spec.time_range {
        text.push_str(&format!("- 声明时间范围（未独立核实）：{} 至 {}\n", markdown_text(&range.start), markdown_text(&range.end)));
    }
    for filter in &spec.filters {
        text.push_str(&format!("- 声明过滤条件（未独立核实）：{}\n", markdown_text(filter)));
    }
    for source in sources {
        let declared = source.summary.declared_ref.as_deref().unwrap_or("-");
        text.push_str(&format!(
            "- 来源 {}：{} 行{}{}，快照版本 {}（静态）\n",
            markdown_text(declared),
            source.summary.rows,
            if source.summary.truncated {
                "（结果被抽样，未全部加载）"
            } else {
                ""
            },
            match &source.summary.tool_call_id {
                Some(call) => format!("，产出调用 {}", markdown_text(call)),
                None => String::new(),
            },
            source.summary.snapshot_at_ms
        ));
    }
    text
}

/// 内置 Markdown renderer：报告/图表/表格共用同一实现对真实结果的读取规则。
struct BuiltinMarkdownRenderer {
    id: &'static str,
}

impl ArtifactRenderer for BuiltinMarkdownRenderer {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn render(
        &self,
        spec: &fluxdb_core::ArtifactSpec,
        sources: &[ArtifactSourceData],
    ) -> Result<RenderedArtifact, ArtifactError> {
        let mut text = render_header(spec, sources);
        let mut points = Vec::new();
        let mut truncated = sources.iter().any(|source| source.summary.truncated);
        truncated |= sources.iter().flat_map(|source| &source.sample)
            .filter_map(serde_json::Value::as_object)
            .flat_map(|row| row.values())
            .any(|cell| cell.as_str().is_some_and(|text| text.chars().count() > BI_SNAPSHOT_MAX_CELL_CHARS));
        // 同一来源同字段的数据在正文里只写一次：重复声明或「图表 + 表格」同源时，
        // 后续块只保留标题与说明，避免导出文档和报告正文里堆满一模一样的数据表。
        let mut emitted_data: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for block in &spec.blocks {
            match block {
                fluxdb_core::ArtifactBlock::Chart {
                    result_ref,
                    chart,
                    x,
                    y,
                } => {
                    let source = source_by_ref(sources, result_ref)?;
                    let (block_points, cut) =
                        chart_points(source, x, y, fluxdb_core::ARTIFACT_MAX_POINTS);
                    truncated |= cut;
                    let headers = vec![x.clone(), y.clone()];
                    let rows: Vec<Vec<String>> = block_points
                        .iter()
                        .map(|point| {
                            vec![
                                cell_text(point.get("x").unwrap_or(&serde_json::Value::Null)),
                                cell_text(point.get("y").unwrap_or(&serde_json::Value::Null)),
                            ]
                        })
                        .collect();
                    text.push_str(&format!(
                        "\n## 图表（{}，来源 {}）\n\n",
                        match chart {
                            fluxdb_core::ArtifactChart::Line => "折线",
                            fluxdb_core::ArtifactChart::Bar => "柱状",
                            fluxdb_core::ArtifactChart::BarHorizontal => "条形",
                            fluxdb_core::ArtifactChart::Area => "面积",
                            fluxdb_core::ArtifactChart::Pie => "饼图",
                        },
                        markdown_text(result_ref)
                    ));
                    let key = markdown_data_key(result_ref, [x.as_str(), y.as_str()], rows.len());
                    if emitted_data.insert(key) {
                        text.push_str(&markdown_table(&headers, &rows));
                    } else {
                        text.push_str(DUPLICATE_DATA_NOTE);
                    }
                    if points.is_empty() {
                        points = block_points;
                    }
                }
                fluxdb_core::ArtifactBlock::Table {
                    result_ref,
                    columns,
                } => {
                    let source = source_by_ref(sources, result_ref)?;
                    let mut rows = Vec::new();
                    for row in &source.sample {
                        let Some(object) = row.as_object() else {
                            continue;
                        };
                        if rows.len() >= BI_SNAPSHOT_MAX_ROWS {
                            truncated = true;
                            break;
                        }
                        rows.push(
                            columns
                                .iter()
                                .map(|column| {
                                    cell_text(object.get(column).unwrap_or(&serde_json::Value::Null))
                                })
                                .collect(),
                        );
                    }
                    text.push_str(&format!("\n## 表格（来源 {}）\n\n", markdown_text(result_ref)));
                    let key = markdown_data_key(
                        result_ref,
                        columns.iter().map(String::as_str),
                        rows.len(),
                    );
                    if emitted_data.insert(key) {
                        text.push_str(&markdown_table(columns, &rows));
                    } else {
                        text.push_str(DUPLICATE_DATA_NOTE);
                    }
                }
                fluxdb_core::ArtifactBlock::Note { text: note } => {
                    text.push_str(&format!("\n## 模型说明（未独立核实）\n\n{}\n", markdown_text(note)));
                }
            }
        }
        if truncated {
            text.push_str("\n## 数据限制\n\n来源结果或展示行数被抽样/截断，本产物不代表全集。\n");
        }
        if text.len() > BI_SNAPSHOT_MAX_BYTES {
            return Err(ArtifactError::new(
                "snapshot_too_large",
                "渲染快照超过单文件上限，请减少块数或缩小来源范围",
            ));
        }
        Ok(RenderedArtifact {
            content: text,
            points,
            truncated,
        })
    }
}

