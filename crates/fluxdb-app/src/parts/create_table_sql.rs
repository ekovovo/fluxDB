fn create_table_mysql_sql_preview(create: &CreateTableState) -> Result<String, String> {
    if let Some(message) = create.validation_error() {
        return Err(message.to_string());
    }

    let table_name = create.table_name.trim();
    let mut lines = create
        .columns
        .iter()
        .filter_map(CreateTableColumn::sql_line)
        .collect::<Vec<_>>();
    if let Some(primary_key) = create_table_primary_key_line(&create.columns) {
        lines.push(primary_key);
    }
    lines.extend(create_table_index_lines(create)?);
    lines.extend(create_table_foreign_key_lines(
        create,
        CreateTableSqlDialect::MySql,
    )?);
    lines.extend(create_table_check_lines(
        create,
        CreateTableSqlDialect::MySql,
    )?);

    let mut sql = format!(
        "CREATE TABLE {} (\n{}\n)",
        fluxdb_core::MySqlDialect.quote_identifier(table_name),
        lines
            .into_iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join(",\n")
    );
    let comment = create.comment.trim();
    if !comment.is_empty() {
        sql.push_str(&format!(" COMMENT={}", quote_mysql_string(comment)));
    }
    let table_options = create_table_mysql_table_options(create);
    if !table_options.is_empty() {
        sql.push(' ');
        sql.push_str(&table_options.join(" "));
    }
    let partition_sql = create_table_mysql_partition_sql(create);
    if !partition_sql.is_empty() {
        sql.push('\n');
        sql.push_str(&partition_sql);
    }
    sql.push(';');
    let trigger_statements = create_table_mysql_trigger_statements(create)?;
    if !trigger_statements.is_empty() {
        sql.push('\n');
        sql.push_str(&trigger_statements.join("\n"));
    }
    Ok(sql)
}

fn create_table_sqlite_sql_preview(create: &CreateTableState) -> Result<String, String> {
    if let Some(message) = create.validation_error() {
        return Err(message.to_string());
    }

    let mut sql = create_table_sqlite_create_table_statement(create, create.table_name.trim())?;
    let index_statements = create_table_sqlite_index_statements(create)?;
    if !index_statements.is_empty() {
        sql.push('\n');
        sql.push_str(&index_statements.join("\n"));
    }
    let trigger_statements = create_table_sqlite_trigger_statements(create)?;
    if !trigger_statements.is_empty() {
        sql.push('\n');
        sql.push_str(&trigger_statements.join("\n"));
    }
    Ok(sql)
}

fn create_table_sqlite_create_table_statement(
    create: &CreateTableState,
    table_name: &str,
) -> Result<String, String> {
    let primary_key_columns = create
        .columns
        .iter()
        .filter(|column| column.primary_key && !column.name.trim().is_empty())
        .collect::<Vec<_>>();
    let inline_auto_increment_id = primary_key_columns.len() == 1
        && primary_key_columns[0].auto_increment
        && matches!(
            create_table_base_type(&primary_key_columns[0].data_type).as_str(),
            "integer" | "int"
        );

    let mut lines = create
        .columns
        .iter()
        .filter_map(|column| {
            create_table_sqlite_column_line(column, inline_auto_increment_id)
        })
        .collect::<Vec<_>>();
    if !inline_auto_increment_id {
        let primary_key = primary_key_columns
            .iter()
            .map(|column| fluxdb_core::SqliteDialect.quote_identifier(column.name.trim()))
            .collect::<Vec<_>>();
        if !primary_key.is_empty() {
            lines.push(format!("PRIMARY KEY ({})", primary_key.join(", ")));
        }
    }
    lines.extend(create_table_foreign_key_lines(
        create,
        CreateTableSqlDialect::Sqlite,
    )?);
    lines.extend(create_table_check_lines(
        create,
        CreateTableSqlDialect::Sqlite,
    )?);

    Ok(format!(
        "CREATE TABLE {table_name} (\n{}\n);",
        lines
            .into_iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        table_name = fluxdb_core::SqliteDialect.quote_identifier(table_name)
    ))
}

fn create_table_sqlite_column_line(
    column: &CreateTableColumn,
    inline_auto_increment_id: bool,
) -> Option<String> {
    let name = column.name.trim();
    if name.is_empty() {
        return None;
    }

    let mut sql = format!(
        "{} {}",
        fluxdb_core::SqliteDialect.quote_identifier(name),
        create_table_sqlite_column_type(&column.data_type)
    );
    if inline_auto_increment_id && column.auto_increment {
        sql.push_str(" PRIMARY KEY AUTOINCREMENT");
    } else if !column.nullable || column.primary_key {
        sql.push_str(" NOT NULL");
    }
    if !column.default_value.trim().is_empty() {
        sql.push_str(" DEFAULT ");
        sql.push_str(column.default_value.trim());
    }
    Some(sql)
}

fn create_table_sqlite_column_type(data_type: &str) -> String {
    let base = create_table_base_type(data_type);
    match base.as_str() {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "boolean" => {
            "INTEGER".to_string()
        }
        "real" | "float" | "double" => "REAL".to_string(),
        "numeric" | "decimal" => "NUMERIC".to_string(),
        "blob" | "binary" | "varbinary" | "tinyblob" | "mediumblob" | "longblob" => {
            "BLOB".to_string()
        }
        _ => "TEXT".to_string(),
    }
}

// SQL Server 建表/设计表 SQL。标识符统一用 `[name]`；ALTER TABLE ADD 只能追加列，
// 不生成 MySQL 的 AFTER，也不尝试改写已有计算列/时态表定义。

pub(crate) const SQLSERVER_CREATE_TABLE_TYPE_OPTIONS: &[&str] = &[
    "int",
    "bigint",
    "smallint",
    "tinyint",
    "bit",
    "decimal",
    "numeric",
    "money",
    "float",
    "real",
    "date",
    "time",
    "datetime",
    "datetime2",
    "datetimeoffset",
    "char",
    "varchar",
    "nchar",
    "nvarchar",
    "binary",
    "varbinary",
    "uniqueidentifier",
    "xml",
];

pub(crate) fn create_table_sqlserver_validation_error(
    create: &CreateTableState,
) -> Option<&'static str> {
    if create.table_name.trim().is_empty() {
        return Some("请输入表名");
    }
    let named_columns = create
        .columns
        .iter()
        .filter(|column| !column.name.trim().is_empty())
        .collect::<Vec<_>>();
    if named_columns.is_empty() {
        return Some("请至少填写一个字段");
    }

    let mut names = BTreeSet::new();
    for column in &named_columns {
        if !names.insert(column.name.trim().to_ascii_lowercase()) {
            return Some("字段名不能重复");
        }
        if !sqlserver_create_table_type_is_supported(&column.data_type) {
            return Some("SQL Server 字段类型不在支持范围内");
        }
        if column.auto_increment
            && (!column.primary_key || !sqlserver_is_identity_type(&column.data_type))
        {
            return Some("IDENTITY 字段必须是整数或 decimal/numeric 主键");
        }
        if column.auto_increment && !column.default_value.trim().is_empty() {
            return Some("IDENTITY 字段不能同时设置默认值");
        }
        if column.unsigned || column.zerofill {
            return Some("SQL Server 不支持 UNSIGNED/ZEROFILL");
        }
        if column.binary
            || !column.charset.trim().is_empty()
            || !column.collation.trim().is_empty()
        {
            return Some("SQL Server 列级字符集/二进制属性暂不支持");
        }
        if sqlserver_is_text_type(&column.data_type)
            && sqlserver_default_needs_quotes(&column.default_value)
        {
            return Some("字符串默认值需要用引号包裹");
        }
    }
    if create
        .columns
        .iter()
        .any(|column| column.primary_key && column.name.trim().is_empty())
    {
        return Some("主键字段名不能为空");
    }
    if sqlserver_index_validation_error(create).is_some() {
        return Some("SQL Server 索引定义无效");
    }
    if let CreateTableMode::Create = create.mode {
        if !create.triggers.is_empty() {
            return Some("SQL Server 新建表向导暂不支持触发器");
        }
        if create.partition_enabled {
            return Some("SQL Server 建表向导暂不支持分区");
        }
    }
    if let CreateTableMode::Design { original, .. } = &create.mode {
        if original.engine != create.engine
            || original.charset != create.charset
            || original.collation != create.collation
            || original.tablespace != create.tablespace
        {
            return Some("SQL Server 设计表暂不支持修改该表选项");
        }
        if original.triggers != create.triggers {
            return Some("SQL Server 设计表暂不支持修改触发器");
        }
        if original.partition_enabled != create.partition_enabled
            || original.partition_sql != create.partition_sql
            || original.partition_method != create.partition_method
            || original.partition_expression != create.partition_expression
        {
            return Some("SQL Server 设计表暂不支持分区");
        }
    }
    if let Some(message) = create_table_foreign_key_validation_error(create) {
        return Some(message);
    }
    if let Some(message) = create_table_check_validation_error(create) {
        return Some(message);
    }
    None
}

pub(crate) fn create_table_sqlserver_sql_preview(
    create: &CreateTableState,
) -> Result<String, String> {
    if let Some(message) = create_table_sqlserver_validation_error(create) {
        return Err(message.to_string());
    }
    Ok(create_table_sqlserver_create_statements(create)?.join("\n"))
}

pub(crate) fn create_table_sqlserver_design_statements(
    create: &CreateTableState,
) -> Result<Vec<String>, String> {
    if let Some(message) = create_table_sqlserver_validation_error(create) {
        return Err(message.to_string());
    }
    let CreateTableMode::Design {
        object,
        original,
        original_ddl,
    } = &create.mode
    else {
        return Ok(Vec::new());
    };
    let mut statements = Vec::new();
    let schema = sqlserver_design_schema(create, object);
    let new_table_name = create.table_name.trim();
    let table = sqlserver_qualified_name(schema.as_deref(), new_table_name);

    if !object.name.trim().eq_ignore_ascii_case(new_table_name) {
        statements.push(sqlserver_rename_table_statement(
            schema.as_deref(),
            object.name.trim(),
            new_table_name,
        )?);
    }

    let old_primary_key = create_table_primary_key_names(&original.columns);
    let new_primary_key = create_table_primary_key_names(&create.columns);
    let primary_key_changed = old_primary_key != new_primary_key;
    if primary_key_changed && !old_primary_key.is_empty() {
        let name = sqlserver_primary_key_constraint_name(original_ddl.as_deref()).ok_or_else(
            || "无法从源 DDL 识别旧主键约束名，请先刷新表结构再修改主键".to_string(),
        )?;
        statements.push(format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            fluxdb_core::SqlServerDialect.quote_identifier(&name)
        ));
    }

    for index in &original.indexes {
        if sqlserver_index_unchanged(create, index) {
            continue;
        }
        if index.name.trim().is_empty() {
            return Err("无法删除名称为空的 SQL Server 索引".to_string());
        }
        statements.push(format!(
            "DROP INDEX {} ON {table};",
            fluxdb_core::SqlServerDialect.quote_identifier(index.name.trim())
        ));
    }

    for column in &original.columns {
        if create.columns.iter().any(|current| current.id == column.id) {
            continue;
        }
        statements.extend(sqlserver_drop_column_statements(
            &table,
            column.name.trim(),
            !column.default_value.trim().is_empty(),
        ));
    }
    for column in &create.columns {
        match original
            .columns
            .iter()
            .find(|original| original.id == column.id)
        {
            None => {
                if let Some(line) = sqlserver_column_line(column) {
                    statements.push(format!("ALTER TABLE {table} ADD {line};"));
                } else {
                    statements.push(sqlserver_column_comment_statement(
                        schema.as_deref(),
                        new_table_name,
                        column,
                    ));
                }
            }
            Some(previous) if previous != column => statements.extend(
                sqlserver_column_change_statements(
                    &table,
                    schema.as_deref(),
                    new_table_name,
                    previous,
                    column,
                )?,
            ),
            Some(_) => {}
        }
    }

    let mut create_with_changed_indexes = create.clone();
    create_with_changed_indexes.indexes = create
        .indexes
        .iter()
        .filter(|index| !sqlserver_index_unchanged(create, index))
        .cloned()
        .collect();
    statements.extend(sqlserver_index_statements(
        &create_with_changed_indexes,
        schema.as_deref(),
        new_table_name,
    )?);

    if primary_key_changed
        && let Some(line) = sqlserver_primary_key_line(&create.columns)
    {
        statements.push(format!(
            "ALTER TABLE {table} ADD CONSTRAINT {} {line};",
            fluxdb_core::SqlServerDialect
                .quote_identifier(&sqlserver_primary_key_name(new_table_name))
        ));
    }

    for foreign_key in &original.foreign_keys {
        let unchanged = create
            .foreign_keys
            .iter()
            .any(|current| current.id == foreign_key.id && current == foreign_key);
        if !unchanged && !foreign_key.name.trim().is_empty() {
            statements.push(format!(
                "ALTER TABLE {table} DROP CONSTRAINT {};",
                fluxdb_core::SqlServerDialect.quote_identifier(foreign_key.name.trim())
            ));
        }
    }
    statements.extend(
        sqlserver_foreign_key_lines(
            create,
            schema.as_deref(),
            &create_table_changed_items(&original.foreign_keys, &create.foreign_keys),
        )?
        .into_iter()
        .map(|line| format!("ALTER TABLE {table} ADD {line};")),
    );

    for check in &original.checks {
        let unchanged = create
            .checks
            .iter()
            .any(|current| current.id == check.id && current == check);
        if unchanged {
            continue;
        }
        if check.name.trim().is_empty() {
            return Err("无法删除名称为空的 SQL Server 检查约束".to_string());
        }
        statements.push(format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            fluxdb_core::SqlServerDialect.quote_identifier(check.name.trim())
        ));
    }
    statements.extend(
        sqlserver_check_lines(&create_table_changed_items(
            &original.checks,
            &create.checks,
        ))?
        .into_iter()
        .map(|line| format!("ALTER TABLE {table} ADD {line};")),
    );

    if create.comment.trim() != original.comment.trim() {
        statements.extend(sqlserver_table_comment_statements(
            schema.as_deref(),
            new_table_name,
            create.comment.trim(),
        ));
    }
    Ok(statements)
}

fn create_table_sqlserver_create_statements(
    create: &CreateTableState,
) -> Result<Vec<String>, String> {
    let table_name = create.table_name.trim();
    let schema = create
        .schema
        .trim()
        .to_string();
    let schema = (!schema.is_empty()).then_some(schema);
    let table = sqlserver_qualified_name(schema.as_deref(), table_name);
    let mut lines = create
        .columns
        .iter()
        .filter_map(sqlserver_column_line)
        .collect::<Vec<_>>();
    if let Some(primary_key) = sqlserver_primary_key_line(&create.columns) {
        lines.push(format!(
            "CONSTRAINT {} {primary_key}",
            fluxdb_core::SqlServerDialect
                .quote_identifier(&sqlserver_primary_key_name(table_name))
        ));
    }
    lines.extend(sqlserver_foreign_key_lines(
        create,
        schema.as_deref(),
        &create.foreign_keys,
    )?);
    lines.extend(sqlserver_check_lines(&create.checks)?);

    let mut statements = vec![format!(
        "CREATE TABLE {table} (\n{}\n);",
        lines
            .into_iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join(",\n")
    )];
    if !create.comment.trim().is_empty() {
        statements.extend(sqlserver_table_comment_statements(
            schema.as_deref(),
            table_name,
            create.comment.trim(),
        ));
    }
    for column in &create.columns {
        if !column.name.trim().is_empty() && !column.comment.trim().is_empty() {
            statements.push(sqlserver_column_comment_statement(
                schema.as_deref(),
                table_name,
                column,
            ));
        }
    }
    statements.extend(sqlserver_index_statements(
        create,
        schema.as_deref(),
        table_name,
    )?);
    Ok(statements)
}

fn sqlserver_column_line(column: &CreateTableColumn) -> Option<String> {
    let name = column.name.trim();
    if name.is_empty() {
        return None;
    }
    let mut sql = format!(
        "{} {}",
        fluxdb_core::SqlServerDialect.quote_identifier(name),
        sqlserver_column_type(column)
    );
    if column.auto_increment {
        sql.push_str(" IDENTITY(1,1)");
    }
    if !column.nullable || column.primary_key {
        sql.push_str(" NOT NULL");
    }
    let default_value = column.default_value.trim();
    if !default_value.is_empty() {
        sql.push_str(" DEFAULT ");
        sql.push_str(default_value);
    }
    Some(sql)
}

fn sqlserver_column_type(column: &CreateTableColumn) -> String {
    let base = create_table_base_type(&column.data_type);
    let base = if base == "integer" { "int" } else { base.as_str() };
    let length = column.length.trim();
    let scale = column.scale.trim();
    let supports_length = matches!(
        base,
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary"
    );
    let modifiers = if supports_length && !length.is_empty() {
        format!("({length})")
    } else if matches!(base, "decimal" | "numeric") && !length.is_empty() {
        if scale.is_empty() {
            format!("({length})")
        } else {
            format!("({length},{scale})")
        }
    } else if matches!(base, "time" | "datetime2" | "datetimeoffset") && !scale.is_empty() {
        format!("({scale})")
    } else {
        String::new()
    };
    format!("{base}{modifiers}")
}

fn sqlserver_primary_key_line(columns: &[CreateTableColumn]) -> Option<String> {
    let parts = columns
        .iter()
        .filter(|column| column.primary_key && !column.name.trim().is_empty())
        .map(|column| fluxdb_core::SqlServerDialect.quote_identifier(column.name.trim()))
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| format!("PRIMARY KEY ({})", parts.join(", ")))
}

fn sqlserver_primary_key_name(table_name: &str) -> String {
    format!("PK_{}", table_name.trim())
}

fn sqlserver_column_change_statements(
    table: &str,
    schema: Option<&str>,
    table_name: &str,
    previous: &CreateTableColumn,
    current: &CreateTableColumn,
) -> Result<Vec<String>, String> {
    if previous.auto_increment != current.auto_increment {
        return Err("SQL Server 设计表暂不支持新增或移除 IDENTITY 属性".to_string());
    }
    let name = current.name.trim();
    let quoted = fluxdb_core::SqlServerDialect.quote_identifier(name);
    let mut statements = Vec::new();
    if previous.name.trim() != name {
        if name.contains('.') {
            return Err("重命名只接受单段新列名（不带表名）".to_string());
        }
        statements.push(format!(
            "EXEC sp_rename N'{}.{}', N'{}', 'COLUMN';",
            sqlserver_string_part(table),
            sqlserver_string_part(previous.name.trim()),
            sqlserver_string_part(name)
        ));
    }
    if previous.default_value.trim() != current.default_value.trim() {
        if !previous.default_value.trim().is_empty() {
            statements.extend(sqlserver_drop_default_constraint_statements(table, name));
        }
        if !current.default_value.trim().is_empty() {
            statements.push(format!(
                "ALTER TABLE {table} ADD CONSTRAINT {} DEFAULT {} FOR {quoted};",
                fluxdb_core::SqlServerDialect.quote_identifier(&sqlserver_default_name(
                    table_name,
                    name
                )),
                current.default_value.trim()
            ));
        }
    }
    let type_changed = previous.data_type != current.data_type
        || previous.length != current.length
        || previous.scale != current.scale
        || previous.nullable != current.nullable;
    if type_changed {
        let data_type = sqlserver_column_type(current);
        let nullability = if current.nullable && !current.primary_key {
            "NULL"
        } else {
            "NOT NULL"
        };
        statements.push(format!(
            "ALTER TABLE {table} ALTER COLUMN {quoted} {data_type} {nullability};"
        ));
    }
    if previous.comment.trim() != current.comment.trim() {
        statements.push(sqlserver_column_comment_statement(
            schema,
            table_name,
            current,
        ));
    }
    Ok(statements)
}

fn sqlserver_default_name(table_name: &str, column_name: &str) -> String {
    format!(
        "DF_{}_{}",
        table_name.trim(),
        column_name.trim()
    )
}

fn sqlserver_drop_column_statements(
    table: &str,
    column_name: &str,
    has_known_default: bool,
) -> Vec<String> {
    let mut statements = Vec::new();
    if has_known_default {
        statements.extend(sqlserver_drop_default_constraint_statements(
            table,
            column_name,
        ));
    }
    statements.push(format!(
        "ALTER TABLE {table} DROP COLUMN {};",
        fluxdb_core::SqlServerDialect.quote_identifier(column_name)
    ));
    statements
}

fn sqlserver_drop_default_constraint_statements(table: &str, column_name: &str) -> Vec<String> {
    let table_literal = sqlserver_string_part(table);
    let column_literal = sqlserver_string_part(column_name);
    vec![format!(
        "DECLARE @df sysname;\n\
         SELECT @df = dc.name\n\
         FROM sys.default_constraints dc\n\
         JOIN sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id\n\
         WHERE dc.parent_object_id = OBJECT_ID(N'{table_literal}') AND c.name = N'{column_literal}';\n\
         IF @df IS NOT NULL EXEC(N'ALTER TABLE {table} DROP CONSTRAINT ' + QUOTENAME(@df));"
    )]
}

fn sqlserver_index_validation_error(create: &CreateTableState) -> Option<&'static str> {
    let mut names = BTreeSet::new();
    for index in &create.indexes {
        let name = index.name.trim();
        if name.is_empty() {
            return Some("索引名称不能为空");
        }
        if name.eq_ignore_ascii_case("PRIMARY") || !names.insert(name.to_ascii_lowercase()) {
            return Some("索引名称重复或使用了 PRIMARY");
        }
        if index.columns.is_empty()
            || index
                .columns
                .iter()
                .any(|column| column.name.trim().is_empty())
        {
            return Some("索引至少需要一个字段");
        }
        let index_type = create_table_normalized_index_type(&index.index_type);
        if matches!(index_type, "FULLTEXT" | "SPATIAL")
            || !create_table_normalized_index_method(&index.index_method).is_empty()
                && create_table_normalized_index_method(&index.index_method) != "BTREE"
        {
            return Some("SQL Server 暂不支持该索引类型");
        }
        if index
            .columns
            .iter()
            .any(|column| !column.sub_part.trim().is_empty())
        {
            return Some("SQL Server 索引不支持前缀长度");
        }
    }
    None
}

fn sqlserver_index_unchanged(create: &CreateTableState, index: &CreateTableIndex) -> bool {
    create
        .indexes
        .iter()
        .any(|current| current.id == index.id && current == index)
}

fn sqlserver_index_statements(
    create: &CreateTableState,
    schema: Option<&str>,
    table_name: &str,
) -> Result<Vec<String>, String> {
    let table = sqlserver_qualified_name(schema, table_name);
    let valid_columns = create_table_valid_column_names(create);
    let mut names = BTreeSet::new();
    let mut statements = Vec::with_capacity(create.indexes.len());
    for index in &create.indexes {
        let name = index.name.trim();
        if name.is_empty() {
            return Err("索引名称不能为空".to_string());
        }
        if name.eq_ignore_ascii_case("PRIMARY") {
            return Err("索引名称不能为 PRIMARY".to_string());
        }
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("索引名称重复：{name}"));
        }
        if index.columns.is_empty() {
            return Err(format!("索引 {name} 至少需要一个字段"));
        }
        let index_type = create_table_normalized_index_type(&index.index_type);
        if matches!(index_type, "FULLTEXT" | "SPATIAL") {
            return Err(format!("SQL Server 不支持 {index_type} 索引"));
        }
        let method = create_table_normalized_index_method(&index.index_method);
        if !method.is_empty() && method != "BTREE" {
            return Err("SQL Server 设计表暂不支持指定索引方法".to_string());
        }
        let mut seen = BTreeSet::new();
        let mut parts = Vec::with_capacity(index.columns.len());
        for column in &index.columns {
            let column_name = column.name.trim();
            if !valid_columns.contains(&column_name.to_ascii_lowercase()) {
                return Err(format!("索引 {name} 引用了不存在的字段：{column_name}"));
            }
            if !seen.insert(column_name.to_ascii_lowercase()) {
                return Err(format!("索引 {name} 存在重复字段：{column_name}"));
            }
            if !column.sub_part.trim().is_empty() {
                return Err("SQL Server 索引不支持前缀长度".to_string());
            }
            let mut part = fluxdb_core::SqlServerDialect.quote_identifier(column_name);
            let sort_order = create_table_normalized_sort_order(&column.sort_order);
            if !sort_order.is_empty() {
                part.push(' ');
                part.push_str(sort_order);
            }
            parts.push(part);
        }
        let unique = if index_type == "UNIQUE" { "UNIQUE " } else { "" };
        statements.push(format!(
            "CREATE {unique}INDEX {} ON {table} ({});",
            fluxdb_core::SqlServerDialect.quote_identifier(name),
            parts.join(", ")
        ));
    }
    Ok(statements)
}

fn sqlserver_foreign_key_lines(
    create: &CreateTableState,
    schema: Option<&str>,
    foreign_keys: &[CreateTableForeignKey],
) -> Result<Vec<String>, String> {
    let valid_columns = create_table_valid_column_names(create);
    let mut names = BTreeSet::new();
    let mut lines = Vec::with_capacity(foreign_keys.len());
    for foreign_key in foreign_keys {
        let name = foreign_key.name.trim();
        if name.is_empty() {
            return Err("外键名称不能为空".to_string());
        }
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("外键名称重复：{name}"));
        }
        if foreign_key.columns.is_empty() {
            return Err(format!("外键 {name} 至少需要一个字段"));
        }
        let mut local = Vec::with_capacity(foreign_key.columns.len());
        let mut seen = BTreeSet::new();
        for column in &foreign_key.columns {
            let column = column.trim();
            if !valid_columns.contains(&column.to_ascii_lowercase()) {
                return Err(format!("外键 {name} 引用了不存在的字段：{column}"));
            }
            if !seen.insert(column.to_ascii_lowercase()) {
                return Err(format!("外键 {name} 存在重复字段：{column}"));
            }
            local.push(fluxdb_core::SqlServerDialect.quote_identifier(column));
        }
        let referenced_columns = create_table_foreign_key_referenced_columns(foreign_key);
        if referenced_columns.len() != local.len() {
            return Err(format!("外键 {name} 目标字段数量需要与本地字段一致"));
        }
        let referenced_table = foreign_key.referenced_table.trim();
        if referenced_table.is_empty() {
            return Err(format!("外键 {name} 需要选择目标表"));
        }
        let referenced_schema = foreign_key.referenced_database.trim();
        let referenced_schema = if referenced_schema.is_empty() {
            schema
        } else {
            Some(referenced_schema)
        };
        let referenced = sqlserver_qualified_name(referenced_schema, referenced_table);
        let target_columns = referenced_columns
            .iter()
            .map(|column| fluxdb_core::SqlServerDialect.quote_identifier(column))
            .collect::<Vec<_>>();
        let mut line = format!(
            "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
            fluxdb_core::SqlServerDialect.quote_identifier(name),
            local.join(", "),
            referenced,
            target_columns.join(", ")
        );
        if !foreign_key.on_delete.trim().is_empty() {
            line.push_str(" ON DELETE ");
            line.push_str(create_table_normalized_foreign_key_action(
                &foreign_key.on_delete,
            ));
        }
        if !foreign_key.on_update.trim().is_empty() {
            line.push_str(" ON UPDATE ");
            line.push_str(create_table_normalized_foreign_key_action(
                &foreign_key.on_update,
            ));
        }
        lines.push(line);
    }
    Ok(lines)
}

fn sqlserver_check_lines(checks: &[CreateTableCheck]) -> Result<Vec<String>, String> {
    let mut names = BTreeSet::new();
    let mut lines = Vec::with_capacity(checks.len());
    for check in checks {
        let name = check.name.trim();
        if !name.is_empty() && !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("检查名称重复：{name}"));
        }
        if check.expression.trim().is_empty() {
            return Err("检查表达式不能为空".to_string());
        }
        if check.not_enforced {
            return Err("SQL Server 不支持 NOT ENFORCED 检查约束".to_string());
        }
        lines.push(if name.is_empty() {
            format!("CHECK ({})", check.expression.trim())
        } else {
            format!(
                "CONSTRAINT {} CHECK ({})",
                fluxdb_core::SqlServerDialect.quote_identifier(name),
                check.expression.trim()
            )
        });
    }
    Ok(lines)
}

fn sqlserver_rename_table_statement(
    schema: Option<&str>,
    old_name: &str,
    new_name: &str,
) -> Result<String, String> {
    if new_name.contains('.') {
        return Err("重命名只接受单段新表名（不带 schema）".to_string());
    }
    let old = sqlserver_qualified_name(schema, old_name);
    Ok(format!(
        "EXEC sp_rename N'{}', N'{}', 'OBJECT';",
        sqlserver_string_part(&old),
        sqlserver_string_part(new_name.trim())
    ))
}

fn sqlserver_qualified_name(schema: Option<&str>, table: &str) -> String {
    match schema.map(str::trim).filter(|schema| !schema.is_empty()) {
        Some(schema) => format!(
            "{}.{}",
            fluxdb_core::SqlServerDialect.quote_identifier(schema),
            fluxdb_core::SqlServerDialect.quote_identifier(table.trim())
        ),
        None => fluxdb_core::SqlServerDialect.quote_identifier(table.trim()),
    }
}

fn sqlserver_string_part(value: &str) -> String {
    value.replace('\'', "''")
}

fn sqlserver_table_comment_statements(
    schema: Option<&str>,
    table: &str,
    comment: &str,
) -> Vec<String> {
    vec![sqlserver_extended_property_statement(
        schema,
        table,
        None,
        comment,
    )]
}

fn sqlserver_column_comment_statement(
    schema: Option<&str>,
    table: &str,
    column: &CreateTableColumn,
) -> String {
    sqlserver_extended_property_statement(
        schema,
        table,
        Some(column.name.trim()),
        column.comment.trim(),
    )
}

fn sqlserver_extended_property_statement(
    schema: Option<&str>,
    table: &str,
    column: Option<&str>,
    comment: &str,
) -> String {
    let table_ref = sqlserver_string_part(&sqlserver_qualified_name(schema, table));
    let table_name = sqlserver_string_part(table.trim());
    let schema_name = sqlserver_string_part(schema.unwrap_or("").trim());
    let comment = sqlserver_string_part(comment);
    let level0 = if schema_name.is_empty() {
        String::new()
    } else {
        format!(", @level0type=N'SCHEMA', @level0name=N'{schema_name}'")
    };
    let (exists, level2, action) = match column.map(str::trim).filter(|name| !name.is_empty()) {
        Some(column) => (
            format!(
                "EXISTS (SELECT 1 FROM sys.extended_properties ep JOIN sys.columns c ON c.object_id = ep.major_id AND c.column_id = ep.minor_id WHERE ep.class = 1 AND ep.major_id = OBJECT_ID(N'{table_ref}') AND ep.name = N'MS_Description' AND c.name = N'{}')",
                sqlserver_string_part(column)
            ),
            format!(
                ", @level2type=N'COLUMN', @level2name=N'{}'",
                sqlserver_string_part(column)
            ),
            if comment.is_empty() {
                "sp_dropextendedproperty"
            } else {
                "sp_updateextendedproperty"
            },
        ),
        None => (
            format!(
                "EXISTS (SELECT 1 FROM sys.extended_properties WHERE class = 1 AND major_id = OBJECT_ID(N'{table_ref}') AND minor_id = 0 AND name = N'MS_Description')"
            ),
            String::new(),
            if comment.is_empty() {
                "sp_dropextendedproperty"
            } else {
                "sp_updateextendedproperty"
            },
        ),
    };
    if comment.is_empty() {
        format!(
            "IF {exists}\n EXEC sys.sp_dropextendedproperty @name=N'MS_Description', @level1type=N'TABLE', @level1name=N'{table_name}'{level0}{level2};"
        )
    } else if action == "sp_updateextendedproperty" {
        format!(
            "IF {exists}\n EXEC sys.sp_updateextendedproperty @name=N'MS_Description', @value=N'{comment}', @level1type=N'TABLE', @level1name=N'{table_name}'{level0}{level2};\nELSE\n EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'{comment}', @level1type=N'TABLE', @level1name=N'{table_name}'{level0}{level2};"
        )
    } else {
        String::new()
    }
}

fn sqlserver_design_schema(create: &CreateTableState, object: &ObjectPath) -> Option<String> {
    let schema = create.schema.trim();
    if !schema.is_empty() {
        return Some(schema.to_string());
    }
    object
        .schema
        .as_deref()
        .map(str::trim)
        .filter(|schema| !schema.is_empty())
        .map(str::to_string)
}

fn sqlserver_primary_key_constraint_name(ddl: Option<&str>) -> Option<String> {
    let ddl = ddl?;
    for entry in create_table_ddl_entries(ddl) {
        let upper = entry.to_ascii_uppercase();
        if !upper.contains("PRIMARY KEY") {
            continue;
        }
        let Some(position) = upper.find("CONSTRAINT") else {
            continue;
        };
        return sqlserver_parse_identifier(entry[position + "CONSTRAINT".len()..].trim_start());
    }
    None
}

fn sqlserver_parse_identifier(value: &str) -> Option<String> {
    let value = value.trim_start();
    let first = value.chars().next()?;
    if first == '[' {
        let end = value[1..].find(']')? + 2;
        return Some(value[1..end - 1].replace("]]", "]"));
    }
    if matches!(first, '"' | '`' | '\'') {
        let end = value[1..].find(first)? + 2;
        return Some(value[1..end - 1].to_string());
    }
    value
        .split_whitespace()
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

pub(crate) fn create_table_apply_sqlserver_design_ddl(
    create: &mut CreateTableState,
    ddl: Option<&str>,
) {
    let Some(ddl) = ddl else {
        return;
    };
    for entry in create_table_ddl_entries(ddl) {
        let Some(name) = sqlserver_ddl_column_name(&entry) else {
            continue;
        };
        let Some(column) = create
            .columns
            .iter_mut()
            .find(|column| column.name.eq_ignore_ascii_case(&name))
        else {
            continue;
        };
        let upper = entry.to_ascii_uppercase();
        column.auto_increment = upper.contains(" IDENTITY");
        if let Some(default_value) = sqlserver_ddl_default_value(&entry) {
            column.default_value = default_value;
        }
    }
}

fn sqlserver_ddl_column_name(entry: &str) -> Option<String> {
    let upper = entry.to_ascii_uppercase();
    if ["CONSTRAINT", "PRIMARY", "FOREIGN", "CHECK", "UNIQUE"]
        .iter()
        .any(|keyword| upper.trim_start().starts_with(keyword))
    {
        return None;
    }
    sqlserver_parse_identifier(entry)
}

fn sqlserver_ddl_default_value(entry: &str) -> Option<String> {
    let upper = entry.to_ascii_uppercase();
    let position = upper.find(" DEFAULT ")? + " DEFAULT ".len();
    let rest = entry[position..].trim_start();
    let mut quote = None;
    let mut depth = 0usize;
    let mut end = rest.len();
    let bytes = rest.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let ch = bytes[index] as char;
        if let Some(end_quote) = quote {
            if ch == end_quote {
                if bytes.get(index + 1) == Some(&(end_quote as u8)) {
                    index += 2;
                    continue;
                }
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' | '[' => quote = Some(if ch == '[' { ']' } else { ch }),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && ch.is_ascii_whitespace() => {
                let tail = rest[index..].trim_start();
                let tail_upper = tail.to_ascii_uppercase();
                if ["CONSTRAINT", "NOT NULL", "NULL", "PRIMARY", "UNIQUE", "CHECK", "REFERENCES"]
                    .iter()
                    .any(|keyword| {
                        tail_upper == *keyword
                            || tail_upper.starts_with(&format!("{keyword} "))
                    })
                {
                    end = index;
                    break;
                }
            }
            _ => {}
        }
        index += 1;
    }
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn sqlserver_create_table_type_is_supported(data_type: &str) -> bool {
    SQLSERVER_CREATE_TABLE_TYPE_OPTIONS.contains(&create_table_base_type(data_type).as_str())
}

fn sqlserver_is_identity_type(data_type: &str) -> bool {
    matches!(
        create_table_base_type(data_type).as_str(),
        "tinyint" | "smallint" | "int" | "integer" | "bigint" | "decimal" | "numeric"
    )
}

fn sqlserver_is_text_type(data_type: &str) -> bool {
    matches!(
        create_table_base_type(data_type).as_str(),
        "char" | "varchar" | "nchar" | "nvarchar"
    )
}

fn sqlserver_default_needs_quotes(default_value: &str) -> bool {
    let value = default_value.trim();
    !value.is_empty()
        && !value.starts_with('\'')
        && !value.starts_with("N'")
        && !value.starts_with('(')
        && value.parse::<f64>().is_err()
        && !value.eq_ignore_ascii_case("NULL")
        && !value.eq_ignore_ascii_case("GETDATE()")
        && !value.eq_ignore_ascii_case("SYSDATETIME()")
        && !value.eq_ignore_ascii_case("NEWID()")
}

#[cfg(test)]
mod sqlserver_create_table_tests {
    use super::*;
    use fluxdb_core::ConnectionId;

    #[test]
    fn sqlserver_create_table_uses_identity_types_defaults_and_comments() {
        let mut create =
            CreateTableState::new(ConnectionId(1), Some("sales".into()), DatabaseKind::SqlServer);
        create.schema = "sales".into();
        create.table_name = "orders".into();
        create.comment = "订单".into();
        create.columns[0].name = "id".into();
        create.columns[0].data_type = "bigint".into();
        create.columns[0].auto_increment = true;
        create.add_column();
        let name = create.columns.last_mut().expect("name column");
        name.name = "name".into();
        name.data_type = "nvarchar".into();
        name.length = "80".into();
        name.nullable = false;
        name.default_value = "N''".into();
        name.comment = "名称".into();

        let sql = create_table_sqlserver_sql_preview(&create).expect("SQL Server create SQL");
        assert!(sql.contains("CREATE TABLE [sales].[orders]"));
        assert!(sql.contains("[id] bigint IDENTITY(1,1) NOT NULL"));
        assert!(sql.contains("[name] nvarchar(80) NOT NULL DEFAULT N''"));
        assert!(sql.contains("CONSTRAINT [PK_orders] PRIMARY KEY ([id])"));
        assert!(sql.contains("@value=N'订单'"));
        assert!(sql.contains("@level2type=N'COLUMN', @level2name=N'name'"));
        assert!(sql.contains("@value=N'名称'"));
    }

    #[test]
    fn sqlserver_design_adds_column_and_renames_without_after() {
        let object = ObjectPath {
            connection_id: ConnectionId(1),
            database: Some("sales".into()),
            schema: Some("dbo".into()),
            name: "orders".into(),
            kind: ObjectKind::Table,
        };
        let columns = vec![CompletionColumn {
            database: object.database.clone(),
            schema: object.schema.clone(),
            table: object.name.clone(),
            name: "id".into(),
            type_name: Some("int".into()),
            nullable: false,
            primary_key: true,
            comment: None,
            stable: None,
        }];
        let ddl = "CREATE TABLE [dbo].[orders] (\n  [id] int IDENTITY(1,1) NOT NULL,\n  CONSTRAINT [PK_orders] PRIMARY KEY ([id])\n);";
        let mut create = CreateTableState::design(
            object,
            DatabaseKind::SqlServer,
            columns,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(ddl.into()),
        );
        assert!(create.columns[0].auto_increment);
        create.add_column();
        let note = create.columns.last_mut().expect("note column");
        note.name = "note".into();
        note.data_type = "nvarchar".into();
        note.length = "200".into();

        let statements =
            create_table_sqlserver_design_statements(&create).expect("SQL Server design SQL");
        assert!(
            statements.contains(
                &"ALTER TABLE [dbo].[orders] ADD [note] nvarchar(200);".to_string()
            ),
            "unexpected statements: {statements:?}"
        );
        assert!(statements.iter().all(|sql| !sql.contains(" AFTER ")));
    }
}

fn create_table_design_sql_preview_for_provider(
    provider: &dyn CreateTableProvider,
    create: &CreateTableState,
) -> Result<String, String> {
    if let Some(message) = provider.validation_error(create) {
        return Err(message.to_string());
    }
    let statements = provider.design_statements(create)?;
    if statements.is_empty() {
        return Err("没有需要保存的变更".to_string());
    }
    Ok(statements.join("\n"))
}

pub fn rename_table_sql_preview(
    database_kind: DatabaseKind,
    schema: Option<&str>,
    old_name: &str,
    new_name: &str,
) -> Result<String, String> {
    let provider = table_action_sql_provider(database_kind);
    let old_name = old_name.trim();
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("表名不能为空".to_string());
    }
    if old_name == new_name {
        return Err("表名没有变化".to_string());
    }
    provider.rename_table_sql(schema, old_name, new_name)
}

pub fn copy_table_sql_preview(
    database_kind: DatabaseKind,
    schema: Option<&str>,
    source_name: &str,
    target_name: &str,
    copy_data: bool,
) -> Result<String, String> {
    copy_table_sql_preview_with_source_ddl(
        database_kind,
        schema,
        source_name,
        target_name,
        copy_data,
        None,
    )
}

pub fn copy_table_sql_preview_with_source_ddl(
    database_kind: DatabaseKind,
    schema: Option<&str>,
    source_name: &str,
    target_name: &str,
    copy_data: bool,
    source_ddl: Option<&str>,
) -> Result<String, String> {
    let provider = table_action_sql_provider(database_kind);
    let source_name = source_name.trim();
    let target_name = target_name.trim();
    if target_name.is_empty() {
        return Err("表名不能为空".to_string());
    }
    if source_name == target_name {
        return Err("新表名不能和原表相同".to_string());
    }
    provider.copy_table_sql(schema, source_name, target_name, copy_data, source_ddl)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForeignKeyCheckMode {
    Default,
    Enable,
    Disable,
}

pub fn drop_table_sql_preview(
    database_kind: DatabaseKind,
    kind: ObjectKind,
    schema: Option<&str>,
    table_name: &str,
    foreign_key_check: ForeignKeyCheckMode,
) -> Result<String, String> {
    let provider = table_action_sql_provider(database_kind);
    let table_name = table_name.trim();
    if table_name.is_empty() {
        return Err("表名不能为空".to_string());
    }
    let sql = provider.drop_table_sql(kind, schema, table_name)?;
    provider.with_foreign_key_check(sql, foreign_key_check)
}

pub fn truncate_table_sql_preview(
    database_kind: DatabaseKind,
    schema: Option<&str>,
    table_name: &str,
    restart_identity: bool,
    foreign_key_check: ForeignKeyCheckMode,
) -> Result<String, String> {
    let provider = table_action_sql_provider(database_kind);
    let table_name = table_name.trim();
    if table_name.is_empty() {
        return Err("表名不能为空".to_string());
    }
    let sql = provider.truncate_table_sql(schema, table_name, restart_identity)?;
    provider.with_foreign_key_check(sql, foreign_key_check)
}

trait TableActionSqlProvider: Sync {
    /// `schema` 仅 PG 使用：同名表必须只操作指定 schema 的对象。
    fn rename_table_sql(
        &self,
        schema: Option<&str>,
        old_name: &str,
        new_name: &str,
    ) -> Result<String, String>;
    fn copy_table_sql(
        &self,
        schema: Option<&str>,
        source_name: &str,
        target_name: &str,
        copy_data: bool,
        source_ddl: Option<&str>,
    ) -> Result<String, String>;
    fn drop_table_sql(
        &self,
        kind: ObjectKind,
        schema: Option<&str>,
        table_name: &str,
    ) -> Result<String, String>;
    fn truncate_table_sql(
        &self,
        schema: Option<&str>,
        table_name: &str,
        restart_identity: bool,
    ) -> Result<String, String>;

    fn with_foreign_key_check(
        &self,
        sql: String,
        foreign_key_check: ForeignKeyCheckMode,
    ) -> Result<String, String> {
        match foreign_key_check {
            ForeignKeyCheckMode::Default => Ok(sql),
            ForeignKeyCheckMode::Enable | ForeignKeyCheckMode::Disable => {
                Err("当前连接类型不支持外键检查选项".to_string())
            }
        }
    }
}
