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
        fluxdb_core::ArtifactChart::BarHorizontal => "条形图",
        fluxdb_core::ArtifactChart::Area => "面积图",
        fluxdb_core::ArtifactChart::Pie => "饼图",
    };
    let mut html = format!(
        "<section class=\"card\"><div class=\"card-head\"><span class=\"title\">{} 按 {}</span><span class=\"kind\">{kind}</span></div>\n",
        html_escape(y),
        html_escape(x),
    );
    match report.chart_series(result_ref, chart, x, y) {
        // 声明类型与真实点不匹配（如饼图分类过多或含负值）：不画图，写出 App 层给出的同一份原因。
        Some(series) => match series.render_issue() {
            Some(issue) => {
                html.push_str(&format!("<p class=\"note\">{}</p>\n", html_escape(&issue)))
            }
            None => html.push_str(&html_chart_svg(&series)),
        },
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
/// 条形图底部留给横向数值轴刻度。
const CHART_HBAR_BOTTOM: f64 = 40.;
/// 分类标签栏宽度上下限（上限避免长分类名把绘图区挤没）。
const CHART_GUTTER_MIN: f64 = 56.;
const CHART_GUTTER_MAX: f64 = CHART_WIDTH * 0.4;
/// 文本宽度估算：10px 字号下半角字符与 CJK/全角字符的每单位像素。
const CHART_HALF_UNIT_PX: f64 = 5.5;
const CHART_FULL_UNIT_PX: f64 = 11.;
/// 饼图圆心与半径、右侧图例的位置。
const CHART_PIE_CENTER_X: f64 = 180.;
const CHART_PIE_RADIUS: f64 = 120.;
const CHART_PIE_LEGEND_X: f64 = 380.;
const CHART_PIE_LEGEND_TOP: f64 = 44.;
const CHART_PIE_LEGEND_STEP: f64 = 30.;
/// 图例文字可用宽度（右侧留白之后的余量）。
const CHART_PIE_LEGEND_TEXT_PX: f64 = CHART_WIDTH - CHART_PIE_LEGEND_X - 26.;

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

    /// 数值在值域里的位置（0..=1）：纵向图与横向条形共用同一套值域换算。
    fn ratio(&self, value: f64) -> f64 {
        (value - self.min) / (self.max - self.min)
    }

    fn y(&self, value: f64) -> f64 {
        CHART_TOP + (1. - self.ratio(value)) * self.plot_height()
    }

    /// 纵轴刻度值：0 与极值之间取 4 段。
    fn ticks(&self) -> Vec<f64> {
        (0..=4)
            .map(|step| self.min + (self.max - self.min) * f64::from(step) / 4.)
            .collect()
    }
}

/// 图表图体：按声明类型分派到直角坐标系或专用绘制（条形、饼图）。
fn html_chart_svg(series: &AiReportChartSeries) -> String {
    match series.chart {
        fluxdb_core::ArtifactChart::BarHorizontal => html_bar_horizontal_svg(series),
        fluxdb_core::ArtifactChart::Pie => html_pie_svg(series),
        fluxdb_core::ArtifactChart::Line
        | fluxdb_core::ArtifactChart::Bar
        | fluxdb_core::ArtifactChart::Area => html_xy_chart_svg(series),
    }
}

/// 直角坐标系图体：网格、刻度与分类轴共用，中间按类型画柱、折线或面积。
fn html_xy_chart_svg(series: &AiReportChartSeries) -> String {
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
    // 分类中点：折线、面积与柱状共用同一条分类轴。
    let centers: Vec<f64> = (0..series.points.len())
        .map(|index| CHART_LEFT + band * (index as f64 + 0.5))
        .collect();
    match series.chart {
        fluxdb_core::ArtifactChart::Bar => {
            let bar_width = (band * 0.6).max(1.);
            for (point, center) in series.points.iter().zip(&centers) {
                let y = scale.y(point.y);
                let top = y.min(baseline);
                let height = (y - baseline).abs().max(1.);
                svg.push_str(&format!(
                    "<rect x=\"{:.2}\" y=\"{top:.2}\" width=\"{bar_width:.2}\" height=\"{height:.2}\" rx=\"2\" fill=\"#5b7cfa\"/>\n",
                    center - bar_width / 2.
                ));
            }
        }
        // 折线与面积同一条折线路径；面积另外把折线下方填成半透明色块，强调量级。
        fluxdb_core::ArtifactChart::Line | fluxdb_core::ArtifactChart::Area => {
            if series.chart == fluxdb_core::ArtifactChart::Area {
                let mut polygon: Vec<String> = series
                    .points
                    .iter()
                    .zip(&centers)
                    .map(|(point, center)| format!("{center:.2},{:.2}", scale.y(point.y)))
                    .collect();
                let left = centers.first().copied().unwrap_or(CHART_LEFT);
                let right = centers.last().copied().unwrap_or(CHART_LEFT);
                polygon.push(format!("{right:.2},{baseline:.2}"));
                polygon.push(format!("{left:.2},{baseline:.2}"));
                svg.push_str(&format!(
                    "<polygon points=\"{}\" fill=\"#5b7cfa\" fill-opacity=\"0.18\"/>\n",
                    polygon.join(" ")
                ));
            }
            let polyline = series
                .points
                .iter()
                .zip(&centers)
                .map(|(point, center)| format!("{center:.2},{:.2}", scale.y(point.y)))
                .collect::<Vec<_>>()
                .join(" ");
            svg.push_str(&format!(
                "<polyline points=\"{polyline}\" fill=\"none\" stroke=\"#5b7cfa\" stroke-width=\"2\"/>\n"
            ));
            for (point, center) in series.points.iter().zip(&centers) {
                svg.push_str(&format!(
                    "<circle cx=\"{center:.2}\" cy=\"{:.2}\" r=\"2.5\" fill=\"#5b7cfa\"/>\n",
                    scale.y(point.y)
                ));
            }
        }
        // 条形与饼图由 `html_chart_svg` 分派到各自的绘制函数，不会走到这里。
        fluxdb_core::ArtifactChart::BarHorizontal | fluxdb_core::ArtifactChart::Pie => {}
    }
    // 基线：纵轴 0 的位置
    svg.push_str(&format!(
        "<line x1=\"{CHART_LEFT}\" y1=\"{baseline:.2}\" x2=\"{:.2}\" y2=\"{baseline:.2}\" stroke=\"#c9ced8\" stroke-width=\"1\"/>\n",
        CHART_WIDTH - CHART_RIGHT
    ));
    // 分类标签：旋转显示完整值（静态文档比弹框宽裕，不截断可避免同名歧义）。
    for (point, center) in series.points.iter().zip(&centers) {
        let label_y = CHART_HEIGHT - CHART_BOTTOM + 14.;
        svg.push_str(&format!(
            "<text x=\"{center:.2}\" y=\"{label_y:.2}\" text-anchor=\"end\" font-size=\"10\" fill=\"#5a6270\" transform=\"rotate(-35 {center:.2} {label_y:.2})\">{}</text>\n",
            html_escape(&point.x)
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

/// 条形（横向柱状）图体：分类名在左侧纵向排布，数值沿横向增长，比竖排长标签好读。
fn html_bar_horizontal_svg(series: &AiReportChartSeries) -> String {
    let scale = ChartScale::new(&series.points);
    let gutter = html_category_gutter(&series.points);
    let plot_left = gutter;
    let plot_right = CHART_WIDTH - CHART_RIGHT;
    let plot_width = plot_right - plot_left;
    let plot_bottom = CHART_HEIGHT - CHART_HBAR_BOTTOM;
    let band = (plot_bottom - CHART_TOP) / series.points.len().max(1) as f64;
    let bar_height = (band * 0.6).max(1.);
    let zero_x = plot_left + scale.ratio(0.) * plot_width;
    let mut svg = format!(
        "<svg viewBox=\"0 0 {CHART_WIDTH} {CHART_HEIGHT}\" role=\"img\" aria-label=\"{}\">\n",
        html_escape(&format!("{} 按 {}", series.y, series.x))
    );
    // 数值轴横置：纵向网格 + 底部刻度。
    for tick in scale.ticks() {
        let x = plot_left + scale.ratio(tick) * plot_width;
        svg.push_str(&format!(
            "<line x1=\"{x:.2}\" y1=\"{CHART_TOP}\" x2=\"{x:.2}\" y2=\"{plot_bottom:.2}\" stroke=\"#e6e8ee\" stroke-width=\"1\"/>\n"
        ));
        svg.push_str(&format!(
            "<text x=\"{x:.2}\" y=\"{:.2}\" text-anchor=\"middle\" font-size=\"10\" fill=\"#8a8f98\">{}</text>\n",
            plot_bottom + 14.,
            html_number(tick)
        ));
    }
    for (index, point) in series.points.iter().enumerate() {
        let center = CHART_TOP + band * (index as f64 + 0.5);
        let value_x = plot_left + scale.ratio(point.y) * plot_width;
        let left = value_x.min(zero_x);
        let width = (value_x - zero_x).abs().max(1.);
        svg.push_str(&format!(
            "<rect x=\"{left:.2}\" y=\"{:.2}\" width=\"{width:.2}\" height=\"{bar_height:.2}\" rx=\"2\" fill=\"#5b7cfa\"/>\n",
            center - bar_height / 2.
        ));
        svg.push_str(&format!(
            "<text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"end\" font-size=\"10\" fill=\"#5a6270\">{}</text>\n",
            plot_left - 8.,
            center + 3.5,
            html_escape(&html_truncate_to_width(&point.x, gutter - 12.))
        ));
    }
    // 基线：数值轴 0 的位置（全部为正值时贴住绘图区左端）。
    svg.push_str(&format!(
        "<line x1=\"{zero_x:.2}\" y1=\"{CHART_TOP}\" x2=\"{zero_x:.2}\" y2=\"{plot_bottom:.2}\" stroke=\"#c9ced8\" stroke-width=\"1\"/>\n"
    ));
    svg.push_str("</svg>\n");
    svg
}

/// 饼图配色：静态文档固定一组低饱和分类色，顺序稳定，不随主题或悬停变化。
const CHART_PIE_COLORS: [&str; 8] = [
    "#5b7cfa", "#4f9a8b", "#9a7fc0", "#c08a5b", "#c07a8f", "#5b9ac0", "#8aa05b", "#8a8f98",
];

/// 饼图图体：切片按真实数值占比，右侧图例给出完整分类名、真实数值与占比。
fn html_pie_svg(series: &AiReportChartSeries) -> String {
    let total: f64 = series.points.iter().map(|point| point.y).sum();
    let (center_x, center_y, radius) = (CHART_PIE_CENTER_X, CHART_HEIGHT / 2., CHART_PIE_RADIUS);
    let mut svg = format!(
        "<svg viewBox=\"0 0 {CHART_WIDTH} {CHART_HEIGHT}\" role=\"img\" aria-label=\"{}\">\n",
        html_escape(&format!("{} 按 {}", series.y, series.x))
    );
    // 从 12 点方向起顺时针铺开，每段圆心角按真实占比计算。
    let mut start = -std::f64::consts::FRAC_PI_2;
    for (index, point) in series.points.iter().enumerate() {
        let sweep = std::f64::consts::TAU * (point.y / total);
        let end = start + sweep;
        let color = CHART_PIE_COLORS[index % CHART_PIE_COLORS.len()];
        if sweep >= std::f64::consts::TAU - 1e-9 {
            // 单个分类占满整圈：圆弧起止点重合画不出路径，用整圆代替。
            svg.push_str(&format!(
                "<circle cx=\"{center_x:.2}\" cy=\"{center_y:.2}\" r=\"{radius:.2}\" fill=\"{color}\"/>\n"
            ));
        } else if sweep > 1e-9 {
            let (x1, y1) = (
                center_x + radius * start.cos(),
                center_y + radius * start.sin(),
            );
            let (x2, y2) = (
                center_x + radius * end.cos(),
                center_y + radius * end.sin(),
            );
            let large_arc = usize::from(sweep > std::f64::consts::PI);
            svg.push_str(&format!(
                "<path d=\"M {center_x:.2} {center_y:.2} L {x1:.2} {y1:.2} A {radius:.2} {radius:.2} 0 {large_arc} 1 {x2:.2} {y2:.2} Z\" fill=\"{color}\"/>\n"
            ));
        }
        // 0 值切片不画扇面，但图例仍然列出它的真实数值。
        let row_y = CHART_PIE_LEGEND_TOP + CHART_PIE_LEGEND_STEP * index as f64;
        svg.push_str(&format!(
            "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{color}\"/>\n",
            CHART_PIE_LEGEND_X,
            row_y - 9.
        ));
        svg.push_str(&format!(
            "<text x=\"{:.2}\" y=\"{row_y:.2}\" font-size=\"12\" fill=\"#3c424d\">{} · {} · {:.1}%</text>\n",
            CHART_PIE_LEGEND_X + 18.,
            html_escape(&html_truncate_to_width(&point.x, CHART_PIE_LEGEND_TEXT_PX)),
            html_number(point.y),
            point.y / total * 100.
        ));
        start = end;
    }
    svg.push_str("</svg>\n");
    svg
}

/// 分类标签栏宽度估算：SVG 不能测量文本，按 10px 字号下半角 5.5px、CJK/全角 11px 估算
/// （与弹框分类轴同一估算口径），再留出标签与绘图区的间距并夹在可绘制范围内。
fn html_category_gutter(points: &[AiReportChartPoint]) -> f64 {
    let widest = points
        .iter()
        .map(|point| html_text_width(&point.x))
        .fold(0.0_f64, f64::max);
    (widest + 12.).clamp(CHART_GUTTER_MIN, CHART_GUTTER_MAX)
}

fn html_text_width(text: &str) -> f64 {
    text.chars()
        .map(|ch| if ch.is_ascii() { CHART_HALF_UNIT_PX } else { CHART_FULL_UNIT_PX })
        .sum()
}

/// 分类名按可用宽度截断并加省略号：SVG 文本没有省略号，超出绘图区会被 viewBox 直接裁掉。
fn html_truncate_to_width(text: &str, max_px: f64) -> String {
    if html_text_width(text) <= max_px {
        return text.to_string();
    }
    let budget = (max_px - CHART_FULL_UNIT_PX).max(0.);
    let mut out = String::new();
    let mut used = 0.0;
    for ch in text.chars() {
        let width = if ch.is_ascii() {
            CHART_HALF_UNIT_PX
        } else {
            CHART_FULL_UNIT_PX
        };
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}
