//! 符号表：按词法作用域收集语句内可见的关系（表/别名）、CTE/派生表与投影别名。
//!
//! 每个 `Query` 是一层作用域：子查询能看到外层关系（相关子查询可用），内层同名绑定
//! 遮蔽外层。消费方不直接遍历作用域树，而是通过 [`ScopeTree::visible_at`] 拿到投影到
//! 某个位置的扁平 [`Symbols`]——槽位策略与语义诊断都只认这个扁平视图。

use core::ops::ControlFlow;
use std::collections::{HashMap, HashSet};

use sqlparser::ast::{
    Expr, ObjectName, ObjectNamePart, Query, Select, SelectItem, SetExpr, Spanned, Statement,
    TableFactor, Visit, Visitor,
};
use sqlparser::tokenizer::Span;

use crate::metadata::TableKey;
use crate::parse::LineColIndex;

/// 一个语句内可见的关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationSymbol {
    pub(crate) key: TableKey,
    pub(crate) alias: Option<String>,
}

/// 投影到某个位置的可见符号。
///
/// 消费方看到的是扁平结构，作用域遮蔽已在投影时完成。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Symbols {
    /// 可见关系，按「外 → 内」排列；JOIN 槽位取末位当本次 JOIN 的表。
    pub(crate) relations: Vec<RelationSymbol>,
    /// 投影别名（`SELECT a AS x` 的 `x`），用于 ORDER BY / GROUP BY 槽位。
    pub(crate) output_aliases: Vec<String>,
    /// 限定符名（小写）→ 物理关系。投影时内层先写入，因此内层遮蔽外层。
    bindings: HashMap<String, TableKey>,
    /// 可见的 CTE / 派生表（名字, 投影列），内层在前；列名由子查询推导，不走宿主元数据。
    virtuals: Vec<(String, Vec<String>)>,
}

impl Symbols {
    /// 解析 `qualifier`（可能是 `schema.table` 形式）命中的关系。
    pub(crate) fn resolve_qualifier(&self, qualifier: &str) -> Vec<TableKey> {
        self.bindings
            .get(&last_segment(qualifier))
            .cloned()
            .into_iter()
            .collect()
    }

    /// 限定符命中的派生列来源（CTE / 派生表遮蔽同名物理表）。
    pub(crate) fn qualifier_virtual_columns(&self, qualifier: &str) -> Option<Vec<String>> {
        let last = last_segment(qualifier);
        self.virtuals
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&last))
            .map(|(_, columns)| columns.clone())
    }

    /// 可见的 CTE / 派生表（名字, 投影列），内层在前。
    pub(crate) fn virtual_bindings(&self) -> &[(String, Vec<String>)] {
        &self.virtuals
    }

    /// 可见的全部关系（用于 `SELECT <cursor>` 这类无限定符场景）。
    pub(crate) fn relation_keys(&self) -> Vec<TableKey> {
        self.relations
            .iter()
            .map(|relation| relation.key.clone())
            .collect()
    }

    /// `name` 是否是 CTE / 派生表别名：别名不是物理表，诊断不做存在性检查。
    pub(crate) fn is_virtual(&self, name: &str) -> bool {
        let last = last_segment(name);
        self.virtuals
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&last))
    }
}

/// 限定符只看最后一段：`public.users` 与 `users` 指向同一绑定。
fn last_segment(qualifier: &str) -> String {
    qualifier
        .rsplit('.')
        .next()
        .unwrap_or(qualifier)
        .to_ascii_lowercase()
}

/// 一个作用域内的原始收集结果。
#[derive(Debug, Clone, Default)]
struct Scope {
    relations: Vec<RelationSymbol>,
    /// CTE / 派生表绑定（限定符名小写 → 投影列），按注册顺序保存。
    virtuals: Vec<(String, Vec<String>)>,
    output_aliases: Vec<String>,
}

/// 语句的作用域树。索引 0 为根：只作兜底锚点，自身不含任何绑定，也不会被 span 命中。
#[derive(Debug, Clone, Default)]
pub(crate) struct ScopeTree {
    scopes: Vec<Scope>,
    parents: Vec<Option<usize>>,
    /// 各作用域的字节区间（end 开区间）；根为 `None`。
    ranges: Vec<Option<(usize, usize)>>,
}

impl ScopeTree {
    /// 投影出 `offset` 处可见的符号。
    ///
    /// 解析被截断（光标落在所有 span 之后）时退化成「整条语句平铺」，即最外层行为：
    /// 宁可不遮蔽，也不能因为 span 不全而少给候选。
    pub(crate) fn visible_at(&self, offset: usize) -> Symbols {
        let chain = self.chain_at(offset); // 内 → 外
        let mut symbols = Symbols::default();

        // relations 按「外 → 内」排，作用域内部保持原序（JOIN 链靠末位识别本次 JOIN 的表）。
        for &id in chain.iter().rev() {
            symbols
                .relations
                .extend(self.scopes[id].relations.iter().cloned());
        }

        // 绑定按「内 → 外」填：先占名者赢，实现内层遮蔽外层。同一作用域内 CTE / 派生表
        // 先占名，因此遮蔽同名物理表。
        let mut claimed: HashSet<String> = HashSet::new();
        for &id in &chain {
            let scope = &self.scopes[id];
            for (name, columns) in &scope.virtuals {
                if claimed.insert(name.clone()) {
                    symbols.virtuals.push((name.clone(), columns.clone()));
                }
            }
            for relation in &scope.relations {
                for name in relation_names(relation) {
                    if claimed.insert(name.clone()) {
                        symbols.bindings.insert(name, relation.key.clone());
                    }
                }
            }
        }

        for &id in &chain {
            for alias in &self.scopes[id].output_aliases {
                if !symbols
                    .output_aliases
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(alias))
                {
                    symbols.output_aliases.push(alias.clone());
                }
            }
        }
        symbols
    }

    /// `offset` 命中的最内层作用域；`None` = 落在所有 span 之外（解析被截断）。
    pub(crate) fn containing_scope_at(&self, offset: usize) -> Option<usize> {
        // 作用域按先序入栈：祖先总在后代之前，因此「最后一个命中」即最内层。
        self.ranges
            .iter()
            .enumerate()
            .filter_map(|(id, range)| {
                range
                    .filter(|(start, end)| *start <= offset && offset <= *end)
                    .map(|_| id)
            })
            .next_back()
    }

    /// 光标所在作用域链，内 → 外。
    fn chain_at(&self, offset: usize) -> Vec<usize> {
        if self.scopes.is_empty() {
            return Vec::new();
        }
        let Some(innermost) = self.containing_scope_at(offset) else {
            return (0..self.scopes.len()).rev().collect();
        };
        let mut chain = Vec::new();
        let mut current = Some(innermost);
        while let Some(id) = current {
            chain.push(id);
            current = self.parents[id];
        }
        chain
    }
}

/// 从语句列表收集作用域树。`index` 必须与产出 span 的文本对应。
pub(crate) fn collect(statements: &[Statement], index: &LineColIndex) -> ScopeTree {
    let mut collector = SymbolCollector {
        tree: ScopeTree::default(),
        index,
        stack: Vec::new(),
    };
    // 根作用域：兜底锚点。语句级 DML（INSERT/UPDATE/DELETE 无 Query）的关系挂在这里。
    collector.push_scope(None);
    for statement in statements {
        let _ = statement.visit(&mut collector);
    }
    collector.tree
}

struct SymbolCollector<'a> {
    tree: ScopeTree,
    index: &'a LineColIndex,
    /// 当前作用域栈，末位为最内层。
    stack: Vec<usize>,
}

impl SymbolCollector<'_> {
    fn push_scope(&mut self, range: Option<(usize, usize)>) -> usize {
        let id = self.tree.scopes.len();
        self.tree.scopes.push(Scope::default());
        self.tree.parents.push(self.stack.last().copied());
        self.tree.ranges.push(range);
        self.stack.push(id);
        id
    }

    fn scope(&mut self) -> &mut Scope {
        let id = *self.stack.last().expect("根作用域始终存在");
        &mut self.tree.scopes[id]
    }

    /// sqlparser span（1 基 line/col，end 闭区间）→ 字节区间（end 开区间）。
    fn range(&self, span: Span) -> (usize, usize) {
        let start = self.index.offset(span.start);
        let end = self.index.offset(span.end).saturating_add(1);
        (start, end.max(start))
    }
}

impl Visitor for SymbolCollector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let range = self.range(query.span());
        self.push_scope(Some(range));
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let name = cte.alias.name.value.to_ascii_lowercase();
                let columns = if cte.alias.columns.is_empty() {
                    projection_columns(&cte.query)
                } else {
                    cte.alias
                        .columns
                        .iter()
                        .map(|column| column.name.value.clone())
                        .collect()
                };
                let scope = self.scope();
                if !scope.virtuals.iter().any(|(existing, _)| *existing == name) {
                    scope.virtuals.push((name, columns));
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.stack.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        let aliases: Vec<String> = select
            .projection
            .iter()
            .filter_map(|item| match item {
                SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
                _ => None,
            })
            .collect();
        let scope = self.scope();
        for alias in aliases {
            if !scope
                .output_aliases
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&alias))
            {
                scope.output_aliases.push(alias);
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, table_factor: &TableFactor) -> ControlFlow<Self::Break> {
        match table_factor {
            TableFactor::Derived {
                subquery,
                alias: Some(alias),
                ..
            } => {
                let name = alias.name.value.to_ascii_lowercase();
                let columns = projection_columns(subquery);
                let scope = self.scope();
                if !scope.virtuals.iter().any(|(existing, _)| *existing == name) {
                    scope.virtuals.push((name, columns));
                }
            }
            TableFactor::Table { name, alias, .. } => {
                let parts = object_name_parts(name);
                let Some(table) = parts.last().cloned() else {
                    return ControlFlow::Continue(());
                };
                let schema = parts
                    .len()
                    .checked_sub(2)
                    .and_then(|index| parts.get(index))
                    .cloned();
                let relation = RelationSymbol {
                    key: TableKey {
                        database: None,
                        schema,
                        table,
                    },
                    alias: alias.as_ref().map(|alias| alias.name.value.clone()),
                };
                let scope = self.scope();
                if !scope.relations.contains(&relation) {
                    scope.relations.push(relation);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// 关系对外的可用限定符名：表名与别名。
///
/// 严格 SQL 里别名会屏蔽表名，这里两者都认——多给一次机会，代价只是
/// `FROM users u` 之后写 `users.` 也能补全。
fn relation_names(relation: &RelationSymbol) -> Vec<String> {
    let mut names = vec![relation.key.table.to_ascii_lowercase()];
    if let Some(alias) = &relation.alias {
        let alias = alias.to_ascii_lowercase();
        if !names.contains(&alias) {
            names.push(alias);
        }
    }
    names
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

    /// 用 `|` 标记光标，返回光标处可见的符号。
    fn at(text: &str) -> Symbols {
        let cursor = text.find('|').expect("用例必须用 | 标记光标");
        let sql = text.replace('|', "");
        let parsed = parse_tolerant(&sql, None, cursor, &GenericDialect {})
            .unwrap_or_else(|| panic!("测试语句应可解析：{sql}"));
        collect(&parsed.statements, &LineColIndex::new(&sql)).visible_at(cursor)
    }

    fn tree(sql: &str) -> ScopeTree {
        let parsed =
            parse_tolerant(sql, None, sql.len(), &GenericDialect {}).expect("测试语句应可解析");
        collect(&parsed.statements, &LineColIndex::new(sql))
    }

    fn tables(symbols: &Symbols) -> Vec<String> {
        symbols
            .relation_keys()
            .into_iter()
            .map(|key| key.table)
            .collect()
    }

    #[test]
    fn collects_relations_with_alias() {
        let symbols = at("|SELECT * FROM users u JOIN orders o ON u.id = o.user_id");
        assert_eq!(symbols.relations.len(), 2);
        assert_eq!(symbols.relations[0].alias.as_deref(), Some("u"));
        assert_eq!(symbols.relations[1].key.table, "orders");
    }

    #[test]
    fn resolves_alias_and_table_name() {
        let symbols = at("|SELECT * FROM public.users u");
        assert_eq!(symbols.resolve_qualifier("u").len(), 1);
        assert_eq!(symbols.resolve_qualifier("users").len(), 1);
        assert_eq!(symbols.resolve_qualifier("public.users").len(), 1);
        assert!(symbols.resolve_qualifier("nope").is_empty());
    }

    #[test]
    fn keeps_schema_from_qualified_name() {
        let symbols = at("|SELECT * FROM public.users");
        assert_eq!(symbols.relations[0].key.schema.as_deref(), Some("public"));
        assert_eq!(symbols.relations[0].key.table, "users");
    }

    #[test]
    fn collects_cte_names() {
        let symbols = at("|WITH active AS (SELECT id FROM users) SELECT * FROM active");
        assert!(symbols.is_virtual("active"));
    }

    /// 内层同名别名遮蔽外层：`u` 在内层指向 orders，外层 users 仍在可见关系里。
    #[test]
    fn inner_alias_shadows_outer_relation() {
        let symbols =
            at("SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders u WHERE |u.id > 0)");
        assert_eq!(symbols.resolve_qualifier("u")[0].table, "orders");
        assert_eq!(tables(&symbols), vec!["users", "orders"]);
    }

    /// 派生表别名遮蔽同名物理表：`u` 变成投影列而不是 orders。
    #[test]
    fn derived_alias_shadows_outer_relation() {
        let symbols = at("SELECT * FROM users u WHERE u.id IN \
             (SELECT x.id FROM (SELECT id FROM orders) u WHERE |u.id > 0)");
        assert_eq!(
            symbols.qualifier_virtual_columns("u"),
            Some(vec!["id".to_string()])
        );
        assert!(symbols.resolve_qualifier("u").is_empty());
    }

    /// 相关子查询仍看得到外层关系（未限定列可用）。
    #[test]
    fn subquery_sees_outer_relation() {
        let symbols = at(
            "SELECT * FROM users u WHERE u.id IN (SELECT user_id FROM orders WHERE |user_id > 0)",
        );
        assert_eq!(tables(&symbols), vec!["users", "orders"]);
    }

    /// 兄弟子查询互不可见：光标在第一个子查询里，第二个子查询的 t3 不该出现。
    #[test]
    fn sibling_subquery_is_not_visible() {
        let symbols = at(
            "SELECT * FROM t1 a WHERE a.x IN (SELECT b.y FROM t2 b WHERE |b.y > 0) \
             AND a.z IN (SELECT c.w FROM t3 c)",
        );
        assert_eq!(tables(&symbols), vec!["t1", "t2"]);
    }

    /// 光标落在所有 span 之外（解析被截断）→ 退化成整条语句平铺，不做遮蔽。
    #[test]
    fn offset_outside_every_scope_falls_back_to_flat() {
        let tree = tree("SELECT * FROM users u WHERE u.id IN (SELECT user_id FROM orders)");
        assert_eq!(tables(&tree.visible_at(0)), vec!["users"]);
        assert_eq!(
            tables(&tree.visible_at(usize::MAX)),
            vec!["users", "orders"]
        );
    }
}
