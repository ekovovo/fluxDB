// redis_editor_adapter/mod.rs —— Redis 编辑器接入层。
//
// 本模块把 fluxdb-editor-core 的通用编辑协议（CompletionProvider /
// ExecutionAdapter）接到 Redis 业务上下文，通过 fluxdb-app 的纯逻辑
// `redis_completion_result` 提供命令/子命令/参数补全。
//
// 分层约束：
//   - 本模块不直接连接数据库，所有补全都来自内存命令规格。
//   - Redis 语义（命令树、参数定位）全部留在 fluxdb-app，本模块只做协议转换。
//   - 命令执行通过 ExecutionAdapter 把文本/选区交给 app controller，UI 不拼命令。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use fluxdb_editor_core::{
    BufferSnapshot, CompletionFuture, CompletionItem, CompletionKind, CompletionProvider,
    CompletionRequest, CompletionResult, DocumentationRequest, DocumentationState, ExecuteMode,
    ExecutionAdapter, ExecutionUnit, InsertTextFormat, Point, Range, Selection, SignatureInfo,
    SignatureProvider, TriggerDecision,
};

/// 把 fluxdb-app 的 `QueryCompletionKind` 映射到 editor-core 的 `CompletionKind`。
fn map_kind(kind: &fluxdb_core::QueryCompletionKind) -> CompletionKind {
    match kind {
        fluxdb_core::QueryCompletionKind::RedisCommand => CompletionKind::Command,
        fluxdb_core::QueryCompletionKind::RedisSubCommand => CompletionKind::Command,
        fluxdb_core::QueryCompletionKind::RedisArgument => CompletionKind::Keyword,
        // 其余类型（SQL 相关）在 Redis 适配器中不会出现，回退到 Text。
        _ => CompletionKind::Text,
    }
}

/// 把 fluxdb-app 的 `QueryCompletionItem` 映射到 editor-core 的 `CompletionItem`。
fn map_item(item: &fluxdb_core::QueryCompletionItem) -> CompletionItem {
    let mut completion_item = CompletionItem::new(&item.label, map_kind(&item.kind));
    completion_item.insert_text = item.insert_text.clone();
    completion_item.detail = item.detail.clone().unwrap_or_default();
    completion_item.documentation = item.documentation.clone().unwrap_or_default();
    completion_item.insert_text_format = match item.insert_text_format {
        fluxdb_core::InsertTextFormat::PlainText => InsertTextFormat::PlainText,
        fluxdb_core::InsertTextFormat::Snippet => InsertTextFormat::Snippet,
    };
    if let Some(filter) = &item.filter_text {
        completion_item.filter_text = filter.clone();
    }
    if let Some(sort) = &item.sort_text {
        completion_item.sort_text = sort.clone();
    }
    completion_item
}

/// Redis 编辑器适配器。
///
/// 持有无状态引用，所有补全逻辑委托给 fluxdb-app 的纯函数。
/// 本身不持有数据库连接、GPUI 类型或命令词典。
/// `latest_request` 用于 latest-wins 取消：`complete` 写入当前 request_id，
/// `cancel_pending` 推进原子，旧结果提交时判废。
#[derive(Clone, Default)]
pub struct RedisAdapter {
    latest_request: Arc<AtomicU64>,
}

impl RedisAdapter {
    /// 构造 Redis 适配器。
    pub fn new() -> Self {
        Self {
            latest_request: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl CompletionProvider for RedisAdapter {
    /// 触发决策：显式触发总是返回 Yes；非显式时，只要光标落在命令名、
    /// 子命令或参数 token 内（含空白后空 token）都触发。
    ///
    /// 与 SQL 不同，Redis 在命令后的空白位置也需要弹出候选（如 `SET ` 后应提示
    /// 参数），因此不能只依赖 `DependsOnPrefix(1)` 门控。
    fn should_trigger(&self, request: &CompletionRequest) -> TriggerDecision {
        if request.explicit {
            return TriggerDecision::Yes;
        }
        // 有前缀文本（命令名/子命令/参数的片段）时触发。
        if !request.query.is_empty() {
            return TriggerDecision::Yes;
        }
        // 空前缀：由编辑器在 trigger_chars（空格、`.`、`@` 等）命中时触发，
        // 此处返回 Yes 让编辑器在空白后也能弹出候选。
        TriggerDecision::Yes
    }

    /// 取消所有 in-flight 补全：推进 latest_request，令旧请求提交时判废。
    fn cancel_pending(&self) {
        self.latest_request.fetch_add(1, Ordering::AcqRel);
    }

    /// 发起补全：读取快照全文与光标，调用 fluxdb-app 的纯逻辑，再把结果
    /// 映射到 editor-core 的 `CompletionResult`。
    ///
    /// 写入当前 request_id 到 latest_request，异步返回前判废（latest-wins）：
    /// 新请求或 cancel 推进原子后，旧结果丢弃，避免覆盖新文本/新选区。
    fn complete(&self, request: CompletionRequest) -> CompletionFuture {
        let snapshot = request.document.clone();
        let cursor = request.cursor;
        let request_id = request.request_id;
        let latest = self.latest_request.clone();
        latest.store(request_id, Ordering::Release);
        Box::pin(async move {
            let snapshot = snapshot.ok_or(fluxdb_editor_core::CompletionError::Provider)?;
            let text = snapshot.to_string();
            let result = fluxdb_app::redis_completion_result(&text, cursor);
            // latest-wins：新请求或 cancel 推进原子后，旧结果丢弃。
            if latest.load(Ordering::Acquire) != request_id {
                return Err(fluxdb_editor_core::CompletionError::Cancelled);
            }
            // 把每个候选的 replace_range 设为统一区间（由 app 层计算）。
            let items: Vec<CompletionItem> = result
                .items
                .iter()
                .map(|item| {
                    let mut mapped = map_item(item);
                    mapped.replace_range = Some(Range::new(result.replace_start, result.replace_end));
                    mapped
                })
                .collect();
            Ok(CompletionResult {
                items,
                has_more: false,
                continuation: None,
            })
        })
    }

    /// 候选项右侧 metadata 详情（F005）：Redis 补全不展示右侧详情面板。
    ///
    /// 返回 None 使编辑器 `completion_doc_state` 保持 None，浮层仅候选列表、
    /// 宽度不加宽。命令文档为静态规格，无需在此兜底展示。
    fn documentation(&self, _request: DocumentationRequest) -> Option<DocumentationState> {
        None
    }
}

impl ExecutionAdapter for RedisAdapter {
    /// 把选区或光标所在行切分成执行单元。
    ///
    /// 有选区时执行选区文本；否则取光标所在行（Redis CLI 以换行分隔命令）。
    /// 执行模式统一为 Execute（Redis 命令不分 Select/Explain）。
    fn execution_units(&self, snapshot: &BufferSnapshot, selection: Selection) -> Vec<ExecutionUnit> {
        if !selection.is_empty() {
            let range = selection.range();
            let start = range.start.min(snapshot.len());
            let end = range.end.min(snapshot.len());
            let unit_text = snapshot.text_in_range(Range::new(start, end));
            return vec![ExecutionUnit {
                range: Range::new(start, end),
                text: unit_text,
                mode: ExecuteMode::Execute,
            }];
        }
        // 无选区时执行光标所在行。
        let cursor = selection.cursor.min(snapshot.len());
        if let Some((start, end)) = redis_line_range(snapshot, cursor) {
            let unit_text = snapshot.text_in_range(Range::new(start, end));
            return vec![ExecutionUnit {
                range: Range::new(start, end),
                text: unit_text,
                mode: ExecuteMode::Execute,
            }];
        }
        Vec::new()
    }
}

impl SignatureProvider for RedisAdapter {
    /// 根据整段文本与光标，返回当前命令的签名提示。
    ///
    /// 委托给 fluxdb-app 的 `redis_command_signature`：由 app 层复用补全上下文解析
    /// 命令名与光标前已输入参数（不重复分词），输出 label + 当前应填参数索引 +
    /// 每参数精确 byte 范围。参数范围交给通用渲染层精确高亮，不靠空格切词数 token。
    fn signature(&self, snapshot: &BufferSnapshot, position: Point) -> Option<SignatureInfo> {
        let cursor = snapshot.point_to_offset(position);
        let text = snapshot.to_string();
        let sig = fluxdb_app::redis_command_signature(&text, cursor)?;
        Some(SignatureInfo {
            label: sig.label,
            documentation: String::new(),
            active_parameter: Some(sig.active_parameter),
            parameter_ranges: sig.parameter_ranges,
        })
    }
}

/// 计算光标所在行的字节区间（以 `\n` 为界）。
fn redis_line_range(snapshot: &BufferSnapshot, cursor: usize) -> Option<(usize, usize)> {
    let len = snapshot.len();
    let cursor = cursor.min(len);
    let text = snapshot.to_string();
    let start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[cursor..]
        .find('\n')
        .map(|i| cursor + i)
        .unwrap_or(len);
    if start < end {
        Some((start, end))
    } else {
        None
    }
}

