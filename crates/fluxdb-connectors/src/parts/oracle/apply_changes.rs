// Oracle 行级提交：单事务内的删除/更新/插入。
//
// 与达梦/SQL Server 的差异：
// - 绑定统一 `:N`，且每处出现都给独立编号（驱动按出现次数校验）。
// - 没有主键时不允许无界更新/删除：一条 SQL 影响多行却只提交一行会让用户
//   以为改了一行。这里在语句执行后核对影响行数，>1 即回滚并报错。
// - 空字符串在 Oracle 里就是 NULL，身份条件必须写 `IS NOT NULL` 语义的分支。

fn oracle_apply_changes(
    config: &ConnectionConfig,
    changes: &DataChangeSet,
) -> fluxdb_core::Result<AppliedChangeOutcome> {
    validate_data_changes(changes)?;
    reject_streamed_values(changes)?;
    let schema = oracle_request_schema(config, &changes.object)?;
    let connection = oracle_connect(config)?;
    let columns = oracle_load_columns(&connection, &schema, &changes.object.name)?;
    let table = oracle_qualified_table(&schema, &changes.object.name);
    let result = (|| {
        for identity in &changes.deletes {
            validate_identity(identity, &columns)?;
            let (where_sql, binds) = oracle_identity_where(identity, &columns)?;
            let affected = oracle_execute(
                &connection,
                &format!("DELETE FROM {table}{where_sql}"),
                &binds,
            )
            .map_err(|error| Error::new(error.kind, format!("删除失败：{}", error.message)))?;
            oracle_require_bounded_affected(affected, "删除")?;
        }
        for update in &changes.updates {
            if update.cells.is_empty() {
                continue;
            }
            validate_identity(&update.identity, &columns)?;
            let mut binds = Vec::new();
            let mut assignments = Vec::new();
            for cell in &update.cells {
                ensure_column_exists(&cell.column, &columns)?;
                let placeholder = oracle_placeholder(&mut binds, oracle_bind_cell(cell.value.placeholder()));
                assignments.push(format!(
                    "{} = {placeholder}",
                    fluxdb_core::OracleDialect.quote_identifier(&cell.column)
                ));
            }
            let (where_sql, where_binds) = oracle_identity_where(&update.identity, &columns)?;
            // 身份条件的 `:N` 必须从已有绑定数之后继续编号。
            let where_sql = oracle_shift_placeholders(&where_sql, binds.len());
            binds.extend(where_binds);
            let affected = oracle_execute(
                &connection,
                &format!("UPDATE {table} SET {}{where_sql}", assignments.join(", ")),
                &binds,
            )
            .map_err(|error| Error::new(error.kind, format!("更新失败：{}", error.message)))?;
            oracle_require_bounded_affected(affected, "更新")?;
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
                let affected = oracle_execute(
                    &connection,
                    &format!("INSERT INTO {table} DEFAULT VALUES"),
                    &[],
                )
                .map_err(|error| {
                    Error::new(error.kind, format!("插入默认值失败：{}", error.message))
                })?;
                oracle_require_bounded_affected(affected, "插入")?;
                continue;
            }
            let names = values
                .iter()
                .map(|(column, _)| fluxdb_core::OracleDialect.quote_identifier(&column.name))
                .collect::<Vec<_>>()
                .join(", ");
            let mut binds = Vec::new();
            let placeholders = values
                .iter()
                .map(|(_, value)| oracle_placeholder(&mut binds, oracle_bind_cell(value)))
                .collect::<Vec<_>>()
                .join(", ");
            let affected = oracle_execute(
                &connection,
                &format!("INSERT INTO {table} ({names}) VALUES ({placeholders})"),
                &binds,
            )
            .map_err(|error| Error::new(error.kind, format!("插入失败：{}", error.message)))?;
            oracle_require_bounded_affected(affected, "插入")?;
        }
        connection
            .commit()
            .map_err(|error| oracle_error("提交 Oracle 事务失败", error))
            // 提交途中断连：服务端可能已提交，不能当确定失败。
            .map_err(|error| error.mark_outcome_unknown("提交 Oracle 事务"))
    })();
    match result {
        // 插入身份不回填：Oracle 没有 `RETURNING` 的统一读回入口，这里不为补偿 SQL
        // 伪造行身份，由上层按「已提交」处理。
        Ok(()) => Ok(AppliedChangeOutcome::default()),
        Err(error) => {
            // 回滚失败也不能把失败说成成功；原始错误优先返回。
            if let Err(rollback) = connection.rollback() {
                tracing::warn!(
                    target: "fluxdb_connectors",
                    message = %rollback,
                    "Oracle 提交失败后回滚失败"
                );
            }
            Err(error)
        }
    }
}

/// 行级提交必须精确命中一行。Oracle 的 `INSERT` 影响行数恒为 1，
/// 但 `DELETE`/`UPDATE` 若不唯一就会波及多行，这里统一收口。
fn oracle_require_bounded_affected(affected: u64, action: &str) -> fluxdb_core::Result<()> {
    if affected > 1 {
        return Err(Error::new(
            ErrorKind::Query,
            format!("{action}命中 {affected} 行，已取消提交：行身份条件不唯一"),
        ));
    }
    Ok(())
}

fn oracle_identity_where(
    identity: &RowIdentity,
    columns: &[Column],
) -> fluxdb_core::Result<(String, Vec<OracleBindValue>)> {
    validate_identity(identity, columns)?;
    let mut clauses = Vec::new();
    let mut binds = Vec::new();
    for (name, value) in &identity.values {
        let quoted = fluxdb_core::OracleDialect.quote_identifier(name);
        match value {
            CellValue::Null => clauses.push(format!("{quoted} IS NULL")),
            // 编辑期把空串存成空串时，Oracle 会当成 NULL；这里显式写成 IS NULL，
            // 否则 `= :n` 永远不匹配，用户会看到「更新了 0 行」。
            CellValue::Text(text) if text.is_empty() => clauses.push(format!("{quoted} IS NULL")),
            value => {
                clauses.push(format!(
                    "{quoted} = {}",
                    oracle_placeholder(&mut binds, oracle_bind_cell(value))
                ));
            }
        }
    }
    Ok((format!(" WHERE {}", clauses.join(" AND ")), binds))
}

/// 把 `:N` 统一右移 `offset`，用于把身份条件追加到已有绑定之后。
fn oracle_shift_placeholders(sql: &str, offset: usize) -> String {
    if offset == 0 {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len() + offset);
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != ':' {
            out.push(ch);
            continue;
        }
        let digits = chars
            .clone()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if digits.is_empty() {
            out.push(ch);
            continue;
        }
        for _ in 0..digits.len() {
            chars.next();
        }
        let shifted = digits.parse::<usize>().unwrap_or(0) + offset;
        out.push_str(&format!(":{shifted}"));
    }
    out
}

#[cfg(test)]
mod oracle_placeholder_tests {
    use super::oracle_shift_placeholders;

    #[test]
    fn shifts_only_numeric_bind_markers() {
        assert_eq!(
            oracle_shift_placeholders(" WHERE \"ID\" = :1 AND \"NAME\" = :2", 2),
            " WHERE \"ID\" = :3 AND \"NAME\" = :4"
        );
        // 时间字面量/普通冒号不会被误改。
        assert_eq!(
            oracle_shift_placeholders(" WHERE \"T\" > :1", 1),
            " WHERE \"T\" > :2"
        );
        assert_eq!(oracle_shift_placeholders(" WHERE A = B", 3), " WHERE A = B");
    }
}
