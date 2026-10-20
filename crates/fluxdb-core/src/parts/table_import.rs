use std::path::Path;

/// 与“导出数据”保持同一格式集合；SQL 导出不作为导入来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableImportFormat {
    Txt,
    Csv,
    Json,
    Xml,
}

impl TableImportFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("txt") => Some(Self::Txt),
            Some("csv") => Some(Self::Csv),
            Some("json") => Some(Self::Json),
            Some("xml") => Some(Self::Xml),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Txt => "TXT",
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::Xml => "XML",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableImportRequest {
    pub config: ConnectionConfig,
    pub object: ObjectPath,
    pub source: PathBuf,
    pub format: TableImportFormat,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableImportReport {
    pub total_rows: u64,
    pub imported_rows: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableImportProgress {
    pub stage: String,
    pub message: String,
}

/// 表数据导入的统一能力边界。
///
/// 实现负责解析文件、校验目标列并按数据库方言执行单条参数化 INSERT；
/// 上层只依赖该 trait，不直接拼 SQL 或处理数据库差异。
pub trait TableDataImport: Send + Sync {
    fn kind(&self) -> DatabaseKind;
    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> crate::Result<TableImportReport>;
}
