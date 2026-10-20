#[derive(Clone, Debug, Eq, PartialEq)]
struct ReferencedTable {
    database: Option<String>,
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SqlDdlImpact {
    tables: BTreeSet<String>,
    database_wide: bool,
}

/// 当前光标作用域的轻量 symbol table。
///
/// 这不是完整 SQL AST；它只保存补全真正消费的三类符号，避免 CTE、别名和
/// 表引用在同一次按键中被分别扫描。外层 CTE 会在嵌套子查询中继承，局部
/// CTE 同名时覆盖外层定义。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SqlScope {
    referenced_tables: Vec<ReferencedTable>,
    select_aliases: Vec<String>,
    cte_columns: BTreeMap<String, Vec<String>>,
    /// 派生表别名 → 其子查询 SELECT 输出列名（`(subquery) t` → t 的可见列）。
    /// 与 CTE 列同构：限定到该别名时，补全列来自此处而非底层表 metadata。
    derived_columns: BTreeMap<String, Vec<String>>,
}

/// 跨 crate 复用的 SQL 作用域符号快照；不携带 connector 或 UI 状态。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlScopeTable {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlScopeSymbols {
    pub referenced_tables: Vec<SqlScopeTable>,
    pub select_aliases: Vec<String>,
    pub cte_columns: BTreeMap<String, Vec<String>>,
    /// 派生表别名 → 输出列名（T082）。与 cte_columns 同构。
    pub derived_columns: BTreeMap<String, Vec<String>>,
    /// AST successfully parsed the statement; callers may disable heuristic checks.
    pub ast_parsed: bool,
    pub function_names: Vec<String>,
    pub cast_types: Vec<String>,
    pub qualified_columns: Vec<(String, String)>,
    pub unqualified_columns: Vec<String>,
    pub relation_names: Vec<String>,
}

/// 提取单个 SQL statement 的轻量作用域符号，供补全和语义诊断共用。
pub fn sql_scope_symbols(sql: &str, dialect: DatabaseKind) -> SqlScopeSymbols {
    let provider = sql_completion_dialect(dialect);
    let scope = build_sql_scope(provider, sql, 0, sql);
    let ast_symbols = ast_semantic_symbols(sql, dialect);
    let (ast_parsed, function_names, cast_types, qualified_columns, unqualified_columns, relation_names) =
        ast_symbols
            .map(|symbols| {
                (
                    true,
                    symbols.function_names,
                    symbols.cast_types,
                    symbols.qualified_columns,
                    symbols.unqualified_columns,
                    symbols.relation_names,
                )
            })
            .unwrap_or_default();
    SqlScopeSymbols {
        referenced_tables: scope
            .referenced_tables
            .into_iter()
            .map(|table| SqlScopeTable {
                database: table.database,
                schema: table.schema,
                name: table.name,
                alias: table.alias,
            })
            .collect(),
        select_aliases: scope.select_aliases,
        cte_columns: scope.cte_columns,
        derived_columns: scope.derived_columns,
        ast_parsed,
        function_names,
        cast_types,
        qualified_columns,
        unqualified_columns,
        relation_names,
    }
}

#[derive(Default)]
struct AstSemanticSymbols {
    function_names: Vec<String>,
    cast_types: Vec<String>,
    qualified_columns: Vec<(String, String)>,
    unqualified_columns: Vec<String>,
    relation_names: Vec<String>,
}

fn ast_semantic_symbols(sql: &str, dialect: DatabaseKind) -> Option<AstSemanticSymbols> {
    let statements = match dialect {
        DatabaseKind::MySql | DatabaseKind::TiDb => Parser::parse_sql(&MySqlDialect {}, sql).ok()?,
        _ => Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, sql).ok()?,
    };
    let mut symbols = AstSemanticSymbols::default();
    let _ = visit_expressions(&statements, |expr| {
        match expr {
            Expr::Function(function) => {
                if let Some(name) = object_name_text(&function.name) {
                    symbols.function_names.push(name.to_ascii_lowercase());
                }
            }
            Expr::Cast { data_type, .. } => {
                symbols.cast_types.push(data_type.to_string().to_ascii_lowercase());
            }
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                if let (Some(qualifier), Some(column)) = (parts.first(), parts.last()) {
                    symbols
                        .qualified_columns
                        .push((qualifier.value.to_ascii_lowercase(), column.value.to_ascii_lowercase()));
                }
            }
            Expr::Identifier(identifier) => {
                symbols
                    .unqualified_columns
                    .push(identifier.value.to_ascii_lowercase());
            }
            _ => {}
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    let _ = visit_relations(&statements, |relation| {
        if let Some(name) = object_name_text(relation) {
            symbols.relation_names.push(name.to_ascii_lowercase());
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
    symbols.function_names.sort_unstable();
    symbols.function_names.dedup();
    symbols.cast_types.sort_unstable();
    symbols.cast_types.dedup();
    symbols.qualified_columns.sort_unstable();
    symbols.qualified_columns.dedup();
    symbols.unqualified_columns.sort_unstable();
    symbols.unqualified_columns.dedup();
    symbols.relation_names.sort_unstable();
    symbols.relation_names.dedup();
    Some(symbols)
}

fn object_name_text(name: &ObjectName) -> Option<String> {
    name.0.iter().rev().find_map(|part| match part {
        ObjectNamePart::Identifier(identifier) => Some(identifier.value.clone()),
        ObjectNamePart::Function(_) => None,
    })
}

fn build_sql_scope(
    dialect: &dyn SqlCompletionDialect,
    context_sql: &str,
    scope_start: usize,
    scope: &str,
) -> SqlScope {
    let mut cte_columns = BTreeMap::new();

    // 嵌套查询只传入自身文本时会丢掉外层 WITH。外层定义先放入 map，
    // 再放入当前 scope 的定义，天然实现 SQL 的 shadowing 规则。
    if scope_start > 0 {
        if let Some(with_start) = find_top_level_sql_keyword(&context_sql[..scope_start], "with") {
            cte_columns.extend(extract_cte_columns_fallback(&context_sql[with_start..]));
        }
    }
    cte_columns.extend(extract_cte_columns_fallback(scope));

    let mut referenced_tables = dialect.referenced_tables(scope);
    // sqlparser 的 FROM 收集不会进入 EXISTS/IN 等表达式子查询；只补入
    // fallback 中明确指向已知 CTE 的别名，避免把普通启发式表扫描带回热路径。
    for table in extract_referenced_tables(scope, dialect.kind()) {
        if table.alias.is_some()
            && cte_columns.contains_key(&table.name.to_ascii_lowercase())
            && !referenced_tables.iter().any(|known| {
                known.name.eq_ignore_ascii_case(&table.name)
                    && known.alias.as_deref() == table.alias.as_deref()
            })
        {
            referenced_tables.push(table);
        }
    }

    // T082：解析当前 scope 内的派生表输出列（`(subquery) t`）。派生列是局部符号，
    // 只从当前 scope 提取，不继承外层（外层派生表在本行后已失效，避免污染）。
    let derived_columns = extract_derived_columns(scope, dialect);

    SqlScope {
        referenced_tables,
        select_aliases: extract_select_aliases(scope),
        cte_columns,
        derived_columns,
    }
}

/// 提取 scope 中最后一个顶层 SELECT 列表里的显式别名（`expr AS alias` 或 MySQL 尾随裸别名）。
/// 仅供 ORDER BY / GROUP BY 上下文提升别名候选（P2.11），失败时返回空列表，保持保守。
fn extract_select_aliases(scope: &str) -> Vec<String> {
    let Some(select_start) = last_select_keyword(scope) else {
        return Vec::new();
    };
    let select_list = &scope[select_start + "select".len()..];
    // 选择列表在上层 (FROM/WHERE/GROUP/ORDER/HAVING/LIMIT/JOIN/UNION) 关键字前结束。
    let end = ["from", "where", "group", "order", "having", "limit", "join", "union"]
        .iter()
        .filter_map(|keyword| find_top_level_sql_keyword(select_list, keyword))
        .min()
        .unwrap_or(select_list.len());
    let list = &select_list[..end];

    let mut aliases = Vec::new();
    let mut seen = BTreeSet::new();
    for item in split_select_items(list) {
        if let Some(alias) = select_item_alias(item) {
            let key = alias.to_ascii_lowercase();
            if seen.insert(key) {
                aliases.push(alias);
            }
        }
    }
    aliases
}

/// scope 中最后一个顶层 `select` 的位置；`WITH` 场景取最外层主查询的 SELECT。
fn last_select_keyword(scope: &str) -> Option<usize> {
    let mut last = None;
    let mut search_from = 0usize;
    while let Some(relative) = find_top_level_sql_keyword(&scope[search_from..], "select") {
        let index = search_from + relative;
        last = Some(index);
        search_from = index + "select".len();
    }
    last
}

/// 在顶层（括号、字符串、注释之外）按英文逗号切分 SELECT 列表项。
fn split_select_items(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut chars = list.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\'' | '"' | '`' => skip_quoted_sql(&mut chars, ch),
            '-' if matches!(chars.peek(), Some((_, '-'))) => skip_line_comment(&mut chars),
            '#' => skip_line_comment(&mut chars),
            '/' if matches!(chars.peek(), Some((_, '*'))) => skip_block_comment(&mut chars),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&list[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

/// 单个 SELECT 项的别名：`AS alias` 优先；否则若表达式不是单纯列引用则取 MySQL 尾随裸标识符。
fn select_item_alias(item: &str) -> Option<String> {
    if let Some(relative) = find_top_level_sql_keyword(item, "as") {
        let after = item[relative + "as".len()..].trim();
        return trailing_single_identifier(after);
    }
    let tokens: Vec<&str> = item.split_whitespace().collect();
    // 单纯一个标识符（如 `name`、`o.customer_id`）视为普通列引用而非别名。
    if tokens.len() < 2 {
        return None;
    }
    let last = tokens.last()?;
    last.chars()
        .all(is_sql_ident_char)
        .then(|| last.to_string())
}

fn trailing_single_identifier(s: &str) -> Option<String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() != 1 {
        return None;
    }
    normalize_sql_identifier(tokens[0])
}

fn normalize_sql_identifier(token: &str) -> Option<String> {
    let token = token.trim();
    let bytes = token.as_bytes();
    if bytes.len() >= 2
        && matches!((bytes[0], bytes[bytes.len() - 1]), (b'`', b'`') | (b'"', b'"'))
    {
        let quote = bytes[0] as char;
        return Some(
            token[1..token.len() - 1]
                .replace(&format!("{quote}{quote}"), &quote.to_string()),
        );
    }
    token.chars().all(is_sql_ident_char).then(|| token.to_string())
}

fn read_sql_identifier_at(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let first = *bytes.get(start)?;
    if matches!(first, b'`' | b'"') {
        let mut index = start + 1;
        while index < bytes.len() {
            if bytes[index] == first {
                if bytes.get(index + 1) == Some(&first) {
                    index += 2;
                    continue;
                }
                let raw = &text[start..=index];
                return normalize_sql_identifier(raw).map(|name| (name, index + 1));
            }
            index += 1;
        }
        return None;
    }
    if !(first.is_ascii_alphanumeric() || matches!(first, b'_' | b'$')) {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'$'))
    {
        end += 1;
    }
    Some((text[start..end].to_ascii_lowercase(), end))
}

trait SqlCompletionDialect: Sync {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable>;

    fn kind(&self) -> DatabaseKind;




}

struct MySqlCompletionDialect;
struct SqliteCompletionDialect;
struct PostgresCompletionDialect;
struct DamengCompletionDialect;
struct GenericCompletionDialect;

impl SqlCompletionDialect for MySqlCompletionDialect {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable> {
        referenced_tables_from_statements(sql, true, DatabaseKind::MySql)
    }

    fn kind(&self) -> DatabaseKind {
        DatabaseKind::MySql
    }


}

impl SqlCompletionDialect for SqliteCompletionDialect {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable> {
        referenced_tables_from_statements(sql, false, DatabaseKind::Sqlite)
    }

    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Sqlite
    }



}

/// PostgreSQL 方言：PG 亦具备过程（CALL/PROCEDURE）与触发器索引，
/// 与 MySQL 一致地参与过程/触发器上下文建议（connector 侧 pg_catalog 已产出两类索引）。
impl SqlCompletionDialect for PostgresCompletionDialect {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable> {
        referenced_tables_from_statements(sql, false, DatabaseKind::Postgres)
    }

    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Postgres
    }


}

/// 达梦与 PostgreSQL 一样以 `schema.table` 解析二段名；DM 登录用户同名 schema
/// 只是默认值，不能据此假定查询会话一定停留在该 schema。
impl SqlCompletionDialect for DamengCompletionDialect {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable> {
        referenced_tables_from_statements(sql, false, DatabaseKind::Dameng)
    }

    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Dameng
    }
}

impl SqlCompletionDialect for GenericCompletionDialect {
    fn referenced_tables(&self, sql: &str) -> Vec<ReferencedTable> {
        referenced_tables_from_statements(sql, false, DatabaseKind::MongoDb)
    }

    fn kind(&self) -> DatabaseKind {
        DatabaseKind::MongoDb
    }
}

/// T083：统一的 referenced table 提取——优先走尾部容忍的 AST 解析收集真实关系表；
/// 仅在解析失败（连尾部裁剪都无法产出语句）时才回退到纯 token scanner。
/// 避免 `where t.` 这类光标不完整输入让整句 AST 失败、从而回退回含 `select alias=id` 噪声的扫描器。
fn referenced_tables_from_statements(
    sql: &str,
    mysql: bool,
    kind: DatabaseKind,
) -> Vec<ReferencedTable> {
    if let Some(statements) = parse_statements_tolerant(sql, mysql) {
        let mut tables = Vec::new();
        for statement in &statements {
            collect_statement_tables(statement, &mut tables, kind);
        }
        if !tables.is_empty() {
            return dedupe_referenced_tables(tables);
        }
    }
    extract_referenced_tables(sql, kind)
}

static MYSQL_COMPLETION_DIALECT: MySqlCompletionDialect = MySqlCompletionDialect;
static SQLITE_COMPLETION_DIALECT: SqliteCompletionDialect = SqliteCompletionDialect;
static POSTGRES_COMPLETION_DIALECT: PostgresCompletionDialect = PostgresCompletionDialect;
static DAMENG_COMPLETION_DIALECT: DamengCompletionDialect = DamengCompletionDialect;
static GENERIC_COMPLETION_DIALECT: GenericCompletionDialect = GenericCompletionDialect;

fn sql_completion_dialect(dialect: DatabaseKind) -> &'static dyn SqlCompletionDialect {
    match dialect {
        DatabaseKind::MySql | DatabaseKind::TiDb => &MYSQL_COMPLETION_DIALECT,
        DatabaseKind::Sqlite => &SQLITE_COMPLETION_DIALECT,
        DatabaseKind::Postgres => &POSTGRES_COMPLETION_DIALECT,
        DatabaseKind::Dameng => &DAMENG_COMPLETION_DIALECT,
        DatabaseKind::MongoDb | DatabaseKind::Redis => &GENERIC_COMPLETION_DIALECT,
    }
}

fn collect_statement_tables(
    statement: &Statement,
    tables: &mut Vec<ReferencedTable>,
    kind: DatabaseKind,
) {
    let cte_sources = BTreeMap::new();
    match statement {
        Statement::Query(query) => collect_query_tables(query, tables, kind),
        Statement::Insert(insert) => {
            match &insert.table {
                TableObject::TableName(name) => {
                    if let Some(table) = referenced_table_from_object_name(name, None, kind) {
                        tables.push(table);
                    }
                }
                TableObject::TableQuery(query) => collect_query_tables(query, tables, kind),
                TableObject::TableFunction(_) => {}
            }
            if let Some(source) = &insert.source {
                collect_query_tables(source, tables, kind);
            }
        }
        Statement::Update(update) => {
            collect_table_with_joins(&update.table, tables, &cte_sources, kind);
            if let Some(from) = &update.from {
                match from {
                    UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from) => {
                        collect_table_with_joins_list(from, tables, &cte_sources, kind)
                    }
                }
            }
        }
        Statement::Delete(delete) => {
            match &delete.from {
                FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from) => {
                    collect_table_with_joins_list(from, tables, &cte_sources, kind)
                }
            }
            if let Some(using) = &delete.using {
                collect_table_with_joins_list(using, tables, &cte_sources, kind);
            }
        }
        Statement::Directory { source, .. } => collect_query_tables(source, tables, kind),
        _ => {}
    }
}

fn collect_query_tables(
    query: &SqlAstQuery,
    tables: &mut Vec<ReferencedTable>,
    kind: DatabaseKind,
) {
    collect_query_tables_with_ctes(query, tables, &BTreeMap::new(), kind);
}

fn extract_cte_columns_fallback(sql: &str) -> BTreeMap<String, Vec<String>> {
    let trimmed = sql.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("with ") && lower != "with" {
        return BTreeMap::new();
    }
    let bytes = trimmed.as_bytes();
    let mut position = 4usize;
    let mut result = BTreeMap::new();
    loop {
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let Some((name, name_end)) = read_sql_identifier_at(trimmed, position) else {
            break;
        };
        position = name_end;
        if name.eq_ignore_ascii_case("recursive") {
            continue;
        }
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let mut output = Vec::new();
        if bytes.get(position) == Some(&b'(') {
            if let Some(close) = find_matching_sql_paren(trimmed, position) {
                output = split_select_items(&trimmed[position + 1..close])
                    .into_iter()
                    .filter_map(|item| trailing_single_identifier(item.trim()))
                    .collect();
                position = close + 1;
            }
        }
        let Some(as_offset) = find_top_level_sql_keyword(&trimmed[position..], "as") else {
            break;
        };
        position += as_offset + 2;
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if bytes.get(position) != Some(&b'(') {
            break;
        }
        let Some(close) = find_matching_sql_paren(trimmed, position) else { break };
        if output.is_empty() {
            output = infer_cte_select_columns(&trimmed[position + 1..close]);
        }
        if !output.is_empty() {
            result.insert(name, output);
        }
        position = close + 1;
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if bytes.get(position) != Some(&b',') {
            break;
        }
        position += 1;
    }
    result
}

fn infer_cte_select_columns(sql: &str) -> Vec<String> {
    let Some(select_start) = last_select_keyword(sql) else { return Vec::new() };
    let projection = &sql[select_start + 6..];
    let end = ["from", "where", "group", "order", "having", "limit"]
        .iter()
        .filter_map(|keyword| find_top_level_sql_keyword(projection, keyword))
        .min()
        .unwrap_or(projection.len());
    split_select_items(&projection[..end])
        .into_iter()
        .filter_map(|item| {
            select_item_alias(item).or_else(|| {
                let token = item.trim().split('.').last()?.trim();
                token.chars().all(is_sql_ident_char).then(|| token.to_string())
            })
        })
        .collect()
}

fn find_matching_sql_paren(sql: &str, open: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    for index in open..bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn collect_query_tables_with_ctes(
    query: &SqlAstQuery,
    tables: &mut Vec<ReferencedTable>,
    parent_ctes: &BTreeMap<String, ReferencedTable>,
    kind: DatabaseKind,
) {
    let mut cte_sources = parent_ctes.clone();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_query_tables_with_ctes(&cte.query, tables, parent_ctes, kind);
            if let Some(source) = single_query_source_table(&cte.query, kind) {
                cte_sources.insert(cte.alias.name.value.to_ascii_lowercase(), source);
            }
        }
    }
    collect_set_expr_tables(&query.body, tables, &cte_sources, kind);
}

fn collect_set_expr_tables(
    expr: &SetExpr,
    tables: &mut Vec<ReferencedTable>,
    cte_sources: &BTreeMap<String, ReferencedTable>,
    kind: DatabaseKind,
) {
    match expr {
        SetExpr::Select(select) => {
            collect_table_with_joins_list(&select.from, tables, cte_sources, kind)
        }
        SetExpr::Query(query) => collect_query_tables_with_ctes(query, tables, cte_sources, kind),
        SetExpr::SetOperation { left, right, .. } => {
            collect_set_expr_tables(left, tables, cte_sources, kind);
            collect_set_expr_tables(right, tables, cte_sources, kind);
        }
        SetExpr::Insert(statement)
        | SetExpr::Update(statement)
        | SetExpr::Delete(statement)
        | SetExpr::Merge(statement) => collect_statement_tables(statement, tables, kind),
        SetExpr::Values(_) | SetExpr::Table(_) => {}
    }
}

fn collect_table_with_joins_list(
    tables_with_joins: &[TableWithJoins],
    tables: &mut Vec<ReferencedTable>,
    cte_sources: &BTreeMap<String, ReferencedTable>,
    kind: DatabaseKind,
) {
    for table_with_joins in tables_with_joins {
        collect_table_with_joins(table_with_joins, tables, cte_sources, kind);
    }
}

fn collect_table_with_joins(
    table_with_joins: &TableWithJoins,
    tables: &mut Vec<ReferencedTable>,
    cte_sources: &BTreeMap<String, ReferencedTable>,
    kind: DatabaseKind,
) {
    collect_table_factor(&table_with_joins.relation, tables, cte_sources, kind);
    for join in &table_with_joins.joins {
        collect_table_factor(&join.relation, tables, cte_sources, kind);
    }
}

fn collect_table_factor(
    table_factor: &TableFactor,
    tables: &mut Vec<ReferencedTable>,
    cte_sources: &BTreeMap<String, ReferencedTable>,
    kind: DatabaseKind,
) {
    match table_factor {
        TableFactor::Table { name, alias, .. } => {
            let cte_source = object_name_identifier_parts(name)
                .first()
                .filter(|_| name.0.len() == 1)
                .and_then(|name| cte_sources.get(&name.to_ascii_lowercase()));
            if let Some(table) = cte_source
                .cloned()
                .map(|source| referenced_table_with_alias(source, alias.as_ref(), name))
                .or_else(|| referenced_table_from_object_name(name, alias.as_ref(), kind))
            {
                tables.push(table);
            }
        }
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            collect_query_tables_with_ctes(subquery, tables, cte_sources, kind);
            if let Some(alias) = alias
                && let Some(source) = single_query_source_table(subquery, kind)
            {
                tables.push(referenced_table_with_alias(source, Some(alias), &ObjectName(vec![])));
            }
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => collect_table_with_joins(table_with_joins, tables, cte_sources, kind),
        TableFactor::Pivot { table, .. } | TableFactor::Unpivot { table, .. } => {
            collect_table_factor(table, tables, cte_sources, kind)
        }
        _ => {}
    }
}

fn referenced_table_from_object_name(
    name: &ObjectName,
    alias: Option<&TableAlias>,
    kind: DatabaseKind,
) -> Option<ReferencedTable> {
    let parts = object_name_identifier_parts(name);
    let name = parts.last()?.clone();
    let (database, schema) = completion_relation_namespace(&parts[..parts.len() - 1], kind);
    Some(ReferencedTable {
        database,
        schema,
        name,
        alias: alias.map(|alias| alias.name.value.clone()),
    })
}

fn referenced_table_with_alias(
    mut table: ReferencedTable,
    alias: Option<&TableAlias>,
    fallback_name: &ObjectName,
) -> ReferencedTable {
    table.alias = alias
        .map(|alias| alias.name.value.clone())
        .or_else(|| object_name_identifier_parts(fallback_name).first().cloned());
    table
}

fn single_query_source_table(
    query: &SqlAstQuery,
    kind: DatabaseKind,
) -> Option<ReferencedTable> {
    let mut tables = Vec::new();
    collect_query_tables(query, &mut tables, kind);
    let mut unique = BTreeMap::new();
    for mut table in tables {
        table.alias = None;
        unique.insert(
            (
                table.database.as_ref().map(|value| value.to_ascii_lowercase()),
                table.schema.as_ref().map(|value| value.to_ascii_lowercase()),
                table.name.to_ascii_lowercase(),
            ),
            table,
        );
    }
    (unique.len() == 1).then(|| unique.into_values().next().unwrap())
}

fn object_name_identifier_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .filter_map(|part| match part {
            ObjectNamePart::Identifier(identifier) => Some(identifier.value.clone()),
            ObjectNamePart::Function(_) => None,
        })
        .collect()
}

/// SQL 对象限定名按方言映射为物理数据库与 schema。
/// PostgreSQL/达梦的二段名是 `schema.table`，三段名才是 `database.schema.table`；
/// MySQL/SQLite 的二段名保持为 `database.table`。
fn completion_relation_namespace(
    qualifiers: &[String],
    kind: DatabaseKind,
) -> (Option<String>, Option<String>) {
    if qualifiers.is_empty() {
        return (None, None);
    }
    if matches!(kind, DatabaseKind::Postgres | DatabaseKind::Dameng) {
        let schema = qualifiers.last().cloned();
        let database = (qualifiers.len() > 1).then(|| qualifiers[..qualifiers.len() - 1].join("."));
        (database, schema)
    } else {
        (Some(qualifiers.join(".")), None)
    }
}

fn dedupe_referenced_tables(tables: Vec<ReferencedTable>) -> Vec<ReferencedTable> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for table in tables {
        let key = (
            table.database.as_ref().map(|value| value.to_ascii_lowercase()),
            table.schema.as_ref().map(|value| value.to_ascii_lowercase()),
            table.name.to_ascii_lowercase(),
            table.alias.as_ref().map(|value| value.to_ascii_lowercase()),
        );
        if seen.insert(key) {
            deduped.push(table);
        }
    }
    deduped
}

fn is_sql_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

/// 光标所处 SQL 函数调用的签名信息（T062）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlCallSignature {
    /// 函数名（schema 限定只取末段）。
    pub name: String,
    /// schema 限定前缀（如 `sales.concat` 的 `sales`）。
    pub qualifier: Option<String>,
    /// 当前正在编辑的参数下标。
    pub active_parameter: usize,
}

/// 推断光标所在 SQL 函数调用（T062）。
///
/// SQL 侧函数名识别独立于通用编辑器：fluxdb-app 不依赖 fluxdb-editor-core，通用编辑器的
/// `signature_at`/`active_parameter_index`（P1.9）用于非 SQL 文件。此处按 SQL 语义
/// 复刻同一算法——识别最内层包围光标的 `func(` 括号并给出 active 参数：
/// - 函数名必须紧邻开括号（`if (` 等控制流不判定为调用）；
/// - 支持嵌套调用（取最内层）与 schema 限定（`schema.func` 取末段名 + 前缀）；
/// - active 参数按括号内深度 0 的逗号计数，忽略字符串/引用字面量内的逗号；
/// - 逆向字符边界扫描与字符串扫描均按 char 边界推进，兼容多字节文本。
pub fn sql_signature_at(query: &str, cursor: usize) -> Option<SqlCallSignature> {
    // 编辑器 offset 始终落在 char 边界；此处保守地把非边界光标回退到前一边界。
    // 显式追加 query.len() 哨兵，避免字符串末尾光标（= len）被误落到最后一个字符起点。
    let cursor = query
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(query.len()))
        .filter(|&i| i <= cursor)
        .last()
        .unwrap_or(0);
    let before = &query[..cursor];

    // 逆向找包围光标的最近（未闭合）开括号。
    let mut depth = 0i32;
    let mut open = None;
    for (i, ch) in before.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    open = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let open = open?;
    let (name, qualifier) = read_call_identifier(before, open)?;
    let inside = &query[open + 1..cursor];
    let active_parameter = sql_active_parameter_index(inside);
    Some(SqlCallSignature {
        name,
        qualifier,
        active_parameter,
    })
}

/// 读开括号左侧紧邻的调用名（含可选 schema 限定）。括号前无紧邻标识符（如 `if (`）返回 None。
fn read_call_identifier(before: &str, open: usize) -> Option<(String, Option<String>)> {
    let bytes = before.as_bytes();
    let mut name_start = open;
    while name_start > 0 && is_sql_ident_char(bytes[name_start - 1] as char) {
        name_start -= 1;
    }
    if name_start == open {
        return None;
    }
    let name = before[name_start..open].to_string();
    let mut qualifier = None;
    if name_start > 0 && bytes[name_start - 1] == b'.' {
        let q_end = name_start - 1;
        let mut q_start = q_end;
        while q_start > 0 && is_sql_ident_char(bytes[q_start - 1] as char) {
            q_start -= 1;
        }
        if q_start < q_end {
            qualifier = Some(before[q_start..q_end].to_string());
        }
    }
    Some((name, qualifier))
}

/// 计算某函数调用括号内当前 active 参数下标：统计深度 0（非嵌套括号）的逗号，
/// 忽略字符串/引用字面量（含转义）内的逗号。空文本或括号内无参数返回 0。
fn sql_active_parameter_index(inside: &str) -> usize {
    let mut depth = 0usize;
    let mut commas = 0usize;
    let mut chars = inside.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '\'' | '"' | '`' => skip_quoted_sql(&mut chars, ch),
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas
}

fn extract_referenced_tables(sql: &str, kind: DatabaseKind) -> Vec<ReferencedTable> {
    let tokens = sql_identifier_tokens(sql);
    let mut tables = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].to_ascii_lowercase();
        let is_table_trigger = matches!(token.as_str(), "from" | "join" | "update" | "into")
            || (token == "delete"
                && tokens
                    .get(index + 1)
                    .is_some_and(|next| next.eq_ignore_ascii_case("from")));
        if !is_table_trigger {
            index += 1;
            continue;
        }
        if token == "delete" {
            index += 1;
        }
        let Some(raw_name) = tokens.get(index + 1).cloned() else {
            break;
        };
        let parts = raw_name.split('.').map(str::to_string).collect::<Vec<_>>();
        let Some(name) = parts.last().cloned() else {
            index += 2;
            continue;
        };
        let (database, schema) = completion_relation_namespace(&parts[..parts.len() - 1], kind);
        let alias = match (tokens.get(index + 2), tokens.get(index + 3)) {
            (Some(as_token), Some(alias)) if as_token.eq_ignore_ascii_case("as") => {
                Some(alias.clone())
            }
            (Some(alias), _)
                if !SQL_JOIN_STOP_WORDS
                    .iter()
                    .any(|word| alias.eq_ignore_ascii_case(word)) =>
            {
                Some(alias.clone())
            }
            _ => None,
        };
        tables.push(ReferencedTable {
            database,
            schema,
            name,
            alias,
        });
        index += 2;
    }
    tables
}

fn sql_identifier_tokens(sql: &str) -> Vec<String> {
    sql.split(|ch: char| !(is_sql_ident_char(ch) || ch == '.'))
        .filter(|token| !token.is_empty())
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// 提取派生表别名 → 子查询 SELECT 输出列名（T082）。
///
/// 例如 `SELECT ... FROM (SELECT id, name FROM Product) t` → `{"t": ["id", "name"]}`。
/// 输出列名取显式别名（`ExprWithAlias`），否则取裸标识符（`UnnamedExpr(Identifier)`）；
/// 复合表达式（函数、运算等）无法确定列名时丢弃该列，交由底层表 metadata 兜底，避免伪造列。
/// 递归进入子查询自身，使嵌套派生表（derived-of-derived）也被收集。
fn extract_derived_columns(
    scope: &str,
    _dialect: &dyn SqlCompletionDialect,
) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // 派生表语法 `(SELECT ...) alias` 在 GenericDialect 与 MySqlDialect 下一致，
    // 此处统一用 GenericDialect 解析即可，避免向 build_sql_scope 再传递 DatabaseKind。
    let Some(statements) = parse_statements_tolerant(scope, false) else {
        return out;
    };
    for statement in &statements {
        collect_statement_derived(statement, &mut out);
    }
    out
}

/// 容忍尾部不完整片段的语句解析（补全场景下光标处常有 `alias.` / 悬空关键字）。
///
/// 逐次剥掉尾部的 identifier / 点号 / 空白，直到 parser 能产出完整语句；
/// 只用于提取「已写完整部分」的符号（派生表来自前面的 FROM），剥掉的只是未完成的尾巴。
/// T083：同一份尾部恢复逻辑同时供派生表提取与 referenced_tables 使用，避免各自散落的字符串回退。
fn parse_statements_tolerant(sql: &str, mysql: bool) -> Option<Vec<Statement>> {
    let mut candidate = sql;
    loop {
        let parsed = if mysql {
            Parser::parse_sql(&MySqlDialect {}, candidate)
        } else {
            Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, candidate)
        };
        if let Ok(statements) = parsed {
            return Some(statements);
        }
        // 去掉末尾一段 identifier / `.` / 空白；若一段都没剥掉（如尾部是不整除的非标识符字符），
        // 则退一格字节，保证循环必定收敛，避免死循环。
        let mut end = candidate.trim_end().len();
        while end > 0 {
            let ch = candidate.as_bytes()[end - 1];
            if (ch as char).is_ascii_alphanumeric() || ch == b'_' || ch == b'$' || ch == b'.' {
                end -= 1;
            } else {
                break;
            }
        }
        if end == candidate.len() {
            end = candidate.trim_end().len().saturating_sub(1);
        }
        // end 可能落在多字节字符中间（UTF-8）→ 回退到最近的字符边界，避免切片 panic。
        // 真实 column comment / 数据可能含 em dash 等多字节字（如 `—`），必须容错。
        candidate = &candidate[..candidate.floor_char_boundary(end)];
        if candidate.is_empty() {
            return None;
        }
    }
}

fn collect_statement_derived(statement: &Statement, out: &mut BTreeMap<String, Vec<String>>) {
    // 顶层 statement 常为 Query；CTE/子查询内的派生表由 collect_query_derived 递归覆盖。
    if let Statement::Query(query) = statement {
        collect_query_derived(query, out);
    }
}

fn collect_query_derived(query: &SqlAstQuery, out: &mut BTreeMap<String, Vec<String>>) {
    collect_derived_set_expr(&query.body, out);
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_query_derived(&cte.query, out);
        }
    }
}

fn collect_derived_set_expr(expr: &SetExpr, out: &mut BTreeMap<String, Vec<String>>) {
    match expr {
        SetExpr::Select(select) => {
            for table_with_joins in &select.from {
                collect_table_factor_derived(&table_with_joins.relation, out);
                for join in &table_with_joins.joins {
                    collect_table_factor_derived(&join.relation, out);
                }
            }
        }
        SetExpr::Query(query) => collect_query_derived(query, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_derived_set_expr(left, out);
            collect_derived_set_expr(right, out);
        }
        _ => {}
    }
}

fn collect_table_factor_derived(table_factor: &TableFactor, out: &mut BTreeMap<String, Vec<String>>) {
    match table_factor {
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            if let Some(alias) = alias {
                let columns = derived_projection_columns(&subquery.body);
                if !columns.is_empty() {
                    out.insert(alias.name.value.to_ascii_lowercase(), columns);
                }
            }
            collect_query_derived(subquery, out);
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            collect_table_factor_derived(&table_with_joins.relation, out);
            for join in &table_with_joins.joins {
                collect_table_factor_derived(&join.relation, out);
            }
        }
        _ => {}
    }
}

/// 派生表子查询的投影列名列表（保持 SELECT 出现顺序）。
fn derived_projection_columns(body: &SetExpr) -> Vec<String> {
    let SetExpr::Select(select) = body else {
        return Vec::new();
    };
    select
        .projection
        .iter()
        .filter_map(|item| match item {
            sqlparser::ast::SelectItem::ExprWithAlias { alias, .. } => {
                Some(alias.value.clone())
            }
            sqlparser::ast::SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                Some(ident.value.clone())
            }
            _ => None,
        })
        .collect()
}

fn split_qualified_table_name(name: &str) -> (Option<String>, String) {
    name.rsplit_once('.')
        .map(|(database, table)| (Some(database.to_string()), table.to_string()))
        .unwrap_or_else(|| (None, name.to_string()))
}

const SQL_JOIN_STOP_WORDS: &[&str] = &[
    "where", "join", "left", "right", "inner", "outer", "full", "cross", "on", "group", "order",
    "limit", "having", "union", "set", "values",
];

/// T071：可关闭的轻量个性化（recency/frequency 加分）。
///
/// 记录用户近期采纳哪些补全候选（仅 label 的匿名统计），在确定性语义分数
/// 之后对其小幅加分。安全：不记录完整 SQL、密码、DSN 或敏感值，只记
/// `label -> (count, last_seen_ts)`；默认 `enabled == false`（关闭），关闭时
/// `score` 恒 0，排序与确定性基线完全一致。
#[derive(Debug)]
pub struct RecencyFrequency {
    enabled: bool,
    /// label -> (采纳次数, 最近采纳时间戳秒)
    counts: std::collections::HashMap<String, (u32, u64)>,
}

impl RecencyFrequency {
    pub fn new() -> Self {
        Self {
            enabled: false,
            counts: std::collections::HashMap::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 记录一次候选采纳（仅匿名 label）。返回关闭时（禁用状态下）不记录。
    pub fn record_accept(&mut self, label: &str, ts: u64) {
        if !self.enabled {
            return;
        }
        let entry = self
            .counts
            .entry(label.to_ascii_lowercase())
            .or_insert((0, ts));
        entry.0 += 1;
        entry.1 = ts;
    }

    /// 个性化分数：frequency 为主、recency 衰减系数为次。关闭或未记录时 0。
    /// 设计为有采纳历史的对象获得确定性优先级之上的小幅加分，不改变过滤。
    pub fn score(&self, label: &str) -> i32 {
        if !self.enabled {
            return 0;
        }
        match self.counts.get(&label.to_ascii_lowercase()) {
            Some((count, last)) => {
                // 轻量 recency 加分：60s 内最近采纳的对象额外 +1。
                let now = now_ts();
                let recency = if *last + 60 >= now { 1 } else { 0 };
                i32::try_from(*count).unwrap_or(i32::MAX) + recency
            }
            None => 0,
        }
    }
}

impl Default for RecencyFrequency {
    fn default() -> Self {
        Self::new()
    }
}

/// 秒级时间戳。独立小函数便于单测注入；实际实现取系统时间（Unix 秒）。
/// ponytail: 直接用 SystemTime，测试通过相对 ts 差值断言，不注入时钟。
fn now_ts() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 排序解释摘要（T024）：给出候选 top-N 的（type:label），用于 debug/perf 日志
/// 回答「为什么这个候选排在前」，不输出完整 SQL，避免泄露敏感内容；仅在 debug 层输出。
fn completion_top_candidates(items: &[QueryCompletionItem], limit: usize) -> String {
    let mut out = String::new();
    for (index, item) in items.iter().take(limit).enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{:?}:{}", item.kind, item.label));
    }
    out
}

/// 判断标识符是否需要引号包裹（P1.6 + §8.4 大小写）。
///
/// 规则：保留字、以数字开头、含非标识符字符时必须加引号；此外 PostgreSQL 把未加引号的
/// 标识符折叠为小写，含大写字母的名字不加引号会指向另一个对象，因此同样必须加引号。
/// MySQL/TiDB/SQLite 不做折叠，保持原判定。
///
/// `reserved` 由调用方提供该方言的保留字判定，避免在纯函数层引入关键字表耦合。
pub fn identifier_needs_quote(
    name: &str,
    kind: DatabaseKind,
    reserved: impl Fn(&str) -> bool,
) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return true;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return true;
    }
    if kind == DatabaseKind::Postgres && name.chars().any(|c| c.is_ascii_uppercase()) {
        return true;
    }
    reserved(name)
}

/// 按方言给标识符加引号并对内部引号做转义（P1.6）。
///
/// 规则：MySQL/TiDB 用反引号并转义反引号；其余（SQLite/PostgreSQL/SQL Server）用双引号并转义双引号。
/// 普通安全标识符（非保留字、无特殊字符）原样返回，避免候选过度带引号。
pub fn quote_identifier(
    name: &str,
    kind: DatabaseKind,
    reserved: impl Fn(&str) -> bool,
) -> String {
    if !identifier_needs_quote(name, kind, &reserved) {
        return name.to_string();
    }
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => fluxdb_core::MySqlDialect.quote_identifier(name),
        _ => fluxdb_core::PostgresDialect.quote_identifier(name),
    }
}

#[allow(dead_code)]
/// 列候选的文档提示（§8.4）：类型、可空性、主键与注释，逐行展示。
///
/// 只在有真实 metadata 时输出对应行，不用占位文本冒充已知信息。
pub fn column_completion_documentation(column: &CompletionColumn) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(type_name) = column
        .type_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("类型：{type_name}"));
    }
    lines.push(format!("可空：{}", if column.nullable { "是" } else { "否" }));
    if column.primary_key {
        lines.push("主键".to_string());
    }
    if let Some(comment) = column
        .comment
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("注释：{}", comment.trim()));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn sql_ddl_impact(sql: &str) -> Option<SqlDdlImpact> {
    let tokens = sql_identifier_tokens(sql);
    let mut impact = SqlDdlImpact::default();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].to_ascii_lowercase();
        match token.as_str() {
            "create" => {
                let Some(kind) = tokens.get(index + 1).map(|value| value.to_ascii_lowercase()) else {
                    impact.database_wide = true;
                    break;
                };
                match kind.as_str() {
                    "table" | "view" | "trigger" => {
                        if let Some(table) = ddl_object_name_after(&tokens, index + 2) {
                            impact.tables.insert(table);
                        } else {
                            impact.database_wide = true;
                        }
                    }
                    "index" | "unique" => {
                        if let Some(on_index) = tokens[index + 1..]
                            .iter()
                            .position(|value| value.eq_ignore_ascii_case("on"))
                            .map(|offset| index + 1 + offset)
                        {
                            if let Some(table) = ddl_object_name_after(&tokens, on_index + 1) {
                                impact.tables.insert(table);
                            } else {
                                impact.database_wide = true;
                            }
                        } else {
                            impact.database_wide = true;
                        }
                    }
                    "database" | "schema" => impact.database_wide = true,
                    _ => impact.database_wide = true,
                }
                index += 1;
            }
            "alter" | "drop" | "truncate" => {
                let next_index = index + 1;
                let next = tokens.get(next_index).map(|value| value.to_ascii_lowercase());
                if matches!(next.as_deref(), Some("database" | "schema")) {
                    impact.database_wide = true;
                } else {
                    let name_index = if matches!(next.as_deref(), Some("table" | "view" | "trigger")) {
                        next_index + 1
                    } else {
                        next_index
                    };
                    if let Some(table) = ddl_object_name_after(&tokens, name_index) {
                        impact.tables.insert(table);
                    } else {
                        impact.database_wide = true;
                    }
                }
                index += 1;
            }
            "rename" => {
                if tokens
                    .get(index + 1)
                    .is_some_and(|value| value.eq_ignore_ascii_case("table"))
                {
                    if let Some(old_table) = ddl_object_name_after(&tokens, index + 2) {
                        impact.tables.insert(old_table);
                    } else {
                        impact.database_wide = true;
                    }
                    if let Some(to_index) = tokens[index + 2..]
                        .iter()
                        .position(|value| value.eq_ignore_ascii_case("to"))
                        .map(|offset| index + 2 + offset)
                    {
                        if let Some(new_table) = ddl_object_name_after(&tokens, to_index + 1) {
                            impact.tables.insert(new_table);
                        }
                    }
                } else {
                    impact.database_wide = true;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    (impact.database_wide || !impact.tables.is_empty()).then_some(impact)
}

fn ddl_object_name_after(tokens: &[String], mut index: usize) -> Option<String> {
    while let Some(token) = tokens.get(index) {
        let lower = token.to_ascii_lowercase();
        if matches!(lower.as_str(), "if" | "not" | "exists" | "temporary") {
            index += 1;
            continue;
        }
        let (_, table) = split_qualified_table_name(token);
        return Some(table.to_ascii_lowercase());
    }
    None
}

