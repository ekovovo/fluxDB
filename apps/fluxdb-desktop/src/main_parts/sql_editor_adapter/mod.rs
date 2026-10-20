// sql_editor_adapter/mod.rs —— SQL 编辑器接入层。
//
// 本模块把 fluxdb-editor-core 的通用编辑协议（LanguageDefinition / SyntaxProvider /
// CompletionProvider / ExecutionAdapter / DecorationProvider）接到 SQL 业务上下文，
// 通过注入的 schema / 补全源提供表、列等业务信息。该模块只做「能力定义」，
// 不直接连接数据库：连接/连接信息一律由构造参数注入。
//
// 由于本文件与 dialect.rs / syntax.rs / completion.rs / execution.rs
// 通过 include! 合并进同一个 `sql_editor_adapter` 模块，所有用于整个模块的共享
// `use` 统一放在本文件顶部，其余文件直接引用同一模块作用域下的类型，不重复 `use`。

use std::future::Future;
use crate::tr;
use gpui::SharedString;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::Instant;

use fluxdb_editor_core::{
    BufferSnapshot, CancellationToken, CodeLens, CodeLensProvider, CompletionContinuation,
    CompletionFuture, CompletionItem, CompletionKind, CompletionProvider,
    CompletionRequest, CompletionResult, Decoration, DecorationProvider, DecorationSet, Diagnostic,
    DiagnosticProvider, ExecuteMode, ExecutionAdapter, ExecutionUnit, Highlight,
    InputEdit, LanguageDefinition, Point, Range, Selection, SignatureInfo, SignatureProvider,
    SyntaxProvider, SyntaxResult, TaskOutcome, TriggerDecision,
};

#[derive(Clone, Debug)]
struct SqlDiagnosticsCache {
    version: u64,
    text_bytes: usize,
    diagnostics: Vec<Diagnostic>,
    statement_ranges: Vec<Range>,
}

#[derive(Clone, Default)]
struct SqlSemanticMetadata {
    columns_by_table: std::collections::HashMap<String, std::collections::HashSet<String>>,
    columns_by_table_exact: std::collections::HashMap<String, std::collections::HashSet<String>>,
    tables: std::collections::HashSet<String>,
    tables_exact: std::collections::HashSet<String>,
    views: std::collections::HashSet<String>,
    views_exact: std::collections::HashSet<String>,
    schemas: std::collections::HashSet<String>,
    schemas_exact: std::collections::HashSet<String>,
    routines: std::collections::HashSet<String>,
    routines_exact: std::collections::HashSet<String>,
    types: std::collections::HashSet<String>,
    types_exact: std::collections::HashSet<String>,
}

/// SQL 编辑器适配器：把 fluxdb-editor-core 的通用协议映射到 SQL 方言 + 注入的业务源。
///
/// 本结构体持有方言与补全来源（schema / 补全索引），本身不做任何网络或数据库操作。
/// provider 相关的方法（parse / complete / execution_units / decorations）全部只读
/// snapshot，由宿主（GPUI 前端）负责调度与落库。
///
/// `status` 是与宿主共享的执行状态仓库：宿主在执行语句前后写入状态，`decorations()`
/// 读取后还原为行背景装饰。作为「状态 → 装饰」的桥，SQL 语义（语句切分、id）全部
/// 收敛在本适配器，通用编辑器只渲染通用的状态行背景。
pub struct SqlAdapter {
    /// 当前 SQL 方言。
    dialect: SqlDialect,
    /// 与宿主编辑器绑定的性能日志 id；未绑定时为 0。
    editor_id: AtomicU64,
    /// 注入的补全来源（schema / 补全索引），保存为 trait object 以解耦具体业务实现。
    sources: Vec<Arc<dyn SqlCompletionSource>>,
    /// 语句执行状态仓库（与宿主共享，执行前后写入）。
    status: Option<SqlStatusStore>,
    /// 可选的宿主 SQL 上下文解析器。仅在显式补全时调用，避免逐字输入访问 connector。
    completion_resolver: Option<SqlCompletionResolver>,
    /// 可选的宿主「选中项 metadata 详情」解析器（候选右侧面板异步填充）。
    documentation_resolver: Option<SqlDocumentationResolver>,
    /// 按文档版本缓存 CodeLens 全集；视口变化只过滤，不重复切分全文。
    code_lens_cache: Arc<Mutex<Option<(u64, usize, usize, Vec<CodeLens>)>>>,
    /// 按文档版本缓存带稳定 id 的语句列表，供执行状态装饰复用。
    statement_runs_cache: Arc<Mutex<Option<(u64, usize, usize, Arc<Vec<SqlStatementRun>>)>>>,
    /// 按连续编辑缓存 SQL parser/tree；版本不连续时自动回退全量解析。
    syntax_cache: Arc<Mutex<Option<SqlSyntaxCache>>>,
    /// 补全采用 latest-wins；metadata provider 在批次边界检查该 epoch。
    ///
    /// 保持原始 `Arc<AtomicU64>`（而非 [`CancellationToken`]）：补全既要
    /// `store` 采纳控制器 request_id，又要同一 Arc 传入 resolver 共享取消，二者
    /// CancellationToken 均不直接支持（见 CancellationToken::inner 说明。
    /// ponytail: 若未来统一 resolver 签名可收敛）。
    completion_latest_request: Arc<AtomicU64>,
    /// 解析任务采用 latest-wins；旧任务在复制全文前直接退出。
    syntax_latest_request: CancellationToken,
    /// 诊断解析同样采用 latest-wins，避免旧版本全文解析继续占用 CPU。
    diagnostics_latest_request: CancellationToken,
    /// 同一快照可能被布局/宿主重复请求；缓存完整诊断结果避免重复全文扫描。
    diagnostics_cache: Arc<Mutex<Option<SqlDiagnosticsCache>>>,
    /// 静态方言/schema 候选的惰性缓存；来源变化时由 builder 失效。
    completion_items_cache: OnceLock<Arc<Vec<CompletionItem>>>,
    /// 静态 schema 语义元数据的惰性缓存；来源变化时由 builder 失效。
    semantic_metadata_cache: OnceLock<SqlSemanticMetadata>,
    /// 外部 source 可能动态变化，不能复用静态候选缓存。
    completion_items_cacheable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SqlCompletionResponse {
    pub items: Vec<CompletionItem>,
    /// Provider 结果是否不完整；下一次编辑应重新触发 provider。
    pub has_more: bool,
}

/// 已知 SQL 函数 → 结果类型（DM-802，轻量白名单）。
///
/// 不建通用类型代数，只对常用聚合/标量函数给出结果类型，用于函数补全项的
/// detail 增强。未知函数返回 None（保守）。键为小写函数名。
pub fn function_result_type(fn_name: &str) -> Option<SharedString> {
    Some(match fn_name.to_ascii_lowercase().as_str() {
        "count" | "sum" | "count_big" | "if" | "ifnull" | "coalesce" => tr("app.completion.type.numeric"),
        "avg" | "max" | "min" => tr("app.completion.type.numeric_date_text"),
        "concat" | "substring" | "substr" | "lower" | "lcase" | "upper" | "ucase"
        | "trim" | "ltrim" | "rtrim" | "replace" | "cast" | "convert" | "now"
        | "current_date" | "current_timestamp" => tr("app.completion.type.text_date"),
        "len" | "length" | "char_length" | "round" | "floor" | "ceil" | "abs"
        | "mod" | "rand" => tr("app.completion.type.numeric"),
        "date" | "year" | "month" | "day" | "hour" | "minute" | "second" => tr("app.completion.type.date_numeric"),
        _ => return None,
    })
}

pub type SqlCompletionResolver = Arc<dyn Fn(String, usize, bool, Arc<AtomicU64>, u64) -> Result<SqlCompletionResponse, String> + Send + Sync>;

/// 补全候选右侧「metadata 详情」的异步状态。由候选项切换触发，latest-wins 取消。
#[derive(Clone, Debug, PartialEq)]
pub enum SqlDocState {
    /// 已触发但尚未就绪（加载中 / 被新选择取消）。详情不缓存，故无数据复用。
    Loading,
    /// 解析成功；`String` 为渲染文本（表/视图→列清单，列→注释，函数等→名称）。
    Ready(String),
    /// 解析失败；`String` 为原因（对象不在索引 / 无注释 / 无可用文档）。
    Error(String),
}

/// 详情解析回调：按候选 type/label/schema/内联注释惰性解析完整 metadata。
///
/// 由宿主（table_state）绑定 fluxdb-app 的 `completion_documentation_for_with_cancel`：
/// 优先读内存 CompletionIndex，索引未覆盖该对象时才按 (库, schema, 表) 取一次元数据并
/// 写回索引。`latest_request`+`request_id` 做 latest-wins，保证切换选中项时旧结果不覆盖
/// 新选择。UI 不直接访问数据库。
pub type SqlDocumentationResolver = Arc<
    dyn Fn(
            fluxdb_editor_core::CompletionKind,
            String,
            Option<String>,
            Option<String>,
            Arc<AtomicU64>,
            u64,
        ) -> SqlDocState
        + Send
        + Sync,
>;

const COMPLETION_WINDOW_BYTES: usize = 64 * 1024;
const CODE_LENS_SYNC_SCAN_LIMIT: usize = 128 * 1024;
const CODE_LENS_VISIBLE_WINDOW_BYTES: usize = 16 * 1024;

fn completion_window(snapshot: &BufferSnapshot, cursor: usize) -> (String, usize) {
    let cursor = cursor.min(snapshot.len());
    let start = cursor.saturating_sub(COMPLETION_WINDOW_BYTES);
    let end = cursor
        .saturating_add(COMPLETION_WINDOW_BYTES)
        .min(snapshot.len());
    (snapshot.text_in_range(Range::new(start, end)), start)
}

fn sql_signature_label(name: &str, qualifier: Option<&str>) -> String {
    let base = match name.to_ascii_lowercase().as_str() {
        "count" => "COUNT(expr)".to_string(),
        "sum" => "SUM(expr)".to_string(),
        "avg" => "AVG(expr)".to_string(),
        "min" => "MIN(expr)".to_string(),
        "max" => "MAX(expr)".to_string(),
        "coalesce" => "COALESCE(value, fallback)".to_string(),
        "ifnull" => "IFNULL(value, fallback)".to_string(),
        "concat" => "CONCAT(value1, value2)".to_string(),
        "substr" => "SUBSTR(string, start, length)".to_string(),
        "substring" => "SUBSTRING(string, start, length)".to_string(),
        _ => format!("{name}()"),
    };
    // 保留 schema 限定前缀（`sales.count(` → `sales.COUNT(expr)`），与调用处一致。
    match qualifier.filter(|q| !q.is_empty()) {
        Some(q) => format!("{q}.{base}"),
        None => base,
    }
}

fn build_code_lenses(text: &str) -> Vec<CodeLens> {
    split_statements(text)
        .into_iter()
        .flat_map(|range| {
            [
                CodeLens {
                    range,
                    title: "Run".to_string(),
                    action: "sql.run".to_string(),
                },
                CodeLens {
                    range,
                    title: "Select".to_string(),
                    action: "sql.select".to_string(),
                },
            ]
        })
        .collect()
}

fn build_code_lenses_from_snapshot(snapshot: &BufferSnapshot) -> Vec<CodeLens> {
    split_statement_ranges_snapshot(snapshot)
        .into_iter()
        .flat_map(|range| {
            [
                CodeLens {
                    range,
                    title: "Run".to_string(),
                    action: "sql.run".to_string(),
                },
                CodeLens {
                    range,
                    title: "Select".to_string(),
                    action: "sql.select".to_string(),
                },
            ]
        })
        .collect()
}

/// 语句执行状态仓库别名：`Arc<Mutex<SqlStatementStatusMap>>`，供宿主与适配器共享。
///
/// `SqlStatementStatusMap` 内类型（`SqlStatementId(u64)` / `SqlStatementStatus`）均为
/// `Send + Sync`，因此该别名满足 `DecorationProvider: Send + Sync` 的对象安全约束。
pub type SqlStatusStore = Arc<Mutex<SqlStatementStatusMap>>;

impl SqlAdapter {
    /// 以指定方言构造适配器。
    pub fn new(dialect: SqlDialect) -> Self {
        Self {
            dialect,
            editor_id: AtomicU64::new(0),
            sources: Vec::new(),
            status: None,
            completion_resolver: None,
            documentation_resolver: None,
            code_lens_cache: Arc::new(Mutex::new(None)),
            statement_runs_cache: Arc::new(Mutex::new(None)),
            syntax_cache: Arc::new(Mutex::new(None)),
            completion_latest_request: Arc::new(AtomicU64::new(0)),
            syntax_latest_request: CancellationToken::default(),
            diagnostics_latest_request: CancellationToken::default(),
            diagnostics_cache: Arc::new(Mutex::new(None)),
            completion_items_cache: OnceLock::new(),
            semantic_metadata_cache: OnceLock::new(),
            completion_items_cacheable: true,
        }
    }

    /// 绑定宿主编辑器的性能日志 id。provider 创建后、编辑器开始接收输入前调用。
    pub fn set_editor_id(&self, editor_id: u64) {
        self.editor_id.store(editor_id, Ordering::Release);
    }

    fn editor_id(&self) -> u64 {
        self.editor_id.load(Ordering::Acquire)
    }

    /// 注入执行状态仓库。宿主在执行语句前后写入状态，`decorations()` 据此还原行背景。
    pub fn with_status_store(mut self, store: SqlStatusStore) -> Self {
        self.status = Some(store);
        self
    }

    /// 注入结构化 schema 上下文（表、列），用于补全。
    pub fn with_schema(mut self, schema: SqlSchemaContext) -> Self {
        self.sources.push(Arc::new(schema));
        self.completion_items_cache = OnceLock::new();
        self.semantic_metadata_cache = OnceLock::new();
        self.diagnostics_cache = Arc::new(Mutex::new(None));
        self
    }

    /// 注入完整 SQL 上下文补全。resolver 由 app/宿主负责线程安全地调度 connector 和索引。
    pub fn with_completion_resolver(mut self, resolver: SqlCompletionResolver) -> Self {
        self.completion_resolver = Some(resolver);
        self
    }

    /// 绑定宿主「选中项 metadata 详情」解析器（候选右侧面板）。
    pub fn with_documentation_resolver(mut self, resolver: SqlDocumentationResolver) -> Self {
        self.documentation_resolver = Some(resolver);
        self
    }

    /// 解析选中项详情的异步结果。由编辑器在候选切换时调用；`latest_request`+`request_id`
    /// 做 latest-wins 取消。无解析器 / 非对象候选项时返回本地降级（列注释或名称）。
    pub fn resolve_documentation(
        &self,
        kind: fluxdb_editor_core::CompletionKind,
        label: &str,
        comment: Option<&str>,
        schema: Option<&str>,
        latest_request: Arc<AtomicU64>,
        request_id: u64,
    ) -> SqlDocState {
        if let Some(resolver) = &self.documentation_resolver {
            // 有解析器时其结果优先级最高：Loading 也要保留给 UI（不降级覆盖），
            // 保证「异步加载中」与「success/error」三态一致。
            return resolver(
                kind,
                label.to_string(),
                comment.map(str::to_string),
                schema.map(str::to_string),
                latest_request,
                request_id,
            );
        }
        // 无解析器：就地降级为候选已携带的内联信息，绝不去连接数据库。
        match kind {
            fluxdb_editor_core::CompletionKind::Column => match comment {
                Some(text) if !text.is_empty() => SqlDocState::Ready(text.to_string()),
                _ => SqlDocState::Error(tr("app.completion.no_column_comment").to_string()),
            },
            fluxdb_editor_core::CompletionKind::Keyword | fluxdb_editor_core::CompletionKind::Text => {
                SqlDocState::Error(tr("app.completion.no_document").to_string())
            }
            _ => SqlDocState::Ready(label.to_string()),
        }
    }

    /// 注入外部补全索引（任意的 `SqlCompletionSource` trait object）。
    #[allow(dead_code)] // 接入层公开 API，当前经 with_schema 注入，保留供任意来源扩展。
    pub fn with_completion_index(mut self, source: Arc<dyn SqlCompletionSource>) -> Self {
        self.sources.push(source);
        self.completion_items_cache = OnceLock::new();
        self.semantic_metadata_cache = OnceLock::new();
        self.completion_items_cacheable = false;
        self.diagnostics_cache = Arc::new(Mutex::new(None));
        self
    }

    /// 切换当前 SQL 方言。
    #[allow(dead_code)] // 接入层公开 API，方言在建构造时固定，保留供运行时切换。
    pub fn set_dialect(&mut self, dialect: SqlDialect) {
        self.dialect = dialect;
        self.completion_items_cache = OnceLock::new();
        self.semantic_metadata_cache = OnceLock::new();
        self.diagnostics_cache = Arc::new(Mutex::new(None));
    }

    /// 当前方言。
    #[allow(dead_code)] // 接入层公开 API，宿主暂未读取，保留备用。
    pub fn dialect(&self) -> SqlDialect {
        self.dialect
    }

    /// 已注入的补全来源列表（只读）。
    #[allow(dead_code)] // 接入层公开 API，宿主暂未读取，保留备用。
    pub fn sources(&self) -> &[Arc<dyn SqlCompletionSource>] {
        &self.sources
    }

    /// 根据语句首词推断执行模式：EXPLAIN → Explain；SELECT / SHOW / WITH / DESC 等 → Select；
    /// 其余 DML / DDL → Execute。
    fn detect_mode(&self, text: &str) -> ExecuteMode {
        let t = text.trim_start();
        let lower = t.to_lowercase();
        if lower.starts_with("explain") {
            ExecuteMode::Explain
        } else if lower.starts_with("select")
            || lower.starts_with("show")
            || lower.starts_with("with")
            || lower.starts_with("desc")
            || lower.starts_with("describe")
        {
            ExecuteMode::Select
        } else {
            ExecuteMode::Execute
        }
    }

    /// 合并方言关键字与注入补全来源，生成候选补全项。
    fn build_completion_items(&self) -> Arc<Vec<CompletionItem>> {
        if !self.completion_items_cacheable {
            return Arc::new(self.build_completion_items_uncached());
        }
        self.completion_items_cache
            .get_or_init(|| Arc::new(self.build_completion_items_uncached()))
            .clone()
    }

    fn build_completion_items_uncached(&self) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = Vec::new();
        for kw in self.dialect.keywords() {
            let mut item = CompletionItem::new(kw.word, kw.kind);
            item.priority = if kw.kind == CompletionKind::Keyword { 50 } else { 40 };
            item.detail = format!("{} {}", self.dialect.name(), self.kind_label(kw.kind));
            items.push(item);
        }
        for src in &self.sources {
            for schema in src.schemas() {
                let mut item = CompletionItem::new(&schema, CompletionKind::Schema);
                item.priority = 58;
                item.detail = tr("app.completion.kind.schema").to_string();
                items.push(item);
            }
            for table in src.tables() {
                let mut item = CompletionItem::new(&table, CompletionKind::Table);
                item.priority = 60;
                item.detail = tr("app.completion.kind.table").to_string();
                items.push(item);
            }
            for view in src.views() {
                let mut item = CompletionItem::new(&view, CompletionKind::Table);
                item.priority = 59;
                item.detail = tr("app.completion.kind.view").to_string();
                items.push(item);
            }
            for (table, column) in src.columns() {
                let mut item = CompletionItem::new(&column, CompletionKind::Column);
                item.priority = 55;
                item.detail = format!("{}.{}", table, column);
                items.push(item);
            }
            for routine in src.routines() {
                let mut item = CompletionItem::new(&routine, CompletionKind::Function);
                item.priority = 54;
                // DM-802：已知函数补全结果类型（轻量白名单，非通用类型代数）。
                item.detail = match function_result_type(&routine) {
                    Some(ty) => tr("app.completion.function_return").replace("%{ty}", ty.as_ref()),
                    None => tr("app.completion.function_or_procedure").to_string(),
                };
                items.push(item);
            }
            for ty in src.types() {
                let mut item = CompletionItem::new(&ty, CompletionKind::Class);
                item.priority = 53;
                item.detail = tr("app.completion.kind.type").to_string();
                items.push(item);
            }
        }
        // 表/列/模式等标识符候选点击插入时补引号（与解析器补全路径一致）：MySQL 反引号，
        // 其余（SQLite/PostgreSQL 等）双引号。函数/关键字保持原样，避免 `COUNT`() 被误引号。
        let quote = |name: &str| match self.dialect {
            SqlDialect::Mysql => format!("`{name}`"),
            _ => format!("\"{name}\""),
        };
        for item in &mut items {
            match item.kind {
                CompletionKind::Table | CompletionKind::Column | CompletionKind::Schema => {
                    item.insert_text = quote(&item.label);
                }
                _ => {}
            }
        }
        for item in &mut items {
            item.filter_text = item.label.to_ascii_lowercase();
        }
        items
    }

    /// 补全项类型的中文标签。
    fn kind_label(&self, kind: CompletionKind) -> SharedString {
        match kind {
            CompletionKind::Keyword => tr("app.completion.kind.keyword"),
            CompletionKind::Function => tr("app.completion.kind.function"),
            CompletionKind::Table => tr("app.completion.kind.table"),
            CompletionKind::Column => tr("app.completion.kind.column"),
            CompletionKind::Schema => tr("app.completion.kind.schema"),
            CompletionKind::Class => tr("app.completion.kind.type"),
            _ => tr("app.completion.kind.text"),
        }
    }
}

impl SyntaxProvider for SqlAdapter {
    fn highlight_visible(&self, snapshot: &BufferSnapshot, visible: Range) -> Vec<Highlight> {
        let start = visible.start.min(snapshot.len());
        let end = visible.end.min(snapshot.len());
        if start >= end {
            return Vec::new();
        }
        tokenize_sql(
            &snapshot.text_in_range(Range::new(start, end)),
            self.dialect,
        )
        .into_iter()
        .map(|mut highlight| {
            highlight.range.start += start;
            highlight.range.end += start;
            highlight
        })
        .collect()
    }

    /// 对快照做一次真实语法高亮（tree-sitter-sequel 驱动）。
    ///
    /// 与逐字编辑路径配合：`changed` 携带本次编辑的脏区间，用于把 dirty_ranges
    /// 收窄到编辑行，供前端只重绘受影响的行。语法解析仍基于完整快照进行
    /// （tree-sitter 需要完整语法树），但结果只按建模渲染可见行高亮，
    /// 因此不会引入整文档文本拷贝（见设计 5.3 / 九）。
    fn parse(
        &self,
        snapshot: &BufferSnapshot,
        changed: InputEdit,
    ) -> Pin<Box<dyn Future<Output = SyntaxResult> + Send>> {
        let version = snapshot.version();
        let dialect = self.dialect;
        let syntax_cache = self.syntax_cache.clone();
        let latest_request = self.syntax_latest_request.clone();
        let request_id = latest_request.request_id();
        let editor_id = self.editor_id();
        let snapshot = snapshot.clone();
        Box::pin(async move {
            let started = Instant::now();
            if !latest_request.check(request_id) {
                tracing::debug!(
                    target: "gdb_sql_perf",
                    op = "syntax_parse_skipped",
                    task_outcome = TaskOutcome::WorkStopped.as_str(),
                    editor_id,
                    edit_id = changed.edit_id,
                    reason = "stale_before_copy",
                    buffer_version = version,
                    request_id,
                );
                return SyntaxResult {
                    buffer_version: version,
                    highlights: Vec::new(),
                    dirty_ranges: Vec::new(),
                    needs_refinement: false,
                };
            }
            if changed.new_text.contains(';')
                || (changed.new_text.is_empty() && changed.old_range.len() == 1)
            {
                if let Some((highlights, dirty_range, cache_mode)) =
                    try_local_statement_window_highlight(
                        &snapshot,
                        dialect,
                        &changed,
                        version,
                        &syntax_cache,
                        latest_request.inner(),
                        request_id,
                    )
                {
                    tracing::debug!(
                        target: "gdb_sql_perf",
                        op = "syntax_parse",
                        editor_id,
                        edit_id = changed.edit_id,
                        elapsed_us = started.elapsed().as_micros() as u64,
                        buffer_version = version,
                        text_bytes = snapshot.len(),
                        highlight_count = highlights.len(),
                        dirty_range_count = 1,
                        cache_mode,
                        snapshot_copy_us = 0u64,
                    );
                    return SyntaxResult {
                        buffer_version: version,
                        highlights,
                        dirty_ranges: vec![dirty_range],
                        needs_refinement: false,
                    };
                }
            }
            if let Some((highlights, dirty_range, cache_mode)) = try_local_statement_highlight(
                &snapshot,
                dialect,
                &changed,
                version,
                &syntax_cache,
                latest_request.inner(),
                request_id,
            ) {
                let result = SyntaxResult {
                    buffer_version: version,
                    highlights,
                    dirty_ranges: vec![dirty_range],
                    needs_refinement: false,
                };
                tracing::debug!(
                    target: "gdb_sql_perf",
                    op = "syntax_parse",
                    editor_id,
                    edit_id = changed.edit_id,
                    elapsed_us = started.elapsed().as_micros() as u64,
                    buffer_version = version,
                    text_bytes = snapshot.len(),
                    highlight_count = result.highlights.len(),
                    dirty_range_count = 1,
                    cache_mode,
                    snapshot_copy_us = 0u64,
                );
                return result;
            }
            let (highlights, cache_mode) =
                highlight_sql_tree_sitter_cached_snapshot(
                    &snapshot,
                    dialect,
                    &changed,
                    version,
                    &syntax_cache,
                    latest_request.inner(),
                    request_id,
                );
            // 全量路径返回整份文档高亮，dirty 区间取全文首尾，宿主据此 `from_highlights` 复建主存储。
            // 增量路径（statement/statement_window）已在上面返回作用域 patch。
            let result = SyntaxResult {
                buffer_version: version,
                highlights,
                dirty_ranges: vec![Range::new(0, snapshot.len())],
                needs_refinement: matches!(
                    cache_mode,
                    "tokenizer_large" | "statement_refinement"
                ),
            };
            let elapsed_us = started.elapsed().as_micros() as u64;
            tracing::debug!(
                target: "gdb_sql_perf",
                op = "syntax_parse",
                editor_id,
                edit_id = changed.edit_id,
                elapsed_us,
                buffer_version = version,
                text_bytes = snapshot.len(),
                highlight_count = result.highlights.len(),
                dirty_range_count = result.dirty_ranges.len(),
                cache_mode,
                snapshot_copy_us = 0u64,
            );
            if elapsed_us > fluxdb_editor_core::BACKGROUND_BUDGET_US {
                tracing::warn!(
                    target: "gdb_sql_perf",
                    op = "syntax_parse_slow",
                    editor_id,
                    edit_id = changed.edit_id,
                    elapsed_us,
                    buffer_version = version,
                    text_bytes = snapshot.len(),
                    cache_mode,
                );
            }
            result
        })
    }
}

impl DiagnosticProvider for SqlAdapter {
    /// 基于 SQL 语法树返回语法错误诊断（如缺分号、语法错误附近的节点）。
    ///
    /// 同步执行，由前端在后台任务中调用并按版本保护落库（见 Editor::request_diagnostics）。
    /// 最小实现：仅返回 parser error 的 range / severity / message，不含语义诊断。
    fn diagnostics(&self, snapshot: &BufferSnapshot) -> Vec<Diagnostic> {
        let started = Instant::now();
        let version = snapshot.version();
        let text_bytes = snapshot.len();
        let editor_id = self.editor_id();
        if self.completion_items_cacheable {
            if let Ok(cache) = self.diagnostics_cache.lock() {
                if let Some(cached) = cache.as_ref()
                    && cached.version == version
                    && cached.text_bytes == text_bytes
                {
                    tracing::debug!(
                        target: "gdb_sql_perf",
                        op = "diagnostics_cache",
                        editor_id,
                        result = "hit",
                        buffer_version = version,
                        text_bytes,
                        diagnostic_count = cached.diagnostics.len(),
                    );
                    return cached.diagnostics.clone();
                }
            }
        }
        let request_id = self.diagnostics_latest_request.request_id();
        let mut result = sql_diagnostics_snapshot_cancellable(
            snapshot,
            self.dialect,
            self.diagnostics_latest_request.inner(),
            request_id,
        )
        .unwrap_or_default();
        // 语义诊断按 statement 局部 materialize；范围扫描直接读取 Rope chunks，避免
        // 为一次诊断把 1MB/10MB 文档拼成完整 String。
        let semantic_started = Instant::now();
        let statement_ranges = split_statement_ranges_snapshot(snapshot);
        let semantic = self.semantic_diagnostics_snapshot_cancellable(
            snapshot,
            &statement_ranges,
            self.diagnostics_latest_request.inner(),
            request_id,
        );
        let semantic_count = semantic.len();
        result.extend(semantic);
        result.sort_unstable_by_key(|diagnostic| diagnostic.range.start);
        if !self.diagnostics_latest_request.check(request_id) {
            tracing::debug!(
                target: "gdb_sql_perf",
                op = "diagnostics_discarded",
                task_outcome = TaskOutcome::ResultDiscarded.as_str(),
                editor_id,
                buffer_version = version,
                request_id,
            );
            return result;
        }
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "diagnostics_semantic",
            editor_id,
            elapsed_us = semantic_started.elapsed().as_micros() as u64,
            buffer_version = snapshot.version(),
            text_bytes = snapshot.len(),
            statement_count = statement_ranges.len(),
            diagnostic_count = semantic_count,
        );
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "diagnostics_parse",
            editor_id,
            elapsed_us = started.elapsed().as_micros() as u64,
            buffer_version = version,
            text_bytes,
            diagnostic_count = result.len(),
            request_id,
        );
        if self.completion_items_cacheable {
            if let Ok(mut cache) = self.diagnostics_cache.lock() {
                *cache = Some(SqlDiagnosticsCache {
                    version,
                    text_bytes,
                    diagnostics: result.clone(),
                    statement_ranges,
                });
            }
        }
        let elapsed_us = started.elapsed().as_micros() as u64;
        if elapsed_us > fluxdb_editor_core::BACKGROUND_BUDGET_US {
            tracing::warn!(
                target: "gdb_sql_perf",
                op = "diagnostics_parse_slow",
                editor_id,
                elapsed_us,
                buffer_version = version,
                text_bytes,
                request_id,
            );
        }
        result
    }

    fn diagnostics_incremental(
        &self,
        snapshot: &BufferSnapshot,
        changed: InputEdit,
    ) -> Vec<Diagnostic> {
        let Some(result) = self.try_incremental_diagnostics(snapshot, &changed) else {
            return self.diagnostics(snapshot);
        };
        result
    }

}

impl SqlAdapter {
    /// 只重解析受编辑影响的单条 statement，并平移其它诊断。
    /// 复杂编辑或无法唯一确定 statement 时返回 None，交给完整诊断路径保证正确性。
    fn try_incremental_diagnostics(
        &self,
        snapshot: &BufferSnapshot,
        changed: &InputEdit,
    ) -> Option<Vec<Diagnostic>> {
        if changed.full_document
            || changed.new_text.contains(';')
            || changed.old_range.len() > 256
            || changed.new_text.len() > 256
        {
            return None;
        }
        let old_range = changed.old_range.sorted();
        let mut cache = self.diagnostics_cache.lock().ok()?.take()?;
        if cache.version.saturating_add(1) != snapshot.version()
            || cache.statement_ranges.is_empty()
        {
            *self.diagnostics_cache.lock().ok()? = Some(cache);
            return None;
        }
        let mut matched = None;
        for range in &cache.statement_ranges {
            let intersects = if old_range.is_empty() {
                range.start <= old_range.start && old_range.start <= range.end
            } else {
                range.start < old_range.end && old_range.start < range.end
            };
            if intersects {
                if matched.is_some() {
                    *self.diagnostics_cache.lock().ok()? = Some(cache);
                    return None;
                }
                matched = Some(*range);
            }
        }
        let Some(old_statement) = matched else {
            *self.diagnostics_cache.lock().ok()? = Some(cache);
            return None;
        };
        let delta = changed.new_text.len() as isize - old_range.len() as isize;
        let new_statement = Range::new(
            old_statement.start,
            shifted_offset(old_statement.end, delta).min(snapshot.len()),
        );
        if new_statement.start >= new_statement.end {
            *self.diagnostics_cache.lock().ok()? = Some(cache);
            return None;
        }
        let statement_text = snapshot.text_in_range(new_statement);
        let mut local = sql_diagnostics_tree_sitter_for_dialect(&statement_text, self.dialect);
        local.extend(self.semantic_diagnostics_text(&statement_text, 0));
        for diagnostic in &mut local {
            diagnostic.range.start += new_statement.start;
            diagnostic.range.end += new_statement.start;
        }
        let mut diagnostics = Vec::with_capacity(cache.diagnostics.len() + local.len());
        for mut diagnostic in cache.diagnostics.drain(..) {
            if diagnostic.range.end <= old_statement.start {
                diagnostics.push(diagnostic);
            } else if diagnostic.range.start >= old_statement.end {
                diagnostic.range.start = shifted_offset(diagnostic.range.start, delta);
                diagnostic.range.end = shifted_offset(diagnostic.range.end, delta);
                diagnostics.push(diagnostic);
            }
        }
        diagnostics.extend(local);
        diagnostics.sort_unstable_by_key(|diagnostic| diagnostic.range.start);
        for range in &mut cache.statement_ranges {
            if *range == old_statement {
                *range = new_statement;
            } else if range.start >= old_statement.end {
                range.start = shifted_offset(range.start, delta).min(snapshot.len());
                range.end = shifted_offset(range.end, delta).min(snapshot.len());
            }
        }
        cache.version = snapshot.version();
        cache.text_bytes = snapshot.len();
        cache.diagnostics = diagnostics.clone();
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "diagnostics_incremental",
            buffer_version = snapshot.version(),
            old_start = old_statement.start,
            old_end = old_statement.end,
            new_start = new_statement.start,
            new_end = new_statement.end,
            diagnostic_count = diagnostics.len(),
        );
        *self.diagnostics_cache.lock().ok()? = Some(cache);
        Some(diagnostics)
    }

    fn semantic_diagnostics_text(&self, text: &str, base_offset: usize) -> Vec<Diagnostic> {
        let metadata = self.semantic_metadata();
        self.semantic_diagnostics_text_with_metadata(text, base_offset, &metadata)
    }

    fn semantic_metadata(&self) -> SqlSemanticMetadata {
        if !self.completion_items_cacheable {
            return self.semantic_metadata_uncached();
        }
        self.semantic_metadata_cache
            .get_or_init(|| self.semantic_metadata_uncached())
            .clone()
    }

    fn semantic_metadata_uncached(&self) -> SqlSemanticMetadata {
        let mut metadata = SqlSemanticMetadata::default();
        for source in &self.sources {
            let tables = source.tables();
            metadata
                .tables
                .extend(tables.iter().map(|name| name.to_ascii_lowercase()));
            metadata.tables_exact.extend(tables);
            let views = source.views();
            metadata
                .views
                .extend(views.iter().map(|name| name.to_ascii_lowercase()));
            metadata.views_exact.extend(views);
            let schemas = source.schemas();
            metadata
                .schemas
                .extend(schemas.iter().map(|name| name.to_ascii_lowercase()));
            metadata.schemas_exact.extend(schemas);
            let routines = source.routines();
            metadata
                .routines
                .extend(routines.iter().map(|name| name.to_ascii_lowercase()));
            metadata.routines_exact.extend(routines);
            let types = source.types();
            metadata
                .types
                .extend(types.iter().map(|name| name.to_ascii_lowercase()));
            metadata.types_exact.extend(types);
            for (table, column) in source.columns() {
                metadata
                    .columns_by_table
                    .entry(table.to_ascii_lowercase())
                    .or_default()
                    .insert(column.to_ascii_lowercase());
                metadata
                    .columns_by_table_exact
                    .entry(table)
                    .or_default()
                    .insert(column);
            }
        }
        metadata
    }

    fn semantic_diagnostics_text_with_metadata(
        &self,
        text: &str,
        base_offset: usize,
        metadata: &SqlSemanticMetadata,
    ) -> Vec<Diagnostic> {
        if self.sources.is_empty() {
            return Vec::new();
        }
        let tables = &metadata.tables;
        let views = &metadata.views;
        let schemas = &metadata.schemas;
        let routines = &metadata.routines;
        let types = &metadata.types;
        let tables_exact = &metadata.tables_exact;
        let views_exact = &metadata.views_exact;
        let schemas_exact = &metadata.schemas_exact;
        let routines_exact = &metadata.routines_exact;
        let types_exact = &metadata.types_exact;
        let tokens = sql_tokens(text);
        let scope_symbols = fluxdb_app::sql_scope_symbols(
            text,
            match self.dialect {
                SqlDialect::Mysql => fluxdb_core::DatabaseKind::MySql,
                _ => fluxdb_core::DatabaseKind::Sqlite,
            },
        );
        let ast_parsed = scope_symbols.ast_parsed;
        let ast_functions = scope_symbols
            .function_names
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let ast_types = scope_symbols
            .cast_types
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let ast_columns = scope_symbols
            .qualified_columns
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let ast_unqualified_columns = scope_symbols
            .unqualified_columns
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let ast_referenced_tables = scope_symbols
            .referenced_tables
            .iter()
            .map(|table| table.name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let ast_relation_names = scope_symbols
            .relation_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let cte_columns = scope_symbols
            .cte_columns
            .into_iter()
            .map(|(name, columns)| (name, columns.into_iter().collect::<std::collections::HashSet<_>>()))
            .collect::<std::collections::HashMap<_, _>>();
        let cte_names = cte_columns.keys().cloned().collect::<std::collections::HashSet<_>>();
        let mut visible_columns = std::collections::HashSet::new();
        let mut visible_columns_exact = std::collections::HashSet::new();
        for table in &scope_symbols.referenced_tables {
            if let Some(columns) = cte_columns.get(&table.name.to_ascii_lowercase()) {
                visible_columns.extend(columns.iter().cloned());
                visible_columns_exact.extend(columns.iter().cloned());
            }
            if let Some(columns) = metadata.columns_by_table.get(&table.name.to_ascii_lowercase()) {
                visible_columns.extend(columns.iter().cloned());
            }
            if let Some(columns) = metadata.columns_by_table_exact.get(&table.name) {
                visible_columns_exact.extend(columns.iter().cloned());
            } else {
                for (known_table, columns) in &metadata.columns_by_table_exact {
                    if known_table.eq_ignore_ascii_case(&table.name) {
                        visible_columns_exact.extend(columns.iter().cloned());
                    }
                }
            }
        }
        let mut scoped_columns = None;
        for table in scope_symbols.referenced_tables {
            let Some(alias) = table.alias else { continue };
            let columns = cte_columns
                .get(&table.name.to_ascii_lowercase())
                .cloned()
                .or_else(|| metadata.columns_by_table.get(&table.name.to_ascii_lowercase()).cloned());
            if let Some(columns) = columns {
                scoped_columns
                    .get_or_insert_with(|| metadata.columns_by_table.clone())
                    .entry(alias)
                    .or_insert(columns);
            }
        }
        for (name, columns) in &cte_columns {
            scoped_columns
                .get_or_insert_with(|| metadata.columns_by_table.clone())
                .entry(name.clone())
                .or_insert_with(|| columns.clone());
        }
        let columns_by_table = scoped_columns
            .as_ref()
            .unwrap_or(&metadata.columns_by_table);
        let object_is_known = |name: &str| {
            if tables.contains(name) || views.contains(name) || cte_names.contains(name) {
                return true;
            }
            let base_name = name.rsplit('.').next().unwrap_or(name);
            tables
                .iter()
                .chain(views.iter())
                .any(|known| known.rsplit('.').next() == Some(base_name))
        };
        let mut diagnostics = Vec::new();

        // 只在对象名位于明确 SQL 子句后时检查存在性，避免把普通标识符误报为表。
        for (index, token) in tokens.iter().enumerate() {
            let expects_object = matches!(
                token.word.as_str(),
                "from" | "join" | "update" | "into" | "references" | "table" | "view"
            );
            if !expects_object {
                continue;
            }
            let Some(object) = tokens.get(index + 1) else { continue };
            if object.word == "(" || (!object.quoted && self.dialect.is_keyword(&object.word)) {
                continue;
            }
            let (object_name, object_end, object_quoted, schema_quoted) = if let Some(next) = tokens.get(index + 2)
                && text[object.end..next.start].trim() == "."
            {
                (
                    format!("{}.{}", object.word, next.word),
                    next.end,
                    next.quoted,
                    object.quoted,
                )
            } else {
                (object.word.clone(), object.end, object.quoted, false)
            };
            let base_name = object_name.rsplit('.').next().unwrap_or(&object_name);
            let object_name_folded = object_name.to_ascii_lowercase();
            let base_name_folded = base_name.to_ascii_lowercase();
            if ast_parsed
                && !cte_names.contains(&object_name_folded)
                && !ast_referenced_tables.contains(&object_name_folded)
                && !ast_referenced_tables.contains(&base_name_folded)
                && !ast_relation_names.contains(&object_name_folded)
                && !ast_relation_names.contains(&base_name_folded)
            {
                continue;
            }
            let known_object = if object_quoted {
                tables_exact.contains(&object_name)
                    || tables_exact.contains(base_name)
                    || views_exact.contains(&object_name)
                    || views_exact.contains(base_name)
            } else {
                object_is_known(&object_name) || object_is_known(base_name)
            };
            if !(tables.is_empty() && views.is_empty() && cte_names.is_empty())
                && !known_object
            {
                diagnostics.push(sql_metadata_diagnostic(
                    base_offset + object.start,
                    base_offset + object_end,
                    tr("app.completion.error.object_missing").replace("%{name}", &object_name),
                ));
            }
            if !schemas.is_empty() && let Some(dot) = object_name.find('.') {
                let schema_name = &object_name[..dot];
                let schema_known = if schema_quoted {
                    schemas_exact.contains(schema_name)
                } else {
                    schemas.contains(&schema_name.to_ascii_lowercase())
                };
                if !schema_known {
                    diagnostics.push(sql_metadata_diagnostic(
                        base_offset + object.start,
                        base_offset + object.start + dot,
                        tr("app.completion.error.schema_missing").replace("%{name}", &schema_name),
                    ));
                }
            }
        }

        // 只在 provider 明确提供 routine/type metadata 时检查自定义函数和类型。
        if !routines.is_empty() {
            for (_index, token) in tokens.iter().enumerate() {
                if !text[token.end..].trim_start().starts_with('(') {
                    continue;
                }
                if !token.quoted
                    && (self.dialect.is_keyword(&token.word)
                        || matches!(token.word.as_str(), "if" | "case" | "cast" | "convert"))
                {
                    continue;
                }
                if ast_parsed && !ast_functions.contains(&token.word.to_ascii_lowercase()) {
                    continue;
                }
                let known_routine = if token.quoted {
                    routines_exact.contains(&token.word)
                } else {
                    routines.contains(&token.word)
                };
                if !known_routine {
                    diagnostics.push(sql_metadata_diagnostic(
                        base_offset + token.start,
                        base_offset + token.end,
                        tr("app.completion.error.function_missing").replace("%{name}", &token.word),
                    ));
                }
            }
        }

        if !types.is_empty() {
            for (index, token) in tokens.iter().enumerate() {
                let is_cast_type = tokens
                    .get(index.wrapping_sub(1))
                    .is_some_and(|prev| prev.word == "as");
                if ast_parsed && !ast_types.contains(&token.word.to_ascii_lowercase()) {
                    continue;
                }
                let known_type = if token.quoted {
                    types_exact.contains(&token.word)
                } else {
                    types.contains(&token.word)
                };
                if is_cast_type
                    && (token.quoted || !self.dialect.is_keyword(&token.word))
                    && !known_type
                {
                    diagnostics.push(sql_metadata_diagnostic(
                        base_offset + token.start,
                        base_offset + token.end,
                        tr("app.completion.error.type_missing").replace("%{name}", &token.word),
                    ));
                }
            }
        }

        if columns_by_table.is_empty() {
            return diagnostics;
        }
        for pair in tokens.windows(2) {
            let [table_token, column_token] = pair else { continue };
            if text[table_token.end..column_token.start].trim() != "." {
                continue;
            }
            let table = table_token.word.as_str();
            let column = column_token.word.as_str();
            if ast_parsed
                && !ast_columns.contains(&(
                    table.to_ascii_lowercase(),
                    column.to_ascii_lowercase(),
                ))
            {
                continue;
            }
            let table_quoted = table_token.quoted;
            let column_quoted = column_token.quoted;
            let known_columns = if table_quoted {
                metadata.columns_by_table_exact.get(table)
            } else {
                columns_by_table.get(&table.to_ascii_lowercase())
            };
            let Some(known_columns) = known_columns else {
                continue;
            };
            let column_known = if column_quoted {
                known_columns.contains(column)
            } else {
                known_columns.contains(&column.to_ascii_lowercase())
            };
            if column_known {
                continue;
            }
            diagnostics.push(Diagnostic {
                range: Range::new(
                    base_offset + column_token.start,
                    base_offset + column_token.end,
                ),
                severity: fluxdb_editor_core::DiagnosticSeverity::Warning,
                message: tr("app.completion.error.column_missing").replace("%{column}", &column).replace("%{table}", &table),
                source: "sql-metadata".to_string(),
            });
        }
        if ast_parsed && !visible_columns.is_empty() {
            let select_aliases = scope_symbols
                .select_aliases
                .iter()
                .map(|alias| alias.to_ascii_lowercase())
                .collect::<std::collections::HashSet<_>>();
            for (index, token) in tokens.iter().enumerate() {
                if (!token.quoted && self.dialect.is_keyword(&token.word))
                    || (ast_unqualified_columns.is_empty()
                        || !ast_unqualified_columns.contains(&token.word.to_ascii_lowercase()))
                    || select_aliases.contains(&token.word.to_ascii_lowercase())
                {
                    continue;
                }
                let qualified = tokens
                    .get(index.wrapping_sub(1))
                    .is_some_and(|previous| text[previous.end..token.start].trim() == ".")
                    || tokens
                        .get(index + 1)
                        .is_some_and(|next| text[token.end..next.start].trim() == ".");
                if qualified {
                    continue;
                }
                let known = if token.quoted {
                    visible_columns_exact.contains(&token.word)
                } else {
                    visible_columns.contains(&token.word.to_ascii_lowercase())
                };
                if !known {
                    diagnostics.push(sql_metadata_diagnostic(
                        base_offset + token.start,
                        base_offset + token.end,
                        tr("app.completion.error.column_out_of_scope").replace("%{name}", &token.word),
                    ));
                }
            }
        }
        diagnostics
    }

    fn semantic_diagnostics_snapshot_cancellable(
        &self,
        snapshot: &BufferSnapshot,
        statement_ranges: &[Range],
        latest_request: &AtomicU64,
        request_id: u64,
    ) -> Vec<Diagnostic> {
        let metadata = self.semantic_metadata();
        let mut diagnostics = Vec::new();
        for range in statement_ranges {
            if latest_request.load(Ordering::Acquire) != request_id {
                break;
            }
            let text = snapshot.text_in_range(*range);
            diagnostics.extend(self.semantic_diagnostics_text_with_metadata(
                &text,
                range.start,
                &metadata,
            ));
        }
        diagnostics
    }
}

#[derive(Clone, Debug)]
struct SqlToken {
    start: usize,
    end: usize,
    word: String,
    quoted: bool,
}

fn sql_tokens(text: &str) -> Vec<SqlToken> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i = skip_quoted_token(bytes, i, b'\'');
                continue;
            }
            b'"' | b'`' => {
                let start = i;
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        let value_end = i;
                        i += 1;
                        result.push(SqlToken {
                            start,
                            end: i,
                            word: text[value_start..value_end].replace(
                                if quote == b'`' { "``" } else { "\"\"" },
                                if quote == b'`' { "`" } else { "\"" },
                            ),
                            quoted: true,
                        });
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'#' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if !is_sql_identifier_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_sql_identifier_byte(bytes[i]) {
            i += 1;
        }
        result.push(SqlToken {
            start,
            end: i,
            word: text[start..i].to_ascii_lowercase(),
            quoted: false,
        });
    }
    result
}

fn skip_quoted_token(bytes: &[u8], mut i: usize, quote: u8) -> usize {
    i += 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if i + 1 < bytes.len() && bytes[i + 1] == quote {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    i
}

fn sql_metadata_diagnostic(start: usize, end: usize, message: String) -> Diagnostic {
    Diagnostic {
        range: Range::new(start, end.max(start + 1)),
        severity: fluxdb_editor_core::DiagnosticSeverity::Warning,
        message,
        source: "sql-metadata".to_string(),
    }
}

fn is_sql_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

impl CompletionProvider for SqlAdapter {
    /// 触发决策：显式或存在前缀即触发；空前缀在 SQL 层探测到「标识符.」（如 `SELECT u.`）
    /// 或 F001 的「子句后空格」（如 `FROM ` / `WHERE `，由 app 层按意图决定候选）时触发，
    /// 且两种探测都会排除字符串/注释/小数中的位置，避免误触发数据库请求。
    fn should_trigger(&self, request: &CompletionRequest) -> TriggerDecision {
        if request.explicit {
            // 显式触发（如 Ctrl+Space）用户强求，不被字符串/注释/签名抑制。
            return TriggerDecision::Yes;
        }
        // 统一取光标前局部窗口文本，供字符串/注释与函数签名上下文共用。
        let before = request.document.as_ref().map(|snapshot| {
            let cursor = request.cursor.min(snapshot.len());
            let start = cursor.saturating_sub(COMPLETION_WINDOW_BYTES);
            snapshot.text_in_range(Range::new(start, cursor))
        });
        let in_call = before
            .as_ref()
            .is_some_and(|b| fluxdb_app::sql_signature_at(b, b.len()).is_some());
        if !request.query.is_empty() {
            // F001：非显式自动触发时，若光标落于未闭合字符串/注释内（词字符被编辑器
            // 当普通前缀提取，query 非空），应抑制——否则在 `'se` / `-- sel` 里打字会误弹。
            // 复用 masked_range_at 判「光标前一字符是否被字符串/注释区间覆盖」。
            if let Some(before) = before.as_ref() {
                let last = before.len().saturating_sub(1);
                if masked_range_at(before, last).is_some() {
                    return TriggerDecision::No;
                }
            }
            // 方案 A：非显式且在函数参数上下文内（`foo(` 之后），补全浮层让位给签名
            // tooltip，避免两者重叠。显式触发（Ctrl+Space）不受影响。
            return if in_call {
                TriggerDecision::No
            } else {
                TriggerDecision::Yes
            };
        }
        let Some(before) = before else {
            return TriggerDecision::No;
        };
        // 方案 A：函数参数上下文内同上抑制（覆盖 query 为空的下标/空格等场景）。
        if in_call {
            return TriggerDecision::No;
        }
        if trailing_ident_qualifier(&before) || sql_space_trigger_context(&before) {
            TriggerDecision::Yes
        } else {
            TriggerDecision::No
        }
    }

    fn cancel_pending(&self) {
        self.completion_latest_request.fetch_add(1, Ordering::AcqRel);
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "completion_cancel_requested",
            task_outcome = TaskOutcome::CancelRequested.as_str(),
            editor_id = self.editor_id(),
        );
    }

    /// 优先走宿主 resolver，保证普通输入也能获得 schema、函数、别名和 JOIN 候选；
    /// resolver 不可用时回退到注入的本地候选。
    fn complete(&self, request: CompletionRequest) -> CompletionFuture {
        let request_started = Instant::now();
        let request_id = request.request_id;
        let buffer_version = request.buffer_version;
        let editor_id = self.editor_id();
        let edit_id = request.edit_id;
        let latest_request = self.completion_latest_request.clone();
        latest_request.store(request_id, Ordering::Release);
        if let (Some(resolver), Some(snapshot)) =
            (self.completion_resolver.clone(), request.document.clone())
        {
            let document_bytes = snapshot.len();
            let (text, text_offset) = completion_window(&snapshot, request.cursor);
            let cursor = request.cursor.saturating_sub(text_offset).min(text.len());
            let explicit = request.explicit;
            let query = request.query;
            let fallback_items = self.build_completion_items();
            tracing::debug!(
                target: "gdb_sql_perf",
                op = "completion_prepare",
                editor_id,
                edit_id,
                elapsed_us = request_started.elapsed().as_micros() as u64,
                request_id,
                buffer_version,
                document_bytes,
                context_bytes = text.len(),
                fallback_count = fallback_items.len(),
                resolver = true,
            );
            return Box::pin(async move {
                let started = Instant::now();
                // 复用共享原子作为 resolver 的 latest-wins 检查点（Arc<AtomicU64> 共享）。
                match resolver(text, cursor, explicit, latest_request.clone(), request_id) {
                    Ok(response) => {
                        let items = response
                            .items
                            .into_iter()
                            .map(|mut item| {
                                if let Some(range) = item.replace_range {
                                    item.replace_range = Some(Range::new(
                                        range.start.saturating_add(text_offset),
                                        range.end.saturating_add(text_offset),
                                    ));
                                }
                                item
                            })
                            .collect::<Vec<_>>();
                        // 上下文结果即权威答案：本地静态回退只在 resolver 不可用时降级使用。
                        tracing::debug!(
                            target: "gdb_sql_completion",
                            op = "completion_fallback_skipped",
                            editor_id,
                            request_id,
                            fallback_count = fallback_items.len(),
                            "resolver 已返回上下文候选，跳过本地静态回退候选"
                        );
                        tracing::debug!(
                            target: "gdb_sql_perf",
                            op = "completion_provider",
                            editor_id,
                            edit_id,
                            elapsed_us = started.elapsed().as_micros() as u64,
                            request_id,
                            buffer_version,
                            item_count = items.len(),
                            has_more = response.has_more,
                            resolver = true,
                        );
                        Ok(CompletionResult {
                            items,
                            has_more: response.has_more,
                            // DM-704：单 provider 无真实分页，仅以 has_more 携带协议游标
                            // （取 request_id 作不透明单调游标），供后续分页升级承载。
                            continuation: if response.has_more {
                                Some(CompletionContinuation(request_id))
                            } else {
                                None
                            },
                        })
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "gdb_sql_completion",
                            error = %error,
                            "SQL completion resolver failed; using local candidates"
                        );
                        let filtered = filter_items(fallback_items.as_ref(), &query);
                        tracing::debug!(
                            target: "gdb_sql_perf",
                            op = "completion_fallback",
                            editor_id,
                            edit_id,
                            elapsed_us = started.elapsed().as_micros() as u64,
                            request_id,
                            buffer_version,
                            item_count = filtered.len(),
                            resolver = true,
                        );
                        Ok(CompletionResult {
                            items: filtered,
                            has_more: false,
                            continuation: None,
                        })
                    }
                }
            });
        }
        let items = self.build_completion_items();
        let query = request.query;
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "completion_prepare",
            editor_id,
            edit_id,
            elapsed_us = request_started.elapsed().as_micros() as u64,
            request_id,
            buffer_version,
            candidate_count = items.len(),
            resolver = false,
        );
        Box::pin(async move {
            let started = Instant::now();
            let filtered = filter_items(&items, &query);
            tracing::debug!(
                target: "gdb_sql_perf",
                op = "completion_filter",
                editor_id,
                edit_id,
                elapsed_us = started.elapsed().as_micros() as u64,
                request_id,
                buffer_version,
                item_count = filtered.len(),
                resolver = false,
            );
            Ok(CompletionResult {
                items: filtered,
                has_more: false,
                continuation: None,
            })
        })
    }

    /// F005：候选项右侧 metadata 详情。由选中项/悬停切换在后台触发，latest-wins 取消。
    /// 优先走宿主 resolver（fluxdb-app：索引命中直接返回，未命中按身份取一次元数据）；
    /// 无 resolver 时回退到候选内联注释。
    fn documentation(&self, request: fluxdb_editor_core::DocumentationRequest) -> Option<fluxdb_editor_core::DocumentationState> {
        use fluxdb_editor_core::DocumentationState as D;
        let kind = request.kind;
        let state = self.resolve_documentation(
            kind,
            request.label.as_str(),
            request.comment.as_deref(),
            request.schema.as_deref(),
            request.latest_request,
            request.request_id,
        );
        Some(match state {
            SqlDocState::Loading => D::Loading,
            SqlDocState::Ready(text) => D::Ready(text),
            SqlDocState::Error(reason) => D::Error(reason),
        })
    }
}

impl SignatureProvider for SqlAdapter {
    /// 函数签名提示（P1.9/F003）：复用 fluxdb-app 的 SQL 语义函数调用定位（保留 schema
    /// 限定前缀、多字节安全、忽略字符串内逗号），生成非阻塞的参数 tooltip。
    /// connector 参数 metadata 未接入时退化为 `name()` 签名（可读降级）；
    /// 光标不在函数调用括号内（或括号左侧非标识符）时返回 None（不显示）。
    fn signature(&self, snapshot: &BufferSnapshot, position: Point) -> Option<SignatureInfo> {
        // 只取光标前局部上下文，避免大文档全量复制（与 core 通用版同窗口）。
        let cursor = snapshot.point_to_offset(position);
        if cursor == 0 {
            return None;
        }
        const SIG_CONTEXT_BYTES: usize = 64 * 1024;
        let context_start = cursor.saturating_sub(SIG_CONTEXT_BYTES);
        let text = snapshot.text_in_range(Range::new(context_start, cursor));
        let call = fluxdb_app::sql_signature_at(&text, cursor - context_start)?;
        Some(SignatureInfo {
            label: sql_signature_label(&call.name, call.qualifier.as_deref()),
            documentation: String::new(),
            active_parameter: Some(call.active_parameter),
            parameter_ranges: Vec::new(),
        })
    }
}

impl ExecutionAdapter for SqlAdapter {
    /// 把选区或光标所在语句切分成执行单元。选区非空时使用选区文本，
    /// 否则取光标所在语句；执行模式根据首词推断。
    fn execution_units(&self, snapshot: &BufferSnapshot, selection: Selection) -> Vec<ExecutionUnit> {
        // 有选区时直接执行选区文本。
        if !selection.is_empty() {
            let range = selection.range();
            let start = range.start.min(snapshot.len());
            let end = range.end.min(snapshot.len());
            let unit_text = snapshot.text_in_range(Range::new(start, end));
            let mode = self.detect_mode(&unit_text);
            return vec![ExecutionUnit {
                range: Range::new(start, end),
                text: unit_text,
                mode,
            }];
        }
        // 无选区时执行光标所在语句。
        let cursor = selection.cursor.min(snapshot.len());
        if let Some((start, end)) = statement_around_snapshot(snapshot, cursor) {
            let unit_text = snapshot.text_in_range(Range::new(start, end));
            let mode = self.detect_mode(&unit_text);
            return vec![ExecutionUnit {
                range: Range::new(start, end),
                text: unit_text,
                mode,
            }];
        }
        Vec::new()
    }
}

impl CodeLensProvider for SqlAdapter {
    /// 为每条 SQL 语句生成中性 Run / Select 动作。
    ///
    /// CodeLens 的合并与 ` | ` 分隔由通用编辑器完成，SQL 适配器只提供语义键。
    fn code_lenses(&self, snapshot: &BufferSnapshot, visible: Range) -> Vec<CodeLens> {
        let started = Instant::now();
        let version = snapshot.version();
        let visible = Range::new(visible.start.min(snapshot.len()), visible.end.min(snapshot.len()));
        let mut cache_hit = false;
        let result = match self.code_lens_cache.lock() {
            Ok(mut cache) => {
                if cache
                    .as_ref()
                    .map(|(cached, bytes, lines, _)| (*cached, *bytes, *lines))
                    == Some((version, snapshot.len(), snapshot.line_count()))
                {
                    cache_hit = true;
                } else if snapshot.len() <= CODE_LENS_SYNC_SCAN_LIMIT {
                    let lenses = build_code_lenses_from_snapshot(snapshot);
                    *cache = Some((version, snapshot.len(), snapshot.line_count(), lenses));
                }
                if let Some((_, _, _, lenses)) = cache.as_ref().filter(|(cached, bytes, lines, _)| {
                    (*cached, *bytes, *lines) == (version, snapshot.len(), snapshot.line_count())
                }) {
                    lenses
                        .iter()
                        .filter(|lens| {
                            lens.range.end >= visible.start && lens.range.start <= visible.end
                        })
                        .cloned()
                        .collect()
                } else if snapshot.len() > CODE_LENS_SYNC_SCAN_LIMIT
                    && visible.start < visible.end
                    && visible.end.saturating_sub(visible.start) < snapshot.len()
                {
                    // 大文档只扫描视口附近窗口；窗口内的语句范围需要平移回文档坐标。
                    let start = visible.start.saturating_sub(CODE_LENS_VISIBLE_WINDOW_BYTES);
                    let end = visible
                        .end
                        .saturating_add(CODE_LENS_VISIBLE_WINDOW_BYTES)
                        .min(snapshot.len());
                    let lenses = build_code_lenses(&snapshot.text_in_range(Range::new(start, end)));
                    lenses
                        .into_iter()
                        .map(|mut lens| {
                            lens.range.start += start;
                            lens.range.end += start;
                            lens
                        })
                        .filter(|lens| {
                            lens.range.end >= visible.start && lens.range.start <= visible.end
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        };
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "code_lens_scan",
            elapsed_us = started.elapsed().as_micros() as u64,
            buffer_version = version,
            text_bytes = snapshot.len(),
            visible_start = visible.start,
            visible_end = visible.end,
            lens_count = result.len(),
            cache_hit,
        );
        result
    }
}

impl DecorationProvider for SqlAdapter {
    /// 把语句执行状态还原为行背景装饰。
    ///
    /// 从共享状态仓库读取状态：仓库为空（未执行任何语句）时直接返回空集，避免每次
    /// 重绘都做语句切分。否则按当前文本切分语句（编辑后 id 变化自然失配），对每个
    /// 有状态的语句，为其覆盖的每一行产出 `LineBackground(row, 通用状态键)`。通用
    /// 编辑器按状态键着色，从而与 SQL 词汇解耦。
    fn decorations(&self, snapshot: &BufferSnapshot, visible: Range) -> DecorationSet {
        let started = Instant::now();
        let Some(store) = &self.status else {
            return DecorationSet { decorations: Vec::new() };
        };
        let status = match store.lock() {
            Ok(status) => status,
            Err(_) => return DecorationSet { decorations: Vec::new() },
        };
        if status.is_empty() {
            return DecorationSet { decorations: Vec::new() };
        }
        let mut cache_hit = false;
        let runs = match self.statement_runs_cache.lock() {
            Ok(mut cache) => {
                let key = (snapshot.version(), snapshot.len(), snapshot.line_count());
                if let Some((version, bytes, lines, runs)) = cache.as_ref()
                    && (*version, *bytes, *lines) == key
                {
                    cache_hit = true;
                    runs.clone()
                } else {
                    let runs = self
                        .syntax_cache
                        .lock()
                        .ok()
                        .and_then(|syntax| {
                            syntax.as_ref().and_then(|syntax| {
                                (syntax.version == snapshot.version()
                                    && syntax.snapshot.len() == snapshot.len())
                                .then(|| {
                                    Arc::new(build_statement_runs_from_snapshot(
                                        snapshot,
                                        &syntax.statement_ranges,
                                    ))
                                })
                            })
                        })
                        .unwrap_or_else(|| {
                            let ranges = split_statement_ranges_snapshot(snapshot);
                            Arc::new(build_statement_runs_from_snapshot(snapshot, &ranges))
                        });
                    *cache = Some((key.0, key.1, key.2, runs.clone()));
                    runs
                }
            }
            Err(_) => {
                let ranges = split_statement_ranges_snapshot(snapshot);
                Arc::new(build_statement_runs_from_snapshot(snapshot, &ranges))
            }
        };
        let mut decorations = Vec::new();
        // 语句 runs 按字节区间升序（statement_runs_cache 命中时滚动不重算切分）。
        // 二分裁剪到可视窗口两侧，把每次滚动帧的 O(全文语句) 遍历收敛为
        // O(log n + 与可视区相交的语句数)。
        let lo = runs.partition_point(|run| run.range.end < visible.start);
        let hi = runs.partition_point(|run| run.range.start <= visible.end);
        for run in &runs[lo..hi] {
            let Some(stmt_status) = status.status_for_run(&run) else {
                continue;
            };
            let kind = match stmt_status {
                SqlStatementStatus::Running => "running",
                SqlStatementStatus::Success => "success",
                SqlStatementStatus::Failure => "failure",
            };
            for row in run.start_row..=run.end_row {
                decorations.push(Decoration::LineBackground(row, kind.to_string()));
            }
        }
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "decoration_scan",
            elapsed_us = started.elapsed().as_micros() as u64,
            buffer_version = snapshot.version(),
            text_bytes = snapshot.len(),
            visible_start = visible.start,
            visible_end = visible.end,
            decoration_count = decorations.len(),
            cache_hit,
        );
        DecorationSet { decorations }
    }
}

