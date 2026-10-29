// MongoDB 文档批量导入：文件格式、字段类型与预览/报告结构。
//
// 与 `table_import` 分开的原因：关系库导入的终态是「行 × 目标列」，MongoDB 导入的
// 终态是「一篇文档」，且 CSV 的每个字段要先推断成 BSON 类型（`_id` 24 位十六进制
// 必须是 `ObjectId`，否则插进去和集合里既有数据对不上）。

/// CSv 字段可指定的 BSON 类型（UI 下拉选项的顺序即此顺序）。
///
/// 只保留导入场景真正用得上的标量类型：嵌套对象/数组由 JSON 导入路径承担，
/// 多一层 CSV 编码语法会让用户多学一套规则。
///
/// `mixed` 表示该列在文件里出现多种类型，写入时逐单元格重新嗅探（与 Compass 一致）；
/// 它是推断结果，用户也可以手动选它来强制逐格判断。
pub const MONGO_IMPORT_FIELD_TYPES: [&str; 9] = [
    "objectId", "string", "int32", "int64", "double", "boolean", "date", "null", "mixed",
];

/// 源文件格式。按内容嗅探，不信任扩展名（`.txt` 里放 JSON 数组是常见情况）。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MongoImportFormat {
    /// 顶层是对象数组：`[{…}, {…}]`。
    #[default]
    JsonArray,
    /// 每行一个对象（JSON Lines / NDJSON）。
    JsonLines,
    /// 逗号等分隔符分隔，首行为字段名。
    Csv,
}

impl MongoImportFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::JsonArray => "JSON",
            Self::JsonLines => "JSONL",
            Self::Csv => "CSV",
        }
    }

    /// 该格式是否按字段类型写入（只有 CSV 需要先推断类型）。
    pub fn has_field_types(self) -> bool {
        matches!(self, Self::Csv)
    }
}

/// 单个字段的预览信息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MongoImportField {
    pub name: String,
    /// 按样本推断出的 BSON 类型名（`MONGO_IMPORT_FIELD_TYPES` 中的一项）。
    pub detected: String,
    /// 首个非空样本值，便于用户判断推断是否合理。
    pub sample: String,
}

/// 用户对某个 CSV 字段的类型覆盖。未出现在列表里的字段按推断类型写入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MongoImportFieldOverride {
    pub name: String,
    pub type_name: String,
}

/// 导入选项。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MongoImportOptions {
    /// CSV 分隔符；空表示逗号。只取第一个字符。
    pub delimiter: String,
    /// CSV 空字符串是否按 `null` 写入（关掉则写成空字符串）。
    pub empty_as_null: bool,
    /// 字段类型覆盖；未列出的字段用推断类型。
    pub field_overrides: Vec<MongoImportFieldOverride>,
}

impl MongoImportOptions {
    /// 默认：逗号分隔、空串按 `null`、不覆盖任何字段。
    pub fn new() -> Self {
        Self {
            delimiter: String::new(),
            empty_as_null: true,
            field_overrides: Vec::new(),
        }
    }

    pub fn delimiter_byte(&self) -> u8 {
        self.delimiter.as_bytes().first().copied().unwrap_or(b',')
    }

    /// 取字段的生效类型：有覆盖用覆盖，否则用推断值。
    pub fn field_type(&self, name: &str, detected: &str) -> String {
        self.field_overrides
            .iter()
            .find(|item| item.name == name)
            .map(|item| item.type_name.clone())
            .unwrap_or_else(|| detected.to_string())
    }
}

/// 导入前预览；只解析有限行，不写数据库。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MongoImportPreview {
    pub format: MongoImportFormat,
    /// CSV 的字段名；JSON 导入没有列概念（每行自带键），为空。
    pub columns: Vec<String>,
    /// CSV 的字段推断结果；JSON 导入为空（类型由文档自身携带）。
    pub fields: Vec<MongoImportField>,
    /// 预览行。CSV 按 `columns` 对齐；JSON 每行只有一个元素（整篇文档文本）。
    pub rows: Vec<Vec<String>>,
    /// 预览只读前若干行/条；真实条数需完整解析后才能确定。
    pub truncated: bool,
}

/// 导入进度；`stage` 是短标签，`message` 是可直接展示的一行说明。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MongoImportProgress {
    pub stage: String,
    pub message: String,
}

/// 导入结果。坏文档按「跳过并继续」处理，因此失败不中断，但要如实计数并给出原因。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MongoImportReport {
    pub inserted: u64,
    pub failed: u64,
    /// 失败原因（按文档序号），只保留前若干条，避免超大文件把内存撑爆。
    pub errors: Vec<String>,
    /// 用户中途取消：已写入的批次保留，不再继续。
    pub cancelled: bool,
}

/// 预览请求：连接配置由连接器上下文提供，这里只带目标集合与源文件。
#[derive(Clone, Debug, PartialEq)]
pub struct MongoImportPreviewRequest {
    pub object: ObjectPath,
    pub source: PathBuf,
    pub options: MongoImportOptions,
}

/// 导入请求。
#[derive(Clone, Debug, PartialEq)]
pub struct MongoImportRequest {
    pub object: ObjectPath,
    pub source: PathBuf,
    pub options: MongoImportOptions,
}
