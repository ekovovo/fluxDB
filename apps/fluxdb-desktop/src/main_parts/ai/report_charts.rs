// T040 报告弹框的图表块：把「真实点 + 声明类型」画成原生图表。
//
// 只画真实点：真实点由 App 层按声明字段从快照读取（`AiReportView::chart_series`），
// 这里不补 0、不推算；分类轴文本按可用宽度预算截断并保证唯一，完整值仍在 tooltip 中。
use gpui_component::chart::{BarChart, LineChart};

/// 图表数据点：`x` 为原始分类值（完整、用于 tooltip 与折线身份），
/// `display` 为分类轴实际绘制的文本（按可用宽度截断并保证唯一），`y` 为真实数值
/// （来自快照，不是模型编造的数字）。
struct AiReportPoint {
    x: gpui::SharedString,
    display: gpui::SharedString,
    y: f64,
}

/// 从快照行里按列名取出图表点：只读真实数值，null/非数值点直接省略。
/// 分类轴文本预算：半角字符计 1 个单位，CJK/全角计 2。绘图区轴标签字号 10px，
/// 半角约 5.5px/单位、全角约 11px，这里按单位估算宽度并预留相邻标签间距；
/// 估算偏乐观时只截断真正放不下的长标签（tooltip 始终显示完整值）。
const AI_BAND_UNIT_PX: f32 = 5.5;
const AI_BAND_GAP_PX: f32 = 8.;

fn ai_report_char_units(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else {
        2
    }
}

fn ai_report_text_units(text: &str) -> usize {
    text.chars().map(ai_report_char_units).sum()
}

/// 按单位预算截断并加省略号（省略号本身也按全角计 2 个单位）；预算不足时尽量保留前缀。
fn ai_report_truncate_units(text: &str, budget_units: usize) -> String {
    if ai_report_text_units(text) <= budget_units {
        return text.to_string();
    }
    let limit = budget_units.saturating_sub(ai_report_char_units('…'));
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = ai_report_char_units(ch);
        if used + width > limit {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

/// 分类轴绘制文本：按可用宽度截断，并保证结果**互不相同**。
///
/// `BarChart` 的 band 值既是轴标签也是 scale 身份，若截断后出现同名，
/// 会把不同分类并成同一个 band；因此这里对截断重名追加序号消除歧义，
/// 完整原值仍通过 tooltip 展示。
fn ai_report_band_labels(categories: &[String], available_px: f32) -> Vec<String> {
    let count = categories.len().max(1);
    let band_px = (available_px.max(120.) / count as f32).max(1.);
    let budget = (((band_px - AI_BAND_GAP_PX) / AI_BAND_UNIT_PX).floor() as i64)
        .clamp(3, 40) as usize;
    let mut labels: Vec<String> = categories
        .iter()
        .map(|text| ai_report_truncate_units(text, budget))
        .collect();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, label) in labels.iter_mut().enumerate() {
        if used.insert(label.clone()) {
            continue;
        }
        let mut suffix = 2usize;
        loop {
            // 后缀也计入宽度预算：宽裕时用「·N」，极窄时退化为纯数字，保证标签不超宽。
            let suffix_text = if budget >= ai_report_char_units('·') + 3 {
                format!("·{suffix}")
            } else {
                format!("{suffix}")
            };
            let base = ai_report_truncate_units(
                &categories[index],
                budget.saturating_sub(ai_report_text_units(&suffix_text)),
            );
            let candidate = format!("{base}{suffix_text}");
            if used.insert(candidate.clone()) {
                *label = candidate;
                break;
            }
            suffix += 1;
            if suffix > 9999 {
                break;
            }
        }
    }
    labels
}

fn ai_report_chart_block(
    report: &fluxdb_app::AiReportView,
    result_ref: &str,
    chart: fluxdb_core::ArtifactChart,
    x: &str,
    y: &str,
    available_px: f32,
    colors: UiColors,
) -> gpui::AnyElement {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("{y} 按 {x}")),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child(match chart {
                    fluxdb_core::ArtifactChart::Line => "折线图",
                    fluxdb_core::ArtifactChart::Bar => "柱状图",
                }),
        );
    let body = match ai_report_source(report, result_ref) {
        None => ai_report_placeholder("来源不可解析，图表未渲染。", colors),
        Some(_) => {
            // 真实点由 App 层按声明字段读取：与指标条、导出的 HTML 图表同一口径。
            match report.chart_series(result_ref, chart, x, y) {
                None => ai_report_placeholder("来源没有可用数值点，图表未渲染。", colors),
                Some(series) => {
                    let mut points: Vec<AiReportPoint> = series
                        .points
                        .into_iter()
                        .map(|point| {
                            let label = gpui::SharedString::from(point.x);
                            AiReportPoint {
                                x: label.clone(),
                                display: label,
                                y: point.y,
                            }
                        })
                        .collect();
                let plot_id =
                    gpui::SharedString::from(format!("ai-report-chart:{result_ref}:{x}:{y}"));
                let chart_element = match chart {
                    fluxdb_core::ArtifactChart::Line => {
                        let stroke = colors.info;
                        LineChart::new(points)
                            .id(plot_id)
                            .x(|point| point.x.clone())
                            .y(|point| point.y)
                            .stroke(stroke)
                            .linear()
                            .into_any_element()
                    }
                    fluxdb_core::ArtifactChart::Bar => {
                        // 分类轴文本按可用宽度自适应，避免长标签互相遮挡；
                        // band 值同时是 scale 身份，必须保持唯一（tooltip 仍显示完整值）。
                        let displays = ai_report_band_labels(
                            &points
                                .iter()
                                .map(|point| point.x.to_string())
                                .collect::<Vec<_>>(),
                            available_px,
                        );
                        for (point, display) in points.iter_mut().zip(displays) {
                            point.display = gpui::SharedString::from(display);
                        }
                        let fill = colors.info;
                        BarChart::new(points)
                            .id(plot_id)
                            .band(|point| point.display.clone())
                            .value(|point| point.y)
                            .tooltip_title(|point| point.x.clone())
                            .fill(move |_, _, _, _| fill)
                            .corner_radii(px(2.))
                            .into_any_element()
                    }
                };
                div()
                    .h(px(240.))
                    .w_full()
                    .debug_selector(|| "ai-report-chart".into())
                    .child(chart_element)
                    .into_any_element()
                }
            }
        }
    };
    div()
        .w_full()
        .rounded(colors.radius)
        .border_1()
        .border_color(colors.border_soft)
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(header)
        .child(body)
        .into_any_element()
}

