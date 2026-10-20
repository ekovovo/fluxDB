//! 符号表：从 AST 收集语句内可见的关系（表/别名）、CTE 与投影别名。
//!
//! Phase 1 采用「整条语句平铺收集」：子查询内层能看到外层关系（相关子查询可用），
//! 代价是内层同名别名遮蔽外层无法区分，后续阶段再引入作用域链细化。

use core::ops::ControlFlow;
use sqlparser::ast::{
    Expr, ObjectName, ObjectNamePart, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    Visit, Visitor,
};

use crate::metadata::TableKey;

/// 一个语句内可见的关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationSymbol {
    pub(crate) key: TableKey,
    pub(crate) alias: Option<String>,
}

impl RelationSymbol {
    /// 该关系是否可用 `qualifier` 引用（别名优先，其次表名）。
    pub(crate) fn matches(&self, qualifier: &str) -> bool {
        self.alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(qualifier))
            || self.key.table.eq_ignore_ascii_case(qualifier)
    }
}

/// 语句内可见的 CTE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CteSymbol {
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
}

/// 派生表（`FROM (SELECT ...) t`）的别名与投影列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedSymbol {
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
}

/// 语句符号表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Symbols {
    pub(crate) relations: Vec<RelationSymbol>,
    pub(crate) ctes: Vec<CteSymbol>,
    pub(crate) derived: Vec<DerivedSymbol>,
    /// 投影别名（`SELECT a AS x` 的 `x`），用于 ORDER BY / GROUP BY 槽位。
    pub(crate) output_aliases: Vec<String>,
}

impl Symbols {
    /// 解析 `qualifier`（可能是 `schema.table` 形式）命中的关系。
    pub(crate) fn resolve_qualifier(&self, qualifier: &str) -> Vec<TableKey> {
        let last = qualifier.rsplit('.').next().unwrap_or(qualifier);
        self.relations
            .iter()
            .filter(|relation| relation.matches(last))
            .map(|relation| relation.key.clone())
            .collect()
    }

    /// 限定符命中的派生列来源（CTE 优先于派生表，二者都遮蔽同名物理表）。
    pub(crate) fn qualifier_virtual_columns(&self, qualifier: &str) -> Option<Vec<String>> {
        let last = qualifier.rsplit('.').next().unwrap_or(qualifier);
        self.ctes
            .iter()
            .find(|cte| cte.name.eq_ignore_ascii_case(last))
            .map(|cte| cte.columns.clone())
            .or_else(|| {
                self.derived
                    .iter()
                    .find(|derived| derived.name.eq_ignore_ascii_case(last))
                    .map(|derived| derived.columns.clone())
            })
    }

    /// 无限定符时可见的 CTE / 派生表列。
    pub(crate) fn virtual_columns(&self) -> Vec<String> {
        self.ctes
            .iter()
            .flat_map(|cte| cte.columns.iter().cloned())
            .chain(
                self.derived
                    .iter()
                    .flat_map(|derived| derived.columns.iter().cloned()),
            )
            .collect()
    }

    /// 语句内出现的全部关系（用于 `SELECT <cursor>` 这类无限定符场景）。
    pub(crate) fn relation_keys(&self) -> Vec<TableKey> {
        self.relations
            .iter()
            .map(|relation| relation.key.clone())
            .collect()
    }
}

/// 从语句列表收集符号。
pub(crate) fn collect(statements: &[Statement]) -> Symbols {
    let mut collector = SymbolCollector::default();
    for statement in statements {
        let _ = statement.visit(&mut collector);
    }
    collector.symbols
}

#[derive(Default)]
struct SymbolCollector {
    symbols: Symbols,
}

impl Visitor for SymbolCollector {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let name = cte.alias.name.value.clone();
                let columns = if cte.alias.columns.is_empty() {
                    projection_columns(&cte.query)
                } else {
                    cte.alias
                        .columns
                        .iter()
                        .map(|column| column.name.value.clone())
                        .collect()
                };
                if !self
                    .symbols
                    .ctes
                    .iter()
                    .any(|existing| existing.name.eq_ignore_ascii_case(&name))
                {
                    self.symbols.ctes.push(CteSymbol { name, columns });
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        for item in &select.projection {
            if let SelectItem::ExprWithAlias { alias, .. } = item
                && !self
                    .symbols
                    .output_aliases
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&alias.value))
            {
                self.symbols.output_aliases.push(alias.value.clone());
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, table_factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Derived {
            subquery, alias, ..
        } = table_factor
        {
            if let Some(alias) = alias {
                self.symbols.derived.push(DerivedSymbol {
                    name: alias.name.value.clone(),
                    columns: projection_columns(subquery),
                });
            }
            return ControlFlow::Continue(());
        }
        if let TableFactor::Table { name, alias, .. } = table_factor {
            let parts = object_name_parts(name);
            let Some(table) = parts.last().cloned() else {
                return ControlFlow::Continue(());
            };
            let schema = parts
                .len()
                .checked_sub(2)
                .and_then(|index| parts.get(index))
                .cloned();
            let alias = alias.as_ref().map(|alias| alias.name.value.clone());
            let key = TableKey {
                database: None,
                schema,
                table,
            };
            if !self
                .symbols
                .relations
                .iter()
                .any(|existing| existing.key == key && existing.alias == alias)
            {
                self.symbols.relations.push(RelationSymbol { key, alias });
            }
        }
        ControlFlow::Continue(())
    }
}

/// 子查询投影列名：别名优先，其次裸列引用。CTE / 派生表都用它推导可见列。
fn projection_columns(query: &Query) -> Vec<String> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Vec::new();
    };
    select
        .projection
        .iter()
        .filter_map(|item| match item {
            SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => Some(ident.value.clone()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                parts.last().map(|ident| ident.value.clone())
            }
            _ => None,
        })
        .collect()
}

/// `ObjectName` 的各段名（去掉引号）。
pub(crate) fn object_name_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => ident.value.clone(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_tolerant;
    use sqlparser::dialect::GenericDialect;

    fn symbols(sql: &str) -> Symbols {
        let parsed =
            parse_tolerant(sql, None, sql.len(), &GenericDialect {}).expect("测试语句应可解析");
        collect(&parsed.statements)
    }

    #[test]
    fn collects_relations_with_alias() {
        let symbols = symbols("SELECT * FROM users u JOIN orders o ON u.id = o.user_id");
        assert_eq!(symbols.relations.len(), 2);
        assert_eq!(symbols.relations[0].alias.as_deref(), Some("u"));
        assert_eq!(symbols.relations[1].key.table, "orders");
    }

    #[test]
    fn resolves_alias_and_table_name() {
        let symbols = symbols("SELECT * FROM public.users u");
        assert_eq!(symbols.resolve_qualifier("u").len(), 1);
        assert_eq!(symbols.resolve_qualifier("users").len(), 1);
        assert_eq!(symbols.resolve_qualifier("public.users").len(), 1);
        assert!(symbols.resolve_qualifier("nope").is_empty());
    }

    #[test]
    fn keeps_schema_from_qualified_name() {
        let symbols = symbols("SELECT * FROM public.users");
        assert_eq!(symbols.relations[0].key.schema.as_deref(), Some("public"));
        assert_eq!(symbols.relations[0].key.table, "users");
    }

    #[test]
    fn collects_cte_names() {
        let symbols = symbols("WITH active AS (SELECT id FROM users) SELECT * FROM active");
        assert!(symbols.ctes.iter().any(|cte| cte.name == "active"));
    }

    #[test]
    fn collects_relations_inside_subquery() {
        let symbols = symbols("SELECT * FROM users u WHERE u.id IN (SELECT user_id FROM orders)");
        assert_eq!(symbols.relations.len(), 2);
    }
}
