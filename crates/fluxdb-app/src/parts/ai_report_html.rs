// 报告导出 HTML：把已生成快照渲染成与报告弹框同构的自包含静态文档（设计 §27、T040）。
//
// 契约：
// - 只消费 `AiReportView`（产物声明 + 来源快照），打开不访库、不调模型、不刷新来源版本。
// - 单文件自包含：内联 CSS + 内联 SVG 图表，**无脚本、无远程资源、无外链样式**；
//   图表由宿主按真实点自绘，不用 JS 图表库、不引外部图片。
// - 所有模型/数据库文本一律转义后按文本输出，不能构造标签或脚本。
// - 数字只做算术（求和/峰值/计数），不推断业务结论；抽样/截断必须显式标注。

/// 表格最多渲染行数（与弹框一致）。
const HTML_TABLE_MAX_ROWS: usize = 50;

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

/// 数字展示：整数不带小数，其余保留两位（与弹框指标条一致）。
fn html_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

/// 单元格文本：null 输出空串，其余按真实值转义输出。
fn html_cell(value: &serde_json::Value) -> String {
    if value.is_null() {
        return String::new();
    }
    match value {
        serde_json::Value::String(text) => html_escape(text),
        other => html_escape(&other.to_string()),
    }
}

/// 自包含 HTML 报告：结构、口径与报告弹框一致，图表内联为 SVG。
pub(crate) fn render_report_html(report: &AiReportView) -> String {
    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", html_escape(&report.title)));

    if let Some(issue) = &report.data_issue {
        body.push_str(&format!(
            "<p class=\"alert\">产物快照有问题，只展示可解释的元数据：{}</p>\n",
            html_escape(issue)
        ));
    }

    if let Some(summary) = report.chart_summary() {
        body.push_str("<div class=\"metrics\">\n");
        body.push_str(&html_metric_card(
            &format!("{} 合计", summary.y),
            html_number(summary.sum),
            &format!("{} 个真实数据点", summary.count),
        ));
        let peak_note = if summary.max_label.is_empty() {
            "来源真实值".to_string()
        } else {
            format!("最高：{}", html_escape(&summary.max_label))
        };
        body.push_str(&html_metric_card(
            &format!("{} 峰值", summary.y),
            html_number(summary.max),
            &peak_note,
        ));
        body.push_str("</div>\n");
    }

    if let Some(metric) = &report.spec.metric {
        let mut text = format!(
            "指标口径：{} · 时区 {}",
            html_escape(&metric.name),
            html_escape(&metric.timezone)
        );
        if let Some(reference) = &metric.definition_ref {
            text.push_str(&format!(" · 定义引用 {}", html_escape(reference)));
        }
        if let Some(assumption) = &metric.assumption {
            text.push_str(&format!(" · 未确认假设：{}", html_escape(assumption)));
        }
        body.push_str(&format!("<p class=\"metric\">{text}</p>\n"));
    }

    for block in &report.spec.blocks {
        match block {
            fluxdb_core::ArtifactBlock::Chart {
                result_ref,
                chart,
                x,
                y,
            } => {
                body.push_str(&html_chart_block(report, result_ref, *chart, x, y));
            }
            fluxdb_core::ArtifactBlock::Table {
                result_ref,
                columns,
            } => {
                body.push_str(&html_table_block(report, result_ref, columns));
            }
            fluxdb_core::ArtifactBlock::Note { text } => {
                body.push_str(&format!(
                    "<p class=\"note\">模型说明（未独立核实）：{}</p>\n",
                    html_escape(text)
                ));
            }
        }
    }
    if report.spec.blocks.is_empty() {
        body.push_str("<p class=\"note\">产物没有图表或表格块。</p>\n");
    }

    format!(
        "<!doctype html>\n<html lang=\"zh\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{}</style>\n</head>\n<body>\n<main>\n{}\n</main>\n</body>\n</html>\n",
        html_escape(&report.title),
        html_css(),
        body,
    )
}

/// 内联样式：浅色静态文档，无外链字体与远程资源。
fn html_css() -> &'static str {
    "body{margin:0;padding:32px 24px;background:#fafbfc;color:#1f2329;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC','Hiragino Sans GB','Microsoft YaHei',sans-serif;line-height:1.55;font-size:13px}\
main{max-width:960px;margin:0 auto;background:#fff;border:1px solid #e6e8ee;border-radius:10px;padding:24px 24px 16px}\
h1{font-size:20px;margin:0 0 12px}\
.alert{border:1px solid #f0c36d;background:#fdf6e6;color:#8a5a00;border-radius:8px;padding:8px 10px;font-size:12px}\
.metrics{display:flex;gap:12px;flex-wrap:wrap;margin:0 0 12px}\
.metric-card{flex:1 1 220px;min-width:200px;border:1px solid #e6e8ee;background:#f8f9fb;border-radius:8px;padding:10px 12px}\
.metric-card .label{color:#5a6270;font-size:11px}\
.metric-card .value{font-size:22px;font-weight:600;margin:2px 0}\
.metric-card .note{color:#5a6270;font-size:11px;margin:0}\
.metric{color:#5a6270;font-size:12px;margin:0 0 14px}\
.card{border:1px solid #e6e8ee;border-radius:8px;padding:12px;margin:0 0 14px}\
.card-head{display:flex;justify-content:space-between;align-items:center;gap:12px;margin:0 0 8px}\
.card-head .title{font-size:13px;font-weight:600}\
.card-head .kind{color:#5a6270;font-size:11px}\
svg{width:100%;height:auto;display:block}\
table{border-collapse:collapse;width:100%;font-size:12px}\
th,td{border:1px solid #e6e8ee;padding:6px 8px;text-align:left;vertical-align:top;word-break:break-word}\
th{background:#f6f7f9;font-weight:600}\
.note{color:#5a6270;font-size:12px;margin:0 0 12px}"
}

fn html_metric_card(label: &str, value: String, note: &str) -> String {
    format!(
        "<div class=\"metric-card\"><div class=\"label\">{}</div><div class=\"value\">{value}</div><p class=\"note\">{}</p></div>\n",
        html_escape(label),
        html_escape(note),
    )
}

/// 图表块：与弹框一致，标注声明字段与图表类型，图体为宿主自绘 SVG。
fn html_chart_block(
    report: &AiReportView,
    result_ref: &str,
    chart: fluxdb_core::ArtifactChart,
    x: &str,
    y: &str,
) -> String {
    let kind = match chart {
        fluxdb_core::ArtifactChart::Line => "折线图",
        fluxdb_core::ArtifactChart::Bar => "柱状图",
    };
    let mut html = format!(
        "<section class=\"card\"><div class=\"card-head\"><span class=\"title\">{} 按 {}</span><span class=\"kind\">{kind}</span></div>\n",
        html_escape(y),
        html_escape(x),
    );
    match report.chart_series(result_ref, chart, x, y) {
        Some(series) => html.push_str(&html_chart_svg(&series)),
        // 来源不可解析或没有可用数值点：明确说明，不画假图。
        None => html.push_str(
            "<p class=\"note\">来源不可解析或没有可用数值点，图表未渲染。</p>\n",
        ),
    }
    html.push_str("</section>\n");
    html
}

/// 表格块：真实列与真实行，超过上限显式标注截断。
fn html_table_block(report: &AiReportView, result_ref: &str, columns: &[String]) -> String {
    let mut html = format!(
        "<section class=\"card\"><div class=\"card-head\"><span class=\"title\">表格（{} 列）</span></div>\n",
        columns.len()
    );
    let source = report
        .sources
        .iter()
        .find(|source| source.declared_ref.as_deref() == Some(result_ref));
    let Some(source) = source else {
        html.push_str("<p class=\"note\">来源不可解析，表格未渲染。</p>\n</section>\n");
        return html;
    };
    if columns.is_empty() {
        html.push_str("<p class=\"note\">表格没有声明列，未渲染。</p>\n</section>\n");
        return html;
    }
    html.push_str("<table><thead><tr>");
    for column in columns {
        html.push_str(&format!("<th>{}</th>", html_escape(column)));
    }
    html.push_str("</tr></thead><tbody>\n");
    let mut shown = 0usize;
    for item in &source.items {
        let Some(object) = item.as_object() else {
            continue;
        };
        if shown >= HTML_TABLE_MAX_ROWS {
            break;
        }
        shown += 1;
        html.push_str("<tr>");
        for column in columns {
            let value = object
                .get(column)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            html.push_str(&format!("<td>{}</td>", html_cell(&value)));
        }
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody></table>\n");
    let remaining = source.rows.saturating_sub(shown);
    if remaining > 0 {
        html.push_str(&format!(
            "<p class=\"note\">还有 {remaining} 行未展示（单份文档最多 {HTML_TABLE_MAX_ROWS} 行，表格按来源快照的前若干行渲染）。</p>\n"
        ));
    }
    html.push_str("</section>\n");
    html
}

/// 绘图区（SVG 用户坐标）。
const CHART_WIDTH: f64 = 960.;
const CHART_HEIGHT: f64 = 320.;
const CHART_LEFT: f64 = 68.;
const CHART_RIGHT: f64 = 16.;
const CHART_TOP: f64 = 16.;
/// 底部留给旋转的分类标签。
const CHART_BOTTOM: f64 = 104.;

/// 图表几何：纵轴范围与坐标换算（负值也画在正确一侧，不裁剪）。
struct ChartScale {
    min: f64,
    max: f64,
}

impl ChartScale {
    fn new(points: &[AiReportChartPoint]) -> Self {
        let lowest = points.iter().map(|point| point.y).fold(0.0_f64, f64::min);
        let highest = points.iter().map(|point| point.y).fold(0.0_f64, f64::max);
        if (highest - lowest).abs() < f64::EPSILON {
            // 全部为 0（或同值）时给一个可见的高度，避免除以 0。
            return Self {
                min: 0.,
                max: if highest.abs() < f64::EPSILON { 1. } else { highest },
            };
        }
        Self {
            min: lowest,
            max: highest,
        }
    }

    fn plot_height(&self) -> f64 {
        CHART_HEIGHT - CHART_TOP - CHART_BOTTOM
    }

    fn y(&self, value: f64) -> f64 {
        let ratio = (value - self.min) / (self.max - self.min);
        CHART_TOP + (1. - ratio) * self.plot_height()
    }

    /// 纵轴刻度值：0 与极值之间取 4 段。
    fn ticks(&self) -> Vec<f64> {
        (0..=4)
            .map(|step| self.min + (self.max - self.min) * f64::from(step) / 4.)
            .collect()
    }
}

fn html_chart_svg(series: &AiReportChartSeries) -> String {
    let scale = ChartScale::new(&series.points);
    let plot_width = CHART_WIDTH - CHART_LEFT - CHART_RIGHT;
    let baseline = scale.y(0.);
    let mut svg = format!(
        "<svg viewBox=\"0 0 {CHART_WIDTH} {CHART_HEIGHT}\" role=\"img\" aria-label=\"{}\">\n",
        html_escape(&format!("{} 按 {}", series.y, series.x))
    );
    // 网格与刻度
    for tick in scale.ticks() {
        let y = scale.y(tick);
        svg.push_str(&format!(
            "<line x1=\"{CHART_LEFT}\" y1=\"{y:.2}\" x2=\"{:.2}\" y2=\"{y:.2}\" stroke=\"#e6e8ee\" stroke-width=\"1\"/>\n",
            CHART_WIDTH - CHART_RIGHT
        ));
        svg.push_str(&format!(
            "<text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"end\" font-size=\"10\" fill=\"#8a8f98\">{}</text>\n",
            CHART_LEFT - 8.,
            y + 3.,
            html_number(tick)
        ));
    }
    let count = series.points.len().max(1) as f64;
    let band = plot_width / count;
    match series.chart {
        fluxdb_core::ArtifactChart::Bar => {
            let bar_width = (band * 0.6).max(1.);
            for (index, point) in series.points.iter().enumerate() {
                let center = CHART_LEFT + band * (index as f64 + 0.5);
                let y = scale.y(point.y);
                let top = y.min(baseline);
                let height = (y - baseline).abs().max(1.);
                svg.push_str(&format!(
                    "<rect x=\"{:.2}\" y=\"{top:.2}\" width=\"{bar_width:.2}\" height=\"{height:.2}\" rx=\"2\" fill=\"#5b7cfa\"/>\n",
                    center - bar_width / 2.
                ));
            }
        }
        fluxdb_core::ArtifactChart::Line => {
            let polyline = series
                .points
                .iter()
                .enumerate()
                .map(|(index, point)| {
                    format!(
                        "{:.2},{:.2}",
                        CHART_LEFT + band * (index as f64 + 0.5),
                        scale.y(point.y)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            svg.push_str(&format!(
                "<polyline points=\"{polyline}\" fill=\"none\" stroke=\"#5b7cfa\" stroke-width=\"2\"/>\n"
            ));
            for (index, point) in series.points.iter().enumerate() {
                svg.push_str(&format!(
                    "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"2.5\" fill=\"#5b7cfa\"/>\n",
                    CHART_LEFT + band * (index as f64 + 0.5),
                    scale.y(point.y)
                ));
            }
        }
    }
    // 基线：纵轴 0 的位置
    svg.push_str(&format!(
        "<line x1=\"{CHART_LEFT}\" y1=\"{baseline:.2}\" x2=\"{:.2}\" y2=\"{baseline:.2}\" stroke=\"#c9ced8\" stroke-width=\"1\"/>\n",
        CHART_WIDTH - CHART_RIGHT
    ));
    // 分类标签：旋转显示完整值（静态文档比弹框宽裕，不截断可避免同名歧义）。
    for (index, point) in series.points.iter().enumerate() {
        let center = CHART_LEFT + band * (index as f64 + 0.5);
        let label_y = CHART_HEIGHT - CHART_BOTTOM + 14.;
        svg.push_str(&format!(
            "<text x=\"{center:.2}\" y=\"{label_y:.2}\" text-anchor=\"end\" font-size=\"10\" fill=\"#5a6270\" transform=\"rotate(-35 {center:.2} {label_y:.2})\">{}</text>\n",
            html_escape(&point.x)
        ));
    }
    svg.push_str("</svg>\n");
    svg
}
