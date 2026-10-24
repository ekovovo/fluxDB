/// 把 UI 的筛选模型转换为 T-SQL 谓词，参数一律追加到 `parameters`
/// 并按 `@P1..@Pn` 顺序绑定。标识符只接受已知列，避免把表用户输入当 SQL。
fn sqlserver_where_clause(
    filters: &[FilterSpec],
    columns: &[Column],
    alias: Option<&str>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
) -> fluxdb_core::Result<String> {
    let mut clauses = Vec::new();
    for filter in filters.iter().filter(|filter| filter.enabled) {
        if !data_filter_clause_is_pushable(filter) {
            return Err(Error::new(ErrorKind::Query, "过滤条件缺少必要的比较值"));
        }
        let column = columns.iter().find(|column| column.name == filter.field).ok_or_else(|| {
            Error::new(
                ErrorKind::Query,
                format!("过滤引用了不存在的列: {}", filter.field),
            )
        })?;
        sqlserver_push_filter(&mut clauses, parameters, filter, column, alias);
    }
    Ok(if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    })
}

fn sqlserver_expression_where_clause(
    expression: &fluxdb_core::FilterExpression,
    columns: &[Column],
    alias: Option<&str>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
) -> fluxdb_core::Result<String> {
    let sql = sqlserver_expression_sql(expression, columns, alias, parameters)?;
    Ok(if sql.trim().is_empty() {
        String::new()
    } else {
        format!(" WHERE {sql}")
    })
}

fn sqlserver_expression_sql(
    expression: &fluxdb_core::FilterExpression,
    columns: &[Column],
    alias: Option<&str>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
) -> fluxdb_core::Result<String> {
    use fluxdb_core::FilterExpression;
    Ok(match expression {
        FilterExpression::Condition(condition) => {
            let column = columns
                .iter()
                .find(|column| column.name == condition.field)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Query,
                        format!("过滤引用了不存在的列: {}", condition.field),
                    )
                })?;
            let name = match alias {
                Some(alias) => format!(
                    "{}.{}",
                    fluxdb_core::SqlServerDialect.quote_identifier(alias),
                    fluxdb_core::SqlServerDialect.quote_identifier(&column.name)
                ),
                None => fluxdb_core::SqlServerDialect.quote_identifier(&column.name),
            };
            sqlserver_expression_condition(condition, &name, column, parameters)?
        }
        FilterExpression::And(items) => {
            let sql = items
                .iter()
                .map(|item| sqlserver_expression_sql(item, columns, alias, parameters))
                .collect::<fluxdb_core::Result<Vec<_>>>()?
                .into_iter()
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if sql.len() <= 1 {
                sql.into_iter().next().unwrap_or_default()
            } else {
                format!("({})", sql.join(" AND "))
            }
        }
        FilterExpression::Or(items) => {
            let sql = items
                .iter()
                .map(|item| sqlserver_expression_sql(item, columns, alias, parameters))
                .collect::<fluxdb_core::Result<Vec<_>>>()?
                .into_iter()
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if sql.len() <= 1 {
                sql.into_iter().next().unwrap_or_default()
            } else {
                format!("({})", sql.join(" OR "))
            }
        }
        FilterExpression::Not(inner) => format!(
            "NOT ({})",
            sqlserver_expression_sql(inner, columns, alias, parameters)?
        ),
        FilterExpression::Raw(raw) => raw.clone(),
    })
}

fn sqlserver_expression_condition(
    condition: &fluxdb_core::FilterCondition,
    name: &str,
    column: &Column,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
) -> fluxdb_core::Result<String> {
    use fluxdb_core::{FilterOperator, FilterValue};
    let bind = |parameters: &mut Vec<fluxdb_core::SqlParameter>, value: &FilterValue| {
        parameters.push(sqlserver_filter_parameter(value));
        sqlserver_placeholder(parameters.len())
    };
    let first = || {
        condition
            .values
            .first()
            .ok_or_else(|| Error::new(ErrorKind::Query, "过滤条件缺少必要的比较值"))
    };
    Ok(match condition.op {
        FilterOperator::Eq => format!("{name} = {}", bind(parameters, first()?)),
        FilterOperator::Ne => format!("{name} != {}", bind(parameters, first()?)),
        FilterOperator::Lt => format!("{name} < {}", bind(parameters, first()?)),
        FilterOperator::Le => format!("{name} <= {}", bind(parameters, first()?)),
        FilterOperator::Gt => format!("{name} > {}", bind(parameters, first()?)),
        FilterOperator::Ge => format!("{name} >= {}", bind(parameters, first()?)),
        FilterOperator::IsNull => format!("{name} IS NULL"),
        FilterOperator::IsNotNull => format!("{name} IS NOT NULL"),
        FilterOperator::IsEmpty => format!("{name} = N''"),
        FilterOperator::IsNotEmpty => format!("{name} <> N''"),
        FilterOperator::Contains
        | FilterOperator::NotContains
        | FilterOperator::StartsWith
        | FilterOperator::NotStartsWith
        | FilterOperator::EndsWith
        | FilterOperator::NotEndsWith => {
            let text = fluxdb_core::filter_value_text(first()?);
            let pattern = crate::filter_dialects::like_pattern(condition.op, &text)
                .ok_or_else(|| Error::new(ErrorKind::Query, "未知的 LIKE 运算符"))?;
            let negative = matches!(
                condition.op,
                FilterOperator::NotContains
                    | FilterOperator::NotStartsWith
                    | FilterOperator::NotEndsWith
            );
            let like_name = if crate::filter_dialects::filter_column_is_text(column) {
                name.to_string()
            } else {
                format!("CAST({name} AS NVARCHAR(MAX))")
            };
            format!(
                "{like_name}{}LIKE {}{}",
                if negative { " NOT " } else { " " },
                bind(parameters, &FilterValue::Text(pattern)),
                if crate::filter_dialects::like_value_needs_escape(&text) {
                    crate::filter_dialects::LIKE_ESCAPE_CLAUSE
                } else {
                    ""
                }
            )
        }
        FilterOperator::Between | FilterOperator::NotBetween => {
            let start = condition.values.first().ok_or_else(|| {
                Error::new(ErrorKind::Query, "过滤条件缺少区间起点")
            })?;
            let end = condition.values.get(1).ok_or_else(|| {
                Error::new(ErrorKind::Query, "过滤条件缺少区间终点")
            })?;
            format!(
                "{name}{} BETWEEN {} AND {}",
                if condition.op == FilterOperator::NotBetween {
                    " NOT"
                } else {
                    ""
                },
                bind(parameters, start),
                bind(parameters, end)
            )
        }
        FilterOperator::InList | FilterOperator::NotInList => {
            if condition.values.is_empty() {
                return Err(Error::new(ErrorKind::Query, "过滤条件缺少列表值"));
            }
            let values = condition
                .values
                .iter()
                .map(|value| bind(parameters, value))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{name}{} IN ({values})",
                if condition.op == FilterOperator::NotInList {
                    " NOT"
                } else {
                    ""
                }
            )
        }
    })
}

fn sqlserver_filter_parameter(value: &fluxdb_core::FilterValue) -> fluxdb_core::SqlParameter {
    use fluxdb_core::{FilterValue, SqlParameter};
    match value {
        FilterValue::Null => SqlParameter::Null,
        FilterValue::Boolean(value) => SqlParameter::Bool(*value),
        FilterValue::Integer(value) => SqlParameter::I64(value.to_string()),
        FilterValue::Float(value) => SqlParameter::F64(*value),
        FilterValue::Text(value) => SqlParameter::Text(value.clone()),
    }
}

fn sqlserver_push_filter(
    clauses: &mut Vec<String>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
    filter: &FilterSpec,
    column: &Column,
    alias: Option<&str>,
) {
    let name = match alias {
        Some(alias) => format!(
            "{}.{}",
            fluxdb_core::SqlServerDialect.quote_identifier(alias),
            fluxdb_core::SqlServerDialect.quote_identifier(&column.name)
        ),
        None => fluxdb_core::SqlServerDialect.quote_identifier(&column.name),
    };
    match filter.op {
        FilterOp::IsNull | FilterOp::NotExists => clauses.push(format!("{name} IS NULL")),
        FilterOp::IsNotNull | FilterOp::Exists => clauses.push(format!("{name} IS NOT NULL")),
        FilterOp::IsEmpty | FilterOp::IsNotEmpty => {
            parameters.push(fluxdb_core::SqlParameter::Text(String::new()));
            clauses.push(format!(
                "{name} {} {}",
                if filter.op == FilterOp::IsNotEmpty { "<>" } else { "=" },
                sqlserver_placeholder(parameters.len())
            ));
        }
        FilterOp::Between | FilterOp::NotBetween => {
            if filter.values.len() < 2 {
                return;
            }
            let start = sqlserver_push_value_parameter(parameters, &filter.values[0]);
            let end = sqlserver_push_value_parameter(parameters, &filter.values[1]);
            clauses.push(format!(
                "{name}{} BETWEEN {start} AND {end}",
                if filter.op == FilterOp::NotBetween { " NOT" } else { "" }
            ));
        }
        FilterOp::InList | FilterOp::NotInList => {
            if filter.values.is_empty() {
                return;
            }
            let placeholders = filter
                .values
                .iter()
                .map(|value| sqlserver_push_value_parameter(parameters, value))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!(
                "{name}{} IN ({placeholders})",
                if filter.op == FilterOp::NotInList { " NOT" } else { "" }
            ));
        }
        FilterOp::Eq | FilterOp::NotEq => sqlserver_push_equality_filter(clauses, parameters, filter, &name),
        FilterOp::Contains
        | FilterOp::NotContains
        | FilterOp::StartsWith
        | FilterOp::NotStartsWith
        | FilterOp::EndsWith
        | FilterOp::NotEndsWith => {
            sqlserver_push_like_filter(clauses, parameters, filter, &name);
        }
        FilterOp::GreaterThan
        | FilterOp::GreaterThanOrEqual
        | FilterOp::LessThan
        | FilterOp::LessThanOrEqual => {
            let Some(value) = filter.values.first() else {
                return;
            };
            let parameter = sqlserver_push_value_parameter(parameters, value);
            let op = match filter.op {
                FilterOp::GreaterThan => ">",
                FilterOp::GreaterThanOrEqual => ">=",
                FilterOp::LessThan => "<",
                _ => "<=",
            };
            clauses.push(format!("{name} {op} {parameter}"));
        }
    }
}

fn sqlserver_push_equality_filter(
    clauses: &mut Vec<String>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
    filter: &FilterSpec,
    name: &str,
) {
    if filter.values.is_empty() {
        return;
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
                let parameter = sqlserver_push_value_parameter(parameters, value);
                format!("{name} {op} {parameter}")
            }
        })
        .collect::<Vec<_>>();
    let expression = predicates.join(joiner);
    clauses.push(if predicates.len() > 1 {
        format!("({expression})")
    } else {
        expression
    });
}

fn sqlserver_push_like_filter(
    clauses: &mut Vec<String>,
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
    filter: &FilterSpec,
    name: &str,
) {
    let Some(value) = filter.values.first() else {
        return;
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
    parameters.push(fluxdb_core::SqlParameter::Text(pattern));
    let placeholder = sqlserver_placeholder(parameters.len());
    clauses.push(format!(
        "{name} {}LIKE {placeholder}",
        if matches!(
            filter.op,
            FilterOp::NotContains | FilterOp::NotStartsWith | FilterOp::NotEndsWith
        ) {
            "NOT "
        } else {
            ""
        }
    ));
}

fn sqlserver_push_value_parameter(
    parameters: &mut Vec<fluxdb_core::SqlParameter>,
    value: &CellValue,
) -> String {
    parameters.push(sqlserver_sql_parameter(value));
    sqlserver_placeholder(parameters.len())
}

fn sqlserver_sql_parameter(value: &CellValue) -> fluxdb_core::SqlParameter {
    match value {
        CellValue::Null => fluxdb_core::SqlParameter::Null,
        CellValue::Bool(value) => fluxdb_core::SqlParameter::Bool(*value),
        CellValue::I64(value) => fluxdb_core::SqlParameter::I64(value.to_string()),
        CellValue::F64(value) => fluxdb_core::SqlParameter::F64(*value),
        CellValue::Text(value) | CellValue::Json(value) => {
            fluxdb_core::SqlParameter::Text(value.clone())
        }
        CellValue::Bytes(value) => fluxdb_core::SqlParameter::Bytes(value.clone()),
        CellValue::BinarySummary(summary) => {
            if summary.is_null {
                fluxdb_core::SqlParameter::Null
            } else {
                fluxdb_core::SqlParameter::Text(summary.preview_hex.clone().unwrap_or_default())
            }
        }
    }
}

