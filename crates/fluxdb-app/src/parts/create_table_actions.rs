struct MySqlTableActionSqlProvider;
struct SqliteTableActionSqlProvider;
struct UnsupportedTableActionSqlProvider;

impl TableActionSqlProvider for MySqlTableActionSqlProvider {
    fn rename_table_sql(
        &self,
        _: Option<&str>,
        old_name: &str,
        new_name: &str,
    ) -> Result<String, String> {
        Ok(format!(
            "ALTER TABLE {} RENAME TO {};",
            fluxdb_core::MySqlDialect.quote_identifier(old_name),
            fluxdb_core::MySqlDialect.quote_identifier(new_name)
        ))
    }

    fn copy_table_sql(
        &self,
        _: Option<&str>,
        source_name: &str,
        target_name: &str,
        copy_data: bool,
        _: Option<&str>,
    ) -> Result<String, String> {
        let source = fluxdb_core::MySqlDialect.quote_identifier(source_name);
        let target = fluxdb_core::MySqlDialect.quote_identifier(target_name);
        let mut statements = vec![format!("CREATE TABLE {target} LIKE {source};")];
        if copy_data {
            statements.push(format!("INSERT INTO {target} SELECT * FROM {source};"));
        }
        Ok(statements.join("\n"))
    }

    fn drop_table_sql(
        &self,
        _: ObjectKind,
        _: Option<&str>,
        table_name: &str,
    ) -> Result<String, String> {
        Ok(format!("DROP TABLE {};", fluxdb_core::MySqlDialect.quote_identifier(table_name)))
    }

    fn truncate_table_sql(
        &self,
        _: Option<&str>,
        table_name: &str,
        _: bool,
    ) -> Result<String, String> {
        Ok(format!(
            "TRUNCATE TABLE {};",
            fluxdb_core::MySqlDialect.quote_identifier(table_name)
        ))
    }

    fn with_foreign_key_check(
        &self,
        sql: String,
        foreign_key_check: ForeignKeyCheckMode,
    ) -> Result<String, String> {
        match foreign_key_check {
            ForeignKeyCheckMode::Default => Ok(sql),
            ForeignKeyCheckMode::Enable => Ok(format!("SET FOREIGN_KEY_CHECKS = 1;\n{sql}")),
            ForeignKeyCheckMode::Disable => Ok(format!("SET FOREIGN_KEY_CHECKS = 0;\n{sql}")),
        }
    }
}
impl TableActionSqlProvider for SqliteTableActionSqlProvider {
    fn rename_table_sql(
        &self,
        _: Option<&str>,
        old_name: &str,
        new_name: &str,
    ) -> Result<String, String> {
        Ok(format!(
            "ALTER TABLE {} RENAME TO {};",
            fluxdb_core::SqliteDialect.quote_identifier(old_name),
            fluxdb_core::SqliteDialect.quote_identifier(new_name)
        ))
    }

    fn copy_table_sql(
        &self,
        _: Option<&str>,
        source_name: &str,
        target_name: &str,
        copy_data: bool,
        source_ddl: Option<&str>,
    ) -> Result<String, String> {
        let source = fluxdb_core::SqliteDialect.quote_identifier(source_name);
        let target = fluxdb_core::SqliteDialect.quote_identifier(target_name);
        let mut statements = vec![if let Some(source_ddl) = source_ddl {
            sqlite_copy_table_structure_sql(source_ddl, target_name)?
        } else {
            format!("CREATE TABLE {target} AS SELECT * FROM {source} WHERE 0;")
        }];
        if copy_data {
            statements.push(format!("INSERT INTO {target} SELECT * FROM {source};"));
        }
        Ok(statements.join("\n"))
    }

    fn drop_table_sql(
        &self,
        _: ObjectKind,
        _: Option<&str>,
        table_name: &str,
    ) -> Result<String, String> {
        Ok(format!("DROP TABLE {};", fluxdb_core::SqliteDialect.quote_identifier(table_name)))
    }

    fn truncate_table_sql(
        &self,
        _: Option<&str>,
        table_name: &str,
        _: bool,
    ) -> Result<String, String> {
        Ok(format!("DELETE FROM {};", fluxdb_core::SqliteDialect.quote_identifier(table_name)))
    }
}

impl TableActionSqlProvider for UnsupportedTableActionSqlProvider {
    fn rename_table_sql(&self, _: Option<&str>, _: &str, _: &str) -> Result<String, String> {
        Err("当前连接类型暂不支持重命名表".to_string())
    }

    fn copy_table_sql(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: bool,
        _: Option<&str>,
    ) -> Result<String, String> {
        Err("当前连接类型暂不支持复制表".to_string())
    }

    fn drop_table_sql(
        &self,
        _: ObjectKind,
        _: Option<&str>,
        _: &str,
    ) -> Result<String, String> {
        Err("当前连接类型暂不支持删除表".to_string())
    }

    fn truncate_table_sql(
        &self,
        _: Option<&str>,
        _: &str,
        _: bool,
    ) -> Result<String, String> {
        Err("当前连接类型暂不支持清空表".to_string())
    }
}

fn sqlite_copy_table_structure_sql(source_ddl: &str, target_name: &str) -> Result<String, String> {
    // ponytail: 只复制 CREATE TABLE DDL；需要完整复制二级索引/触发器时再改写 sqlite_schema 相关 DDL。
    let ddl = source_ddl.trim().trim_end_matches(';').trim();
    let lower = ddl.to_ascii_lowercase();
    let Some(mut index) = lower.find("create table") else {
        return Err("未读取到可复制的 SQLite 表结构 DDL".to_string());
    };
    index += "create table".len();
    index = skip_ascii_whitespace(ddl, index);
    for keyword in ["if", "not", "exists"] {
        if ascii_keyword_at(ddl, index, keyword) {
            index += keyword.len();
            index = skip_ascii_whitespace(ddl, index);
        }
    }
    let name_start = index;
    let name_end = sqlite_create_table_name_end(ddl, name_start)?;
    let mut sql = String::with_capacity(ddl.len() + target_name.len() + 4);
    sql.push_str(&ddl[..name_start]);
    sql.push_str(&fluxdb_core::SqliteDialect.quote_identifier(target_name));
    sql.push_str(ddl[name_end..].trim_end());
    sql.push(';');
    Ok(sql)
}

fn skip_ascii_whitespace(value: &str, mut index: usize) -> usize {
    while value
        .as_bytes()
        .get(index)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        index += 1;
    }
    index
}

fn ascii_keyword_at(value: &str, index: usize, keyword: &str) -> bool {
    value
        .get(index..index + keyword.len())
        .is_some_and(|part| part.eq_ignore_ascii_case(keyword))
        && value
            .as_bytes()
            .get(index + keyword.len())
            .is_none_or(|byte| byte.is_ascii_whitespace())
}

fn sqlite_create_table_name_end(value: &str, start: usize) -> Result<usize, String> {
    let bytes = value.as_bytes();
    let Some(first) = bytes.get(start).copied() else {
        return Err("未读取到 SQLite 表名".to_string());
    };
    match first {
        b'"' | b'\'' | b'`' => quoted_identifier_end(bytes, start, first),
        b'[' => bytes[start + 1..]
            .iter()
            .position(|byte| *byte == b']')
            .map(|offset| start + 1 + offset + 1)
            .ok_or_else(|| "SQLite 表名引用未闭合".to_string()),
        _ => {
            let end = bytes[start..]
                .iter()
                .position(|byte| byte.is_ascii_whitespace() || *byte == b'(')
                .map(|offset| start + offset)
                .unwrap_or(value.len());
            if end == start {
                Err("未读取到 SQLite 表名".to_string())
            } else {
                Ok(end)
            }
        }
    }
}

fn quoted_identifier_end(bytes: &[u8], start: usize, quote: u8) -> Result<usize, String> {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
            } else {
                return Ok(index + 1);
            }
        } else {
            index += 1;
        }
    }
    Err("SQLite 表名引用未闭合".to_string())
}

static MYSQL_TABLE_ACTION_SQL_PROVIDER: MySqlTableActionSqlProvider = MySqlTableActionSqlProvider;
static SQLITE_TABLE_ACTION_SQL_PROVIDER: SqliteTableActionSqlProvider =
    SqliteTableActionSqlProvider;
static POSTGRES_TABLE_ACTION_SQL_PROVIDER: PostgresTableActionSqlProvider =
    PostgresTableActionSqlProvider;
static UNSUPPORTED_TABLE_ACTION_SQL_PROVIDER: UnsupportedTableActionSqlProvider =
    UnsupportedTableActionSqlProvider;

fn table_action_sql_provider(database_kind: DatabaseKind) -> &'static dyn TableActionSqlProvider {
    match database_kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => &MYSQL_TABLE_ACTION_SQL_PROVIDER,
        DatabaseKind::Sqlite => &SQLITE_TABLE_ACTION_SQL_PROVIDER,
        DatabaseKind::Postgres => &POSTGRES_TABLE_ACTION_SQL_PROVIDER,
        DatabaseKind::MongoDb | DatabaseKind::Redis => &UNSUPPORTED_TABLE_ACTION_SQL_PROVIDER,
        DatabaseKind::Dameng => &DAMENG_TABLE_ACTION_SQL_PROVIDER,
    }
}

struct DamengTableActionSqlProvider;

impl TableActionSqlProvider for DamengTableActionSqlProvider {
    fn rename_table_sql(
        &self,
        schema: Option<&str>,
        old_name: &str,
        new_name: &str,
    ) -> Result<String, String> {
        if new_name.contains('.') {
            return Err("重命名只接受单段新表名（不带 schema）".to_string());
        }
        Ok(format!(
            "ALTER TABLE {} RENAME TO {};",
            dameng_action_qualified(schema, old_name),
            fluxdb_core::DamengDialect.quote_identifier(new_name)
        ))
    }

    fn copy_table_sql(
        &self,
        schema: Option<&str>,
        source_name: &str,
        target_name: &str,
        copy_data: bool,
        source_ddl: Option<&str>,
    ) -> Result<String, String> {
        let source_ddl =
            source_ddl.ok_or_else(|| "未读取到源表达梦 DDL，无法复制表".to_string())?;
        dameng_copy_table_sql(schema, source_name, target_name, copy_data, source_ddl)
    }

    fn drop_table_sql(
        &self,
        kind: ObjectKind,
        schema: Option<&str>,
        table_name: &str,
    ) -> Result<String, String> {
        Ok(format!(
            "DROP {} {};",
            if kind == ObjectKind::View { "VIEW" } else { "TABLE" },
            dameng_action_qualified(schema, table_name)
        ))
    }

    fn truncate_table_sql(
        &self,
        schema: Option<&str>,
        table_name: &str,
        _: bool,
    ) -> Result<String, String> {
        Ok(format!(
            "TRUNCATE TABLE {};",
            dameng_action_qualified(schema, table_name)
        ))
    }

    fn with_foreign_key_check(
        &self,
        sql: String,
        foreign_key_check: ForeignKeyCheckMode,
    ) -> Result<String, String> {
        match foreign_key_check {
            ForeignKeyCheckMode::Default => Ok(sql),
            ForeignKeyCheckMode::Enable | ForeignKeyCheckMode::Disable => Err(
                "达梦不支持以连接级开关临时禁用外键检查".to_string(),
            ),
        }
    }
}

static DAMENG_TABLE_ACTION_SQL_PROVIDER: DamengTableActionSqlProvider =
    DamengTableActionSqlProvider;

fn dameng_action_qualified(schema: Option<&str>, name: &str) -> String {
    match schema.map(str::trim).filter(|schema| !schema.is_empty()) {
        Some(schema) => format!(
            "{}.{}",
            fluxdb_core::DamengDialect.quote_identifier(schema),
            fluxdb_core::DamengDialect.quote_identifier(name.trim())
        ),
        None => fluxdb_core::DamengDialect.quote_identifier(name.trim()),
    }
}

fn dameng_copy_table_sql(
    schema: Option<&str>,
    source_name: &str,
    target_name: &str,
    copy_data: bool,
    source_ddl: &str,
) -> Result<String, String> {
    let source = dameng_action_qualified(schema, source_name);
    let target = dameng_action_qualified(schema, target_name);
    let statements = dameng_split_sql_statements(source_ddl)?;
    if statements.is_empty() {
        return Err("未读取到可复制的达梦表结构 DDL".to_string());
    }
    let mut rewritten = statements
        .iter()
        .map(|sql| dameng_rewrite_copy_statement(sql, &source, &target, target_name))
        .collect::<Result<Vec<_>, _>>()?;
    if copy_data {
        let columns = dameng_copy_table_columns(source_ddl)?;
        let column_list = columns
            .iter()
            .map(|column| fluxdb_core::DamengDialect.quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let identity_columns = dameng_copy_identity_columns(source_ddl)?;
        let has_identity = !identity_columns.is_empty();
        let insert = if column_list.is_empty() {
            format!("INSERT INTO {target} SELECT * FROM {source};")
        } else {
            format!(
                "INSERT INTO {target} ({column_list}) SELECT {column_list} FROM {source};"
            )
        };
        if has_identity {
            // DM 只在 SET IDENTITY_INSERT 为 ON 且语句带列清单时才允许给自增列赋值。
            rewritten.push(format!("SET IDENTITY_INSERT {target} ON;"));
            rewritten.push(insert);
            rewritten.push(format!("SET IDENTITY_INSERT {target} OFF;"));
        } else {
            rewritten.push(insert);
        }
    }
    Ok(rewritten.join("\n"))
}

/// 从建表 DDL 里取字段名列表（保持 DDL 中的顺序）。
fn dameng_copy_table_columns(source_ddl: &str) -> Result<Vec<String>, String> {
    let Some(open) = source_ddl.find('(') else {
        return Ok(Vec::new());
    };
    let body = &source_ddl[open + 1..];
    let mut columns = Vec::new();
    for entry in dameng_split_top_level_entries(body) {
        let trimmed = entry.trim();
        let upper = trimmed.to_ascii_uppercase();
        if trimmed.is_empty()
            || upper.starts_with("CONSTRAINT")
            || upper.starts_with("PRIMARY")
            || upper.starts_with("FOREIGN")
            || upper.starts_with("CHECK")
            || upper.starts_with("UNIQUE")
        {
            continue;
        }
        let Some(name) = dameng_ddl_identifier(trimmed) else {
            continue;
        };
        columns.push(name);
    }
    Ok(columns)
}

/// 从建表 DDL 里取自增字段名列表。
fn dameng_copy_identity_columns(source_ddl: &str) -> Result<Vec<String>, String> {
    let Some(open) = source_ddl.find('(') else {
        return Ok(Vec::new());
    };
    let body = &source_ddl[open + 1..];
    let mut identity = Vec::new();
    for entry in dameng_split_top_level_entries(body) {
        let trimmed = entry.trim();
        let upper = trimmed.to_ascii_uppercase();
        if !upper.contains("IDENTITY") && !upper.contains("AUTO_INCREMENT") {
            continue;
        }
        let Some(name) = dameng_ddl_identifier(trimmed) else {
            continue;
        };
        identity.push(name);
    }
    Ok(identity)
}

/// 取一条 DDL 条目的第一个标识符（跳过引号与空白）。
fn dameng_ddl_identifier(entry: &str) -> Option<String> {
    let trimmed = entry.trim_start();
    let first = trimmed.chars().next()?;
    if matches!(first, '"' | '`') {
        let end = trimmed[1..].find(first)? + 2;
        return Some(trimmed[1..end - 1].replace(&format!("{first}{first}"), &first.to_string()));
    }
    trimmed
        .split_whitespace()
        .next()
        .map(str::to_string)
        .filter(|name| !name.is_empty())
}

/// 按顶层逗号切分 DDL 字段列表（忽略括号与引号内的逗号）。
fn dameng_split_top_level_entries(body: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quote = None;
    for (index, ch) in body.char_indices() {
        if let Some(end_quote) = quote {
            if ch == end_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 => {
                let entry = body[start..index].trim();
                if !entry.is_empty() {
                    entries.push(entry.to_string());
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let entry = body[start..].trim();
    if !entry.is_empty() {
        entries.push(entry.to_string());
    }
    entries
}

fn dameng_split_sql_statements(sql: &str) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    for (index, ch) in sql.char_indices() {
        if let Some(end_quote) = quote {
            if ch == end_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ';' => {
                let statement = sql[start..index].trim();
                if !statement.is_empty() {
                    statements.push(format!("{statement};"));
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return Err("达梦表结构 DDL 引号未闭合".to_string());
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        statements.push(format!("{tail};"));
    }
    Ok(statements)
}

fn dameng_rewrite_copy_statement(
    statement: &str,
    source: &str,
    target: &str,
    target_name: &str,
) -> Result<String, String> {
    let trimmed = statement.trim();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("CREATE TABLE ") {
        return dameng_rewrite_create_table(trimmed, target, target_name);
    }
    if upper.starts_with("CREATE INDEX ")
        || upper.starts_with("CREATE UNIQUE INDEX ")
        || upper.starts_with("CREATE BITMAP INDEX ")
    {
        return dameng_rewrite_index(trimmed, target, target_name);
    }
    if upper.starts_with("COMMENT ON TABLE ") || upper.starts_with("COMMENT ON COLUMN ") {
        return Ok(trimmed.replacen(source, target, 1));
    }
    Ok(trimmed.to_string())
}

fn dameng_rewrite_create_table(
    statement: &str,
    target: &str,
    target_name: &str,
) -> Result<String, String> {
    let upper = statement.to_ascii_uppercase();
    let Some(create_pos) = upper.find("CREATE TABLE") else {
        return Err("达梦 DDL 缺少 CREATE TABLE".to_string());
    };
    let name_start = create_pos + "CREATE TABLE".len();
    let Some(paren_offset) = statement[name_start..].find('(') else {
        return Err("达梦 CREATE TABLE 缺少字段定义".to_string());
    };
    let body_start = name_start + paren_offset;
    let mut body = format!(
        "CREATE TABLE {target}\n{}",
        dameng_rename_constraint_names(&statement[body_start..], target_name)
    );
    if !body.trim_end().ends_with(';') {
        body.push(';');
    }
    Ok(body)
}

fn dameng_rename_constraint_names(statement: &str, target_name: &str) -> String {
    let mut output = String::with_capacity(statement.len());
    let mut rest = statement;
    while let Some(offset) = rest.to_ascii_uppercase().find("CONSTRAINT") {
        let keyword_end = offset + "CONSTRAINT".len();
        output.push_str(&rest[..keyword_end]);
        let after = rest[keyword_end..].trim_start();
        let whitespace = &rest[keyword_end..keyword_end + (rest[keyword_end..].len() - after.len())];
        output.push_str(whitespace);
        let Some(quote) = after.chars().next().filter(|ch| matches!(ch, '"' | '`')) else {
            output.push_str(after);
            return output;
        };
        let Some(end) = after[1..].find(quote).map(|index| index + 2) else {
            output.push_str(after);
            return output;
        };
        let old_name = &after[1..end - 1];
        let new_name = dameng_copy_object_name(target_name, old_name);
        output.push(quote);
        output.push_str(&new_name.replace(quote, &format!("{quote}{quote}")));
        output.push(quote);
        rest = &after[end..];
    }
    output.push_str(rest);
    output
}

fn dameng_rewrite_index(
    statement: &str,
    target: &str,
    target_name: &str,
) -> Result<String, String> {
    let upper = statement.to_ascii_uppercase();
    let Some(index_pos) = upper.find("INDEX") else {
        return Err("达梦索引 DDL 缺少 INDEX".to_string());
    };
    let after_index = &statement[index_pos + "INDEX".len()..];
    let name_start = after_index
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(index, _)| index)
        .ok_or_else(|| "达梦索引 DDL 缺少索引名".to_string())?;
    let name_rest = &after_index[name_start..];
    let (old_name, old_end) = dameng_parse_quoted_identifier(name_rest)?;
    let before_name = &statement[..index_pos + "INDEX".len() + name_start];
    let after_name = &name_rest[old_end..];
    let upper_after = after_name.to_ascii_uppercase();
    let Some(on_pos) = upper_after.find(" ON ") else {
        return Err("达梦索引 DDL 缺少 ON".to_string());
    };
    let table_start = on_pos + " ON ".len();
    let table_rest = &after_name[table_start..];
    let table_end = table_rest
        .find(char::is_whitespace)
        .or_else(|| table_rest.find('('))
        .ok_or_else(|| "达梦索引 DDL 缺少索引字段".to_string())?;
    let new_name = dameng_copy_object_name(target_name, &old_name);
    Ok(format!(
        "{before_name}\"{new_name}\"{} ON {target}{}",
        &after_name[..on_pos],
        &table_rest[table_end..]
    ))
}

fn dameng_parse_quoted_identifier(value: &str) -> Result<(String, usize), String> {
    let Some(quote) = value.chars().next().filter(|ch| matches!(ch, '"' | '`')) else {
        return Err("达梦 DDL 标识符未引用".to_string());
    };
    let end = value[1..]
        .find(quote)
        .map(|index| index + 2)
        .ok_or_else(|| "达梦 DDL 标识符引用未闭合".to_string())?;
    Ok((value[1..end - 1].to_string(), end))
}

fn dameng_copy_object_name(target_name: &str, old_name: &str) -> String {
    format!("{}_{}", target_name.trim(), old_name.trim())
        .chars()
        .take(128)
        .collect()
}

#[cfg(test)]
mod dameng_table_action_tests {
    use super::*;

    #[test]
    fn dameng_actions_are_schema_qualified_and_rename_uses_bare_target() {
        assert_eq!(
            rename_table_sql_preview(DatabaseKind::Dameng, Some("APPS"), "ORDERS", "ORDERS_2")
                .expect("rename SQL"),
            "ALTER TABLE \"APPS\".\"ORDERS\" RENAME TO \"ORDERS_2\";"
        );
        assert_eq!(
            drop_table_sql_preview(
                DatabaseKind::Dameng,
                ObjectKind::Table,
                Some("APPS"),
                "ORDERS",
                ForeignKeyCheckMode::Default,
            )
            .expect("drop SQL"),
            "DROP TABLE \"APPS\".\"ORDERS\";"
        );
        assert_eq!(
            truncate_table_sql_preview(
                DatabaseKind::Dameng,
                Some("APPS"),
                "ORDERS",
                false,
                ForeignKeyCheckMode::Default,
            )
            .expect("truncate SQL"),
            "TRUNCATE TABLE \"APPS\".\"ORDERS\";"
        );
    }

    #[test]
    fn dameng_copy_rewrites_table_constraint_index_and_comment_names() {
        let ddl = "CREATE TABLE \"APPS\".\"ORDERS\" (\n  \"ID\" INT IDENTITY(1,1) NOT NULL,\n  \"NAME\" VARCHAR(50),\n  CONSTRAINT \"FK_ORDERS_CUSTOMER\" FOREIGN KEY (\"ID\") REFERENCES \"APPS\".\"CUSTOMERS\" (\"ID\")\n);\nCREATE UNIQUE INDEX \"UK_ORDERS_NAME\" ON \"APPS\".\"ORDERS\" (\"NAME\");\nCOMMENT ON TABLE \"APPS\".\"ORDERS\" IS 'orders';\nCOMMENT ON COLUMN \"APPS\".\"ORDERS\".\"NAME\" IS 'name';";
        let sql = copy_table_sql_preview_with_source_ddl(
            DatabaseKind::Dameng,
            Some("APPS"),
            "ORDERS",
            "ORDERS_COPY",
            true,
            Some(ddl),
        )
        .expect("copy SQL");

        assert!(sql.contains("CREATE TABLE \"APPS\".\"ORDERS_COPY\""));
        assert!(sql.contains("CONSTRAINT \"ORDERS_COPY_FK_ORDERS_CUSTOMER\""));
        assert!(sql.contains("REFERENCES \"APPS\".\"CUSTOMERS\""));
        assert!(sql.contains("CREATE UNIQUE INDEX \"ORDERS_COPY_UK_ORDERS_NAME\" ON \"APPS\".\"ORDERS_COPY\""));
        assert!(sql.contains("COMMENT ON TABLE \"APPS\".\"ORDERS_COPY\" IS 'orders';"));
        assert!(sql.contains("COMMENT ON COLUMN \"APPS\".\"ORDERS_COPY\".\"NAME\" IS 'name';"));
        assert!(sql.contains("SET IDENTITY_INSERT \"APPS\".\"ORDERS_COPY\" ON;"));
        assert!(sql.contains(
            "INSERT INTO \"APPS\".\"ORDERS_COPY\" (\"ID\", \"NAME\") SELECT \"ID\", \"NAME\" FROM \"APPS\".\"ORDERS\";"
        ));
        assert!(sql.contains("SET IDENTITY_INSERT \"APPS\".\"ORDERS_COPY\" OFF;"));
    }

    #[test]
    fn dameng_copy_accepts_formatted_ddl() {
        let ddl = "CREATE TABLE \"APPS\".\"ORDERS\" (\n  \"ID\" INT IDENTITY(1,1) NOT NULL,\n  \"NAME\" VARCHAR(50)\n);";
        let formatted = format_sql_text_for_dialect(ddl, DatabaseKind::Dameng);
        let sql = copy_table_sql_preview_with_source_ddl(
            DatabaseKind::Dameng,
            Some("APPS"),
            "ORDERS",
            "ORDERS_COPY",
            true,
            Some(&formatted),
        )
        .expect("copy formatted DDL");
        assert!(sql.contains("IDENTITY"));
        assert!(sql.contains("SET IDENTITY_INSERT \"APPS\".\"ORDERS_COPY\" ON;"));
    }
}
