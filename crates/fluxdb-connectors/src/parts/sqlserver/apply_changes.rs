fn sqlserver_apply_changes(
    config: &ConnectionConfig,
    changes: &DataChangeSet,
) -> fluxdb_core::Result<AppliedChangeOutcome> {
    validate_data_changes(changes)?;
    let schema = sqlserver_request_schema(config, &changes.object)?;
    let database = sqlserver_request_database(config, changes.object.database.as_deref())?;
    let mut client = sqlserver_connect_database(config, Some(&database))?;
    let result = sqlserver_block_on(async {
        let columns = sqlserver_load_columns(&mut client, &schema, &changes.object.name)?;
        let table = sqlserver_qualified_table(&schema, &changes.object.name);
        sqlserver_exec_transaction(&mut client, "BEGIN TRANSACTION").await?;
        let result = sqlserver_apply_changes_in_transaction(
            &mut client,
            changes,
            &columns,
            &table,
        )
        .await;
        match result {
            Ok(outcome) => {
                sqlserver_exec_transaction(&mut client, "COMMIT TRANSACTION").await?;
                Ok(outcome)
            }
            Err(error) => {
                // 回滚失败不能覆盖原错误，但需要保留事务可能仍打开的事实。
                if let Err(rollback) = sqlserver_exec_transaction(&mut client, "ROLLBACK TRANSACTION").await {
                    tracing::warn!(
                        target: "fluxdb_connectors",
                        message = %rollback.message,
                        "SQL Server 回滚失败，连接将丢弃"
                    );
                }
                Err(error)
            }
        }
    });
    sqlserver_block_on(sqlserver_close(client));
    result
}

async fn sqlserver_exec_transaction(
    client: &mut SqlServerClient,
    sql: &str,
) -> fluxdb_core::Result<()> {
    // 事务控制语句必须走 batch（simple_query）。`Client::execute` 使用 sp_executesql，
    // 在其中执行 BEGIN/COMMIT 会让事务计数与连接级计数不一致（错误 266），
    // 提交时被服务端判定为未配对的事务。
    let mut stream = client.simple_query(sql).await.map_err(sqlserver_driver_error)?;
    while stream.try_next().await.map_err(sqlserver_driver_error)?.is_some() {}
    Ok(())
}

async fn sqlserver_apply_changes_in_transaction(
    client: &mut SqlServerClient,
    changes: &DataChangeSet,
    columns: &[SqlServerColumnInfo],
    table: &str,
) -> fluxdb_core::Result<AppliedChangeOutcome> {
    for identity in &changes.deletes {
        sqlserver_delete_row(client, identity, columns, table).await?;
    }
    for update in &changes.updates {
        sqlserver_update_row(client, update, columns, table).await?;
    }
    for (index, row) in changes.inserts.iter().enumerate() {
        sqlserver_insert_row(client, row, changes.insert_intents_for(index), columns, table).await?;
    }
    Ok(AppliedChangeOutcome::default())
}

async fn sqlserver_delete_row(
    client: &mut SqlServerClient,
    identity: &RowIdentity,
    columns: &[SqlServerColumnInfo],
    table: &str,
) -> fluxdb_core::Result<()> {
    let (where_sql, values) = sqlserver_identity_predicate(identity, columns)?;
    if !columns.iter().any(|column| column.primary_key) && !identity.values.is_empty() {
        sqlserver_require_single_row(client, table, &where_sql, &values).await?;
    }
    let sql = format!("DELETE FROM {table}{where_sql}");
    let affected = sqlserver_execute_bound(client, &sql, &values)
        .await
        .map_err(|error| Error::new(error.kind, format!("删除失败：{}", error.message)))?;
    if affected > 1 {
        return Err(Error::new(
            ErrorKind::Query,
            format!("删除影响了 {affected} 行，事务已回滚"),
        ));
    }
    Ok(())
}

async fn sqlserver_update_row(
    client: &mut SqlServerClient,
    update: &RowUpdate,
    columns: &[SqlServerColumnInfo],
    table: &str,
) -> fluxdb_core::Result<()> {
    if update.cells.is_empty() {
        return Ok(());
    }
    let mut assignments = Vec::new();
    let mut values = Vec::new();
    for cell in &update.cells {
        let column = columns
            .iter()
            .find(|column| column.name == cell.column)
            .ok_or_else(|| Error::new(ErrorKind::Query, format!("字段不存在或已变更：{}", cell.column)))?;
        if column.identity || column.generated || column.computed {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("字段 {} 由数据库生成，不能更新", column.name),
            ));
        }
        assignments.push(format!(
            "{} = {}",
            fluxdb_core::SqlServerDialect.quote_identifier(&cell.column),
            sqlserver_placeholder(values.len() + 1)
        ));
        values.push(cell.value.clone());
    }
    // 无主键时需要先用完整原值确认唯一行。该查询独立绑定，占位符从 @P1 开始。
    if !columns.iter().any(|column| column.primary_key) && !update.identity.values.is_empty() {
        let (predicate, predicate_values) =
            sqlserver_identity_predicate_from(&update.identity, columns, 0)?;
        sqlserver_require_single_row(client, table, &predicate, &predicate_values).await?;
    }
    // SET 已经占用了 @P1..@Pn，WHERE 的占位符必须从下一个序号继续，
    // 否则同一行谓词会绑定到被更新的值上。
    let (where_sql, where_values) =
        sqlserver_identity_predicate_from(&update.identity, columns, values.len())?;
    values.extend(where_values);
    let sql = format!("UPDATE {table} SET {}{where_sql}", assignments.join(", "));
    let affected = sqlserver_execute_bound(client, &sql, &values)
        .await
        .map_err(|error| Error::new(error.kind, format!("更新失败：{}", error.message)))?;
    if affected > 1 {
        return Err(Error::new(
            ErrorKind::Query,
            format!("更新影响了 {affected} 行，事务已回滚"),
        ));
    }
    Ok(())
}

async fn sqlserver_insert_row(
    client: &mut SqlServerClient,
    row: &Row,
    intents: Option<&[WriteValue]>,
    columns: &[SqlServerColumnInfo],
    table: &str,
) -> fluxdb_core::Result<()> {
    let mut values = Vec::<(String, CellValue)>::new();
    match intents {
        Some(intents) if intents.len() == columns.len() => {
            for (column, intent) in columns.iter().zip(intents) {
                if column.generated || column.computed {
                    continue;
                }
                match intent {
                    WriteValue::Default => {}
                    WriteValue::Null | WriteValue::Value(_) if column.identity => {}
                    WriteValue::Null => values.push((column.name.clone(), CellValue::Null)),
                    WriteValue::Value(value) => {
                        values.push((column.name.clone(), value.clone()));
                    }
                }
            }
        }
        _ => {
            let page_columns = sqlserver_page_columns(columns);
            let insert_values = non_null_insert_values(row, &page_columns)?;
            for (column, value) in insert_values {
                let Some(info) = columns.iter().find(|info| info.name == column.name) else {
                    continue;
                };
                if info.generated || info.computed {
                    continue;
                }
                values.push((info.name.clone(), value.clone()));
            }
        }
    }
    if values.is_empty() {
        let sql = format!("INSERT INTO {table} DEFAULT VALUES");
        sqlserver_execute_bound(client, &sql, &[])
            .await
            .map_err(|error| Error::new(error.kind, format!("插入失败：{}", error.message)))?;
        return Ok(());
    }
    let names = values
        .iter()
        .map(|(name, _)| fluxdb_core::SqlServerDialect.quote_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=values.len())
        .map(sqlserver_placeholder)
        .collect::<Vec<_>>()
        .join(", ");
    let params = values.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>();
    let sql = format!("INSERT INTO {table} ({names}) VALUES ({placeholders})");
    sqlserver_execute_bound(client, &sql, &params)
        .await
        .map_err(|error| Error::new(error.kind, format!("插入失败：{}", error.message)))?;
    Ok(())
}

fn sqlserver_page_columns(columns: &[SqlServerColumnInfo]) -> Vec<Column> {
    columns
        .iter()
        .map(|column| Column {
            name: column.name.clone(),
            type_name: Some(column.data_type.clone()),
            nullable: column.nullable,
            primary_key: column.primary_key,
            comment: column.comment.clone(),
        })
        .collect()
}

fn sqlserver_identity_predicate(
    identity: &RowIdentity,
    columns: &[SqlServerColumnInfo],
) -> fluxdb_core::Result<(String, Vec<CellValue>)> {
    sqlserver_identity_predicate_from(identity, columns, 0)
}

/// `parameter_offset` 是调用方已经占用的参数个数，谓词占位符从其后继续编号。
fn sqlserver_identity_predicate_from(
    identity: &RowIdentity,
    columns: &[SqlServerColumnInfo],
    parameter_offset: usize,
) -> fluxdb_core::Result<(String, Vec<CellValue>)> {
    if identity.values.is_empty() {
        return Err(Error::new(ErrorKind::Query, "缺少行身份条件，已取消提交"));
    }
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    for (name, value) in &identity.values {
        if !columns.iter().any(|column| column.name == *name) {
            return Err(Error::new(
                ErrorKind::Query,
                format!("字段不存在或已变更：{name}"),
            ));
        }
        let quoted = fluxdb_core::SqlServerDialect.quote_identifier(name);
        if matches!(value, CellValue::Null) {
            clauses.push(format!("{quoted} IS NULL"));
        } else {
            clauses.push(format!(
                "{quoted} = {}",
                sqlserver_placeholder(parameter_offset + values.len() + 1)
            ));
            values.push(value.clone());
        }
    }
    Ok((format!(" WHERE {}", clauses.join(" AND ")), values))
}

async fn sqlserver_require_single_row(
    client: &mut SqlServerClient,
    table: &str,
    where_sql: &str,
    values: &[CellValue],
) -> fluxdb_core::Result<()> {
    let sql = format!("SELECT COUNT_BIG(*) FROM {table}{where_sql}");
    let batch = sqlserver_run_query_bound(client, &sql, values).await?;
    let count = batch
        .result_sets
        .first()
        .and_then(|set| set.rows.first())
        .and_then(|row| row.values.first())
        .and_then(cell_i64)
        .unwrap_or(0);
    if count != 1 {
        return Err(Error::new(
            ErrorKind::Query,
            "无主键行的原值不能唯一确定一行，已取消提交；请添加主键或唯一索引",
        ));
    }
    Ok(())
}

async fn sqlserver_run_query_bound(
    client: &mut SqlServerClient,
    sql: &str,
    values: &[CellValue],
) -> fluxdb_core::Result<SqlServerBatchResult> {
    let query = sqlserver_bound_query(sql, values)?;
    let mut stream = query.query(client).await.map_err(sqlserver_driver_error)?;
    let mut result = SqlServerBatchResult::default();
    while let Some(item) = stream.try_next().await.map_err(sqlserver_driver_error)? {
        match item {
            tiberius::QueryItem::Metadata(meta) => result.result_sets.push(SqlServerResultSet {
                columns: meta.columns().iter().map(sqlserver_query_column).collect(),
                rows: Vec::new(),
            }),
            tiberius::QueryItem::Row(row) => {
                if let Some(set) = result.result_sets.last_mut() {
                    set.rows.push(sqlserver_row(row));
                }
            }
        }
    }
    Ok(result)
}

async fn sqlserver_execute_bound(
    client: &mut SqlServerClient,
    sql: &str,
    values: &[CellValue],
) -> fluxdb_core::Result<u64> {
    let query = sqlserver_bound_query(sql, values)?;
    query
        .execute(client)
        .await
        .map(|result| result.total())
        .map_err(sqlserver_driver_error)
}

/// identity 显式写入必须在同一会话内临时开启 `IDENTITY_INSERT`。首版不自动写
/// identity（插入意图里 identity 一律省略），避免用户误以为可指定自增值。
#[allow(dead_code)]
async fn sqlserver_with_identity_insert(
    client: &mut SqlServerClient,
    table: &str,
    body: impl std::future::Future<Output = fluxdb_core::Result<()>>,
) -> fluxdb_core::Result<()> {
    sqlserver_exec_transaction(client, &format!("SET IDENTITY_INSERT {table} ON")).await?;
    let result = body.await;
    let off = sqlserver_exec_transaction(client, &format!("SET IDENTITY_INSERT {table} OFF")).await;
    result.and(off)
}

