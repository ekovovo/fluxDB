fn validate_data_changes(changes: &DataChangeSet) -> fluxdb_core::Result<()> {
    if changes.object.kind != ObjectKind::Table {
        return Err(Error::new(ErrorKind::Unsupported, "仅表对象支持数据提交"));
    }

    if changes.is_empty() {
        return Err(Error::new(ErrorKind::Query, "没有需要提交的更改"));
    }

    reject_truncated_values(changes)
}

/// 拒绝提交任何只加载了预览的值。
///
/// 预览是片段，写回去等于用片段覆盖完整数据，且不可逆。所有连接器的写入口都先经过
/// [`validate_data_changes`]，因此这一处校验覆盖 MySQL / SQLite / PostgreSQL / 达梦 /
/// SQL Server / Oracle 的全部写入路径。完整值应由上层先按需加载（详情面板）再提交。
fn reject_truncated_values(changes: &DataChangeSet) -> fluxdb_core::Result<()> {
    let truncated_cell = |value: &CellValue| value.is_truncated();
    let in_insert = changes
        .inserts
        .iter()
        .any(|row| row.values.iter().any(truncated_cell));
    let in_update = changes
        .updates
        .iter()
        .any(|update| update.cells.iter().any(|cell| truncated_cell(&cell.value)));
    if in_insert || in_update {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "提交包含只加载了预览的文本（截断的 CLOB），请先加载完整值再保存",
        ));
    }
    Ok(())
}

fn validate_identity(identity: &fluxdb_core::RowIdentity, columns: &[Column]) -> fluxdb_core::Result<()> {
    if identity.values.is_empty() {
        return Err(Error::new(ErrorKind::Query, "缺少行身份条件，已取消提交"));
    }

    for column in identity.values.keys() {
        ensure_column_exists(column, columns)?;
    }

    Ok(())
}

fn ensure_column_exists(column: &str, columns: &[Column]) -> fluxdb_core::Result<()> {
    if columns.iter().any(|existing| existing.name == column) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Query,
            format!("字段不存在或已变更：{column}"),
        ))
    }
}

fn non_null_insert_values<'a>(
    row: &'a Row,
    columns: &'a [Column],
) -> fluxdb_core::Result<Vec<(&'a Column, &'a CellValue)>> {
    if row.values.len() != columns.len() {
        return Err(Error::new(
            ErrorKind::Query,
            "新增行字段数量和当前表结构不一致",
        ));
    }

    Ok(columns
        .iter()
        .zip(row.values.iter())
        .filter(|(_, value)| !matches!(value, CellValue::Null))
        .collect())
}

fn push_insert_column_list<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    values: &[(&Column, &CellValue)],
    quote_identifier: fn(&str) -> String,
) where
    DB: sqlx::Database,
{
    for (index, (column, _)) in values.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        builder.push(quote_identifier(&column.name));
    }
}

fn push_mysql_bind(builder: &mut QueryBuilder<'_, MySql>, value: &CellValue) {
    match value {
        CellValue::Null => {
            builder.push_bind(Option::<String>::None);
        }
        CellValue::Bool(value) => {
            builder.push_bind(*value);
        }
        CellValue::I64(value) => {
            builder.push_bind(*value);
        }
        CellValue::F64(value) => {
            builder.push_bind(*value);
        }
        CellValue::Text(value) | CellValue::Json(value) => {
            builder.push_bind(value.clone());
        }
        // 写入路径已在 `validate_data_changes` 拒绝截断值；这里只会被筛选条件用到，
        // 按可见预览比较即可，不会落库。
        CellValue::TruncatedText { preview, .. } => {
            builder.push_bind(preview.clone());
        }
        CellValue::Bytes(value) => {
            builder.push_bind(value.clone());
        }
        CellValue::BinarySummary(_) => {
            builder.push_bind(Option::<Vec<u8>>::None);
        }
    }
}

fn push_sqlite_bind(builder: &mut QueryBuilder<'_, Sqlite>, value: &CellValue) {
    match value {
        CellValue::Null => {
            builder.push_bind(Option::<String>::None);
        }
        CellValue::Bool(value) => {
            builder.push_bind(*value);
        }
        CellValue::I64(value) => {
            builder.push_bind(*value);
        }
        CellValue::F64(value) => {
            builder.push_bind(*value);
        }
        CellValue::Text(value) | CellValue::Json(value) => {
            builder.push_bind(value.clone());
        }
        // 同 `push_mysql_bind`：写入前已拒绝截断值，此处仅供筛选比较。
        CellValue::TruncatedText { preview, .. } => {
            builder.push_bind(preview.clone());
        }
        CellValue::Bytes(value) => {
            builder.push_bind(value.clone());
        }
        CellValue::BinarySummary(_) => {
            builder.push_bind(Option::<Vec<u8>>::None);
        }
    }
}

fn push_mysql_identity_where(
    builder: &mut QueryBuilder<'_, MySql>,
    identity: &fluxdb_core::RowIdentity,
    columns: &[Column],
) -> fluxdb_core::Result<()> {
    push_identity_where(
        builder,
        identity,
        columns,
        |value| fluxdb_core::MySqlDialect.quote_identifier(value),
        push_mysql_bind,
    )
}

fn push_sqlite_identity_where(
    builder: &mut QueryBuilder<'_, Sqlite>,
    identity: &fluxdb_core::RowIdentity,
    columns: &[Column],
) -> fluxdb_core::Result<()> {
    push_identity_where(
        builder,
        identity,
        columns,
        |value| fluxdb_core::SqliteDialect.quote_identifier(value),
        push_sqlite_bind,
    )
}

fn push_identity_where<DB>(
    builder: &mut QueryBuilder<'_, DB>,
    identity: &fluxdb_core::RowIdentity,
    columns: &[Column],
    quote_identifier: fn(&str) -> String,
    push_bind: fn(&mut QueryBuilder<'_, DB>, &CellValue),
) -> fluxdb_core::Result<()>
where
    DB: sqlx::Database,
{
    validate_identity(identity, columns)?;
    builder.push(" WHERE ");

    for (index, (column, value)) in identity.values.iter().enumerate() {
        if index > 0 {
            builder.push(" AND ");
        }
        builder.push(quote_identifier(column));
        if matches!(value, CellValue::Null) {
            builder.push(" IS NULL");
        } else {
            builder.push(" = ");
            push_bind(builder, value);
        }
    }

    Ok(())
}

fn mysql_create_database_sql(request: &CreateDatabaseRequest) -> fluxdb_core::Result<String> {
    let database = request.name.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "数据库名称不能为空"));
    }

    let charset = request.charset.trim();
    if charset.is_empty() {
        return Err(Error::new(ErrorKind::Query, "字符集不能为空"));
    }
    if !is_mysql_option_name(charset) {
        return Err(Error::new(ErrorKind::Query, "字符集名称不合法"));
    }

    let collation = request.collation.trim();
    if collation.is_empty() {
        return Err(Error::new(ErrorKind::Query, "排序规则不能为空"));
    }
    if !is_mysql_option_name(collation) {
        return Err(Error::new(ErrorKind::Query, "排序规则名称不合法"));
    }

    Ok(format!(
        "CREATE DATABASE {} DEFAULT CHARACTER SET {} COLLATE {}",
        fluxdb_core::MySqlDialect.quote_identifier(database),
        charset,
        collation
    ))
}

fn mysql_delete_database_sql(database: &str) -> fluxdb_core::Result<String> {
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "数据库名称不能为空"));
    }

    Ok(format!("DROP DATABASE {}", fluxdb_core::MySqlDialect.quote_identifier(database)))
}

fn is_mysql_option_name(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn mysql_create_database(
    config: &ConnectionConfig,
    request: &CreateDatabaseRequest,
) -> fluxdb_core::Result<()> {
    if !is_mysql_protocol_kind(config.kind) {
        return Err(Error::new(ErrorKind::Connection, "连接类型不支持新建数据库"));
    }
    if config.id != request.connection_id {
        return Err(Error::new(ErrorKind::Connection, "连接不匹配"));
    }

    let sql = mysql_create_database_sql(request)?;
    let mut server_config = config.clone();
    if let Endpoint::Tcp { database, .. } = &mut server_config.endpoint {
        *database = None;
    }
    let url = mysql_connection_url(&server_config)?;
    let options = url
        .parse::<MySqlConnectOptions>()
        .map_err(|error| Error::new(ErrorKind::Connection, error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;

    runtime.block_on(async {
        let connect = tokio::time::timeout(Duration::from_secs(5), options.connect()).await;
        let mut connection = match connect {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => return Err(mysql_error(error)),
            Err(_) => return Err(Error::new(ErrorKind::Connection, "连接超时")),
        };

        sqlx::query(&sql)
            .execute(&mut connection)
            .await
            .map_err(mysql_error)?;
        connection.close().await.map_err(mysql_error)
    })
}

fn mysql_delete_database(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
) -> fluxdb_core::Result<()> {
    if !is_mysql_protocol_kind(config.kind) {
        return Err(Error::new(ErrorKind::Connection, "连接类型不支持删除数据库"));
    }
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "连接不匹配"));
    }

    let sql = mysql_delete_database_sql(database)?;
    let mut server_config = config.clone();
    if let Endpoint::Tcp { database, .. } = &mut server_config.endpoint {
        *database = None;
    }
    let url = mysql_connection_url(&server_config)?;
    let options = url
        .parse::<MySqlConnectOptions>()
        .map_err(|error| Error::new(ErrorKind::Connection, error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;

    runtime.block_on(async {
        let connect = tokio::time::timeout(Duration::from_secs(5), options.connect()).await;
        let mut connection = match connect {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => return Err(mysql_error(error)),
            Err(_) => return Err(Error::new(ErrorKind::Connection, "连接超时")),
        };

        sqlx::query(&sql)
            .execute(&mut connection)
            .await
            .map_err(mysql_error)?;
        connection.close().await.map_err(mysql_error)
    })
}

fn sqlite_quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod truncated_write_guard_tests {
    use super::reject_truncated_values;
    use fluxdb_core::{CellUpdate, CellValue, DataChangeSet, ObjectKind, ObjectPath, Row, RowIdentity, RowUpdate};
    use std::collections::BTreeMap;

    fn changes() -> DataChangeSet {
        DataChangeSet {
            object: ObjectPath {
                connection_id: fluxdb_core::ConnectionId(1),
                database: None,
                schema: None,
                name: "CLOB_SAMPLES".to_string(),
                kind: ObjectKind::Table,
            },
            inserts: Vec::new(),
            updates: Vec::new(),
            deletes: Vec::new(),
            insert_intents: None,
        }
    }

    fn truncated() -> CellValue {
        CellValue::TruncatedText {
            preview: "前缀".to_string(),
            total_chars: 100_000,
        }
    }

    fn identity() -> RowIdentity {
        RowIdentity {
            values: BTreeMap::from([("ID".to_string(), CellValue::I64(1))]),
        }
    }

    /// 更新里带预览必须被拒绝：所有连接器写入口共用这一处校验。
    #[test]
    fn rejects_truncated_value_in_update() {
        let mut set = changes();
        set.updates.push(RowUpdate {
            identity: identity(),
            cells: vec![CellUpdate {
                column: "NOTE".to_string(),
                value: truncated(),
            }],
        });
        assert!(reject_truncated_values(&set).is_err());
    }

    #[test]
    fn rejects_truncated_value_in_insert() {
        let mut set = changes();
        set.inserts.push(Row {
            values: vec![CellValue::I64(1), truncated()],
        });
        assert!(reject_truncated_values(&set).is_err());
    }

    /// 完整文本（含恰好等于预览上限的内容）必须照常放行，不能误伤正常写入。
    #[test]
    fn allows_complete_text() {
        let mut set = changes();
        set.updates.push(RowUpdate {
            identity: identity(),
            cells: vec![CellUpdate {
                column: "NOTE".to_string(),
                value: CellValue::Text("完整内容".to_string()),
            }],
        });
        assert!(reject_truncated_values(&set).is_ok());
    }
}
