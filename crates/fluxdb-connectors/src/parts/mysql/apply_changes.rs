fn mysql_apply_changes(config: &ConnectionConfig, changes: &DataChangeSet) -> fluxdb_core::Result<()> {
    validate_data_changes(changes)?;

    let database = mysql_database_for_path(config, &changes.object)?;
    let options = mysql_connection_url(config)?
        .parse::<MySqlConnectOptions>()
        .map_err(|error| Error::new(ErrorKind::Connection, error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;

    runtime.block_on(async {
        let mut connection = options.connect().await.map_err(mysql_error)?;
        let result = async {
            let columns = mysql_columns(&mut connection, &database, &changes.object.name).await?;
            let mut transaction = connection.begin().await.map_err(mysql_error)?;
            mysql_apply_deletes(&mut transaction, &database, changes, &columns).await?;
            mysql_apply_updates(&mut transaction, &database, changes, &columns).await?;
            mysql_apply_inserts(&mut transaction, &database, changes, &columns).await?;
            transaction.commit().await.map_err(|error| {
                // 服务端回了错误报文 = 事务已被终止，结果是确定的失败；
                // 没有报文（IO/连接断开）= 提交是否生效未知，必须让调用方核实。
                let unknown = !matches!(error, sqlx::Error::Database(_));
                let error = mysql_error(error);
                if unknown {
                    error.mark_outcome_unknown("提交 MySQL 事务")
                } else {
                    error
                }
            })?;
            Ok(())
        }
        .await;
        connection.close().await.map_err(mysql_error)?;
        result
    })
}

async fn mysql_apply_inserts(
    connection: &mut sqlx::Transaction<'_, MySql>,
    database: &str,
    changes: &DataChangeSet,
    columns: &[Column],
) -> fluxdb_core::Result<()> {
    let table_name = mysql_qualified_table(database, &changes.object.name);
    for row in &changes.inserts {
        let insert_values = non_null_insert_values(row, columns)?;
        if insert_values.is_empty() {
            sqlx::query(&format!("INSERT INTO {table_name} () VALUES ()"))
                .execute(&mut **connection)
                .await
                .map_err(mysql_error)?;
            continue;
        }

        let mut builder = QueryBuilder::<MySql>::new(format!("INSERT INTO {table_name} ("));
        push_insert_column_list(&mut builder, &insert_values, |value| fluxdb_core::MySqlDialect.quote_identifier(value));
        builder.push(") VALUES (");
        for (index, (_, value)) in insert_values.iter().enumerate() {
            if index > 0 {
                builder.push(", ");
            }
            push_mysql_bind(&mut builder, *value);
        }
        builder.push(")");
        builder
            .build()
            .execute(&mut **connection)
            .await
            .map_err(mysql_error)?;
    }

    Ok(())
}

async fn mysql_apply_updates(
    connection: &mut sqlx::Transaction<'_, MySql>,
    database: &str,
    changes: &DataChangeSet,
    columns: &[Column],
) -> fluxdb_core::Result<()> {
    let table_name = mysql_qualified_table(database, &changes.object.name);
    for update in &changes.updates {
        if update.cells.is_empty() {
            continue;
        }
        validate_identity(&update.identity, columns)?;

        let mut builder = QueryBuilder::<MySql>::new(format!("UPDATE {table_name} SET "));
        for (index, cell) in update.cells.iter().enumerate() {
            ensure_column_exists(&cell.column, columns)?;
            if index > 0 {
                builder.push(", ");
            }
            builder
                .push(fluxdb_core::MySqlDialect.quote_identifier(&cell.column))
                .push(" = ");
            push_mysql_bind(&mut builder, &cell.value);
        }
        push_mysql_identity_where(&mut builder, &update.identity, columns)?;
        builder
            .build()
            .execute(&mut **connection)
            .await
            .map_err(mysql_error)?;
    }

    Ok(())
}

async fn mysql_apply_deletes(
    connection: &mut sqlx::Transaction<'_, MySql>,
    database: &str,
    changes: &DataChangeSet,
    columns: &[Column],
) -> fluxdb_core::Result<()> {
    let table_name = mysql_qualified_table(database, &changes.object.name);
    for identity in &changes.deletes {
        validate_identity(identity, columns)?;

        let mut builder = QueryBuilder::<MySql>::new(format!("DELETE FROM {table_name}"));
        push_mysql_identity_where(&mut builder, identity, columns)?;
        builder
            .build()
            .execute(&mut **connection)
            .await
            .map_err(mysql_error)?;
    }

    Ok(())
}

