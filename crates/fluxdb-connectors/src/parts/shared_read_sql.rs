/// 按方言切分脚本。词法（`q'…'`、`#`、`\'`、`$$…$$`）与块边界交给 [`crate::script_syntax`]；
/// 词法器失败时退回 [`legacy_split_sql_statements`]，保持这些输入的历史行为。
fn split_sql_statements(sql: &str, kind: DatabaseKind) -> Vec<String> {
    crate::script_syntax::split(sql, kind).unwrap_or_else(|| legacy_split_sql_statements(sql))
}

/// 历史手写扫描器。仅在词法器失败时使用：Oracle 条件编译 `$IF $$x` 会被当成美元引号。
/// 它没有方言概念，`#` 一律按行注释、`\` 一律按转义——正是新切分器要修的两处。
fn legacy_split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut dollar_tag: Option<&str> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut chars = sql.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            continue;
        }
        if block_comment {
            if ch == '*'
                && let Some((_, '/')) = chars.peek().copied()
            {
                chars.next();
                block_comment = false;
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == '\\' {
                chars.next();
                continue;
            }
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if let Some(active_delim) = dollar_tag {
            // 处于 $tag$…$tag$ 美元引号体内：只认配对的结束标签，忽略内部任何 ; 与引号。
            if sql[index..].starts_with(active_delim) {
                // 当前字符是结束标签的第一个 `$`，跳过剩余 len-1 个字符（Peekable::nth(n) 会
                // 再消耗一个元素，故用 len-2 才能恰好停在标签之后，否则会吞掉标签后的 `;`，
                // 导致相邻的 DO/函数块黏成一条、prepare 时报多命令错）。
                chars.nth(active_delim.len() - 2);
                dollar_tag = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '-' if matches!(chars.peek(), Some((_, '-'))) => {
                chars.next();
                line_comment = true;
            }
            '#' => line_comment = true,
            '/' if matches!(chars.peek(), Some((_, '*'))) => {
                chars.next();
                block_comment = true;
            }
            // PostgreSQL 美元引用 $tag$…$tag$（含无标签 $$）：开启时忽略体内分号。
            '$' => {
                if let Some(len) = dollar_quote_len(&sql[index..]) {
                    // 与闭合逻辑同理：当前字符是起始标签的第一个 `$`，跳过剩余 len-1 个字符
                    // 需用 len-2（Peekable::nth(n) 多消耗一个元素），否则会吞掉标签后的首字符。
                    chars.nth(len - 2);
                    dollar_tag = Some(&sql[index..index + len]);
                }
            }
            ';' | '；' => {
                if create_trigger_statement_needs_more(&sql[start..index]) {
                    continue;
                }
                push_statement(sql, start, index, &mut statements);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    push_statement(sql, start, sql.len(), &mut statements);
    statements
}

/// 从 `$` 处解析美元引用开始标签 `$$` 或 `$tag$`（tag 为 [A-Za-z_][A-Za-z0-9_]*），
/// 成功返回完整结束定界符 `${tag}$` 的长度；`$` 不是合法标签起点或未闭合时返回 None。
fn dollar_quote_len(sql: &str) -> Option<usize> {
    let rest = &sql[1..];
    let tag_len = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    if rest.as_bytes().get(tag_len) == Some(&b'$') {
        Some(tag_len + 2)
    } else {
        None
    }
}

fn create_trigger_statement_needs_more(statement: &str) -> bool {
    let statement = statement.trim();
    let upper = statement.to_ascii_uppercase();
    let words = upper.split_whitespace().take(4).collect::<Vec<_>>();
    let is_create_trigger = matches!(
        words.as_slice(),
        ["CREATE", "TRIGGER", ..]
            | ["CREATE", "TEMP", "TRIGGER", ..]
            | ["CREATE", "TEMPORARY", "TRIGGER", ..]
    );
    is_create_trigger && upper.contains("BEGIN") && upper.split_whitespace().last() != Some("END")
}

fn query_statements_for_execution(request: &QueryRequest, kind: DatabaseKind) -> Vec<String> {
    if request.options.split_statements {
        return split_sql_statements(&request.text, kind);
    }

    let statement = request.text.trim();
    if statement.is_empty() {
        Vec::new()
    } else {
        vec![statement.to_string()]
    }
}

fn push_statement(sql: &str, start: usize, end: usize, statements: &mut Vec<String>) {
    let statement = sql[start..end].trim();
    if !statement.is_empty() {
        statements.push(statement.to_string());
    }
}

fn sqlite_trigger_word(sql: &str, words: &[&str]) -> String {
    let upper = sql.to_ascii_uppercase();
    words
        .iter()
        .find(|word| upper.contains(**word))
        .copied()
        .unwrap_or("UNKNOWN")
        .to_string()
}

fn data_order_by_clause(
    sort: &[SortSpec],
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
) -> String {
    let parts = sort
        .iter()
        .filter_map(|spec| {
            let column = columns.iter().find(|column| column.name == spec.field)?;
            let direction = match spec.direction {
                SortDirection::Asc => "ASC",
                SortDirection::Desc => "DESC",
            };
            Some(format!("{} {direction}", quote_identifier(&column.name)))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {}", parts.join(", "))
    }
}

fn data_export_preview_sql(
    table_name: &str,
    fields: &[String],
    sort: &[SortSpec],
    filters: &[FilterSpec],
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
) -> String {
    let select_list = data_export_preview_select_list(fields, columns, quote_identifier);
    let mut sql = format!("SELECT {select_list}\nFROM {table_name}");
    sql.push_str(&data_where_clause_preview(filters, columns, quote_identifier));
    sql.push_str(&data_order_by_clause(sort, columns, quote_identifier));
    sql
}

fn data_export_preview_select_list(
    fields: &[String],
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
) -> String {
    let selected = fields
        .iter()
        .filter_map(|field| columns.iter().find(|column| column.name == *field))
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        "*".to_string()
    } else {
        selected.join(", ")
    }
}

fn data_where_clause_preview(
    filters: &[FilterSpec],
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
) -> String {
    let clauses = filters
        .iter()
        .filter(|filter| filter.enabled && data_filter_clause_is_pushable(filter))
        .filter_map(|filter| {
            let column = columns.iter().find(|column| column.name == filter.field)?;
            data_filter_clause_preview(filter, column, quote_identifier)
        })
        .collect::<Vec<_>>();
    if clauses.is_empty() {
        String::new()
    } else {
        format!("\nWHERE {}", clauses.join("\n  AND "))
    }
}

fn data_filter_clause_preview(
    filter: &FilterSpec,
    column: &Column,
    quote_identifier: fn(&str) -> String,
) -> Option<String> {
    let column = quote_identifier(&column.name);
    Some(match filter.op {
        FilterOp::IsNull | FilterOp::NotExists => format!("{column} IS NULL"),
        FilterOp::IsNotNull | FilterOp::Exists => format!("{column} IS NOT NULL"),
        FilterOp::IsEmpty => format!("{column} = ''"),
        FilterOp::IsNotEmpty => format!("{column} != ''"),
        FilterOp::Between | FilterOp::NotBetween => {
            let (Some(start), Some(end)) = (filter.values.first(), filter.values.get(1)) else {
                return None;
            };
            let negative = if filter.op == FilterOp::NotBetween { " NOT" } else { "" };
            format!(
                "{column}{negative} BETWEEN {} AND {}",
                data_filter_literal(start),
                data_filter_literal(end)
            )
        }
        FilterOp::InList | FilterOp::NotInList => {
            if filter.values.is_empty() {
                return None;
            }
            let negative = if filter.op == FilterOp::NotInList { " NOT" } else { "" };
            let values = filter
                .values
                .iter()
                .map(data_filter_literal)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{column}{negative} IN ({values})")
        }
        FilterOp::Eq | FilterOp::NotEq => data_filter_multi_value_preview(
            &column,
            if filter.op == FilterOp::Eq { " = " } else { " != " },
            if filter.op == FilterOp::Eq { " OR " } else { " AND " },
            &filter.values,
        )?,
        FilterOp::Contains
        | FilterOp::NotContains
        | FilterOp::StartsWith
        | FilterOp::NotStartsWith
        | FilterOp::EndsWith
        | FilterOp::NotEndsWith => data_filter_like_preview(&column, filter.op, &filter.values)?,
        FilterOp::GreaterThan
        | FilterOp::GreaterThanOrEqual
        | FilterOp::LessThan
        | FilterOp::LessThanOrEqual => {
            let value = filter.values.first()?;
            let op = match filter.op {
                FilterOp::GreaterThan => " > ",
                FilterOp::GreaterThanOrEqual => " >= ",
                FilterOp::LessThan => " < ",
                FilterOp::LessThanOrEqual => " <= ",
                _ => unreachable!(),
            };
            format!("{column}{op}{}", data_filter_literal(value))
        }
    })
}

fn data_filter_multi_value_preview(
    column: &str,
    op: &str,
    joiner: &str,
    values: &[CellValue],
) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let parts = values
        .iter()
        .map(|value| format!("{column}{op}{}", data_filter_literal(value)))
        .collect::<Vec<_>>();
    if parts.len() > 1 {
        Some(format!("({})", parts.join(joiner)))
    } else {
        parts.into_iter().next()
    }
}

fn data_filter_like_preview(column: &str, op: FilterOp, values: &[CellValue]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let negative = matches!(
        op,
        FilterOp::NotContains | FilterOp::NotStartsWith | FilterOp::NotEndsWith
    );
    let joiner = if negative { " AND " } else { " OR " };
    let parts = values
        .iter()
        .map(|value| {
            let text = data_filter_value_text(value);
            let pattern = match op {
                FilterOp::Contains | FilterOp::NotContains => format!("%{text}%"),
                FilterOp::StartsWith | FilterOp::NotStartsWith => format!("{text}%"),
                FilterOp::EndsWith | FilterOp::NotEndsWith => format!("%{text}"),
                _ => unreachable!(),
            };
            let negative = if negative { " NOT" } else { "" };
            format!("{column}{negative} LIKE {}", data_filter_literal(&CellValue::Text(pattern)))
        })
        .collect::<Vec<_>>();
    if parts.len() > 1 {
        Some(format!("({})", parts.join(joiner)))
    } else {
        parts.into_iter().next()
    }
}

fn data_filter_literal(value: &CellValue) -> String {
    match value {
        CellValue::Null => "NULL".to_string(),
        CellValue::Bool(value) => {
            if *value {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        CellValue::I64(value) => value.to_string(),
        CellValue::F64(value) => value.to_string(),
        CellValue::Text(value) | CellValue::Json(value) => {
            format!("'{}'", value.replace('\'', "''"))
        }
        // 筛选只做前缀比较：预览是完整值的前缀，等值/前缀语义都成立，
        // 不会因为只有预览而漏掉真正匹配的行。
        CellValue::TruncatedText { preview, .. } => {
            format!("'{}'", preview.replace('\'', "''"))
        }
        CellValue::Bytes(value) => {
            let hex = value
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>();
            format!("X'{hex}'")
        }
        CellValue::BinarySummary(summary) => {
            if summary.is_null {
                "NULL".to_string()
            } else {
                "'<binary>'".to_string()
            }
        }
    }
}

fn push_data_where_clause<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    filters: &[FilterSpec],
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
    push_bind: fn(&mut QueryBuilder<'_, DB>, &CellValue),
) where
    DB: sqlx::Database,
{
    let mut pushed = false;
    for filter in filters.iter().filter(|filter| filter.enabled) {
        if !data_filter_clause_is_pushable(filter) {
            continue;
        }
        let Some(column) = columns.iter().find(|column| column.name == filter.field) else {
            continue;
        };
        if !pushed {
            builder.push(" WHERE ");
            pushed = true;
        } else {
            builder.push(" AND ");
        }

        push_data_filter_clause(builder, filter, column, quote_identifier, push_bind);
    }
}

/// 统一表达式树的连接器侧适配：方言层只产出占位符，驱动侧负责把值绑定成
/// sqlx 参数。这样 AND/OR/括号不再散落在各连接器。
pub(crate) fn push_expression_where_clause<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    tree: &fluxdb_core::CompiledFilterNode,
    push_value: fn(&mut QueryBuilder<'_, DB>, &fluxdb_core::FilterValue),
) where
    DB: sqlx::Database,
{
    // sqlx 的 push_bind 会立即追加参数；因此只能在确定需要 WHERE 后遍历一次。
    // 先用零副作用的结构检查判断是否为空，再渲染同一棵树一次。
    if !compiled_expression_is_empty(tree) {
        builder.push(" WHERE ");
        push_expression_node(builder, tree, push_value);
    }
}

fn compiled_expression_is_empty(tree: &fluxdb_core::CompiledFilterNode) -> bool {
    match tree {
        fluxdb_core::CompiledFilterNode::Condition { sql, .. } => sql.trim().is_empty(),
        fluxdb_core::CompiledFilterNode::And(items)
        | fluxdb_core::CompiledFilterNode::Or(items) => {
            items.iter().all(compiled_expression_is_empty)
        }
        fluxdb_core::CompiledFilterNode::Not(inner) => compiled_expression_is_empty(inner),
    }
}

fn push_expression_node<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    tree: &fluxdb_core::CompiledFilterNode,
    push_value: fn(&mut QueryBuilder<'_, DB>, &fluxdb_core::FilterValue),
) -> bool
where
    DB: sqlx::Database,
{
    match tree {
        fluxdb_core::CompiledFilterNode::Condition { sql, parameters } => {
            if sql.trim().is_empty() {
                return false;
            }
            let mut parts = sql.split('?');
            builder.push(parts.next().unwrap_or_default());
            for (parameter, suffix) in parameters.iter().zip(parts) {
                push_value(builder, parameter);
                builder.push(suffix);
            }
            true
        }
        fluxdb_core::CompiledFilterNode::And(items) => {
            render_expression_binary(builder, items, " AND ", push_value)
        }
        fluxdb_core::CompiledFilterNode::Or(items) => {
            render_expression_binary(builder, items, " OR ", push_value)
        }
        fluxdb_core::CompiledFilterNode::Not(inner) => {
            builder.push("NOT (");
            let rendered = push_expression_node(builder, inner, push_value);
            builder.push(")");
            rendered
        }
    }
}

fn render_expression_binary<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    items: &[fluxdb_core::CompiledFilterNode],
    separator: &str,
    push_value: fn(&mut QueryBuilder<'_, DB>, &fluxdb_core::FilterValue),
) -> bool
where
    DB: sqlx::Database,
{
    if items.is_empty() {
        return false;
    }
    let rendered_count = items
        .iter()
        .filter(|item| !compiled_expression_is_empty(item))
        .count();
    if rendered_count > 1 {
        builder.push("(");
    }
    let mut rendered = false;
    for item in items {
        if compiled_expression_is_empty(item) {
            continue;
        }
        if rendered {
            builder.push(separator);
        }
        if push_expression_node(builder, item, push_value) {
            rendered = true;
        }
    }
    if rendered_count > 1 {
        builder.push(")");
    }
    rendered
}

fn data_filter_clause_is_pushable(filter: &FilterSpec) -> bool {
    match filter.op {
        FilterOp::IsNull
        | FilterOp::IsNotNull
        | FilterOp::IsEmpty
        | FilterOp::IsNotEmpty
        | FilterOp::Exists
        | FilterOp::NotExists => true,
        FilterOp::Between | FilterOp::NotBetween => filter.values.len() >= 2,
        _ => !filter.values.is_empty(),
    }
}

#[cfg(test)]
mod compiled_filter_binding_tests {
    use super::*;

    fn push_value(
        builder: &mut QueryBuilder<'_, sqlx::MySql>,
        value: &fluxdb_core::FilterValue,
    ) {
        match value {
            fluxdb_core::FilterValue::Null => {
                builder.push_bind(Option::<String>::None);
            }
            fluxdb_core::FilterValue::Text(value) => {
                builder.push_bind(value.clone());
            }
            fluxdb_core::FilterValue::Integer(value) => {
                builder.push_bind(*value);
            }
            fluxdb_core::FilterValue::Float(value) => {
                builder.push_bind(*value);
            }
            fluxdb_core::FilterValue::Boolean(value) => {
                builder.push_bind(*value);
            }
        }
    }

    #[test]
    fn expression_is_rendered_and_bound_once_in_tree_order() {
        let tree = fluxdb_core::CompiledFilterNode::And(vec![
            fluxdb_core::CompiledFilterNode::Condition {
                sql: "`status` = ?".to_string(),
                parameters: vec![fluxdb_core::FilterValue::Text("failed".into())],
            },
            fluxdb_core::CompiledFilterNode::Condition {
                sql: "`score` > ?".to_string(),
                parameters: vec![fluxdb_core::FilterValue::Integer(10)],
            },
        ]);
        let mut builder = QueryBuilder::<sqlx::MySql>::new("SELECT * FROM `records`");
        push_expression_where_clause(&mut builder, &tree, push_value);
        assert_eq!(
            builder.sql(),
            "SELECT * FROM `records` WHERE (`status` = ? AND `score` > ?)"
        );
    }
}

fn push_data_filter_clause<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    filter: &FilterSpec,
    column: &Column,
    quote_identifier: fn(&str) -> String,
    push_bind: fn(&mut QueryBuilder<'_, DB>, &CellValue),
) -> bool
where
    DB: sqlx::Database,
{
    let column = quote_identifier(&column.name);
    match filter.op {
        FilterOp::IsNull | FilterOp::NotExists => {
            builder.push(column).push(" IS NULL");
            true
        }
        FilterOp::IsNotNull | FilterOp::Exists => {
            builder.push(column).push(" IS NOT NULL");
            true
        }
        FilterOp::IsEmpty => {
            builder.push(column).push(" = ");
            push_bind(builder, &CellValue::Text(String::new()));
            true
        }
        FilterOp::IsNotEmpty => {
            builder.push(column).push(" != ");
            push_bind(builder, &CellValue::Text(String::new()));
            true
        }
        FilterOp::Between | FilterOp::NotBetween => {
            let (Some(start), Some(end)) = (filter.values.first(), filter.values.get(1)) else {
                return false;
            };
            builder.push(column);
            if filter.op == FilterOp::NotBetween {
                builder.push(" NOT");
            }
            builder.push(" BETWEEN ");
            push_bind(builder, start);
            builder.push(" AND ");
            push_bind(builder, end);
            true
        }
        FilterOp::InList | FilterOp::NotInList => {
            if filter.values.is_empty() {
                return false;
            }
            builder.push(column);
            if filter.op == FilterOp::NotInList {
                builder.push(" NOT");
            }
            builder.push(" IN (");
            for (index, value) in filter.values.iter().enumerate() {
                if index > 0 {
                    builder.push(", ");
                }
                push_bind(builder, value);
            }
            builder.push(")");
            true
        }
        FilterOp::Eq | FilterOp::NotEq => push_data_multi_value_clause(
            builder,
            &column,
            if filter.op == FilterOp::Eq {
                " = "
            } else {
                " != "
            },
            if filter.op == FilterOp::Eq {
                " OR "
            } else {
                " AND "
            },
            &filter.values,
            push_bind,
        ),
        FilterOp::Contains
        | FilterOp::NotContains
        | FilterOp::StartsWith
        | FilterOp::NotStartsWith
        | FilterOp::EndsWith
        | FilterOp::NotEndsWith => {
            push_data_like_clause(builder, &column, filter.op, &filter.values, push_bind)
        }
        FilterOp::GreaterThan
        | FilterOp::GreaterThanOrEqual
        | FilterOp::LessThan
        | FilterOp::LessThanOrEqual => {
            let Some(value) = filter.values.first() else {
                return false;
            };
            let op = match filter.op {
                FilterOp::GreaterThan => " > ",
                FilterOp::GreaterThanOrEqual => " >= ",
                FilterOp::LessThan => " < ",
                FilterOp::LessThanOrEqual => " <= ",
                _ => unreachable!(),
            };
            builder.push(column).push(op);
            push_bind(builder, value);
            true
        }
    }
}

fn push_data_multi_value_clause<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    column: &str,
    op: &str,
    joiner: &str,
    values: &[CellValue],
    push_bind: fn(&mut QueryBuilder<'_, DB>, &CellValue),
) -> bool
where
    DB: sqlx::Database,
{
    if values.is_empty() {
        return false;
    }
    if values.len() > 1 {
        builder.push("(");
    }
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            builder.push(joiner);
        }
        builder.push(column).push(op);
        push_bind(builder, value);
    }
    if values.len() > 1 {
        builder.push(")");
    }
    true
}

fn push_data_like_clause<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    column: &str,
    op: FilterOp,
    values: &[CellValue],
    push_bind: fn(&mut QueryBuilder<'_, DB>, &CellValue),
) -> bool
where
    DB: sqlx::Database,
{
    if values.is_empty() {
        return false;
    }
    let negative = matches!(
        op,
        FilterOp::NotContains | FilterOp::NotStartsWith | FilterOp::NotEndsWith
    );
    if values.len() > 1 {
        builder.push("(");
    }
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            builder.push(if negative { " AND " } else { " OR " });
        }
        let text = data_filter_value_text(value);
        let pattern = match op {
            FilterOp::Contains | FilterOp::NotContains => format!("%{text}%"),
            FilterOp::StartsWith | FilterOp::NotStartsWith => format!("{text}%"),
            FilterOp::EndsWith | FilterOp::NotEndsWith => format!("%{text}"),
            _ => unreachable!(),
        };
        builder.push(column);
        if negative {
            builder.push(" NOT");
        }
        builder.push(" LIKE ");
        push_bind(builder, &CellValue::Text(pattern));
    }
    if values.len() > 1 {
        builder.push(")");
    }
    true
}

fn data_filter_value_text(value: &CellValue) -> String {
    match value {
        CellValue::Null => String::new(),
        CellValue::Bool(value) => value.to_string(),
        CellValue::I64(value) => value.to_string(),
        CellValue::F64(value) => value.to_string(),
        CellValue::Text(value) | CellValue::Json(value) => value.clone(),
        CellValue::TruncatedText { preview, .. } => preview.clone(),
        CellValue::Bytes(value) => value.iter().map(|byte| format!("{byte:02X}")).collect(),
        CellValue::BinarySummary(_) => value.display_label(),
    }
}
