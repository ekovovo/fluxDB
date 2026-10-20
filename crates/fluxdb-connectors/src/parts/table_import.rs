use fluxdb_core::{
    ParameterizedQueryRequest, QueryExecutionOptions, QueryMode, SqlParameter, TableDataImport,
    TableImportFormat, TableImportProgress, TableImportReport, TableImportRequest,
};
use std::{
    collections::BTreeSet,
    fs,
    sync::atomic::{AtomicBool, Ordering},
};

const TABLE_IMPORT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const TABLE_IMPORT_MAX_ROWS: usize = 10_000;
const TABLE_IMPORT_MAX_PARAMS: usize = 50_000;

pub fn table_data_import(kind: DatabaseKind) -> fluxdb_core::Result<Box<dyn TableDataImport>> {
    match kind {
        DatabaseKind::MySql => Ok(Box::new(MySqlTableImport)),
        DatabaseKind::TiDb => Ok(Box::new(TiDbTableImport)),
        DatabaseKind::Postgres => Ok(Box::new(PostgresTableImport)),
        DatabaseKind::Sqlite => Ok(Box::new(SqliteTableImport)),
        DatabaseKind::Dameng => Ok(Box::new(DamengTableImport)),
        DatabaseKind::Redis | DatabaseKind::MongoDb => Err(Error::new(
            ErrorKind::Unsupported,
            "当前连接类型没有可导入的表数据",
        )),
    }
}

struct MySqlTableImport;
struct TiDbTableImport;
struct PostgresTableImport;
struct SqliteTableImport;
struct DamengTableImport;

impl TableDataImport for MySqlTableImport {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::MySql
    }

    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> fluxdb_core::Result<TableImportReport> {
        import_relational(DatabaseKind::MySql, request, cancel, progress)
    }
}

impl TableDataImport for TiDbTableImport {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::TiDb
    }

    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> fluxdb_core::Result<TableImportReport> {
        import_relational(DatabaseKind::TiDb, request, cancel, progress)
    }
}

impl TableDataImport for PostgresTableImport {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Postgres
    }

    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> fluxdb_core::Result<TableImportReport> {
        import_relational(DatabaseKind::Postgres, request, cancel, progress)
    }
}

impl TableDataImport for SqliteTableImport {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Sqlite
    }

    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> fluxdb_core::Result<TableImportReport> {
        import_relational(DatabaseKind::Sqlite, request, cancel, progress)
    }
}

impl TableDataImport for DamengTableImport {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Dameng
    }

    fn import(
        &self,
        request: &TableImportRequest,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(TableImportProgress),
    ) -> fluxdb_core::Result<TableImportReport> {
        import_relational(DatabaseKind::Dameng, request, cancel, progress)
    }
}

#[derive(Debug)]
struct ParsedTable {
    columns: Vec<String>,
    rows: Vec<Vec<CellValue>>,
}

fn import_relational(
    kind: DatabaseKind,
    request: &TableImportRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(TableImportProgress),
) -> fluxdb_core::Result<TableImportReport> {
    if request.config.kind != kind {
        return Err(Error::new(ErrorKind::Internal, "导入执行器与连接类型不匹配"));
    }
    if request.object.kind != ObjectKind::Table {
        return Err(Error::new(ErrorKind::Unsupported, "仅表对象支持数据导入"));
    }
    check_cancel(cancel)?;
    progress(TableImportProgress {
        stage: "读取".into(),
        message: format!("正在读取 {} 文件", request.format.label()),
    });
    let parsed = parse_import_file(request, cancel, progress)?;
    if parsed.rows.is_empty() {
        return Ok(TableImportReport {
            total_rows: 0,
            imported_rows: 0,
            warnings: Vec::new(),
        });
    }
    let connector = connector_for(&request.config)?;
    let target_columns = connector.list_completion_columns(
        request.object.database.as_deref(),
        request.object.schema.as_deref(),
        &request.object.name,
    )?;
    if target_columns.is_empty() {
        return Err(Error::new(ErrorKind::Query, "目标表没有可导入字段"));
    }
    let mapping = map_import_columns(&parsed.columns, &target_columns)?;

    let ddl = connector.table_ddl(&request.object).ok();
    let (excluded, identity_columns) = ddl_import_column_flags(kind, ddl.as_deref());
    let mut warnings = Vec::new();
    let mut selected = Vec::new();
    for (import_index, target_index) in mapping.into_iter().enumerate() {
        let column = &target_columns[target_index];
        let upper = column.name.to_ascii_uppercase();
        if excluded.contains(&upper) {
            warnings.push(format!("字段 {} 由数据库生成，已跳过", column.name));
            continue;
        }
        selected.push((import_index, target_index));
    }
    if selected.is_empty() {
        return Err(Error::new(ErrorKind::Query, "导入文件没有可写入字段"));
    }
    if selected.len() * parsed.rows.len() > TABLE_IMPORT_MAX_PARAMS {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "导入行数或字段数超过单次上限，请拆分文件后重试",
        ));
    }
    check_cancel(cancel)?;
    let table = import_qualified_table(kind, request)?;
    let column_list = selected
        .iter()
        .map(|(_, target_index)| {
            import_quote_identifier(kind, &target_columns[*target_index].name)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let overriding = if kind == DatabaseKind::Postgres
        && selected.iter().any(|(_, target_index)| {
            identity_columns.contains(&target_columns[*target_index].name.to_ascii_uppercase())
        })
    {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    let mut parameters = Vec::with_capacity(selected.len() * parsed.rows.len());
    let mut values = Vec::with_capacity(parsed.rows.len());
    for row in &parsed.rows {
        let mut placeholders = Vec::with_capacity(selected.len());
        for (import_index, target_index) in &selected {
            let value = row.get(*import_index).cloned().unwrap_or(CellValue::Null);
            let parameter = import_sql_parameter_for_column(
                &value,
                &target_columns[*target_index],
            )?;
            placeholders.push(match kind {
                DatabaseKind::Postgres => {
                    parameters.push(parameter);
                    let index = parameters.len();
                    let column = &target_columns[*target_index];
                    if matches!(value, CellValue::Null)
                        || pg_import_needs_text_cast(&value, column)
                    {
                        format!(
                            "CAST(CAST(${index} AS text) AS {})",
                            column.type_name.as_deref().unwrap_or("text")
                        )
                    } else {
                        format!("${index}")
                    }
                }
                _ => {
                    parameters.push(parameter);
                    "?".to_string()
                }
            });
        }
        values.push(format!("({})", placeholders.join(", ")));
    }
    let sql = format!(
        "INSERT INTO {table} ({column_list}){overriding} VALUES {}",
        values.join(", ")
    );
    let query = QueryRequest {
        connection_id: request.config.id,
        database: request.object.database.clone(),
        schema: request.object.schema.clone(),
        session_id: None,
        text: sql,
        mode: QueryMode::All,
        options: QueryExecutionOptions::default(),
    };
    progress(TableImportProgress {
        stage: "写入".into(),
        message: format!("正在写入 {} 行", parsed.rows.len()),
    });
    check_cancel(cancel)?;
    let result = connector.execute_parameterized(&ParameterizedQueryRequest {
        request: query,
        parameters,
    })?;
    if let Some(summary) = result.summaries.iter().find(|summary| !summary.success) {
        return Err(Error::new(ErrorKind::Query, summary.message.clone()));
    }
    let affected = result
        .summaries
        .iter()
        .map(|summary| summary.affected_rows)
        .max()
        .unwrap_or(0);
    Ok(TableImportReport {
        total_rows: parsed.rows.len() as u64,
        imported_rows: if affected > 0 {
            affected as u64
        } else {
            parsed.rows.len() as u64
        },
        warnings,
    })
}

fn parse_import_file(
    request: &TableImportRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(TableImportProgress),
) -> fluxdb_core::Result<ParsedTable> {
    let metadata = fs::metadata(&request.source)
        .map_err(|error| Error::new(ErrorKind::Internal, format!("读取导入文件失败：{error}")))?;
    if !metadata.is_file() {
        return Err(Error::new(ErrorKind::Internal, "导入源不是普通文件"));
    }
    if metadata.len() > TABLE_IMPORT_MAX_BYTES {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "导入文件超过 64 MiB，请拆分后重试",
        ));
    }
    let bytes = fs::read(&request.source)
        .map_err(|error| Error::new(ErrorKind::Internal, format!("读取导入文件失败：{error}")))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(ErrorKind::Query, "导入文件必须是 UTF-8 文本"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let parsed = match request.format {
        TableImportFormat::Txt => parse_delimited(text, b'\t', cancel, progress)?,
        TableImportFormat::Csv => parse_delimited(text, b',', cancel, progress)?,
        TableImportFormat::Json => parse_json(text, cancel, progress)?,
        TableImportFormat::Xml => parse_xml(text, cancel, progress)?,
    };
    if parsed.rows.len() > TABLE_IMPORT_MAX_ROWS {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "导入行数超过单次上限，请拆分文件后重试",
        ));
    }
    Ok(parsed)
}

fn parse_delimited(
    text: &str,
    delimiter: u8,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(TableImportProgress),
) -> fluxdb_core::Result<ParsedTable> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(false)
        .from_reader(text.as_bytes());
    let headers = reader
        .headers()
        .map_err(|error| Error::new(ErrorKind::Query, format!("导入表头无效：{error}")))?
        .iter()
        .map(|value| value.trim().to_string())
        .collect::<Vec<_>>();
    validate_import_columns(&headers)?;
    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        check_cancel(cancel)?;
        let record = record.map_err(|error| {
            Error::new(
                ErrorKind::Query,
                format!("导入第 {} 行无效：{error}", index + 2),
            )
        })?;
        if record.len() != headers.len() {
            return Err(Error::new(
                ErrorKind::Query,
                format!("导入第 {} 行字段数与表头不一致", index + 2),
            ));
        }
        rows.push(
            record
                .iter()
                .map(import_text_cell)
                .collect::<Vec<_>>(),
        );
        if (index + 1) % 1000 == 0 {
            progress(TableImportProgress {
                stage: "读取".into(),
                message: format!("已读取 {} 行", index + 1),
            });
        }
    }
    Ok(ParsedTable {
        columns: headers,
        rows,
    })
}

fn parse_json(
    text: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(TableImportProgress),
) -> fluxdb_core::Result<ParsedTable> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| Error::new(ErrorKind::Query, format!("JSON 导入文件无效：{error}")))?;
    let rows = value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Query, "JSON 导入文件顶层必须是数组"))?;
    let Some(first) = rows.first().and_then(serde_json::Value::as_object) else {
        return Err(Error::new(
            ErrorKind::Query,
            "JSON 导入文件必须包含对象数组",
        ));
    };
    let columns = first.keys().cloned().collect::<Vec<_>>();
    validate_import_columns(&columns)?;
    let mut parsed = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        check_cancel(cancel)?;
        let row = row.as_object().ok_or_else(|| {
            Error::new(
                ErrorKind::Query,
                format!("JSON 第 {} 行不是对象", index + 1),
            )
        })?;
        if row.len() != columns.len()
            || columns.iter().any(|column| !row.contains_key(column))
        {
            return Err(Error::new(
                ErrorKind::Query,
                format!("JSON 第 {} 行字段与首行不一致", index + 1),
            ));
        }
        parsed.push(
            columns
                .iter()
                .map(|column| import_json_cell(&row[column]))
                .collect(),
        );
        if (index + 1) % 1000 == 0 {
            progress(TableImportProgress {
                stage: "读取".into(),
                message: format!("已读取 {} 行", index + 1),
            });
        }
    }
    Ok(ParsedTable {
        columns,
        rows: parsed,
    })
}

fn parse_xml(
    text: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(TableImportProgress),
) -> fluxdb_core::Result<ParsedTable> {
    let document = roxmltree::Document::parse(text)
        .map_err(|error| Error::new(ErrorKind::Query, format!("XML 导入文件无效：{error}")))?;
    let root = document.root_element();
    let row_nodes = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "row")
        .collect::<Vec<_>>();
    let Some(first) = row_nodes.first() else {
        return Err(Error::new(ErrorKind::Query, "XML 导入文件没有 row 节点"));
    };
    let columns = xml_row_columns(first);
    validate_import_columns(&columns)?;
    let mut rows = Vec::with_capacity(row_nodes.len());
    for (index, row) in row_nodes.iter().enumerate() {
        check_cancel(cancel)?;
        let values = xml_row_values(row, &columns).map_err(|message| {
            tracing::debug!(
                target: "fluxdb_connectors",
                row = index + 1,
                %message,
                "XML 导入行解析失败"
            );
            Error::new(ErrorKind::Query, format!("XML 第 {} 行格式无效", index + 1))
        })?;
        rows.push(values);
        if (index + 1) % 1000 == 0 {
            progress(TableImportProgress {
                stage: "读取".into(),
                message: format!("已读取 {} 行", index + 1),
            });
        }
    }
    Ok(ParsedTable { columns, rows })
}

fn xml_row_columns(row: &roxmltree::Node<'_, '_>) -> Vec<String> {
    row.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "field")
        .filter_map(|node| node.attribute("name").map(str::to_string))
        .collect()
}

fn xml_row_values(
    row: &roxmltree::Node<'_, '_>,
    columns: &[String],
) -> Result<Vec<CellValue>, String> {
    let mut values = std::collections::BTreeMap::new();
    for field in row
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "field")
    {
        let name = field
            .attribute("name")
            .ok_or_else(|| "field 缺少 name 属性".to_string())?;
        if field.children().any(|node| node.is_element()) {
            return Err(format!("field {name} 包含嵌套元素，暂不支持"));
        }
        if values.insert(name.to_string(), import_text_cell(field.text().unwrap_or(""))).is_some() {
            return Err(format!("field {name} 重复"));
        }
    }
    if values.len() != columns.len() {
        return Err("字段数与首行不一致".to_string());
    }
    columns
        .iter()
        .map(|column| {
            values
                .remove(column)
                .ok_or_else(|| format!("缺少字段 {column}"))
        })
        .collect()
}

fn validate_import_columns(columns: &[String]) -> fluxdb_core::Result<()> {
    if columns.is_empty() {
        return Err(Error::new(ErrorKind::Query, "导入文件没有字段"));
    }
    let mut seen = BTreeSet::new();
    for column in columns {
        if column.trim().is_empty() {
            return Err(Error::new(ErrorKind::Query, "导入字段名不能为空"));
        }
        if !seen.insert(column.to_ascii_lowercase()) {
            return Err(Error::new(
                ErrorKind::Query,
                format!("导入字段名重复：{column}"),
            ));
        }
    }
    Ok(())
}

fn map_import_columns(
    imported: &[String],
    target: &[CompletionColumn],
) -> fluxdb_core::Result<Vec<usize>> {
    imported
        .iter()
        .map(|name| {
            let exact = target
                .iter()
                .position(|column| column.name == *name);
            if let Some(index) = exact {
                return Ok(index);
            }
            let matches = target
                .iter()
                .enumerate()
                .filter(|(_, column)| column.name.eq_ignore_ascii_case(name))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [index] => Ok(*index),
                [] => Err(Error::new(
                    ErrorKind::Query,
                    format!("目标表不存在字段：{name}"),
                )),
                _ => Err(Error::new(
                    ErrorKind::Query,
                    format!("目标表字段名大小写不唯一：{name}"),
                )),
            }
        })
        .collect()
}

fn ddl_import_column_flags(
    kind: DatabaseKind,
    ddl: Option<&str>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut excluded = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let Some(ddl) = ddl else {
        return (excluded, identities);
    };
    for entry in ddl_column_entries(ddl) {
        let Some(name) = ddl_entry_name(&entry) else {
            continue;
        };
        let upper = entry.to_ascii_uppercase();
        if kind == DatabaseKind::Postgres && upper.contains("IDENTITY") {
            identities.insert(name.to_ascii_uppercase());
        } else if (kind == DatabaseKind::Dameng && upper.contains("IDENTITY"))
            || (upper.contains("GENERATED")
                && !upper.contains("IDENTITY")
                && !upper.contains("AUTO_INCREMENT"))
        {
            excluded.insert(name.to_ascii_uppercase());
        }
    }
    (excluded, identities)
}

fn ddl_column_entries(ddl: &str) -> Vec<String> {
    let Some(open) = ddl.find('(') else {
        return Vec::new();
    };
    let mut depth = 0usize;
    let mut quote = None;
    let mut start = open + 1;
    let mut entries = Vec::new();
    for (index, ch) in ddl.char_indices().filter(|(index, _)| *index >= open) {
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
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            ',' if depth == 1 => {
                entries.push(ddl[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    let tail = ddl[start..].trim();
    if !tail.is_empty() {
        entries.push(tail.to_string());
    }
    entries
}

fn ddl_entry_name(entry: &str) -> Option<String> {
    let trimmed = entry.trim();
    let upper = trimmed.to_ascii_uppercase();
    if ["CONSTRAINT", "PRIMARY", "FOREIGN", "CHECK", "UNIQUE"]
        .iter()
        .any(|keyword| upper.starts_with(keyword))
    {
        return None;
    }
    let first = trimmed.chars().next()?;
    if matches!(first, '"' | '`') {
        let end = trimmed[1..].find(first)? + 2;
        return Some(trimmed[1..end - 1].to_string());
    }
    trimmed.split_whitespace().next().map(str::to_string)
}

fn import_qualified_table(
    kind: DatabaseKind,
    request: &TableImportRequest,
) -> fluxdb_core::Result<String> {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => {
            let database = request
                .object
                .database
                .as_deref()
                .ok_or_else(|| Error::new(ErrorKind::Query, "缺少数据库名称"))?;
            Ok(format!(
                "{}.{}",
                fluxdb_core::MySqlDialect.quote_identifier(database),
                fluxdb_core::MySqlDialect.quote_identifier(&request.object.name)
            ))
        }
        DatabaseKind::Postgres => {
            let schema = request.object.schema.as_deref().unwrap_or("public");
            Ok(format!(
                "{}.{}",
                fluxdb_core::PostgresDialect.quote_identifier(schema),
                fluxdb_core::PostgresDialect.quote_identifier(&request.object.name)
            ))
        }
        DatabaseKind::Sqlite => Ok(fluxdb_core::SqliteDialect
            .quote_identifier(&request.object.name)),
        DatabaseKind::Dameng => {
            let schema = request
                .object
                .schema
                .as_deref()
                .or_else(|| {
                    request
                        .config
                        .dameng_profile
                        .as_ref()
                        .map(|profile| profile.basic.schema.as_str())
                })
                .ok_or_else(|| Error::new(ErrorKind::Query, "缺少达梦 schema 上下文"))?;
            Ok(format!(
                "{}.{}",
                fluxdb_core::DamengDialect.quote_identifier(schema),
                fluxdb_core::DamengDialect.quote_identifier(&request.object.name)
            ))
        }
        DatabaseKind::Redis | DatabaseKind::MongoDb => Err(Error::new(
            ErrorKind::Unsupported,
            "当前连接类型没有可导入的表数据",
        )),
    }
}

fn import_quote_identifier(kind: DatabaseKind, identifier: &str) -> String {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => {
            fluxdb_core::MySqlDialect.quote_identifier(identifier)
        }
        DatabaseKind::Postgres => fluxdb_core::PostgresDialect.quote_identifier(identifier),
        DatabaseKind::Sqlite => fluxdb_core::SqliteDialect.quote_identifier(identifier),
        DatabaseKind::Dameng => fluxdb_core::DamengDialect.quote_identifier(identifier),
        DatabaseKind::Redis | DatabaseKind::MongoDb => identifier.to_string(),
    }
}

fn import_text_cell(value: &str) -> CellValue {
    if value.eq_ignore_ascii_case("NULL") {
        CellValue::Null
    } else {
        CellValue::Text(value.to_string())
    }
}

fn import_json_cell(value: &serde_json::Value) -> CellValue {
    match value {
        serde_json::Value::Null => CellValue::Null,
        serde_json::Value::Bool(value) => CellValue::Bool(*value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(CellValue::I64)
            .or_else(|| value.as_f64().map(CellValue::F64))
            .unwrap_or_else(|| CellValue::Text(value.to_string())),
        serde_json::Value::String(value) => CellValue::Text(value.clone()),
        other => CellValue::Json(other.to_string()),
    }
}

fn import_sql_parameter(value: &CellValue) -> fluxdb_core::Result<SqlParameter> {
    Ok(match value {
        CellValue::Null => SqlParameter::Null,
        CellValue::Bool(value) => SqlParameter::Bool(*value),
        CellValue::I64(value) => SqlParameter::I64(value.to_string()),
        CellValue::F64(value) if value.is_finite() => SqlParameter::F64(*value),
        CellValue::F64(_) => {
            return Err(Error::new(ErrorKind::Query, "导入数据包含非有限浮点数"));
        }
        CellValue::Text(value) | CellValue::Json(value) => SqlParameter::Text(value.clone()),
        CellValue::Bytes(value) => SqlParameter::Bytes(value.clone()),
        CellValue::BinarySummary(_) => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "导入不支持未完整加载的二进制字段",
            ));
        }
    })
}

fn import_sql_parameter_for_column(
    value: &CellValue,
    column: &CompletionColumn,
) -> fluxdb_core::Result<SqlParameter> {
    let base = column
        .type_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .split('(')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    match value {
        CellValue::Null | CellValue::Bool(_) | CellValue::Bytes(_) | CellValue::BinarySummary(_) => {
            import_sql_parameter(value)
        }
        CellValue::I64(value)
            if matches!(
                base.as_str(),
                "numeric" | "decimal" | "number"
            ) =>
        {
            Ok(SqlParameter::Decimal(value.to_string()))
        }
        CellValue::F64(value)
            if matches!(base.as_str(), "numeric" | "decimal" | "number") && value.is_finite() =>
        {
            Ok(SqlParameter::Decimal(value.to_string()))
        }
        CellValue::Text(value) | CellValue::Json(value) => {
            if matches!(
                base.as_str(),
                "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "int2"
                    | "int4" | "int8" | "serial" | "bigserial" | "smallserial"
            ) {
                value
                    .trim()
                    .parse::<i64>()
                    .map(|value| SqlParameter::I64(value.to_string()))
                    .map_err(|_| Error::new(ErrorKind::Query, "导入整数值格式无效"))
            } else if matches!(base.as_str(), "numeric" | "decimal" | "number") {
                Ok(SqlParameter::Decimal(value.trim().to_string()))
            } else if matches!(
                base.as_str(),
                "real" | "float4" | "float8" | "double" | "double precision" | "float"
            ) {
                value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(SqlParameter::F64)
                    .ok_or_else(|| Error::new(ErrorKind::Query, "导入浮点值格式无效"))
            } else if matches!(base.as_str(), "boolean" | "bool") {
                match value.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "t" | "yes" => Ok(SqlParameter::Bool(true)),
                    "0" | "false" | "f" | "no" => Ok(SqlParameter::Bool(false)),
                    _ => Err(Error::new(ErrorKind::Query, "导入布尔值格式无效")),
                }
            } else {
                Ok(SqlParameter::Text(value.clone()))
            }
        }
        _ => import_sql_parameter(value),
    }
}

fn pg_import_needs_text_cast(value: &CellValue, column: &CompletionColumn) -> bool {
    if !matches!(value, CellValue::Text(_) | CellValue::Json(_)) {
        return false;
    }
    let base = column
        .type_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    !matches!(
        base.as_str(),
        "text"
            | "varchar"
            | "character varying"
            | "char"
            | "character"
            | "bpchar"
            | "name"
            | "citext"
            | "smallint"
            | "integer"
            | "int"
            | "int2"
            | "int4"
            | "int8"
            | "bigint"
            | "real"
            | "float"
            | "float4"
            | "float8"
            | "double"
            | "double precision"
            | "boolean"
            | "bool"
            | "numeric"
            | "decimal"
    )
}

fn check_cancel(cancel: &AtomicBool) -> fluxdb_core::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::new(ErrorKind::Unsupported, "导入已取消"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod table_import_tests {
    use super::*;
    use std::io::Write;

    fn parse_sample(
        format: TableImportFormat,
        extension: &str,
        text: &str,
    ) -> ParsedTable {
        let dir = std::env::temp_dir().join(format!(
            "fluxdb-import-test-{}-{}",
            std::process::id(),
            extension
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("sample.{extension}"));
        let mut file = std::fs::File::create(&path).expect("sample file");
        file.write_all(text.as_bytes()).expect("write sample");
        let request = TableImportRequest {
            config: crate::ConnectionConfig {
                id: crate::ConnectionId(1),
                name: "import test".into(),
                kind: crate::DatabaseKind::Sqlite,
                endpoint: crate::Endpoint::SqliteFile {
                    path: path.clone(),
                    read_only: true,
                },
                options: std::collections::BTreeMap::new(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
            },
            object: crate::ObjectPath {
                connection_id: crate::ConnectionId(1),
                database: None,
                schema: None,
                name: "sample".into(),
                kind: crate::ObjectKind::Table,
            },
            source: path,
            format,
        };
        parse_import_file(&request, &AtomicBool::new(false), &mut |_| {}).expect("parse")
    }

    #[test]
    fn parses_exported_txt_and_csv_with_null_sentinel() {
        for (format, extension, text) in [
            (TableImportFormat::Txt, "txt", "ID\tNAME\n1\talpha\n2\tNULL\n"),
            (TableImportFormat::Csv, "csv", "ID,NAME\n1,alpha\n2,NULL\n"),
        ] {
            let parsed = parse_sample(format, extension, text);
            assert_eq!(parsed.columns, vec!["ID", "NAME"]);
            assert_eq!(parsed.rows[0][1], CellValue::Text("alpha".into()));
            assert_eq!(parsed.rows[1][1], CellValue::Null);
        }
    }

    #[test]
    fn parses_exported_json_and_xml_shapes() {
        let json = parse_sample(
            TableImportFormat::Json,
            "json",
            r#"[{"ID":1,"NAME":"alpha"},{"ID":2,"NAME":null}]"#,
        );
        assert_eq!(json.rows[0][0], CellValue::I64(1));
        assert_eq!(json.rows[1][1], CellValue::Null);
        let xml = parse_sample(
            TableImportFormat::Xml,
            "xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><rows><row><field name="ID">1</field><field name="NAME">alpha</field></row><row><field name="ID">2</field><field name="NAME">NULL</field></row></rows>"#,
        );
        assert_eq!(xml.rows[0][1], CellValue::Text("alpha".into()));
        assert_eq!(xml.rows[1][1], CellValue::Null);
    }
}
