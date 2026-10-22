//! 语义诊断：复用容错解析与符号表，标出未知的表 / schema / 列。
//!
//! 与补全共用 [`parse_tolerant`] + [`collect`]，不再维护第二套解析或词法启发式。
//! 只报「元数据明确覆盖、但名字对不上」的对象：宿主没加载这张表、没拉过这张表的列
//! 时一律不报——宁可漏报，不可误报。

use core::ops::ControlFlow;
use std::collections::{HashMap, HashSet};

use sqlparser::ast::{
    Expr, Ident, ObjectName, Spanned, Statement, TableFactor, TableObject, Visit, Visitor,
};
use sqlparser::tokenizer::Span;

use crate::dialect::CompletionDialect;
use crate::metadata::{MetadataSource, TableKey, TableMeta};
use crate::parse::{LineColIndex, TolerantParse, parse_tolerant};
use crate::symbols::{ScopeTree, Symbols, collect, object_name_parts};

/// 诊断类别。展示层据此映射文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    UnknownTable,
    UnknownSchema,
    UnknownColumn,
}

/// 一条语义诊断。偏移相对 [`DiagnosticRequest::text`] 的字节下标，`end` 为开区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    pub kind: DiagnosticKind,
    /// 出问题的原始名字（未加引号），供展示层拼文案。
    pub name: String,
}

/// 诊断输入。
pub struct DiagnosticRequest<'a> {
    pub text: &'a str,
    /// 连接所在库；仅用于给表键补上下文。
    pub database: Option<&'a str>,
    /// 连接默认 schema。
    pub schema: Option<&'a str>,
}

/// 对一段 SQL 做语义诊断。
///
/// 元数据为空（宿主还没加载任何对象）或语句四级容错解析全败时返回空：这两种情况
/// 都无法区分「对象不存在」与「我们不知道」，报出来只会是噪声。
pub fn diagnose(
    request: &DiagnosticRequest<'_>,
    dialect: &dyn CompletionDialect,
    meta: &dyn MetadataSource,
) -> Vec<Diagnostic> {
    let text = request.text;
    if text.trim().is_empty() {
        return Vec::new();
    }
    let tables = meta.tables(request.schema);
    let schemas = meta.schemas();
    if tables.is_empty() && schemas.is_empty() {
        return Vec::new();
    }
    let Some(parsed) = parse_tolerant(text, None, text.len(), dialect.parser_dialect()) else {
        return Vec::new();
    };
    let mut diagnoser = Diagnoser {
        meta,
        tables,
        schemas,
        database: request.database,
        schema: request.schema,
        parsed: &parsed,
        index: LineColIndex::new(text),
        text,
        columns_cache: HashMap::new(),
        tree: ScopeTree::default(),
        symbols: Symbols::default(),
        current_scope: None,
        diagnostics: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    // 逐语句独立收集符号：跨语句的表集合不该互相可见，否则未限定列检查会拿别的
    // 语句的表当可见范围。
    for statement in &parsed.statements {
        diagnoser.tree = collect(std::slice::from_ref(statement), &diagnoser.index);
        diagnoser.current_scope = None;
        diagnoser.diagnostics.clear();
        // INSERT 的目标不是 TableFactor，单独取一次。
        if let Statement::Insert(insert) = statement
            && let TableObject::TableName(name) = &insert.table
        {
            diagnoser.refresh_symbols(name.span());
            diagnoser.check_table(name);
        }
        let _ = statement.visit(&mut diagnoser);
        diagnostics.append(&mut diagnoser.diagnostics);
    }
    diagnostics.sort_unstable_by_key(|diagnostic| diagnostic.start);
    diagnostics
}

struct Diagnoser<'a> {
    meta: &'a dyn MetadataSource,
    /// 连接可见的表与视图。
    tables: Vec<TableMeta>,
    schemas: Vec<String>,
    database: Option<&'a str>,
    schema: Option<&'a str>,
    parsed: &'a TolerantParse,
    index: LineColIndex,
    text: &'a str,
    /// 列名（小写）按表缓存：一次诊断内同一张表只问宿主一次。
    columns_cache: HashMap<TableKey, HashSet<String>>,
    /// 当前语句的作用域树，每条语句重算。
    tree: ScopeTree,
    /// 当前节点处可见的符号（`tree` 按节点位置投影）。
    symbols: Symbols,
    /// `symbols` 对应的作用域，避免同一作用域内重复投影。
    current_scope: Option<usize>,
    diagnostics: Vec<Diagnostic>,
}

impl Diagnoser<'_> {
    /// 把符号表切到 `span` 起点所在的作用域。
    ///
    /// 诊断按节点遍历而非按光标，同一个作用域里的节点共用一次投影。
    fn refresh_symbols(&mut self, span: Span) {
        let offset = self.index.offset(span.start);
        // 落在所有 span 之外（解析被截断）时统一记为 `usize::MAX`，与初始的 `None` 区分。
        let scope = self.tree.containing_scope_at(offset).unwrap_or(usize::MAX);
        if Some(scope) == self.current_scope {
            return;
        }
        self.current_scope = Some(scope);
        self.symbols = self.tree.visible_at(offset);
    }

    /// sqlparser span（1 基 line/col，end 闭区间）→ 原文字节区间（end 开区间）。
    fn range(&self, span: Span) -> (usize, usize) {
        let start = self.parsed.map_offset(self.index.offset(span.start));
        let end = self
            .parsed
            .map_offset(self.index.end_offset(span.end))
            .min(self.text.len());
        (start, end.max(start))
    }

    /// 检查表名存在性；限定名还要检查 schema 段。
    fn check_table(&mut self, name: &ObjectName) {
        let parts = object_name_parts(name);
        let Some(table) = parts.last().cloned() else {
            return;
        };
        // CTE / 派生表别名不是物理表，符号表已知道它们。
        if self.symbols.is_virtual(&table) {
            return;
        }
        let (start, end) = self.range(name.span());
        if !self.schemas.is_empty()
            && let Some(schema) = parts
                .len()
                .checked_sub(2)
                .and_then(|index| parts.get(index))
            && !self
                .schemas
                .iter()
                .any(|known| known.eq_ignore_ascii_case(schema))
        {
            self.diagnostics.push(Diagnostic {
                start,
                end: (start + schema.len()).min(end),
                kind: DiagnosticKind::UnknownSchema,
                name: schema.clone(),
            });
        }
        if !self
            .tables
            .iter()
            .any(|known| known.name.eq_ignore_ascii_case(&table))
        {
            self.diagnostics.push(Diagnostic {
                start,
                end,
                kind: DiagnosticKind::UnknownTable,
                name: table,
            });
        }
    }

    /// 限定符可见的列名（小写）。
    ///
    /// `None` 表示这个限定符不是语句内已知的关系，或它的列元数据缺失——两种情况都
    /// 不该报「未知列」：错的是限定符（另由表名检查负责），或我们的元数据不够。
    fn qualifier_columns(&mut self, qualifier: &str) -> Option<HashSet<String>> {
        if let Some(columns) = self.symbols.qualifier_virtual_columns(qualifier) {
            return Some(lowercase(&columns));
        }
        let keys = self.symbols.resolve_qualifier(qualifier);
        if keys.is_empty() {
            return None;
        }
        let mut visible = HashSet::new();
        for key in &keys {
            let columns = self.table_columns(key);
            if columns.is_empty() {
                return None;
            }
            visible.extend(columns);
        }
        Some(visible)
    }

    /// 无限定符可见的列名（小写）。
    ///
    /// `None` = 语句里有关系拿不到列元数据（或根本没有关系），此时不做未限定列检查：
    /// 少报一个列，比在 `SELECT a FROM t1, t2` 里把 `t2` 的列全标红好。
    fn unqualified_columns(&mut self) -> Option<HashSet<String>> {
        let keys = self.symbols.relation_keys();
        if keys.is_empty() {
            return None;
        }
        let mut visible = HashSet::new();
        for key in &keys {
            let virtual_columns = self.symbols.qualifier_virtual_columns(&key.table);
            let columns = match virtual_columns {
                Some(columns) => lowercase(&columns),
                None => self.table_columns(key),
            };
            if columns.is_empty() {
                return None;
            }
            visible.extend(columns);
        }
        Some(visible)
    }

    /// 某表的列名（小写）。空 = 宿主没有这张表的列元数据。
    fn table_columns(&mut self, key: &TableKey) -> HashSet<String> {
        let meta = self.meta;
        let key = key.with_context(self.database, self.schema);
        let cached = self.columns_cache.entry(key.clone()).or_insert_with(|| {
            let names: Vec<String> = meta
                .columns(&key)
                .iter()
                .map(|column| column.name.clone())
                .collect();
            lowercase(&names)
        });
        cached.clone()
    }

    fn check_qualified_column(&mut self, parts: &[Ident]) {
        let Some(column) = parts.last() else {
            return;
        };
        let qualifier = &parts[parts.len() - 2];
        let Some(visible) = self.qualifier_columns(&qualifier.value) else {
            return;
        };
        if visible.contains(&column.value.to_ascii_lowercase()) {
            return;
        }
        let (start, end) = self.range(column.span);
        self.diagnostics.push(Diagnostic {
            start,
            end,
            kind: DiagnosticKind::UnknownColumn,
            name: column.value.clone(),
        });
    }

    fn check_unqualified_column(&mut self, ident: &Ident) {
        // ORDER BY / GROUP BY 里可以引用投影别名，那不是列。
        if self
            .symbols
            .output_aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(&ident.value))
        {
            return;
        }
        let Some(visible) = self.unqualified_columns() else {
            return;
        };
        if visible.contains(&ident.value.to_ascii_lowercase()) {
            return;
        }
        let (start, end) = self.range(ident.span);
        self.diagnostics.push(Diagnostic {
            start,
            end,
            kind: DiagnosticKind::UnknownColumn,
            name: ident.value.clone(),
        });
    }
}

impl Visitor for Diagnoser<'_> {
    type Break = ();

    fn pre_visit_table_factor(&mut self, table_factor: &TableFactor) -> ControlFlow<Self::Break> {
        // 只认真正的关系引用；DDL（`CREATE TABLE foo`）不走这里，不会把新建对象标红。
        if let TableFactor::Table { name, .. } = table_factor {
            self.refresh_symbols(name.span());
            self.check_table(name);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        match expr {
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                if let Some(column) = parts.last() {
                    self.refresh_symbols(column.span);
                }
                self.check_qualified_column(parts);
            }
            Expr::Identifier(ident) => {
                self.refresh_symbols(ident.span);
                self.check_unqualified_column(ident);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn lowercase(names: &[String]) -> HashSet<String> {
    names.iter().map(|name| name.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{ColumnMeta, ForeignKeyMeta, TableKind};

    /// 表 `users(id, name)`；`orders` 只在需要时给列元数据，用来验证「元数据不全就不报」。
    struct MockMeta {
        orders_columns_known: bool,
    }

    impl MockMeta {
        fn full() -> Self {
            Self {
                orders_columns_known: true,
            }
        }

        fn users_only() -> Self {
            Self {
                orders_columns_known: false,
            }
        }
    }

    fn column(table: &str, name: &str) -> ColumnMeta {
        ColumnMeta {
            table: table.to_string(),
            name: name.to_string(),
            type_name: Some("int".to_string()),
            nullable: true,
            primary_key: false,
            comment: None,
        }
    }

    impl MetadataSource for MockMeta {
        fn tables(&self, _schema: Option<&str>) -> Vec<TableMeta> {
            vec![
                TableMeta {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    kind: TableKind::Table,
                    comment: None,
                },
                TableMeta {
                    schema: Some("public".to_string()),
                    name: "orders".to_string(),
                    kind: TableKind::Table,
                    comment: None,
                },
            ]
        }

        fn schemas(&self) -> Vec<String> {
            vec!["public".to_string()]
        }

        fn columns(&self, key: &TableKey) -> Vec<ColumnMeta> {
            match key.table.as_str() {
                "users" => vec![column("users", "id"), column("users", "name")],
                "orders" if self.orders_columns_known => {
                    vec![column("orders", "id"), column("orders", "user_id")]
                }
                _ => Vec::new(),
            }
        }

        fn foreign_keys(&self, _key: &TableKey) -> Vec<ForeignKeyMeta> {
            Vec::new()
        }
    }

    fn run(sql: &str, meta: &dyn MetadataSource) -> Vec<Diagnostic> {
        let request = DiagnosticRequest {
            text: sql,
            database: None,
            schema: None,
        };
        diagnose(&request, &crate::dialect::GENERIC, meta)
    }

    fn kinds(diagnostics: &[Diagnostic]) -> Vec<(DiagnosticKind, String)> {
        diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.kind, diagnostic.name.clone()))
            .collect()
    }

    #[test]
    fn reports_unknown_table_after_from() {
        let diagnostics = run("SELECT * FROM nope", &MockMeta::full());
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownTable, "nope".to_string())]
        );
    }

    #[test]
    fn accepts_known_tables_and_aliases() {
        let sql = "SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id";
        let diagnostics = run(sql, &MockMeta::full());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn reports_unknown_schema() {
        let diagnostics = run("SELECT * FROM nope.users", &MockMeta::full());
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownSchema, "nope".to_string())]
        );
    }

    #[test]
    fn accepts_known_schema() {
        let diagnostics = run("SELECT * FROM public.users", &MockMeta::full());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn reports_unknown_qualified_column() {
        let diagnostics = run("SELECT u.nope FROM users u", &MockMeta::full());
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownColumn, "nope".to_string())]
        );
    }

    #[test]
    fn ignores_qualifier_without_column_metadata() {
        let diagnostics = run("SELECT o.nope FROM orders o", &MockMeta::users_only());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn reports_unknown_unqualified_column() {
        let diagnostics = run("SELECT nope FROM users", &MockMeta::full());
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownColumn, "nope".to_string())]
        );
    }

    #[test]
    fn skips_unqualified_check_when_any_relation_lacks_columns() {
        // orders 没有列元数据 → 不能断言 nope 不是列，整条语句放弃未限定列检查。
        let diagnostics = run("SELECT nope FROM users, orders", &MockMeta::users_only());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_output_alias_in_order_by() {
        let diagnostics = run(
            "SELECT id AS total FROM users ORDER BY total",
            &MockMeta::full(),
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_cte_and_derived_aliases() {
        let cte = run(
            "WITH active AS (SELECT id FROM users) SELECT active.id FROM active",
            &MockMeta::full(),
        );
        assert!(cte.is_empty(), "{cte:?}");
        let derived = run(
            "SELECT t.id FROM (SELECT id FROM users) t",
            &MockMeta::full(),
        );
        assert!(derived.is_empty(), "{derived:?}");
    }

    /// 兄弟子查询的表不在作用域里：第二个子查询的 `user_id` 不能靠第一个子查询的
    /// `orders` 蒙混过关。
    #[test]
    fn sibling_subquery_relations_are_not_visible() {
        let diagnostics = run(
            "SELECT * FROM users WHERE name IN (SELECT id FROM orders) \
             AND user_id IN (SELECT id FROM users)",
            &MockMeta::full(),
        );
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownColumn, "user_id".to_string())]
        );
    }

    #[test]
    fn reports_unknown_insert_target() {
        let diagnostics = run("INSERT INTO nope (id) VALUES (1)", &MockMeta::full());
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownTable, "nope".to_string())]
        );
    }

    #[test]
    fn flat_source_without_schemas_skips_schema_checks() {
        // desktop 的 SqlCompletionSource 只有裸表名、没有 schema 维度：限定名只按末段
        // 判存在性，schema 段无法证伪就不报。
        struct FlatMeta;
        impl MetadataSource for FlatMeta {
            fn tables(&self, _schema: Option<&str>) -> Vec<TableMeta> {
                vec![TableMeta {
                    schema: None,
                    name: "users".to_string(),
                    kind: TableKind::Table,
                    comment: None,
                }]
            }
            fn schemas(&self) -> Vec<String> {
                Vec::new()
            }
            fn columns(&self, _key: &TableKey) -> Vec<ColumnMeta> {
                vec![column("users", "id"), column("users", "name")]
            }
            fn foreign_keys(&self, _key: &TableKey) -> Vec<ForeignKeyMeta> {
                Vec::new()
            }
        }
        assert!(run("SELECT * FROM public.users", &FlatMeta).is_empty());
        assert!(run("SELECT * FROM nope.users", &FlatMeta).is_empty());
        let diagnostics = run("SELECT * FROM users WHERE nope = 1", &FlatMeta);
        assert_eq!(
            kinds(&diagnostics),
            vec![(DiagnosticKind::UnknownColumn, "nope".to_string())]
        );
    }

    #[test]
    fn reports_nothing_without_metadata() {
        struct NoMeta;
        impl MetadataSource for NoMeta {
            fn tables(&self, _schema: Option<&str>) -> Vec<TableMeta> {
                Vec::new()
            }
            fn schemas(&self) -> Vec<String> {
                Vec::new()
            }
            fn columns(&self, _key: &TableKey) -> Vec<ColumnMeta> {
                Vec::new()
            }
            fn foreign_keys(&self, _key: &TableKey) -> Vec<ForeignKeyMeta> {
                Vec::new()
            }
        }
        let diagnostics = run("SELECT nope FROM nowhere", &NoMeta);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}
