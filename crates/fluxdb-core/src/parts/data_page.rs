#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataPage {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub offset: u64,
    pub limit: u64,
    /// 本页之后是否还有行（正常的「还有下一页」）。
    ///
    /// 与 [`DataPage::incomplete`] 语义独立：`has_more` 只说明结果没取完，
    /// 不说明本页是否被资源上限截短。调用方翻页必须用 [`DataPage::next_offset`]，
    /// 不能用 `offset + limit` 推算。
    pub has_more: bool,
    /// 本页因资源上限（内存预算/磁盘/读取能力）未能收录全部连续行。
    ///
    /// 置位时 `rows` 是**已经完整收录的前缀**，后面的行没有被展示也没有被越过：
    /// 下一段从 [`DataPage::next_offset`] 继续。不能把这种情况伪装成普通 `has_more`，
    /// 否则用户看不到原因，还会被按配置页大小推进的分页直接跳过。
    /// 旧持久化数据没有该字段，反序列化按「完整」处理。
    #[serde(default)]
    pub incomplete: Option<PageIncomplete>,
}

/// 本页未完整收录的原因（用户可见文案）。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageIncomplete {
    pub reason: String,
}

impl DataPage {
    /// 下一页起点：本页实际连续收录的最后一行之后。
    ///
    /// 普通完整页等于 `offset + limit`；预算截断的短页会停在真实消费位置，
    /// 从而保证下一条未展示记录不会因为按配置页大小推进而被越过。
    pub fn next_offset(&self) -> u64 {
        self.offset.saturating_add(self.rows.len() as u64)
    }
}

/// 大字段预览上限（字符）。表格里只展示这么长的前缀。
pub const CELL_PREVIEW_CHARS: usize = 1024;
/// 单元格详情单次读取的分块大小（字节）。按 UTF-8 字符边界对齐后返回。
pub const CELL_CHUNK_BYTES: usize = 256 * 1024;
/// 详情面板为「完整值」保留的缓存上限（字节）。
///
/// 超过该值的字段只能分段只读查看，编辑与 JSON 格式化必须禁用：
/// 把上百 MiB 全文读进 UI 进程既没有上限也无法保证响应。
pub const CELL_DETAIL_CACHE_BYTES: u64 = 1024 * 1024;
/// 单字段超过该字节数才外置到内容存储；小字段仍作为普通 `Text`/`Json`/`Bytes` 返回。
pub const CELL_SPILL_THRESHOLD_BYTES: usize = 64 * 1024;
/// 上传替换走流式写入的阈值（字节）。
///
/// 小于该值的文件一次读入内存、单条语句写完：这条路径没有暂存落盘，也没有
/// 多次往返，小文件仍是最快路径；16 MiB 的瞬时内存对桌面进程完全可接受。
///
/// 达到该值（`>=`）时完整内容先进受控内容存储，写入时按 [`BLOB_STREAM_CHUNK_BYTES`]
/// 分块追加：**进程内存峰值由分块大小决定，与文件大小无关**，这是流式唯一的
/// 目的——不是更快，而是几百 MiB 的文件不再撑爆内存、不再卡住界面。
pub const BLOB_UPLOAD_STREAM_THRESHOLD_BYTES: u64 = 16 * 1024 * 1024;

/// 流式上传每次追加的字节数。
///
/// 重要事实：MySQL / SQLite / PostgreSQL 的追加写法（`c = c || ?`、
/// `CONCAT(c, ?)`）都会**重写整个值**，代价是 O(n²)，不是 O(n)。本机实测
/// （MySQL 8，64 MiB 目标）：
/// - 4 MiB 分块 / 16 次追加：8305 ms
/// - 16 MiB 分块 / 4 次追加：2050 ms
/// - 32 MiB 分块 / 2 次追加：980 ms
///
/// 所以分块越小反而越慢；只有在 Oracle 的 LOB 定位符这类真流式 API 上，
/// 分块大小才只影响往返次数。16 MiB 是折中：比 4 MiB 快 4 倍，峰值内存
/// 仍固定在 16 MiB 量级，且留在 MySQL 默认 `max_allowed_packet`（64 MiB）
/// 的安全余量内。连接器可据服务端能力下调，但不得上调到超过服务端包上限的一半。
pub const BLOB_STREAM_CHUNK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub type_name: Option<String>,
    pub nullable: bool,
    pub primary_key: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub is_primary: bool,
    pub index_type: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub column: String,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_column: String,
    /// 复合外键的有序列对（保持原始序号）；空则视为单列（column/ref_column）。
    /// 供 ER/约束身份按完整有序配对处理复合外键，不靠展示名拼接（§6.2）。
    pub columns: Vec<String>,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerInfo {
    pub name: String,
    pub event: String,
    pub timing: String,
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<CellValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BinaryCellSummary {
    pub type_name: String,
    pub is_null: bool,
    pub byte_length: u64,
    pub preview_hex: Option<String>,
}

/// 上传替换的写入负载：清空，或用某个本地文件替换整值。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UploadPayload {
    SetNull,
    FilePath(PathBuf),
}

/// 超大字段的种类：决定详情面板如何预览/保存，以及是否可能是合法 JSON。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LargeValueKind {
    Text,
    Json,
    Binary,
}

/// 超大字段的内容引用。
///
/// 与 [`CellValue::Text`]/[`CellValue::Json`] 严格区分：`preview` 只是有界前缀，
/// 权威内容在受控内容存储里，只能经 [`crate::CellContentRef`] 分段读取。
/// `content_id` 是**不透明句柄**，不含任何文件路径：UI、事件、日志、快照、
/// AI 上下文都只能看到它，拿不到也不能拼出临时文件位置。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LargeCellValue {
    pub content_id: String,
    pub kind: LargeValueKind,
    /// 完整内容的字节数；库端拿不到长度时为 `None`（不能瞎猜成预览长度）。
    pub byte_length: Option<u64>,
    /// 有界预览：文本按字符切、二进制为 hex 前缀。绝不当作权威数据。
    pub preview: String,
    /// 预览是否覆盖了完整内容（`preview.len() == byte_length` 时才可能为真）。
    pub preview_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CellValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Text(String),
    Bytes(Vec<u8>),
    BinarySummary(BinaryCellSummary),
    Json(String),
    /// 只加载了前缀的文本（如超过预览上限的 Oracle CLOB）。
    ///
    /// 必须与 `Text` 区分开：导出、备份、写回都会把 `Text` 当权威数据，
    /// 拿预览去写等于用片段覆盖完整值。消费方要么拒绝，要么先取完整值，
    /// 不能像旧实现那样把预览和一句说明拼成 `Text` 混进数据。
    TruncatedText { preview: String, total_chars: u64 },
    /// 超大字段的内容引用：完整内容已外置到受控存储，本值只带预览。
    ///
    /// 与 `TruncatedText` 的区别是**完整值仍可获得**（按内容引用分段读取），
    /// 因此结果不能被丢掉；但它同样不是权威数据，写回/导出/复制/AI 上下文
    /// 都必须走内容引用或显式拒绝。
    LargeValue(LargeCellValue),
}

impl CellValue {
    pub fn display_label(&self) -> String {
        match self {
            CellValue::Null => "NULL".to_string(),
            CellValue::Bool(value) => value.to_string(),
            CellValue::I64(value) => value.to_string(),
            CellValue::F64(value) => value.to_string(),
            CellValue::Text(value) => value.clone(),
            CellValue::Bytes(value) => format!("(BLOB) {} bytes", value.len()),
            CellValue::BinarySummary(summary) => {
                if summary.is_null {
                    "NULL".to_string()
                } else {
                    format!(
                        "{} [{}]",
                        summary.type_name.to_ascii_uppercase(),
                        format_byte_length(summary.byte_length)
                    )
                }
            }
            CellValue::Json(value) => value.clone(),
            CellValue::TruncatedText {
                preview,
                total_chars,
            } => format!("{preview}\n…（共 {total_chars} 字符，仅预览）"),
            CellValue::LargeValue(large) => match large.byte_length {
                Some(length) => format!("{}…（共 {}）", large.preview, format_byte_length(length)),
                None => format!("{}…（完整大小未知）", large.preview),
            },
        }
    }

    /// 该值是否只是预览（不是完整数据）。
    ///
    /// 写回、导出、备份、AI 上下文等「把值当权威数据」的路径统一用这个判定。
    pub fn is_truncated(&self) -> bool {
        matches!(
            self,
            CellValue::TruncatedText { .. } | CellValue::LargeValue(_)
        )
    }

    /// 超大字段的内容引用（只有它才能取回完整值）。
    pub fn large_value(&self) -> Option<&LargeCellValue> {
        match self {
            CellValue::LargeValue(large) => Some(large),
            _ => None,
        }
    }
}

pub fn is_binary_type_name(type_name: &str) -> bool {
    matches!(
        normalized_type_base(type_name).as_str(),
        "binary"
            | "varbinary"
            | "tinyblob"
            | "blob"
            | "mediumblob"
            | "longblob"
            | "bytea"
            | "image"
            | "raw"
            | "long raw"
    )
}

/// 可「上传替换」的文本类型（按基名判定）。
///
/// 只列真正能装下整份文本的类型：上传替换写的是整值，`CHAR`/`VARCHAR` 这类
/// 定长字段也支持，但它们有容量上限，由 [`text_type_max_bytes`] 预检。
pub fn is_text_type_name(type_name: &str) -> bool {
    matches!(
        normalized_type_base(type_name).as_str(),
        "char"
            | "character"
            | "bpchar"
            | "nchar"
            | "varchar"
            | "varchar2"
            | "nvarchar"
            | "character varying"
            | "tinytext"
            | "text"
            | "ntext"
            | "mediumtext"
            | "longtext"
            | "clob"
            | "nclob"
            | "long"
            | "citext"
            | "name"
    )
}

/// 无长度上限的「大文本」类型：界面据此给出上传替换入口。
///
/// 与 [`is_text_type_name`] 分开：`CHAR`/`VARCHAR` 也支持上传替换（有容量上限、
/// 会被预检拒绝），但它们是普通字段，标题行上再放一个上传按钮只会挤占空间。
pub fn is_large_text_type_name(type_name: &str) -> bool {
    let base = normalized_type_base(type_name);
    // SQL Server 用 `varchar(max)`/`nvarchar(max)` 表示无上限文本，去掉长度参数后
    // 只剩基名，必须单独按 `(max)` 判定，否则大文本列拿不到上传入口。
    let unbounded = matches!(
        base.as_str(),
        "char" | "character" | "varchar" | "varchar2" | "nvarchar" | "nchar" | "character varying"
    ) && declares_max_length(type_name);
    unbounded
        || matches!(
            base.as_str(),
            "text" | "ntext" | "mediumtext" | "longtext" | "clob" | "nclob" | "long" | "citext"
        )
}

/// 类型声明的长度参数是否为 `max`（SQL Server 的无上限写法）。
fn declares_max_length(type_name: &str) -> bool {
    let Some(start) = type_name.find('(') else {
        return false;
    };
    let Some(end) = type_name[start + 1..].find(')') else {
        return false;
    };
    type_name[start + 1..start + 1 + end]
        .trim()
        .eq_ignore_ascii_case("max")
}

/// JSON 专用类型：内容不是自由文本，追加分块无法保证每一块都是合法 JSON。
pub fn is_json_type_name(type_name: &str) -> bool {
    matches!(normalized_type_base(type_name).as_str(), "json" | "jsonb")
}

/// 该单元格能否「上传替换」，以及按哪种内容种类写入。
///
/// 返回 `None` 表示不能：JSON 列的分块追加无法保证每一块都是合法 JSON，
/// 数值/时间等类型本来也不该用文件覆盖。`type_name` 缺失时只按值兜底——
/// 只有已经是二进制或大字段的单元格才放开，避免把普通文本列误当大字段。
pub fn upload_replace_kind(type_name: Option<&str>, value: &CellValue) -> Option<LargeValueKind> {
    if let Some(name) = type_name {
        if is_binary_type_name(name) {
            return Some(LargeValueKind::Binary);
        }
        if is_json_type_name(name) {
            return None;
        }
        if is_text_type_name(name) {
            return Some(LargeValueKind::Text);
        }
    }
    match value {
        CellValue::Bytes(_) | CellValue::BinarySummary(_) => Some(LargeValueKind::Binary),
        CellValue::LargeValue(large) => Some(match large.kind {
            LargeValueKind::Binary => LargeValueKind::Binary,
            _ => LargeValueKind::Text,
        }),
        _ => None,
    }
}

/// 文本类型的字节上限（尽力而为的预检，最终仍由数据库校验）。
///
/// 带长度的字符类型按 **4 字节/字符** 估算：宁可让数据库拒绝，也不要误拒
/// 合法的多字节内容。`TEXT`/`CLOB` 一类没有声明长度，返回 `None`（仅受全局上限约束）。
pub fn text_type_max_bytes(type_name: &str) -> Option<u64> {
    let normalized = type_name.trim().to_ascii_lowercase();
    match normalized_type_base(&normalized).as_str() {
        "tinytext" => Some(255),
        "text" | "ntext" => Some(65_535),
        "mediumtext" => Some(16_777_215),
        "longtext" | "long" => Some(4_294_967_295),
        "char" | "character" | "bpchar" | "nchar" | "varchar" | "varchar2" | "nvarchar"
        | "character varying" => binary_type_length_arg(&normalized).map(|len| len.saturating_mul(4)),
        _ => None,
    }
}

/// 类型名归一化：去掉 `(n)` 与多余空白，统一小写。
fn normalized_type_base(type_name: &str) -> String {
    type_name
        .trim()
        .split('(')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub fn binary_type_max_bytes(type_name: &str) -> Option<u64> {
    let normalized = type_name.trim().to_ascii_lowercase();
    match normalized_type_base(&normalized).as_str() {
        "binary" | "varbinary" | "raw" => binary_type_length_arg(&normalized),
        "tinyblob" => Some(255),
        "blob" => Some(65_535),
        "mediumblob" => Some(16_777_215),
        "longblob" => Some(4_294_967_295),
        _ => None,
    }
}

fn binary_type_length_arg(type_name: &str) -> Option<u64> {
    let start = type_name.find('(')?;
    let end = type_name[start + 1..].find(')')? + start + 1;
    type_name[start + 1..end].trim().parse().ok()
}

pub fn format_byte_length(byte_length: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    match byte_length {
        0..=1023 => format!("{byte_length} B"),
        1024..=1_048_575 => format!("{:.1} KB", byte_length as f64 / KB),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", byte_length as f64 / MB),
        1_073_741_824..=1_099_511_627_775 => format!("{:.1} GB", byte_length as f64 / GB),
        _ => format!("{:.1} TB", byte_length as f64 / TB),
    }
}

#[cfg(test)]
mod type_name_tests {
    use super::*;

    /// SQL Server 的无上限文本用 `(max)` 表示：去掉长度参数后只剩基名，
    /// 必须仍被判成大文本，否则界面不给上传入口。
    #[test]
    fn sqlserver_max_length_text_is_large_text() {
        assert!(is_large_text_type_name("nvarchar(max)"));
        assert!(is_large_text_type_name("VARCHAR(MAX)"));
        assert!(is_text_type_name("varchar(max)"));
        // 声明了具体长度的仍是普通字段。
        assert!(!is_large_text_type_name("varchar(255)"));
        assert!(!is_large_text_type_name("nvarchar(100)"));
        // `MAX` 只对字符/二进制类型有意义，不能把 `decimal(10, 2)` 之类误判。
        assert!(!is_large_text_type_name("decimal(10, 2)"));
    }

    #[test]
    fn upload_replace_kind_follows_column_type() {
        let empty = CellValue::Null;
        assert_eq!(
            upload_replace_kind(Some("LONGTEXT"), &empty),
            Some(LargeValueKind::Text)
        );
        assert_eq!(
            upload_replace_kind(Some("nvarchar(max)"), &empty),
            Some(LargeValueKind::Text)
        );
        assert_eq!(
            upload_replace_kind(Some("varbinary(max)"), &empty),
            Some(LargeValueKind::Binary)
        );
        // JSON 列分块追加无法保证每块合法，明确不开放。
        assert_eq!(upload_replace_kind(Some("jsonb"), &empty), None);
        // 类型缺失时按值兜底：普通值不放开，已是二进制/大字段才放开。
        assert_eq!(upload_replace_kind(None, &CellValue::Text("x".into())), None);
        assert_eq!(
            upload_replace_kind(None, &CellValue::Bytes(vec![1])),
            Some(LargeValueKind::Binary)
        );
    }

    #[test]
    fn text_type_max_bytes_covers_declared_limits() {
        assert_eq!(text_type_max_bytes("tinytext"), Some(255));
        assert_eq!(text_type_max_bytes("text"), Some(65_535));
        assert_eq!(text_type_max_bytes("varchar(10)"), Some(40));
        // 无上限类型与 `(max)` 都不设容量上限。
        assert_eq!(text_type_max_bytes("longtext"), Some(4_294_967_295));
        assert_eq!(text_type_max_bytes("nvarchar(max)"), None);
        assert_eq!(text_type_max_bytes("clob"), None);
    }
}
