fn dm_execute_query(
    config: &ConnectionConfig,
    request: &QueryRequest,
) -> fluxdb_core::Result<QueryExecutionResult> {
    dm_execute_query_with_progress(config, request, &mut |_| {}, &|| false)
}

fn dm_execute_query_with_progress(
    config: &ConnectionConfig,
    request: &QueryRequest,
    on_summary: &mut dyn FnMut(QueryExecutionSummary),
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<QueryExecutionResult> {
    let statements = query_statements_for_execution(request);
    if statements.is_empty() {
        return Err(Error::new(ErrorKind::Query, "查询不能为空"));
    }
    let mut client = dm_connect(config)?;
    if request.options.read_only {
        dm_execute(&mut client, "SET TRANSACTION READ ONLY", &[])?;
    }
    let pagination = query_execution_pagination(request.options.page_offset, request.options.page_size);
    let mut execution = QueryExecutionResult {
        summaries: Vec::new(),
        results: Vec::new(),
        rollback_snapshots: Vec::new(),
    };
    for statement in statements {
        if should_cancel() {
            push_query_summary(
                &mut execution,
                failed_query_summary(
                    statement,
                    QueryStatementKind::Command,
                    "已取消：未执行".into(),
                    0,
                ),
                on_summary,
            );
            continue;
        }
        let started = std::time::Instant::now();
        match dm_execute(&mut client, &statement, &[]) {
            Ok(ExecuteResult::Query { columns, rows }) => {
                let columns = columns
                    .iter()
                    .map(|column| dm_column(column, false))
                    .collect::<Vec<_>>();
                let returned_rows = rows.len() as u64;
                let page = dm_rows_to_page(columns, rows, pagination);
                let message = if page.has_more {
                    format!("返回 {returned_rows} 行，当前仅展示 {} 行（结果已截断）", page.rows.len())
                } else {
                    format!("返回 {returned_rows} 行")
                };
                push_query_summary(
                    &mut execution,
                    QueryExecutionSummary {
                        sql: statement,
                        kind: QueryStatementKind::ResultSet,
                        success: true,
                        message,
                        returned_rows,
                        affected_rows: 0,
                        elapsed_ms: elapsed_ms(started),
                    },
                    on_summary,
                );
                execution.results.push(page);
            }
            Ok(ExecuteResult::Affected(affected)) | Ok(ExecuteResult::Call { affected, .. }) => {
                push_query_summary(
                    &mut execution,
                    QueryExecutionSummary {
                        sql: statement,
                        kind: QueryStatementKind::Command,
                        success: true,
                        message: format!("OK，影响 {affected} 行"),
                        returned_rows: 0,
                        affected_rows: affected,
                        elapsed_ms: elapsed_ms(started),
                    },
                    on_summary,
                );
            }
            Err(error) => {
                tracing::warn!(target: "fluxdb_connectors", message = %error.message, "达梦执行失败");
                push_query_summary(
                    &mut execution,
                    failed_query_summary(
                        statement,
                        QueryStatementKind::Command,
                        error.message.clone(),
                        elapsed_ms(started),
                    ),
                    on_summary,
                );
                if !request.options.continue_on_error {
                    break;
                }
            }
        }
    }
    client.close();
    Ok(execution)
}

fn dm_rows_to_page(columns: Vec<Column>, rows: Vec<Vec<Value>>, pagination: Pagination) -> DataPage {
    let offset = pagination.offset.min(rows.len() as u64) as usize;
    let capacity = query_execution_row_capacity(pagination.limit);
    let has_more = rows.len().saturating_sub(offset) > capacity;
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(capacity)
        .map(|row| dm_row(&row))
        .collect();
    DataPage {
        columns,
        rows,
        offset: pagination.offset,
        limit: pagination.limit,
        has_more,
    }
}

fn dm_execute_parameterized(
    config: &ConnectionConfig,
    request: &QueryRequest,
    parameters: &[fluxdb_core::SqlParameter],
) -> fluxdb_core::Result<QueryExecutionResult> {
    let statement = request.text.trim().to_string();
    if statement.is_empty() {
        return Err(Error::new(ErrorKind::Query, "查询不能为空"));
    }
    let params = parameters
        .iter()
        .map(|parameter| match parameter {
            fluxdb_core::SqlParameter::Null => Param::Null,
            fluxdb_core::SqlParameter::Bool(value) => Param::Bool(*value),
            fluxdb_core::SqlParameter::I64(value) | fluxdb_core::SqlParameter::U64(value) | fluxdb_core::SqlParameter::Decimal(value) => value
                .parse::<i64>()
                .map(Param::Int)
                .unwrap_or_else(|_| Param::Str(value.clone())),
            fluxdb_core::SqlParameter::F64(value) => Param::Float(*value),
            fluxdb_core::SqlParameter::Text(value) => Param::Str(value.clone()),
            fluxdb_core::SqlParameter::Bytes(value) => Param::Bytes(value.clone()),
        })
        .collect::<Vec<_>>();
    let mut client = dm_connect(config)?;
    let started = std::time::Instant::now();
    let pagination = query_execution_pagination(request.options.page_offset, request.options.page_size);
    let result = dm_execute(&mut client, &statement, &params)?;
    client.close();
    match result {
        ExecuteResult::Query { columns, rows } => {
            let columns = columns
                .iter()
                .map(|column| dm_column(column, false))
                .collect::<Vec<_>>();
            let returned = rows.len() as u64;
            let page = dm_rows_to_page(columns, rows, pagination);
            Ok(QueryExecutionResult {
                summaries: vec![QueryExecutionSummary {
                    sql: statement,
                    kind: QueryStatementKind::ResultSet,
                    success: true,
                    message: format!("返回 {returned} 行结果表（参数化绑定）"),
                    returned_rows: returned,
                    affected_rows: 0,
                    elapsed_ms: elapsed_ms(started),
                }],
                results: vec![page],
                rollback_snapshots: Vec::new(),
            })
        }
        ExecuteResult::Affected(affected) | ExecuteResult::Call { affected, .. } => {
            Ok(QueryExecutionResult {
                summaries: vec![QueryExecutionSummary {
                    sql: statement,
                    kind: QueryStatementKind::Command,
                    success: true,
                    message: format!("OK，影响 {affected} 行"),
                    returned_rows: 0,
                    affected_rows: affected,
                    elapsed_ms: elapsed_ms(started),
                }],
                results: Vec::new(),
                rollback_snapshots: Vec::new(),
            })
        }
    }
}
