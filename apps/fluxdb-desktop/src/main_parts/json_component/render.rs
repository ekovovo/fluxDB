// json_component/render.rs —— JSON 组件渲染。
//
// - `Preview`：复用 `json_editor` 的行拆分（`json_editor_rows`）、语法高亮
//   （`redis_json_text_row`）与主题（`JsonEditorTheme`），非法 JSON 时安全回退为原文。
// - `Edit`：绘制组件内部持有的多行代码编辑器（JSON 语法高亮），换行不会被吞掉。

/// 把组件渲染成展示元素（`component.rs` 的渲染入口）。
#[allow(dead_code)] // 完整渲染入口（含编辑态）；Workbench 轻量接入走 `render_preview`
fn json_component_view(
    component: &JsonComponent,
    colors: UiColors,
    _window: &mut Window,
    _cx: &mut Context<NavicatMain>,
) -> Div {
    let theme = JsonEditorTheme::from_colors(colors);
    match component.mode {
        JsonMode::Preview => {
            let rows = json_editor_rows(component.state());
            // 只读预览：结构化行 + 语法高亮；非法 JSON 时 `json_editor_rows` 会回退为原文行。
            json_component_rows_block(&rows, theme, component.config.indent_size, colors)
        }
        JsonMode::Edit => {
            // 编辑态：复用组件内部持有的多行编辑框。调用方应在 `begin_edit(window, cx)`
            // 后进入此分支；此处直接绘制组件内部输入实体。
            json_component_edit_body(component, colors, false)
        }
    }
}

/// 只读预览：渲染结构化 JSON 行（带语法高亮）。
///
/// 内容高度按其行数自然增长，由外层滚动容器（如 Workbench 结果卡片 body）负责纵向滚动；
/// 非法 JSON 时 `json_editor_rows` 回退为原文行，保证预览始终可读、不空白。
fn json_component_rows_block(
    rows: &[JsonViewRow],
    theme: JsonEditorTheme,
    indent_size: usize,
    colors: UiColors,
) -> Div {
    div()
        .size_full()
        .min_h(px(0.))
        .bg(colors.input_bg)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .children(rows.iter().map(|row| {
                    redis_json_text_row(row, theme, indent_size, true)
                })),
        )
}

/// 编辑态主体：绘制组件内部持有的多行编辑框（组件自持，不向外暴露）。
fn json_component_edit_body(component: &JsonComponent, colors: UiColors, applying: bool) -> Div {
    let theme = JsonEditorTheme::from_colors(colors);
    let diagnostic = component.diagnostic();
    let border = if diagnostic.is_some() {
        theme.error_border
    } else {
        colors.border
    };
    let body = div()
        .size_full()
        .min_h(px(0.))
        .rounded(colors.radius)
        .border_1()
        .border_color(border)
        .bg(colors.input_bg)
        .overflow_hidden();
    let Some(input) = component.input() else {
        // 防御：Edit 模式未初始化编辑框时回退为原文，避免空白。
        return body.child(
            div()
                .px_2()
                .py_2()
                .font_family(crate::PLATFORM_MONOSPACE_FONT)
                .text_size(px(13.))
                .text_color(colors.text)
                .child(component.source()),
        );
    };
    body.child(
        Editor::new(input)
            .appearance(false)
            .bordered(false)
            .disabled(applying)
            .w_full()
            .h_full()
            .px_2()
            .py_2()
            .font_family(crate::PLATFORM_MONOSPACE_FONT)
            .text_size(px(13.)),
    )
}
