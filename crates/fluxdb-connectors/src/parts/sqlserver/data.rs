fn sqlserver_load_data(
    config: &ConnectionConfig,
    path: &ObjectPath,
    offset: u64,
    limit: u64,
    sort: &[SortSpec],
    filters: &[FilterSpec],
) -> fluxdb_core::Result<DataPage> {
    sqlserver_load_data_with_expression(config, path, offset, limit, sort, filters, None)
}

fn sqlserver_load_data_expression(
    config: &ConnectionConfig,
    path: &ObjectPath,
    offset: u64,
    limit: u64,
    sort: &[SortSpec],
    expression: &fluxdb_core::FilterExpression,
) -> fluxdb_core::Result<DataPage> {
    sqlserver_load_data_with_expression(config, path, offset, limit, sort, &[], Some(expression))
}

fn sqlserver_load_data_with_expression(
    config: &ConnectionConfig,
    path: &ObjectPath,
    offset: u64,
    limit: u64,
    sort: &[SortSpec],
    filters: &[FilterSpec],
    expression: Option<&fluxdb_core::FilterExpression>,
) -> fluxdb_core::Result<DataPage> {
    if !matches!(path.kind, ObjectKind::Table | ObjectKind::View) {
        return Err(Error::new(ErrorKind::Unsupported, "仅表和视图支持数据读取"));
    }
    let schema = sqlserver_request_schema(config, path)?;
    let database = sqlserver_request_database(config, path.database.as_deref())?;
    let pagination = Pagination::new(offset, limit);
    let mut client = sqlserver_connect_database(config, Some(&database))?;
    let columns = sqlserver_load_columns(&mut client, &schema, &path.name)?;
    let query_columns = sqlserver_columns_to_page_columns(&columns);
    let table = sqlserver_qualified_table(&schema, &path.name);
    let mut parameters = Vec::new();
    let where_sql = match expression {
        Some(expression) => {
            sqlserver_expression_where_clause(expression, &query_columns, None, &mut parameters)?
        }
        None => sqlserver_where_clause(filters, &query_columns, None, &mut parameters)?,
    };
    let order = sqlserver_data_order_by(sort, &query_columns, None);
    let select_list = sqlserver_data_select_list(&query_columns);
    let fetch_limit = pagination.limit.saturating_add(1);
    let sql = if pagination.offset == 0 {
        parameters.push(fluxdb_core::SqlParameter::I64(fetch_limit.to_string()));
        let top = sqlserver_placeholder(parameters.len());
        format!("SELECT TOP ({top}) {select_list} FROM {table}{where_sql}{order}")
    } else {
        parameters.push(fluxdb_core::SqlParameter::I64(pagination.offset.to_string()));
        let skip = sqlserver_placeholder(parameters.len());
        parameters.push(fluxdb_core::SqlParameter::I64(fetch_limit.to_string()));
        let fetch = sqlserver_placeholder(parameters.len());
        let order_by = if order.is_empty() {
            " ORDER BY (SELECT NULL)".to_string()
        } else {
            order.clone()
        };
        // 用 ROW_NUMBER 而不是 OFFSET/FETCH：后者要求 SQL Server 2012+，
        // 且无排序时必须额外构造 ORDER BY。ROW_NUMBER 在 2005+ 均可工作。
        // `order_by` 自带 `ORDER BY` 前缀，窗口函数内必须保留。
        let order_by = order_by.trim_start();
        format!(
            "SELECT {select_list} FROM (SELECT {select_list}, ROW_NUMBER() OVER ({order_by}) AS [__fluxdb_row_num] FROM {table}{where_sql}) AS [__fluxdb_page] WHERE [__fluxdb_row_num] > {skip} AND [__fluxdb_row_num] <= {skip} + {fetch} ORDER BY [__fluxdb_row_num]"
        )
    };
    let batch = sqlserver_block_on(async {
        let batch = sqlserver_run_query(&mut client, &sql, &parameters).await?;
        sqlserver_close(client).await;
        Ok(batch)
    })?;
    let mut rows = sqlserver_first_rows(&batch, "SQL Server 数据页返回类型异常")?;
    let has_more = rows.len() > pagination.limit as usize;
    rows.truncate(pagination.limit as usize);
    Ok(DataPage {
        columns: query_columns,
        rows,
        offset: pagination.offset,
        limit: pagination.limit,
        has_more,
    })
}

/// 显式列清单：`sql_variant` 在 tiberius 0.12.3 的列元数据解码中是未实现分支
/// （`todo!()`），直接 `SELECT *` 会让整个桌面进程 abort。这里把 `sql_variant`
/// 转成 `nvarchar(max)`，既避免驱动崩溃，也让用户能看到可读值。
fn sqlserver_data_select_list(columns: &[Column]) -> String {
    columns
        .iter()
        .map(|column| {
            let quoted = fluxdb_core::SqlServerDialect.quote_identifier(&column.name);
            if sqlserver_is_sql_variant(column) {
                format!("CAST({quoted} AS nvarchar(max)) AS {quoted}")
            } else {
                quoted
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sqlserver_is_sql_variant(column: &Column) -> bool {
    column.type_name.as_deref().is_some_and(|type_name| {
        type_name
            .trim()
            .split(['(', ' '])
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case("sql_variant"))
    })
}

fn sqlserver_data_order_by(
    sort: &[SortSpec],
    columns: &[Column],
    alias: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    for spec in sort {
        if columns.iter().any(|column| column.name == spec.field) {
            parts.push(format!(
                "{} {}",
                sqlserver_column_reference(&spec.field, alias),
                match spec.direction {
                    SortDirection::Asc => "ASC",
                    SortDirection::Desc => "DESC",
                }
            ));
        }
    }
    let sorted = sort.iter().map(|spec| spec.field.as_str()).collect::<Vec<_>>();
    for column in columns
        .iter()
        .filter(|column| column.primary_key && !sorted.contains(&column.name.as_str()))
    {
        parts.push(format!(
            "{} ASC",
            sqlserver_column_reference(&column.name, alias)
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {}", parts.join(", "))
    }
}

/// 导出预览：统计满足过滤条件的行数，并生成与实际导出一致的 SELECT。
///
/// 这里不复用 `load_data`，因为预览只需要计数和 SQL 文本，不应把整页数据
/// 读进内存；过滤参数仍走真实绑定，避免把用户值拼进 SQL。
fn sqlserver_preview_data_export(
    config: &ConnectionConfig,
    path: &ObjectPath,
    fields: &[String],
    sort: &[SortSpec],
    filters: &[FilterSpec],
) -> fluxdb_core::Result<DataExportPreview> {
    if !matches!(path.kind, ObjectKind::Table | ObjectKind::View) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "仅表和视图支持导出预览",
        ));
    }
    let schema = sqlserver_request_schema(config, path)?;
    let database = sqlserver_request_database(config, path.database.as_deref())?;
    let mut client = sqlserver_connect_database(config, Some(&database))?;
    let columns = sqlserver_columns_to_page_columns(&sqlserver_load_columns(
        &mut client,
        &schema,
        &path.name,
    )?);
    let select_list = sqlserver_data_select_list(&columns);
    let table = sqlserver_qualified_table(&schema, &path.name);
    let mut parameters = Vec::new();
    let where_sql = sqlserver_where_clause(filters, &columns, None, &mut parameters)?;
    let count_sql = format!("SELECT COUNT_BIG(*) FROM {table}{where_sql}");
    let batch = sqlserver_block_on(async {
        let batch = sqlserver_run_query(&mut client, &count_sql, &parameters).await?;
        sqlserver_close(client).await;
        Ok(batch)
    })?;
    let row_count = sqlserver_first_rows(&batch, "SQL Server 导出行数返回类型异常")?
        .first()
        .and_then(|row| row.values.first())
        .and_then(cell_i64)
        .unwrap_or(0)
        .max(0) as u64;
    Ok(DataExportPreview {
        sql: sqlserver_export_preview_sql(&table, fields, sort, filters, &columns, &select_list),
        row_count,
    })
}

/// 生成导出 SQL（与实际导出一致：显式字段、过滤、排序）。
///
/// 不追加 TOP：导出是完整结果集；行数上限由导出执行器分页控制。
fn sqlserver_export_preview_sql(
    table: &str,
    fields: &[String],
    sort: &[SortSpec],
    filters: &[FilterSpec],
    columns: &[Column],
    data_select_list: &str,
) -> String {
    let selected = fields
        .iter()
        .filter_map(|field| columns.iter().find(|column| column.name == *field))
        .map(|column| fluxdb_core::SqlServerDialect.quote_identifier(&column.name))
        .collect::<Vec<_>>();
    let select_list = if selected.is_empty() {
        data_select_list.to_string()
    } else {
        selected.join(", ")
    };
    // 预览 SQL 给用户看，不绑定参数；过滤值直接渲染成安全字面量。
    let where_sql = sqlserver_preview_where_clause(filters, columns);
    let order = sqlserver_data_order_by(sort, columns, None);
    format!("SELECT {select_list}\nFROM {table}{where_sql}{order}")
}

/// 生成供展示的 WHERE 子句。只接受可下推的过滤条件；标识符来自已加载列，
/// 值全部按类型渲染成字面量（字符串用 `N''` 并转义单引号）。
fn sqlserver_preview_where_clause(
    filters: &[FilterSpec],
    columns: &[Column],
) -> String {
    let mut clauses = Vec::new();
    for filter in filters.iter().filter(|filter| filter.enabled) {
        if !data_filter_clause_is_pushable(filter) {
            continue;
        }
        let Some(column) = columns.iter().find(|column| column.name == filter.field) else {
            continue;
        };
        let name = fluxdb_core::SqlServerDialect.quote_identifier(&column.name);
        let literal = |value: &CellValue| format!(
            "{}",
            sqlserver_preview_value_literal(value)
        );
        let clause = match filter.op {
            FilterOp::IsNull | FilterOp::NotExists => format!("{name} IS NULL"),
            FilterOp::IsNotNull | FilterOp::Exists => format!("{name} IS NOT NULL"),
            FilterOp::IsEmpty => format!("{name} = N''"),
            FilterOp::IsNotEmpty => format!("{name} <> N''"),
            FilterOp::Between | FilterOp::NotBetween => {
                let Some(start) = filter.values.first() else {
                    continue;
                };
                let Some(end) = filter.values.get(1) else {
                    continue;
                };
                format!(
                    "{name}{} BETWEEN {} AND {}",
                    if filter.op == FilterOp::NotBetween {
                        " NOT"
                    } else {
                        ""
                    },
                    literal(start),
                    literal(end)
                )
            }
            FilterOp::InList | FilterOp::NotInList => {
                if filter.values.is_empty() {
                    continue;
                }
                format!(
                    "{name}{} IN ({})",
                    if filter.op == FilterOp::NotInList {
                        " NOT"
                    } else {
                        ""
                    },
                    filter
                        .values
                        .iter()
                        .map(literal)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            FilterOp::Eq | FilterOp::NotEq => {
                if filter.values.is_empty() {
                    continue;
                }
                let op = if filter.op == FilterOp::Eq { "=" } else { "<>" };
                let joiner = if filter.op == FilterOp::Eq { " OR " } else { " AND " };
                let predicates = filter
                    .values
                    .iter()
                    .map(|value| {
                        if matches!(value, CellValue::Null) {
                            if filter.op == FilterOp::Eq {
                                format!("{name} IS NULL")
                            } else {
                                format!("{name} IS NOT NULL")
                            }
                        } else {
                            format!("{name} {op} {}", literal(value))
                        }
                    })
                    .collect::<Vec<_>>();
                if predicates.len() > 1 {
                    format!("({})", predicates.join(joiner))
                } else {
                    predicates.join(joiner)
                }
            }
            FilterOp::Contains
            | FilterOp::NotContains
            | FilterOp::StartsWith
            | FilterOp::NotStartsWith
            | FilterOp::EndsWith
            | FilterOp::NotEndsWith => {
                let Some(value) = filter.values.first() else {
                    continue;
                };
                let text = match value {
                    CellValue::Text(value) | CellValue::Json(value) => value.clone(),
                    other => other.display_label(),
                };
                let pattern = match filter.op {
                    FilterOp::Contains | FilterOp::NotContains => format!("%{text}%"),
                    FilterOp::StartsWith | FilterOp::NotStartsWith => format!("{text}%"),
                    _ => format!("%{text}"),
                };
                let negative = matches!(
                    filter.op,
                    FilterOp::NotContains | FilterOp::NotStartsWith | FilterOp::NotEndsWith
                );
                format!(
                    "{name} {}LIKE N'{}'",
                    if negative { "NOT " } else { "" },
                    pattern.replace('\'', "''")
                )
            }
            FilterOp::GreaterThan
            | FilterOp::GreaterThanOrEqual
            | FilterOp::LessThan
            | FilterOp::LessThanOrEqual => {
                let Some(value) = filter.values.first() else {
                    continue;
                };
                let op = match filter.op {
                    FilterOp::GreaterThan => ">",
                    FilterOp::GreaterThanOrEqual => ">=",
                    FilterOp::LessThan => "<",
                    _ => "<=",
                };
                format!("{name} {op} {}", literal(value))
            }
        };
        clauses.push(clause);
    }
    if clauses.is_empty() {
        String::new()
    } else {
        format!("\nWHERE {}", clauses.join(" AND "))
    }
}

fn sqlserver_preview_value_literal(value: &CellValue) -> String {
    match value {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(value) => if *value { "1" } else { "0" }.into(),
        CellValue::I64(value) => value.to_string(),
        CellValue::F64(value) if value.is_finite() => value.to_string(),
        CellValue::F64(_) => "NULL".into(),
        CellValue::Text(value) | CellValue::Json(value) => {
            format!("N'{}'", value.replace('\'', "''"))
        }
        // 预览 SQL 里不能出现半截值：写 NULL 并用注释说明，避免用户把它当可执行导出。
        CellValue::TruncatedText { total_chars, .. } => {
            format!("NULL /* 截断的 CLOB：共 {total_chars} 字符，需完整读取 */")
        }
        CellValue::Bytes(value) => format!(
            "0x{}",
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
        CellValue::BinarySummary(summary) => summary
            .preview_hex
            .as_deref()
            .filter(|_| !summary.is_null)
            .map(|hex| format!("0x{hex}"))
            .unwrap_or_else(|| "NULL".into()),
    }
}

fn sqlserver_column_reference(column: &str, alias: Option<&str>) -> String {
    match alias {
        Some(alias) => format!(
            "{}.{}",
            fluxdb_core::SqlServerDialect.quote_identifier(alias),
            fluxdb_core::SqlServerDialect.quote_identifier(column)
        ),
        None => fluxdb_core::SqlServerDialect.quote_identifier(column),
    }
}
