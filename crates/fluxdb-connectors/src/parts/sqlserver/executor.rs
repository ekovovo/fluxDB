fn sqlserver_execute_query(
    config: &ConnectionConfig,
    request: &QueryRequest,
) -> fluxdb_core::Result<QueryExecutionResult> {
    sqlserver_execute_query_with_progress(config, request, &mut |_| {}, &|| false)
}

fn sqlserver_execute_query_with_progress(
    config: &ConnectionConfig,
    request: &QueryRequest,
    on_summary: &mut dyn FnMut(QueryExecutionSummary),
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<QueryExecutionResult> {
    let statements = query_statements_for_execution(request, config.kind);
    if statements.is_empty() {
        return Err(Error::new(ErrorKind::Query, "查询不能为空"));
    }
    let database = sqlserver_request_database(config, request.database.as_deref())?;
    let pagination = query_execution_pagination(request.options.page_offset, request.options.page_size);
    let query_timeout = Duration::from_secs(u64::from(
        sqlserver_profile(config)?.advanced.query_timeout_secs,
    ));

    if let Some(session_id) = request.session_id {
        let client = sqlserver_session_client(config, &database, session_id)?;
        let mut guard = client
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "SQL Server 查询会话锁失效"))?;
        let (execution, connection_broken) = sqlserver_block_on(
            sqlserver_execute_on_client(
                &mut guard,
                request,
                statements,
                pagination,
                query_timeout,
                on_summary,
                should_cancel,
            ),
        )?;
        if connection_broken {
            // Timeout/cancel closes the socket after the guard drops. Remove the
            // registry entry so a later run cannot reuse a session whose outcome
            // is still uncertain on the server.
            sqlserver_close_query_session(config.id, session_id);
        }
        return Ok(execution);
    }

    let mut client = sqlserver_connect_database(config, Some(&database))?;
    let (execution, connection_broken) = sqlserver_block_on(sqlserver_execute_on_client(
        &mut client,
        request,
        statements,
        pagination,
        query_timeout,
        on_summary,
        should_cancel,
    ))?;
    if connection_broken {
        drop(client);
    } else {
        sqlserver_block_on(sqlserver_close(client));
    }
    Ok(execution)
}

async fn sqlserver_execute_on_client(
    client: &mut SqlServerClient,
    request: &QueryRequest,
    statements: Vec<String>,
    pagination: Pagination,
    query_timeout: Duration,
    on_summary: &mut dyn FnMut(QueryExecutionSummary),
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<(QueryExecutionResult, bool)> {
    let mut execution = QueryExecutionResult {
        summaries: Vec::new(),
        results: Vec::new(),
        rollback_snapshots: Vec::new(),
    };
    let mut connection_broken = false;
    // SQL Server 没有 `EXPLAIN`；`SET SHOWPLAN_ALL ON` 让后续语句只返回计划。
    // 该开关是会话级且会自动回滚在事务内，因此这里不开显式事务（见下方）。
    let explain_only = request.options.explain_only;
    if explain_only {
        sqlserver_exec_transaction(client, "SET SHOWPLAN_ALL ON").await?;
    }
    // AI 只读入口：语句放进一个显式事务，执行后无条件回滚。
    // SQL Server 没有会话级只读开关，仅靠客户端 AST 分类不足以挡住
    // 未识别的写入语法；服务端回滚是第二道防线。
    let read_only_wrap = request.options.read_only && !explain_only;
    if read_only_wrap {
        sqlserver_exec_transaction(client, "BEGIN TRANSACTION").await?;
    }
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
        match sqlserver_run_batch_cancellable(
            client,
            &statement,
            query_timeout,
            should_cancel,
        )
        .await
        {
            Ok(batch) => {
                if batch.result_sets.is_empty() {
                    // 只读入口出现无结果集的命令，说明策略层漏掉了写语句；
                    // 事务随后会回滚，必须明确报“未提交”，不能声称执行成功。
                    let (success, message) = if read_only_wrap {
                        (false, "只读入口拒绝：该语句不是只读查询，已回滚".to_string())
                    } else {
                        (true, "命令执行完成".to_string())
                    };
                    push_query_summary(
                        &mut execution,
                        QueryExecutionSummary {
                            sql: statement,
                            kind: QueryStatementKind::Command,
                            success,
                            message,
                            returned_rows: 0,
                            affected_rows: 0,
                            elapsed_ms: elapsed_ms(started),
                        },
                        on_summary,
                    );
                } else {
                    let batch_truncated = batch.truncated;
                    for set in batch.result_sets {
                        let returned_rows = set.rows.len() as u64;
                        let mut page = sqlserver_query_page(set.columns, set.rows, pagination);
                        if batch_truncated {
                            page.has_more = true;
                        }
                        push_query_summary(
                            &mut execution,
                            QueryExecutionSummary {
                                sql: statement.clone(),
                                kind: QueryStatementKind::ResultSet,
                                success: true,
                                message: if batch_truncated {
                                    "结果超过 16 MiB，后续行未载入".to_string()
                                } else if page.has_more {
                                    format!(
                                        "返回 {returned_rows} 行，当前仅展示 {} 行（结果已截断）",
                                        page.rows.len()
                                    )
                                } else {
                                    format!("返回 {returned_rows} 行")
                                },
                                returned_rows,
                                affected_rows: 0,
                                elapsed_ms: elapsed_ms(started),
                            },
                            on_summary,
                        );
                        execution.results.push(page);
                    }
                }
            }
            Err(error) => {
                let broken = matches!(
                    error.kind,
                    ErrorKind::Cancelled | ErrorKind::Timeout
                );
                tracing::warn!(
                    target: "fluxdb_connectors",
                    message = %error.message,
                    "SQL Server 执行失败"
                );
                push_query_summary(
                    &mut execution,
                    failed_query_summary(
                        statement,
                        QueryStatementKind::Command,
                        error.message,
                        elapsed_ms(started),
                    ),
                    on_summary,
                );
                if broken {
                    connection_broken = true;
                    break;
                }
                if !request.options.continue_on_error {
                    break;
                }
            }
        }
    }
    if read_only_wrap && !connection_broken {
        // 即使语句自身 BEGIN/COMMIT，外层事务仍会回滚其未提交的部分；
        // 已提交的内部事务无法回滚，因此策略层同时拒绝事务控制语句。
        if let Err(error) =
            sqlserver_exec_transaction(client, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await
        {
            // 未提交的事务会随连接断开由服务端回滚，但这次调用不能返回成功：
            // 「已回滚」是事实陈述，不能只说日志里。
            tracing::warn!(
                target: "fluxdb_connectors",
                message = %error.message,
                "SQL Server 只读事务回滚失败"
            );
            return Err(Error::new(
                ErrorKind::Query,
                format!("SQL Server 只读事务回滚失败：{}", error.message),
            ));
        }
    }
    if explain_only && !connection_broken {
        // 计划已取到；关闭开关，避免连接被复用时后续语句只返回计划。
        if let Err(error) = sqlserver_exec_transaction(client, "SET SHOWPLAN_ALL OFF").await {
            tracing::warn!(
                target: "fluxdb_connectors",
                message = %error.message,
                "SQL Server 关闭 SHOWPLAN 失败"
            );
        }
    }
    Ok((execution, connection_broken))
}

async fn sqlserver_run_batch_cancellable(
    client: &mut SqlServerClient,
    statement: &str,
    query_timeout: Duration,
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<SqlServerBatchResult> {
    let started = std::time::Instant::now();
    let run = sqlserver_run_batch(client, statement);
    tokio::pin!(run);
    loop {
        tokio::select! {
            result = &mut run => return result,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if should_cancel() {
                    return Err(Error::new(
                        ErrorKind::Cancelled,
                        "查询已取消，SQL Server 连接已关闭；执行结果待核实",
                    ));
                }
                if !query_timeout.is_zero() && started.elapsed() >= query_timeout {
                    return Err(Error::new(
                        ErrorKind::Timeout,
                        "SQL Server 查询超时，连接已关闭；执行结果待核实",
                    ));
                }
            }
        }
    }
}

fn sqlserver_query_page(
    columns: Vec<Column>,
    rows: Vec<Row>,
    pagination: Pagination,
) -> DataPage {
    let offset = pagination.offset.min(rows.len() as u64) as usize;
    let capacity = query_execution_row_capacity(pagination.limit);
    let has_more = rows.len().saturating_sub(offset) > capacity;
    let rows = rows.into_iter().skip(offset).take(capacity).collect();
    DataPage {
        columns,
        rows,
        offset: pagination.offset,
        limit: pagination.limit,
        has_more,
    }
}

fn sqlserver_execute_parameterized(
    config: &ConnectionConfig,
    request: &QueryRequest,
    parameters: &[fluxdb_core::SqlParameter],
) -> fluxdb_core::Result<QueryExecutionResult> {
    let statement = request.text.trim().to_string();
    if statement.is_empty() {
        return Err(Error::new(ErrorKind::Query, "查询不能为空"));
    }
    let database = sqlserver_request_database(config, request.database.as_deref())?;
    let mut client = sqlserver_connect_database(config, Some(&database))?;
    let started = std::time::Instant::now();
    let pagination = query_execution_pagination(request.options.page_offset, request.options.page_size);
    let batch = sqlserver_block_on(async {
        let batch = sqlserver_run_query(&mut client, &statement, parameters).await?;
        sqlserver_close(client).await;
        Ok(batch)
    })?;
    let mut summaries = Vec::new();
    let mut results = Vec::new();
    if batch.result_sets.is_empty() {
        summaries.push(QueryExecutionSummary {
            sql: statement,
            kind: QueryStatementKind::Command,
            success: true,
            message: "命令执行完成（参数化绑定）".into(),
            returned_rows: 0,
            affected_rows: 0,
            elapsed_ms: elapsed_ms(started),
        });
    } else {
        for set in batch.result_sets {
            let returned = set.rows.len() as u64;
            let page = sqlserver_query_page(set.columns, set.rows, pagination);
            summaries.push(QueryExecutionSummary {
                sql: statement.clone(),
                kind: QueryStatementKind::ResultSet,
                success: true,
                message: format!("返回 {returned} 行结果表（参数化绑定）"),
                returned_rows: returned,
                affected_rows: 0,
                elapsed_ms: elapsed_ms(started),
            });
            results.push(page);
        }
    }
    Ok(QueryExecutionResult {
        summaries,
        results,
        rollback_snapshots: Vec::new(),
    })
}
