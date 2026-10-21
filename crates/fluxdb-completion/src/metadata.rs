//! 元数据来源抽象：引擎只认这组 trait，宿主负责接自己的缓存/索引。

/// 表的稳定引用。宿主的索引键（连接/库/schema/表）在实现侧还原。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TableKey {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

impl TableKey {
    pub fn new(
        database: Option<impl Into<String>>,
        schema: Option<impl Into<String>>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            database: database.map(Into::into),
            schema: schema.map(Into::into),
            table: table.into(),
        }
    }

    /// 仅按表名构造，用于宿主未提供库/schema 上下文的场景。
    pub fn bare(table: impl Into<String>) -> Self {
        Self {
            database: None,
            schema: None,
            table: table.into(),
        }
    }

    /// 用连接级上下文补全缺失的库/schema（AST 里通常只有名字）。
    pub fn with_context(&self, database: Option<&str>, schema: Option<&str>) -> Self {
        Self {
            database: self
                .database
                .clone()
                .or_else(|| database.map(str::to_string)),
            schema: self.schema.clone().or_else(|| schema.map(str::to_string)),
            table: self.table.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Table,
    View,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMeta {
    pub schema: Option<String>,
    pub name: String,
    pub kind: TableKind,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub table: String,
    pub name: String,
    pub type_name: Option<String>,
    pub nullable: bool,
    pub primary_key: bool,
    pub comment: Option<String>,
}

/// 外键：本表 `columns` 指向 `ref_table` 的 `ref_columns`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyMeta {
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Function,
    Procedure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineMeta {
    pub schema: Option<String>,
    pub name: String,
    pub signature: Option<String>,
    pub kind: RoutineKind,
}

/// 引擎向宿主索取元数据。实现必须是同步的；取消与缓存由宿主内部处理。
pub trait MetadataSource {
    /// 指定 schema 下的表与视图；`schema` 为 `None` 时取连接默认作用域。
    fn tables(&self, schema: Option<&str>) -> Vec<TableMeta>;

    /// 可用的 schema 列表。
    fn schemas(&self) -> Vec<String>;

    /// 指定表的列。
    fn columns(&self, key: &TableKey) -> Vec<ColumnMeta>;

    /// 指定表的外键。无 FK 元数据时返回空，引擎按「无 FK」回退。
    fn foreign_keys(&self, key: &TableKey) -> Vec<ForeignKeyMeta>;

    /// 例程（过程/函数）。默认空。
    fn routines(&self, _schema: Option<&str>) -> Vec<RoutineMeta> {
        Vec::new()
    }
}
