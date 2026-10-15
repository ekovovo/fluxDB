// 自动审批只接受主键等值、常量赋值的单表 UPDATE。
// 在 BEGIN IMMEDIATE 内重新读 schema，拒绝触发器、外键、CHECK、虚拟表与表达式，
// 不把“预估一行”当作已经证明；执行后在 COMMIT 前核对真实影响。
fn bounded_sqlite_update_shape(sql: &str) -> fluxdb_core::Result<(String, String, Vec<String>)> {
    use sqlparser::ast::{
        AssignmentTarget, BinaryOperator, Expr, ObjectNamePart, Statement, TableFactor,
    };
    let reject = || {
        Error::new(
            ErrorKind::Unsupported,
            "删除、结构变更、批量或复杂表达式需要人工审核",
        )
    };
    let statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, sql)
            .map_err(|_| reject())?;
    let [Statement::Update(update)] = statements.as_slice() else {
        return Err(reject());
    };
    let TableFactor::Table {
        name,
        alias: None,
        args: None,
        ..
    } = &update.table.relation
    else {
        return Err(reject());
    };
    let [ObjectNamePart::Identifier(table)] = name.0.as_slice() else {
        return Err(reject());
    };
    if !update.table.joins.is_empty()
        || update.from.is_some()
        || update.returning.is_some()
        || update.output.is_some()
        || update.or.is_some()
        || !update.order_by.is_empty()
        || update.limit.is_some()
        || !update.optimizer_hints.is_empty()
    {
        return Err(reject());
    }
    let Some(Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    }) = &update.selection
    else {
        return Err(reject());
    };
    let (Expr::Identifier(key), Expr::Value(_)) = (left.as_ref(), right.as_ref()) else {
        return Err(reject());
    };
    let mut columns = Vec::new();
    for assignment in &update.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            return Err(reject());
        };
        let [ObjectNamePart::Identifier(column)] = name.0.as_slice() else {
            return Err(reject());
        };
        if !matches!(assignment.value, Expr::Value(_))
            || column.value.eq_ignore_ascii_case(&key.value)
        {
            return Err(reject());
        }
        columns.push(column.value.clone());
    }
    if columns.is_empty() {
        return Err(reject());
    }
    Ok((table.value.clone(), key.value.clone(), columns))
}

async fn verify_bounded_sqlite_update(
    connection: &mut sqlx::SqliteConnection,
    sql: &str,
) -> fluxdb_core::Result<String> {
    let (table, key, columns) = bounded_sqlite_update_shape(sql)?;
    let reject = || {
        Error::new(
            ErrorKind::Unsupported,
            "间接副作用、敏感对象或唯一行边界无法核实，需要人工审核",
        )
    };
    let schema: Option<(String, String)> =
        sqlx::query_as("SELECT type,sql FROM sqlite_schema WHERE name=? COLLATE NOCASE")
            .bind(&table)
            .fetch_optional(&mut *connection)
            .await
            .map_err(sqlite_error)?;
    let Some((kind, definition)) = schema else {
        return Err(reject());
    };
    let definition = definition.to_ascii_lowercase();
    // 约束表达式可含自定义函数，虚拟表可调用扩展；都不能视为普通可回滚更新。
    if kind != "table"
        || definition.contains("virtual")
        || definition.contains("check")
        || definition.contains("generated")
        || table.to_ascii_lowercase().starts_with("sqlite_")
    {
        return Err(reject());
    }
    let triggers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE type='trigger'")
            .fetch_one(&mut *connection)
            .await
            .map_err(sqlite_error)?;
    if triggers != 0 {
        return Err(reject());
    }
    let foreign_keys: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema s JOIN pragma_foreign_key_list(s.name) f WHERE s.type='table'",
    ).fetch_one(&mut *connection).await.map_err(sqlite_error)?;
    if foreign_keys != 0 {
        return Err(reject());
    }
    let primary_keys: Vec<(String, String)> =
        sqlx::query_as("SELECT name,type FROM pragma_table_info(?) WHERE pk>0")
            .bind(&table)
            .fetch_all(&mut *connection)
            .await
            .map_err(sqlite_error)?;
    // INTEGER PRIMARY KEY 是 SQLite 的唯一 rowid；普通复合/可 NULL 主键不作此保证。
    if primary_keys.len() != 1
        || !primary_keys[0].0.eq_ignore_ascii_case(&key)
        || !primary_keys[0].1.eq_ignore_ascii_case("INTEGER")
    {
        return Err(reject());
    }
    // 唯一约束的 REPLACE 冲突策略可能隐式删除另一行，不能依赖 affected_rows 发现。
    if definition.contains("replace") || definition.contains("collate") {
        return Err(reject());
    }
    let _ = columns;
    Ok(r#"{"operation":"update","database_kind":"sqlite","predicate":"integer_primary_key_equals_value","max_affected_rows":1,"assignments":"literal_or_bound_values","triggers":false,"foreign_keys":false,"rollback_verified":true,"business_values":"omitted","object_names":"omitted"}"#.into())
}

fn sqlite_bounded_update(
    config: &ConnectionConfig,
    request: &fluxdb_core::ParameterizedQueryRequest,
    execute: bool,
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<(String, QueryExecutionResult)> {
    if matches!(
        &config.endpoint,
        Endpoint::SqliteFile {
            read_only: true,
            ..
        }
    ) || config.options.get("read_only").is_some_and(|v| v == "true")
    {
        return Err(Error::new(ErrorKind::Permission, "只读连接禁止修改"));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?;
    runtime.block_on(async {
        let mut connection = sqlite_connect(config).await?;
        sqlx::query("PRAGMA busy_timeout=5000")
            .execute(&mut connection)
            .await
            .map_err(sqlite_error)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut connection)
            .await
            .map_err(sqlite_error)?;
        let outcome = async {
            if should_cancel() {
                return Err(Error::new(ErrorKind::Query, "已取消"));
            }
            let facts =
                verify_bounded_sqlite_update(&mut connection, &request.request.text).await?;
            let mut result = QueryExecutionResult {
                summaries: vec![],
                results: vec![],
                rollback_snapshots: vec![],
            };
            if execute {
                let mut query = sqlx::query(&request.request.text);
                for value in &request.parameters {
                    query = sqlite_bind_parameter(query, value)?;
                }
                let affected = query
                    .execute(&mut connection)
                    .await
                    .map_err(sqlite_error)?
                    .rows_affected();
                if affected > 1 || should_cancel() {
                    return Err(Error::new(
                        ErrorKind::Query,
                        "自动审批写入超限或已取消，事务已回滚",
                    ));
                }
                result.summaries.push(QueryExecutionSummary {
                    sql: request.request.text.clone(),
                    kind: QueryStatementKind::Command,
                    success: true,
                    message: format!("有界提交，影响 {affected} 行"),
                    returned_rows: 0,
                    affected_rows: affected,
                    elapsed_ms: 0,
                });
                sqlx::query("COMMIT")
                    .execute(&mut connection)
                    .await
                    .map_err(|_| {
                        Error::new(ErrorKind::Internal, "有界写入提交结果未知，禁止自动重试")
                    })?;
            } else {
                sqlx::query("ROLLBACK")
                    .execute(&mut connection)
                    .await
                    .map_err(sqlite_error)?;
            }
            Ok((facts, result))
        }
        .await;
        if outcome.is_err() {
            let rollback = sqlx::query("ROLLBACK").execute(&mut connection).await;
            if execute && rollback.is_err() {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "有界写入回滚结果无法核实，禁止自动重试",
                ));
            }
        }
        // COMMIT 后关闭失败不能诱发重放；连接 Drop 仍会回收资源。
        let _ = connection.close().await;
        outcome
    })
}
