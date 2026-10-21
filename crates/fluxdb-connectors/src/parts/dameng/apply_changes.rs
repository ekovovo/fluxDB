fn dm_apply_changes(
    config: &ConnectionConfig,
    changes: &DataChangeSet,
) -> fluxdb_core::Result<AppliedChangeOutcome> {
    validate_data_changes(changes)?;
    let schema = dm_request_schema(config, &changes.object)?;
    let mut client = dm_connect(config)?;
    let columns = dm_load_columns(&mut client, &schema, &changes.object.name)?;
    let table = dm_qualified_table(&schema, &changes.object.name);
    client
        .begin()
        .map_err(|error| dm_error("开启达梦事务失败", error))?;
    let result = (|| {
        for identity in &changes.deletes {
            validate_identity(identity, &columns)?;
            let (where_sql, params) = dm_identity_where(identity, &columns)?;
            dm_execute(
                &mut client,
                &format!("DELETE FROM {table}{where_sql}"),
                &params,
            )
            .map_err(|error| Error::new(error.kind, format!("删除失败：{}", error.message)))?;
        }
        for update in &changes.updates {
            if update.cells.is_empty() {
                continue;
            }
            validate_identity(&update.identity, &columns)?;
            let mut params = Vec::new();
            let assignments = update
                .cells
                .iter()
                .map(|cell| {
                    ensure_column_exists(&cell.column, &columns)?;
                    params.push(dm_param(&cell.value));
                    Ok(format!(
                        "{} = ?",
                        fluxdb_core::DamengDialect.quote_identifier(&cell.column)
                    ))
                })
                .collect::<fluxdb_core::Result<Vec<_>>>()?;
            let (where_sql, where_params) = dm_identity_where(&update.identity, &columns)?;
            params.extend(where_params);
            dm_execute(
                &mut client,
                &format!("UPDATE {table} SET {}{where_sql}", assignments.join(", ")),
                &params,
            )
            .map_err(|error| Error::new(error.kind, format!("更新失败：{}", error.message)))?;
        }
        for (index, row) in changes.inserts.iter().enumerate() {
            let intents = changes.insert_intents_for(index);
            let values = match intents {
                Some(intents) if intents.len() == columns.len() => columns
                    .iter()
                    .zip(intents)
                    .filter_map(|(column, intent)| match intent {
                        WriteValue::Default => None,
                        WriteValue::Null => Some((column, CellValue::Null)),
                        WriteValue::Value(value) => Some((column, value.clone())),
                    })
                    .collect::<Vec<_>>(),
                _ => non_null_insert_values(row, &columns)?
                    .into_iter()
                    .map(|(column, value)| (column, value.clone()))
                    .collect(),
            };
            if values.is_empty() {
                dm_execute(&mut client, &format!("INSERT INTO {table} DEFAULT VALUES"), &[])
                    .map_err(|error| {
                        Error::new(error.kind, format!("插入默认值失败：{}", error.message))
                    })?;
                continue;
            }
            let names = values
                .iter()
                .map(|(column, _)| fluxdb_core::DamengDialect.quote_identifier(&column.name))
                .collect::<Vec<_>>()
                .join(", ");
            let placeholders = std::iter::repeat_n("?", values.len())
                .collect::<Vec<_>>()
                .join(", ");
            let params = values.iter().map(|(_, value)| dm_param(value)).collect::<Vec<_>>();
            dm_execute(
                &mut client,
                &format!("INSERT INTO {table} ({names}) VALUES ({placeholders})"),
                &params,
            )
            .map_err(|error| Error::new(error.kind, format!("插入失败：{}", error.message)))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if let Err(error) = client.commit().map_err(|error| dm_error("提交达梦事务失败", error)) {
                let _ = client.rollback();
                client.close();
                return Err(error);
            }
            client.close();
            Ok(AppliedChangeOutcome::default())
        }
        Err(error) => {
            let _ = client.rollback();
            client.close();
            Err(error)
        }
    }
}

fn dm_identity_where(
    identity: &RowIdentity,
    columns: &[Column],
) -> fluxdb_core::Result<(String, Vec<Param>)> {
    validate_identity(identity, columns)?;
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for (name, value) in &identity.values {
        let quoted = fluxdb_core::DamengDialect.quote_identifier(name);
        if matches!(value, CellValue::Null) {
            clauses.push(format!("{quoted} IS NULL"));
        } else {
            clauses.push(format!("{quoted} = ?"));
            params.push(dm_param(value));
        }
    }
    Ok((format!(" WHERE {}", clauses.join(" AND ")), params))
}
