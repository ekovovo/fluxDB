// json_component/component.rs —— 可复用的 JSON 组件核心。
//
// 与 `json_editor`（作为 `NavicatMain` 的一部分工作）不同，`JsonComponent` 是一个
// 自包含单元：它只依赖 `json_editor` 中已抽出的纯逻辑（校验 / pretty / 折叠 / 诊断），
// 自己持有值、展示状态，并在 `Edit` 模式内部创建和管理多行编辑器。
//
// 调用方只需传入 `value`、模式与展示配置，不直接持有输入框状态：
// - `Preview`：只读展示，内部不创建编辑框。
// - `Edit`：内部创建并维护 `EditorState`（多行 + JSON 语法高亮），调用方通过组件方法读写。

/// JSON 组件：一个可复用的 JSON 展示 / 编辑单元。
///
/// 通过 `JsonMode` 参数选择行为；`EditorState` 仅存在于 `Edit` 模式且始终由组件内部持有。
pub(crate) struct JsonComponent {
    mode: JsonMode,
    config: JsonEditorConfig,
    /// 纯展示 / 折叠 / 诊断状态（复用 `json_editor::types` 的 `JsonEditorState`）。
    state: JsonEditorState,
    /// `Edit` 模式内部持有的多行编辑器；`Preview` 模式可为 `None`。调用方不直接接触该字段。
    ///
    /// 使用 `EditorState` 而非 `InputState`：
    /// - 后者是单行输入，`set_value` 会删除换行，pretty 后的 JSON 会被压成一行；
    /// - 前者是代码编辑器布局，配合 `language("json")` 提供 JSON 语法高亮与诊断高亮。
    input: Option<Entity<EditorState>>,
    /// 折叠路径集合，随 `load` / `toggle_fold` 维护，用于重建折叠展示文本。
    folds: BTreeSet<String>,
}

impl JsonComponent {
    /// 以给定配置创建一个只读组件（不创建编辑框，供轻量预览调用点使用）。
    pub(crate) fn new(config: JsonEditorConfig) -> Self {
        JsonComponent {
            mode: JsonMode::Preview,
            state: JsonEditorState::new(config.clone()),
            config,
            input: None,
            folds: BTreeSet::new(),
        }
    }

    /// 以指定模式创建组件；`Edit` 模式会立即创建多行编辑框。
    pub(crate) fn new_with_mode(
        config: JsonEditorConfig,
        mode: JsonMode,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) -> Self {
        let mut component = JsonComponent::new(config);
        component.set_mode(mode);
        // 需要长期持有编辑框的调用方（如 Redis 值详情）在 Preview 阶段也要能拿到实体
        // 以建立事件订阅，因此这里统一创建；仅 `new()` 的轻量预览调用点不创建。
        component.ensure_input(window, cx);
        component
    }

    /// 当前展示模式。
    #[allow(dead_code)] // 组件公开 API：供调用方判断当前模式
    pub(crate) fn mode(&self) -> JsonMode {
        self.mode
    }

    /// 切换展示模式。编辑框一旦创建就保留（事件订阅绑定在该实体上），仅切换渲染分支。
    pub(crate) fn set_mode(&mut self, mode: JsonMode) {
        self.mode = mode;
        self.state.editing = mode.is_editable();
    }

    /// 组件持有的只读状态。
    pub(crate) fn state(&self) -> &JsonEditorState {
        &self.state
    }

    /// 组件持有的可变状态（调用方维护权威全文 / 折叠 / 诊断）。
    pub(crate) fn state_mut(&mut self) -> &mut JsonEditorState {
        &mut self.state
    }

    /// 编辑框实体；`Preview` 模式或尚未加载时为 `None`。
    pub(crate) fn input(&self) -> Option<&Entity<EditorState>> {
        self.input.as_ref()
    }

    /// 确保编辑框存在并返回其实体（仅 `Edit` 模式有意义）。
    fn ensure_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) -> Entity<EditorState> {
        self.input
            .get_or_insert_with(|| {
                // 多行 + 软换行：换行符得以保留，长 JSON 行不会横向溢出。
                // `language("json")` 启用 tree-sitter JSON 语法高亮（颜色取自当前主题）。
                cx.new(|cx| EditorState::new(window, cx).language("json").soft_wrap(true))
            })
            .clone()
    }

    /// 载入源文本（纯逻辑，不依赖窗口）：pretty 规整并重建折叠节点，保留仍然存在的折叠路径。
    pub(crate) fn load(&mut self, source: &str) {
        let indent = self.config.indent_size;
        // `format_on_load`：加载即按标准 pretty 规整，作为查看 / 编辑的一致基线。
        let source = if self.config.format_on_load {
            format_pretty(source, indent).unwrap_or_else(|_| source.to_string())
        } else {
            source.to_string()
        };
        // `preserve_fold_state`：为 true 时保留仍存在路径的折叠，否则彻底展开。
        let old_folds = if self.config.preserve_fold_state {
            self.folds.clone()
        } else {
            BTreeSet::new()
        };
        match build_pretty(&source, indent, &BTreeSet::new()) {
            Ok(pretty) => {
                let kept = preserve_fold_paths(&old_folds, &pretty.nodes);
                let rebuilt = build_pretty(&source, indent, &kept).unwrap_or(pretty);
                self.folds = kept;
                self.state = JsonEditorState {
                    config: self.config.clone(),
                    source,
                    display: rebuilt.text,
                    diagnostic: None,
                    nodes: rebuilt.nodes,
                    dirty: false,
                    editing: self.mode.is_editable(),
                };
            }
            Err(dia) => {
                self.folds.clear();
                self.state = JsonEditorState {
                    config: self.config.clone(),
                    source,
                    display: String::new(),
                    diagnostic: Some(dia),
                    nodes: Vec::new(),
                    dirty: false,
                    editing: self.mode.is_editable(),
                };
            }
        }
    }

    /// 权威全文（未折叠 JSON）。
    pub(crate) fn source(&self) -> String {
        self.state.source.clone()
    }

    /// 当前诊断；`None` 表示 JSON 合法。
    pub(crate) fn diagnostic(&self) -> Option<JsonEditorDiagnostic> {
        self.state.diagnostic.clone()
    }

    /// 是否处于编辑态（`Edit` 模式且已持有编辑框）。
    #[allow(dead_code)] // 组件公开 API：供调用方判断编辑态
    pub(crate) fn is_editing(&self) -> bool {
        self.mode.is_editable() && self.input.is_some()
    }

    /// 切换某条折叠路径（仅查看态意义；编辑态不展示折叠）。
    pub(crate) fn toggle_fold(&mut self, path: &[JsonKey]) {
        toggle_fold_state(&mut self.state, path);
        self.folds = self.state.folded_set();
    }

    /// 进入编辑态：内部创建多行编辑框并标记 `editing`。
    pub(crate) fn begin_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) {
        self.mode = JsonMode::Edit;
        self.state.editing = true;
        self.ensure_input(window, cx);
    }

    /// 退出编辑态：清空编辑草稿与诊断，回到 `Preview`；编辑框实体保留以维持事件订阅。
    pub(crate) fn cancel_edit(&mut self) {
        self.state.editing = false;
        self.state.dirty = false;
        self.state.diagnostic = None;
        self.mode = JsonMode::Preview;
    }

    /// 写入编辑框全文（如进入编辑态的 pretty 基线、格式化回填）。
    pub(crate) fn set_edit_text(
        &self,
        text: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) {
        if let Some(input) = &self.input {
            let text = text.into();
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    /// 读取编辑框当前全文。
    pub(crate) fn edit_text(&self, cx: &Context<NavicatMain>) -> String {
        self.input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.state.source.clone())
    }

    /// 聚焦编辑框。
    pub(crate) fn focus_edit(&self, window: &mut Window, cx: &mut Context<NavicatMain>) {
        if let Some(input) = &self.input {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// 把 JSON 诊断写入编辑框内建诊断集（当前多行编辑器不支持时静默跳过）。
    pub(crate) fn set_diagnostics(
        &self,
        diagnostic: Option<JsonEditorDiagnostic>,
        cx: &mut Context<NavicatMain>,
    ) {
        use gpui_component::highlighter::{Diagnostic, DiagnosticSeverity};
        let Some(input) = &self.input else {
            return;
        };
        input.update(cx, |input, _| {
            let Some(diagnostics) = input.diagnostics_mut() else {
                return;
            };
            diagnostics.clear();
            if let Some(dia) = diagnostic {
                let start = Position::new(dia.line as u32, dia.column as u32);
                let end = Position::new(dia.line as u32, dia.column as u32 + 1);
                diagnostics.push(
                    Diagnostic::new(start..end, dia.bubble_text())
                        .with_severity(DiagnosticSeverity::Error),
                );
            }
        });
    }

    /// 渲染主体：按当前模式派发到只读预览或编辑态界面（见 `render.rs` 的 `json_component_view`）。
    #[allow(dead_code)] // 完整渲染入口；Redis 值详情按需使用 `render_edit_body` / `render_preview`
    pub(crate) fn render(
        &self,
        colors: UiColors,
        window: &mut Window,
        cx: &mut Context<NavicatMain>,
    ) -> Div {
        json_component_view(self, colors, window, cx)
    }

    /// 编辑态主体：组件内部持有的多行编辑框（调用方只负责外围工具栏 / 保存按钮）。
    pub(crate) fn render_edit_body(&self, colors: UiColors, applying: bool) -> Div {
        json_component_edit_body(self, colors, applying)
    }

    /// 只读预览渲染（`Preview` 模式）：不依赖窗口 / 交互，直接产出结构化高亮预览。
    ///
    /// 这是供轻量调用点（如 Workbench 结果区 `Text / JSON` 切换）复用的入口：
    /// 组件内部按 `Preview` 模态渲染，非法 JSON 自动回退为原文，保证结果始终可读。
    pub(crate) fn render_preview(&self, colors: UiColors) -> Div {
        let rows = json_editor_rows(self.state());
        json_component_rows_block(&rows, JsonEditorTheme::from_colors(colors), self.config.indent_size, colors)
    }
}
