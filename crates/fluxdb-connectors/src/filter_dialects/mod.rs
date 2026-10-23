//! 数据页筛选的方言适配层。
//!
//! 表单、条件文本和真实查询共用 [`fluxdb_core::FilterExpression`]；本目录只放
//! 各方言差异，不允许 UI 或控制器按数据库类型重复拼 WHERE。

use fluxdb_core::{
    CompiledFilterExpression, CompiledFilterNode, DatabaseKind, FilterCondition, FilterDialect,
    FilterExpression, FilterExpressionError, FilterOperator, FilterValue, ParsedFilterExpression,
};
use sqlparser::ast::{BinaryOperator, Expr, Ident, UnaryOperator, Value};
use sqlparser::dialect::{
    Dialect as SqlParserDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
};
use sqlparser::parser::Parser;

pub mod dameng;
pub mod mysql;
pub mod postgres;
pub mod sqlite;
pub mod sqlserver;

/// 按数据库类型选择筛选方言。MongoDB/Redis 没有 SQL WHERE 语义，明确返回 `None`。
pub fn filter_dialect(kind: DatabaseKind) -> Option<&'static dyn FilterDialect> {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => Some(&mysql::MYSQL_SYNTAX),
        DatabaseKind::Postgres => Some(&postgres::POSTGRES_SYNTAX),
        DatabaseKind::Sqlite => Some(&sqlite::SQLITE_SYNTAX),
        DatabaseKind::SqlServer => Some(&sqlserver::SQLSERVER_SYNTAX),
        // 达梦当前连接器使用 Oracle 风格标识符和 LIMIT/OFFSET；以 PostgreSQL
        // 的表达式语法为基础，差异只在引用与函数层，后续可独立替换实现。
        DatabaseKind::Dameng => Some(&dameng::DAMENG_SYNTAX),
        DatabaseKind::MongoDb | DatabaseKind::Redis => None,
    }
}

#[derive(Clone, Copy)]
pub enum FilterSyntax {
    Postgres,
    MySql,
    SqlServer,
    Sqlite,
}

impl FilterDialect for FilterSyntax {
    fn parse(
        &self,
        text: &str,
    ) -> std::result::Result<ParsedFilterExpression, FilterExpressionError> {
        parse_with_syntax(*self, text)
    }

    fn render(
        &self,
        expression: &FilterExpression,
    ) -> std::result::Result<String, FilterExpressionError> {
        render_expression(expression, *self)
    }

    fn compile(
        &self,
        expression: &FilterExpression,
    ) -> std::result::Result<CompiledFilterExpression, FilterExpressionError> {
        compile_expression(expression, *self)
    }
}

/// 把统一表达式编译成连接器可绑定的条件树。参数顺序与树遍历顺序一致。
pub fn compile_filter_tree(
    kind: DatabaseKind,
    expression: &FilterExpression,
) -> Result<Option<CompiledFilterNode>, FilterExpressionError> {
    let Some(syntax) = filter_syntax(kind) else {
        return Ok(None);
    };
    let tree = compile_tree(expression, syntax)?;
    if let CompiledFilterNode::And(items) = &tree
        && items.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(tree))
}

fn filter_syntax(kind: DatabaseKind) -> Option<FilterSyntax> {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => Some(FilterSyntax::MySql),
        DatabaseKind::Postgres => Some(FilterSyntax::Postgres),
        DatabaseKind::Sqlite => Some(FilterSyntax::Sqlite),
        DatabaseKind::SqlServer => Some(FilterSyntax::SqlServer),
        DatabaseKind::Dameng => Some(FilterSyntax::Postgres),
        DatabaseKind::MongoDb | DatabaseKind::Redis => None,
    }
}

fn parser_dialect(syntax: FilterSyntax) -> &'static dyn SqlParserDialect {
    static POSTGRES: PostgreSqlDialect = PostgreSqlDialect {};
    static MYSQL: MySqlDialect = MySqlDialect {};
    static SQLSERVER: MsSqlDialect = MsSqlDialect {};
    static SQLITE: SQLiteDialect = SQLiteDialect {};
    match syntax {
        FilterSyntax::Postgres => &POSTGRES,
        FilterSyntax::MySql => &MYSQL,
        FilterSyntax::SqlServer => &SQLSERVER,
        FilterSyntax::Sqlite => &SQLITE,
    }
}

fn parse_expression(
    syntax: FilterSyntax,
    text: &str,
) -> Result<FilterExpression, FilterExpressionError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(FilterExpression::And(Vec::new()));
    }
    let trimmed = trimmed.trim_end_matches(';').trim();
    // 条件输入框只接受表达式。显式包一层 SELECT 可复用 sqlparser 的完整
    // WHERE 解析，而不开放任意查询语句。
    let statement = format!("SELECT 1 WHERE {trimmed}");
    let parsed = Parser::parse_sql(parser_dialect(syntax), &statement)
        .map_err(|error| FilterExpressionError::new(format!("筛选条件语法错误：{error}")))?;
    if parsed.len() != 1 {
        return Err(FilterExpressionError::new(
            "筛选条件只能包含一个 SQL 表达式",
        ));
    }
    let Some(sqlparser::ast::Statement::Query(query)) = parsed.into_iter().next() else {
        return Err(FilterExpressionError::new("筛选条件不是 SQL 表达式"));
    };
    let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Err(FilterExpressionError::new("筛选条件不是 SQL 表达式"));
    };
    let Some(selection) = select.selection.as_ref() else {
        return Err(FilterExpressionError::new("筛选条件为空"));
    };
    expression_from_sql(selection, trimmed.to_string(), syntax)
}

fn expression_from_sql(
    expression: &Expr,
    raw: String,
    syntax: FilterSyntax,
) -> Result<FilterExpression, FilterExpressionError> {
    match expression {
        Expr::Nested(inner) => expression_from_sql(inner, raw, syntax),
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => Ok(FilterExpression::And(vec![
                expression_from_sql(left, raw.clone(), syntax)?,
                expression_from_sql(right, raw, syntax)?,
            ])),
            BinaryOperator::Or => Ok(FilterExpression::Or(vec![
                expression_from_sql(left, raw.clone(), syntax)?,
                expression_from_sql(right, raw, syntax)?,
            ])),
            BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                let Some(field) = field_name(left) else {
                    return Ok(FilterExpression::Raw(raw));
                };
                let Some(value) = literal_value(right) else {
                    return Ok(FilterExpression::Raw(raw));
                };
                let operator = match op {
                    BinaryOperator::Eq => FilterOperator::Eq,
                    BinaryOperator::NotEq => FilterOperator::Ne,
                    BinaryOperator::Lt => FilterOperator::Lt,
                    BinaryOperator::LtEq => FilterOperator::Le,
                    BinaryOperator::Gt => FilterOperator::Gt,
                    BinaryOperator::GtEq => FilterOperator::Ge,
                    _ => unreachable!(),
                };
                Ok(FilterExpression::Condition(FilterCondition {
                    field,
                    op: operator,
                    values: vec![value],
                }))
            }
            _ => Ok(FilterExpression::Raw(raw)),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => Ok(FilterExpression::Not(Box::new(expression_from_sql(
            expr, raw, syntax,
        )?))),
        Expr::IsNull(inner) => {
            let field = field_name(inner)
                .ok_or_else(|| FilterExpressionError::new("IS NULL 左侧必须是字段"))?;
            Ok(FilterExpression::Condition(FilterCondition {
                field,
                op: FilterOperator::IsNull,
                values: Vec::new(),
            }))
        }
        Expr::IsNotNull(inner) => {
            let field = field_name(inner)
                .ok_or_else(|| FilterExpressionError::new("IS NOT NULL 左侧必须是字段"))?;
            Ok(FilterExpression::Condition(FilterCondition {
                field,
                op: FilterOperator::IsNotNull,
                values: Vec::new(),
            }))
        }
        Expr::Like {
            negated,
            expr,
            pattern,
            ..
        } => {
            let field = field_name(expr)
                .ok_or_else(|| FilterExpressionError::new("LIKE 左侧必须是字段"))?;
            let Some(FilterValue::Text(pattern)) = literal_value(pattern) else {
                return Ok(FilterExpression::Raw(raw));
            };
            let (op, value) = like_operator(*negated, pattern);
            Ok(FilterExpression::Condition(FilterCondition {
                field,
                op,
                values: vec![FilterValue::Text(value)],
            }))
        }
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let field = field_name(expr)
                .ok_or_else(|| FilterExpressionError::new("BETWEEN 左侧必须是字段"))?;
            let Some(low) = literal_value(low) else {
                return Ok(FilterExpression::Raw(raw));
            };
            let Some(high) = literal_value(high) else {
                return Ok(FilterExpression::Raw(raw));
            };
            Ok(FilterExpression::Condition(FilterCondition {
                field,
                op: if *negated {
                    FilterOperator::NotBetween
                } else {
                    FilterOperator::Between
                },
                values: vec![low, high],
            }))
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let field =
                field_name(expr).ok_or_else(|| FilterExpressionError::new("IN 左侧必须是字段"))?;
            let Some(values) = list.iter().map(literal_value).collect::<Option<Vec<_>>>() else {
                return Ok(FilterExpression::Raw(raw));
            };
            Ok(FilterExpression::Condition(FilterCondition {
                field,
                op: if *negated {
                    FilterOperator::NotInList
                } else {
                    FilterOperator::InList
                },
                values,
            }))
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            let _ = syntax;
            Ok(FilterExpression::Raw(raw))
        }
        _ => Ok(FilterExpression::Raw(raw)),
    }
}

fn field_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Identifier(Ident { value, .. }) => Some(value.clone()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|ident| ident.value.clone()),
        Expr::Nested(inner) => field_name(inner),
        _ => None,
    }
}

fn literal_value(expression: &Expr) -> Option<FilterValue> {
    match expression {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(value) | Value::DoubleQuotedString(value) => {
                Some(FilterValue::Text(value.clone()))
            }
            Value::DollarQuotedString(value) => Some(FilterValue::Text(value.value.clone())),
            Value::Number(value, _) => Some(if let Ok(value) = value.parse::<i64>() {
                FilterValue::Integer(value)
            } else {
                FilterValue::Float(value.parse().ok()?)
            }),
            Value::Boolean(value) => Some(FilterValue::Boolean(*value)),
            Value::Null => Some(FilterValue::Null),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match literal_value(expr)? {
            FilterValue::Integer(value) => Some(FilterValue::Integer(-value)),
            FilterValue::Float(value) => Some(FilterValue::Float(-value)),
            _ => None,
        },
        Expr::Nested(inner) => literal_value(inner),
        _ => None,
    }
}

fn like_operator(negated: bool, pattern: String) -> (FilterOperator, String) {
    let starts = pattern.starts_with('%');
    let ends = pattern.ends_with('%');
    let value = pattern.trim_matches('%').to_string();
    match (negated, starts, ends) {
        (false, false, false) => (FilterOperator::Eq, value),
        (false, true, false) => (FilterOperator::EndsWith, value),
        (false, false, true) => (FilterOperator::StartsWith, value),
        (false, true, true) => (FilterOperator::Contains, value),
        (true, false, false) => (FilterOperator::Ne, value),
        (true, true, false) => (FilterOperator::NotEndsWith, value),
        (true, false, true) => (FilterOperator::NotStartsWith, value),
        (true, true, true) => (FilterOperator::NotContains, value),
    }
}

fn parse_with_syntax(
    syntax: FilterSyntax,
    text: &str,
) -> Result<ParsedFilterExpression, FilterExpressionError> {
    let expression = parse_expression(syntax, text)?;
    let text = render_expression(&expression, syntax)?;
    Ok(ParsedFilterExpression { expression, text })
}

pub(crate) fn parse_for_syntax(
    syntax: FilterSyntax,
    text: &str,
) -> Result<ParsedFilterExpression, FilterExpressionError> {
    parse_with_syntax(syntax, text)
}

pub(crate) fn render_for_syntax(
    syntax: FilterSyntax,
    expression: &FilterExpression,
) -> Result<String, FilterExpressionError> {
    render_expression(expression, syntax)
}

pub(crate) fn compile_for_syntax(
    syntax: FilterSyntax,
    expression: &FilterExpression,
) -> Result<CompiledFilterExpression, FilterExpressionError> {
    compile_expression(expression, syntax)
}

fn render_expression(
    expression: &FilterExpression,
    syntax: FilterSyntax,
) -> Result<String, FilterExpressionError> {
    match expression {
        FilterExpression::Condition(condition) => render_condition(condition, syntax),
        FilterExpression::And(items) => Ok(render_group(items, syntax, "AND")),
        FilterExpression::Or(items) => Ok(render_group(items, syntax, "OR")),
        FilterExpression::Not(inner) => Ok(format!("NOT ({})", render_expression(inner, syntax)?)),
        FilterExpression::Raw(raw) => Ok(raw.clone()),
    }
}

fn render_group(items: &[FilterExpression], syntax: FilterSyntax, operator: &str) -> String {
    let rendered = items
        .iter()
        .map(|item| render_expression(item, syntax).unwrap_or_default())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if rendered.len() <= 1 {
        rendered.into_iter().next().unwrap_or_default()
    } else {
        format!("({})", rendered.join(&format!(" {operator} ")))
    }
}

fn render_condition(
    condition: &FilterCondition,
    syntax: FilterSyntax,
) -> Result<String, FilterExpressionError> {
    let field = quote_identifier(&condition.field, syntax);
    let value = |index: usize| {
        condition
            .values
            .get(index)
            .map(|value| render_literal(value, syntax))
            .ok_or_else(|| FilterExpressionError::new("筛选条件缺少必要的比较值"))
    };
    if !condition.op.requires_values() {
        return Ok(match condition.op {
            FilterOperator::IsNull => format!("{field} IS NULL"),
            FilterOperator::IsNotNull => format!("{field} IS NOT NULL"),
            FilterOperator::IsEmpty => format!("{field} = ''"),
            FilterOperator::IsNotEmpty => format!("{field} != ''"),
            _ => unreachable!(),
        });
    }
    if condition.values.is_empty() {
        return Err(FilterExpressionError::new("筛选条件缺少必要的比较值"));
    }
    if matches!(
        condition.op,
        FilterOperator::Between | FilterOperator::NotBetween
    ) && condition.values.len() < 2
    {
        return Err(FilterExpressionError::new("介于条件缺少区间终点"));
    }
    Ok(match condition.op {
        FilterOperator::Eq => format!("{field} = {}", value(0)?),
        FilterOperator::Ne => format!("{field} != {}", value(0)?),
        FilterOperator::Lt => format!("{field} < {}", value(0)?),
        FilterOperator::Le => format!("{field} <= {}", value(0)?),
        FilterOperator::Gt => format!("{field} > {}", value(0)?),
        FilterOperator::Ge => format!("{field} >= {}", value(0)?),
        FilterOperator::Contains => {
            format!(
                "{field} LIKE {}",
                render_like("%", &condition.values[0], "%")
            )
        }
        FilterOperator::NotContains => {
            format!(
                "{field} NOT LIKE {}",
                render_like("%", &condition.values[0], "%")
            )
        }
        FilterOperator::StartsWith => {
            format!(
                "{field} LIKE {}",
                render_like("", &condition.values[0], "%")
            )
        }
        FilterOperator::NotStartsWith => {
            format!(
                "{field} NOT LIKE {}",
                render_like("", &condition.values[0], "%")
            )
        }
        FilterOperator::EndsWith => {
            format!(
                "{field} LIKE {}",
                render_like("%", &condition.values[0], "")
            )
        }
        FilterOperator::NotEndsWith => {
            format!(
                "{field} NOT LIKE {}",
                render_like("%", &condition.values[0], "")
            )
        }
        FilterOperator::IsNull => format!("{field} IS NULL"),
        FilterOperator::IsNotNull => format!("{field} IS NOT NULL"),
        FilterOperator::IsEmpty => format!("{field} = ''"),
        FilterOperator::IsNotEmpty => format!("{field} != ''"),
        FilterOperator::Between => format!("{field} BETWEEN {} AND {}", value(0)?, value(1)?),
        FilterOperator::NotBetween => {
            format!("{field} NOT BETWEEN {} AND {}", value(0)?, value(1)?)
        }
        FilterOperator::InList => format!(
            "{field} IN ({})",
            condition
                .values
                .iter()
                .map(|value| render_literal(value, syntax))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        FilterOperator::NotInList => format!(
            "{field} NOT IN ({})",
            condition
                .values
                .iter()
                .map(|value| render_literal(value, syntax))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn render_like(prefix: &str, value: &FilterValue, suffix: &str) -> String {
    let FilterValue::Text(value) = value else {
        return "'".to_string();
    };
    format!("'{}{}{}'", prefix, value.replace('\'', "''"), suffix)
}

fn render_literal(value: &FilterValue, syntax: FilterSyntax) -> String {
    match value {
        FilterValue::Text(value) => format!("'{}'", value.replace('\'', "''")),
        FilterValue::Integer(value) => value.to_string(),
        FilterValue::Float(value) => value.to_string(),
        FilterValue::Boolean(value) => {
            if matches!(syntax, FilterSyntax::SqlServer) {
                if *value {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            } else {
                value.to_string()
            }
        }
        FilterValue::Null => "NULL".to_string(),
    }
}

fn quote_identifier(value: &str, syntax: FilterSyntax) -> String {
    match syntax {
        FilterSyntax::MySql => format!("`{}`", value.replace('`', "``")),
        FilterSyntax::SqlServer => format!("[{}]", value.replace(']', "]]")),
        _ => format!("\"{}\"", value.replace('"', "\"\"")),
    }
}

fn compile_expression(
    expression: &FilterExpression,
    syntax: FilterSyntax,
) -> Result<CompiledFilterExpression, FilterExpressionError> {
    let mut parameters = Vec::new();
    let sql = compile_sql(expression, syntax, &mut parameters)?;
    Ok(CompiledFilterExpression { sql, parameters })
}

fn compile_sql(
    expression: &FilterExpression,
    syntax: FilterSyntax,
    parameters: &mut Vec<FilterValue>,
) -> Result<String, FilterExpressionError> {
    match expression {
        FilterExpression::Condition(condition) => compile_condition(condition, syntax, parameters),
        FilterExpression::And(items) => {
            let sql = items
                .iter()
                .map(|item| compile_sql(item, syntax, parameters))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            Ok(if sql.len() <= 1 {
                sql.into_iter().next().unwrap_or_default()
            } else {
                format!("({})", sql.join(" AND "))
            })
        }
        FilterExpression::Or(items) => {
            let sql = items
                .iter()
                .map(|item| compile_sql(item, syntax, parameters))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            Ok(if sql.len() <= 1 {
                sql.into_iter().next().unwrap_or_default()
            } else {
                format!("({})", sql.join(" OR "))
            })
        }
        FilterExpression::Not(inner) => {
            Ok(format!("NOT ({})", compile_sql(inner, syntax, parameters)?))
        }
        FilterExpression::Raw(raw) => Ok(raw.clone()),
    }
}

fn compile_tree(
    expression: &FilterExpression,
    syntax: FilterSyntax,
) -> Result<CompiledFilterNode, FilterExpressionError> {
    match expression {
        FilterExpression::Condition(condition) => {
            let mut parameters = Vec::new();
            let sql = compile_condition(condition, syntax, &mut parameters)?;
            Ok(CompiledFilterNode::Condition { sql, parameters })
        }
        FilterExpression::And(items) => Ok(CompiledFilterNode::And(
            items
                .iter()
                .map(|item| compile_tree(item, syntax))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        FilterExpression::Or(items) => Ok(CompiledFilterNode::Or(
            items
                .iter()
                .map(|item| compile_tree(item, syntax))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        FilterExpression::Not(inner) => Ok(CompiledFilterNode::Not(Box::new(compile_tree(
            inner, syntax,
        )?))),
        FilterExpression::Raw(raw) => Ok(CompiledFilterNode::Condition {
            sql: raw.clone(),
            parameters: Vec::new(),
        }),
    }
}

fn compile_condition(
    condition: &FilterCondition,
    syntax: FilterSyntax,
    parameters: &mut Vec<FilterValue>,
) -> Result<String, FilterExpressionError> {
    let field = quote_identifier(&condition.field, syntax);
    let value = |index: usize| {
        condition
            .values
            .get(index)
            .cloned()
            .ok_or_else(|| FilterExpressionError::new("筛选条件缺少必要的比较值"))
    };
    let bind = |parameters: &mut Vec<FilterValue>, value: FilterValue| {
        parameters.push(value);
        match syntax {
            FilterSyntax::Postgres => format!("${}", parameters.len()),
            FilterSyntax::SqlServer => format!("@P{}", parameters.len()),
            _ => "?".to_string(),
        }
    };
    if !condition.op.requires_values() {
        return Ok(match condition.op {
            FilterOperator::IsNull => format!("{field} IS NULL"),
            FilterOperator::IsNotNull => format!("{field} IS NOT NULL"),
            FilterOperator::IsEmpty => format!("{field} = ''"),
            FilterOperator::IsNotEmpty => format!("{field} != ''"),
            _ => unreachable!(),
        });
    }
    if condition.values.is_empty() {
        return Err(FilterExpressionError::new("筛选条件缺少必要的比较值"));
    }
    if matches!(
        condition.op,
        FilterOperator::Between | FilterOperator::NotBetween
    ) && condition.values.len() < 2
    {
        return Err(FilterExpressionError::new("介于条件缺少区间终点"));
    }
    Ok(match condition.op {
        FilterOperator::Eq => format!("{field} = {}", bind(parameters, value(0)?)),
        FilterOperator::Ne => format!("{field} != {}", bind(parameters, value(0)?)),
        FilterOperator::Lt => format!("{field} < {}", bind(parameters, value(0)?)),
        FilterOperator::Le => format!("{field} <= {}", bind(parameters, value(0)?)),
        FilterOperator::Gt => format!("{field} > {}", bind(parameters, value(0)?)),
        FilterOperator::Ge => format!("{field} >= {}", bind(parameters, value(0)?)),
        FilterOperator::Contains
        | FilterOperator::NotContains
        | FilterOperator::StartsWith
        | FilterOperator::NotStartsWith
        | FilterOperator::EndsWith
        | FilterOperator::NotEndsWith => {
            let negated = matches!(
                condition.op,
                FilterOperator::NotContains
                    | FilterOperator::NotStartsWith
                    | FilterOperator::NotEndsWith
            );
            let FilterValue::Text(value) = value(0)? else {
                return Err(FilterExpressionError::new("LIKE 需要文本值"));
            };
            let pattern = match condition.op {
                FilterOperator::Contains | FilterOperator::NotContains => {
                    format!("%{value}%")
                }
                FilterOperator::StartsWith | FilterOperator::NotStartsWith => {
                    format!("{value}%")
                }
                _ => format!("%{value}"),
            };
            let placeholder = bind(parameters, FilterValue::Text(pattern));
            format!(
                "{field}{} LIKE {placeholder}",
                if negated { " NOT" } else { "" }
            )
        }
        FilterOperator::IsNull => format!("{field} IS NULL"),
        FilterOperator::IsNotNull => format!("{field} IS NOT NULL"),
        FilterOperator::IsEmpty => format!("{field} = ''"),
        FilterOperator::IsNotEmpty => format!("{field} != ''"),
        FilterOperator::Between | FilterOperator::NotBetween => {
            let first = bind(parameters, value(0)?);
            let second = bind(parameters, value(1)?);
            format!(
                "{field}{} BETWEEN {first} AND {second}",
                if condition.op == FilterOperator::NotBetween {
                    " NOT"
                } else {
                    ""
                }
            )
        }
        FilterOperator::InList | FilterOperator::NotInList => {
            let values = condition
                .values
                .iter()
                .map(|value| bind(parameters, value.clone()))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{field}{} IN ({values})",
                if condition.op == FilterOperator::NotInList {
                    " NOT"
                } else {
                    ""
                }
            )
        }
    })
}

#[cfg(test)]
mod filter_dialect_tests {
    use super::*;

    #[test]
    fn postgres_parses_and_or_precedence_into_a_shared_tree() {
        let parsed = postgres::POSTGRES_SYNTAX
            .parse("status = 'failed' OR amount > 10 AND name LIKE 'A%'")
            .expect("valid filter");
        let FilterExpression::Or(items) = parsed.expression else {
            panic!("OR must be the root");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], FilterExpression::Condition(_)));
        assert!(matches!(&items[1], FilterExpression::And(nodes) if nodes.len() == 2));
    }

    #[test]
    fn each_dialect_quotes_fields_and_compiles_bound_parameters() {
        let expression =
            FilterExpression::And(vec![FilterExpression::Condition(FilterCondition {
                field: "status".to_string(),
                op: FilterOperator::Eq,
                values: vec![FilterValue::Text("failed".to_string())],
            })]);
        let mysql = mysql::MYSQL_SYNTAX.render(&expression).unwrap();
        let postgres = postgres::POSTGRES_SYNTAX.render(&expression).unwrap();
        let sqlserver = sqlserver::SQLSERVER_SYNTAX.render(&expression).unwrap();
        assert_eq!(mysql, "`status` = 'failed'");
        assert_eq!(postgres, "\"status\" = 'failed'");
        assert_eq!(sqlserver, "[status] = 'failed'");

        let compiled = postgres::POSTGRES_SYNTAX.compile(&expression).unwrap();
        assert_eq!(compiled.sql, "\"status\" = $1");
        assert_eq!(
            compiled.parameters,
            vec![FilterValue::Text("failed".into())]
        );
    }

    #[test]
    fn unsupported_expression_is_preserved_as_raw_dialect_text() {
        let parsed = postgres::POSTGRES_SYNTAX
            .parse("lower(name) = 'abc'")
            .expect("parse expression");
        assert!(matches!(parsed.expression, FilterExpression::Raw(_)));
        let rendered = postgres::POSTGRES_SYNTAX
            .render(&parsed.expression)
            .expect("render raw");
        assert!(rendered.to_ascii_lowercase().contains("lower(name)"));
    }

    #[test]
    fn parse_rejects_multiple_statements() {
        let error = postgres::POSTGRES_SYNTAX
            .parse("status = 'ok'; DELETE FROM users")
            .expect_err("multiple statements must be rejected");
        assert!(error.to_string().contains("只能包含一个"));
    }

    #[test]
    fn semicolon_inside_literal_is_not_a_statement_separator() {
        let parsed = postgres::POSTGRES_SYNTAX
            .parse("status = 'a;b';")
            .expect("literal semicolon");
        assert_eq!(
            parsed.expression,
            FilterExpression::Condition(FilterCondition {
                field: "status".into(),
                op: FilterOperator::Eq,
                values: vec![FilterValue::Text("a;b".into())],
            })
        );
    }

    #[test]
    fn projection_keeps_unflattenable_subtrees_as_advanced_rules() {
        let parsed = postgres::POSTGRES_SYNTAX
            .parse("status = 'failed' AND lower(name) = 'abc'")
            .expect("valid expression");
        let projection =
            fluxdb_core::filter_form_projection_from_expression(&parsed.expression, |expression| {
                postgres::POSTGRES_SYNTAX
                    .render(expression)
                    .unwrap_or_default()
            });
        assert_eq!(projection.rules.len(), 2);
        assert_eq!(projection.rules[0].field.as_deref(), Some("status"));
        assert!(projection.rules[1].field.is_none());
        assert!(
            fluxdb_core::filter_value_text(&projection.rules[1].values[0])
                .to_ascii_lowercase()
                .contains("lower(name)")
        );
    }

    #[test]
    fn between_values_keep_source_order() {
        let parsed = postgres::POSTGRES_SYNTAX
            .parse("score BETWEEN 100 AND 10")
            .expect("valid expression");
        let FilterExpression::Condition(condition) = parsed.expression else {
            panic!("single condition");
        };
        assert_eq!(
            condition.values,
            vec![FilterValue::Integer(100), FilterValue::Integer(10)]
        );
        assert_eq!(
            postgres::POSTGRES_SYNTAX
                .render(&FilterExpression::Condition(condition))
                .unwrap(),
            "\"score\" BETWEEN 100 AND 10"
        );
    }
}
