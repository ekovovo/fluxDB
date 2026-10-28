/// 超大字段完整内容的受控存储。
///
/// 为什么需要这一层：结果页只装预览，完整值可能上百 MiB，不能塞进 `DataPage`
/// 内存，也不能留在数据库驱动里等下一次重查（任意 SQL 结果无法按行身份重查）。
/// 实现方负责把完整内容落到**受控目录**（私有权限、不记录正文日志），并在
/// 结果关闭、分页切换、标签关闭、取消、退出时清理。
///
/// 安全约束（实现方必须遵守）：
/// - `content_id` 是不透明句柄，不得是文件路径，也不得能直接反推出路径；
/// - 只按 `content_id` 读取，不接受调用方传路径；
/// - 读取必须有上限（[`CELL_CHUNK_BYTES`] 量级），不能一次返回整份内容。
pub trait CellContentStore: Send + Sync {
    /// 写入一份完整内容，返回不透明句柄。
    ///
    /// 配额不足或磁盘不足时必须返回错误，让调用方显式停下；不能悄悄写半份。
    fn put(&self, owner: &str, kind: crate::LargeValueKind, bytes: &[u8]) -> crate::Result<String>;

    /// 从读取端流式写入一份完整内容，返回不透明句柄。
    ///
    /// 上传替换必须走这里：待写入的内容可能是几百 MiB，先整份读进内存就违背了
    /// 流式的目的。实现方按固定大小缓冲拷贝，进程内存占用与内容大小无关；
    /// 默认实现退化为整份读入（仅供内存实现/测试）。
    fn put_from_reader(
        &self,
        owner: &str,
        kind: crate::LargeValueKind,
        reader: &mut dyn std::io::Read,
    ) -> crate::Result<String> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| crate::Error::new(crate::ErrorKind::Internal, format!("读取待写入内容失败：{error}")))?;
        self.put(owner, kind, &bytes)
    }

    /// 只读元信息；句柄失效（已清理/已过期）返回 `None`。
    fn stat(&self, content_id: &str) -> Option<CellContentMeta>;

    /// 读取 `[offset, offset + len)`，返回实际读到的字节（可能更短）。
    ///
    /// 调用方负责按需要对齐 UTF-8 边界；本方法只保证不越过内容末尾。
    fn read(&self, content_id: &str, offset: u64, len: usize) -> crate::Result<Vec<u8>>;

    /// 释放一份内容；重复释放安全。结果关闭/切页/关标签时调用。
    fn release(&self, content_id: &str);

    /// 释放某个归属（标签/结果）下的全部内容。
    fn release_owner(&self, owner: &str);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellContentMeta {
    pub byte_length: u64,
    pub kind: crate::LargeValueKind,
}

/// 暂存在受控内容存储里、等待写入数据库的一份内容。
///
/// 生命周期跟着引用走：最后一个持有者被丢弃时（改动集清空、换页、关标签、
/// 同一格被重新替换）自动释放，不需要调用方在每个清理点单独记账。释放只按
/// 不透明 id，不接触路径。
///
/// 不实现 `Clone` 以外的任何派生（尤其不派生 `Serialize`）：内容引用可以进改动集，
/// 但不能被序列化进事件、快照或 AI 上下文。
pub struct StagedContent {
    content_id: String,
    byte_length: u64,
}

impl StagedContent {
    /// 把一份已落盘的内容登记为暂存引用。只有内容存储的实现方/应用层该调用它。
    pub fn new(content_id: impl Into<String>, byte_length: u64) -> Self {
        Self {
            content_id: content_id.into(),
            byte_length,
        }
    }

    pub fn content_id(&self) -> &str {
        &self.content_id
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

impl std::fmt::Debug for StagedContent {
    /// 只打长度，不把不透明 id 之外的东西抖出去（id 本身也不进日志的正文位置）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedContent")
            .field("byte_length", &self.byte_length)
            .finish_non_exhaustive()
    }
}

impl Drop for StagedContent {
    fn drop(&mut self) {
        if let Some(store) = cell_content_store() {
            store.release(&self.content_id);
        }
    }
}

/// 结果内容的归属标识：一次执行 = 一个归属，重新执行/关标签时整批释放。
///
/// 连接器与 app 必须用同一个公式，否则会漏释放（内容一直占配额）或误删
/// 别的结果（用户拿到已失效的引用）。只做标识，不含路径。
pub fn result_content_owner(
    session_id: Option<crate::QuerySessionId>,
    connection_id: crate::ConnectionId,
) -> String {
    match session_id {
        Some(session) => format!("result-{session}-{}", connection_id.0),
        None => format!("result-once-{}", connection_id.0),
    }
}

/// 表/视图浏览页的内容归属：同一张表的多次翻页共用一个归属，重新加载时整批替换。
///
/// 不能复用 [`result_content_owner`]：表浏览没有查询会话，若按连接归属，
/// 刷新一张表会把同连接其他表页的内容引用一起释放（用户那边表现为「已失效」）。
pub fn table_content_owner(path: &crate::ObjectPath) -> String {
    let database = path.database.as_deref().unwrap_or("");
    let schema = path.schema.as_deref().unwrap_or("");
    format!(
        "data-{}-{database}.{schema}.{}",
        path.connection_id.0, path.name
    )
}

/// 进程级内容存储注册表。
///
/// 连接器在取数时手里没有应用存储根（`QueryExecutionOptions` 是 `Copy`，塞不进
/// `Arc`；`QueryRequest` 有几十处字面量，不为了传一个句柄全改）。沿用仓库已有的
/// `register_filter_dialect_resolver` 做法：应用启动时注册一次，连接器按需取用。
/// 未注册时连接器**不得**静默丢行，必须把页标成资源不完整并给出原因。
static CELL_CONTENT_STORE: std::sync::OnceLock<
    std::sync::RwLock<Option<std::sync::Arc<dyn CellContentStore>>>,
> = std::sync::OnceLock::new();

pub fn register_cell_content_store(store: std::sync::Arc<dyn CellContentStore>) {
    let slot = CELL_CONTENT_STORE.get_or_init(|| std::sync::RwLock::new(None));
    if let Ok(mut slot) = slot.write() {
        *slot = Some(store);
    }
}

pub fn cell_content_store() -> Option<std::sync::Arc<dyn CellContentStore>> {
    CELL_CONTENT_STORE
        .get()
        .and_then(|slot| slot.read().ok().and_then(|slot| slot.clone()))
}

/// 把一份完整的大字段内容外置，返回只带预览的 [`crate::CellValue::LargeValue`]。
///
/// 连接器取数时调用：内容已在内存（驱动本来就要解码完整值），这里把它交给受控
/// 存储落盘，页里只留预览与不透明句柄，从而「大字段不丢行」。
/// 存储未注册或写入失败时返回错误，调用方必须把页标成资源不完整，不能丢行。
pub fn spill_large_value(
    owner: &str,
    kind: crate::LargeValueKind,
    bytes: &[u8],
) -> crate::Result<crate::CellValue> {
    let store = cell_content_store().ok_or_else(|| {
        crate::Error::new(
            crate::ErrorKind::Unsupported,
            "未配置大字段内容存储，无法保留完整内容",
        )
    })?;
    let content_id = store.put(owner, kind, bytes)?;
    let preview = match kind {
        crate::LargeValueKind::Binary => binary_preview_hex(bytes),
        _ => text_preview(bytes),
    };
    let byte_length = bytes.len() as u64;
    // 预览覆盖的字节数：文本按字符前缀的实际编码长度算，二进制按固定前缀长度算。
    let covered = match kind {
        crate::LargeValueKind::Binary => byte_length.min(BINARY_PREVIEW_BYTES as u64),
        _ => preview.len() as u64,
    };
    Ok(crate::CellValue::LargeValue(crate::LargeCellValue {
        content_id,
        kind,
        byte_length: Some(byte_length),
        preview,
        preview_truncated: covered < byte_length,
    }))
}

/// 文本预览：按 [`crate::CELL_PREVIEW_CHARS`] 个字符截断，天然落在 UTF-8 字符边界。
pub fn text_preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.chars().take(crate::CELL_PREVIEW_CHARS).collect()
}

const BINARY_PREVIEW_BYTES: usize = 64;

fn binary_preview_hex(bytes: &[u8]) -> String {
    let head = bytes.get(..bytes.len().min(BINARY_PREVIEW_BYTES)).unwrap_or(bytes);
    let mut out = String::with_capacity(head.len() * 2);
    for byte in head {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}
