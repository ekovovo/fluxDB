// SQL 写入风险的确定性证据（设计 §29.2）。
//
// 分工：本模块只产出**代码规则**这一半——AST 结构 + 连接器能力 → 脱敏信号与出站事实；
// 评审模型那一半在 `ai_write_review.rs`，两者合成在 `fluxdb_core::combine_write_risk`。
//
// 出站边界：`evidence_json` 不含 SQL 原文、参数值、对象名、库/schema 名或凭据，
// 只有机制层面的形状与能力标签。审批卡的 SQL 展示走既有 `approval_details`，两者不复用。

/// 规则分档位：语句形状的确定性 × 连接器的执行保证。
///
/// 数值刻意让「形状确定 + 有界提交」在没有任何模型时也能自动放行（机制上已证明），
/// 而「形状确定 + 普通执行」落在默认阈值之上：需要评审模型明确背书才会降到阈值以下。
/// 这正是"把权限放给评审模型"的可调档位——模型的作用是给把握背书，不是兜底放行。
mod score {
    /// 形状确定且连接器能在事务内复核真实影响。
    pub const DETERMINED_BOUNDED: f64 = 0.10;
    /// 形状确定，但连接器只能普通执行：真实行数与间接影响事后才知道。
    pub const DETERMINED_ORDINARY: f64 = 0.40;
    /// 定位不到确定行数（非唯一谓词、多行 VALUES），可能有级联或触发器影响。
    pub const UNCERTAIN_SHAPE: f64 = 0.55;
    /// 结构/权限变更：机制上不可回滚。同时也是硬信号下界。
    pub const IRREVERSIBLE: f64 = 0.95;
}

/// 一条写入的 AST 形状（只描述机制，不含对象名与值）。
#[derive(Clone, Debug, PartialEq)]
struct WriteShape {
    /// 语句类别：`insert` / `update` / `delete` / 其它（DDL、MERGE…）。
    operation: &'static str,
    /// 单条语句、解析成功、不含事务/会话控制。
    single_statement: bool,
    /// 目标表数量（JOIN/USING 会 >1）。
    tables: usize,
    /// 是否带谓词（WHERE）；无谓词等于整表。
    has_predicate: bool,
    /// 谓词是不是"单列等值"，具备在唯一键上定位到确定行数的语法形态。
    key_equality: bool,
    /// 赋值右侧是否全为字面量或绑定值（表达式可能依赖行内旧值或有副作用）。
    literal_assignments: bool,
    /// 插入来源是否全为字面量 VALUES，而不是查询。
    literal_values: bool,
    /// 插入行数（`VALUES (..),(..)` 的元组数）。
    value_rows: usize,
    /// 是否带 RETURNING/OUTPUT/ON CONFLICT/ON DUPLICATE 等附加语义。
    has_side_clauses: bool,
}

/// 连接器对本次写入提供的执行保证。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectorGuarantee {
    /// 有界提交：执行时在事务内复核 schema 与真实影响，超限即回滚。
    Bounded,
    /// 普通执行：语句形状明确，但服务端不额外提供影响核实。
    Ordinary,
}

/// 生成 SQL 写入风险证据。
///
/// `bounded_capable` 是连接器能力（是否实现有界提交），不由 SQL 文本推断；
/// 但能力不等于本次适用：有界提交只覆盖单表唯一键等值 UPDATE，
/// 因此两者都满足时才按"有界保证"评分，否则退回普通执行路径。
pub fn sql_write_risk_facts(
    sql: &str,
    policy: &dyn SqlPolicy,
    bounded_capable: bool,
) -> WriteRiskFacts {
    let shape = write_shape(sql, policy);
    let bounded = bounded_capable && shape_is_bounded_update(&shape);
    let guarantee = if bounded {
        ConnectorGuarantee::Bounded
    } else {
        ConnectorGuarantee::Ordinary
    };
    let mut facts = WriteRiskFacts::new(sql_evidence_json(&shape, guarantee));
    facts.bounded_capable = bounded;
    sql_score_shape(&shape, guarantee, &mut facts);
    facts
}

/// 本次写入是否落在连接器有界提交的覆盖范围内。
///
/// 与 `sqlite_bounded_update` 的复核条件保持一致：单表、带谓词、单列等值、
/// 字面量赋值、无附加语义。连接器执行时仍会重新核实，这里只用于选择执行路径与评分档位。
fn shape_is_bounded_update(shape: &WriteShape) -> bool {
    shape.single_statement
        && shape.operation == "update"
        && shape.tables == 1
        && shape.has_predicate
        && shape.key_equality
        && shape.literal_assignments
        && !shape.has_side_clauses
}

/// 按「形状确定性 × 连接器保证」打分。
///
/// 硬信号（[`WriteRiskSignal::hard`]）表示"语句本身给不出边界"，评审模型无权下穿；
/// 软信号（[`WriteRiskSignal::soft`]）表示"未核实的不确定度"，模型可以背书降分。
fn sql_score_shape(shape: &WriteShape, guarantee: ConnectorGuarantee, facts: &mut WriteRiskFacts) {
    if !shape.single_statement {
        facts.hard("multi_statement", 1.0, "多语句、含事务控制或无法解析");
        return;
    }
    let determined = if guarantee == ConnectorGuarantee::Bounded {
        score::DETERMINED_BOUNDED
    } else {
        score::DETERMINED_ORDINARY
    };
    match shape.operation {
        "insert" => {
            if !shape.literal_values {
                facts.hard("insert_from_query", score::UNCERTAIN_SHAPE, "插入来源是查询而非字面量");
            } else if shape.value_rows > 1 {
                facts.soft(
                    "multi_row_values",
                    score::UNCERTAIN_SHAPE,
                    format!("单条 VALUES 含 {} 行", shape.value_rows),
                );
            } else {
                facts.soft("single_row_insert", determined, "单行字面量插入");
            }
        }
        "update" => {
            if shape.tables != 1 {
                facts.hard("multi_table_write", score::UNCERTAIN_SHAPE, "多表或带 JOIN 的更新");
            } else if !shape.literal_assignments {
                facts.hard(
                    "expression_assignment",
                    score::UNCERTAIN_SHAPE,
                    "赋值依赖表达式或行内旧值",
                );
            } else if !shape.has_predicate {
                facts.hard("no_predicate", score::UNCERTAIN_SHAPE, "无谓词的整表更新");
            } else if shape.key_equality {
                facts.soft("key_equality", determined, "单列等值谓词定位");
            } else {
                facts.soft("unbounded_predicate", score::UNCERTAIN_SHAPE, "谓词无法定位到确定行数");
            }
        }
        // 删除不进入有界提交路径（当前只覆盖 UPDATE），即使有理查唯一键的谓词
        // 也拿不到"删除前已核实行数"的保证：行删掉就回不来，按不确定处理。
        "delete" => {
            if shape.tables != 1 {
                facts.hard("multi_table_write", score::UNCERTAIN_SHAPE, "多表或带 USING 的删除");
            } else if !shape.has_predicate {
                facts.hard("no_predicate", score::UNCERTAIN_SHAPE, "无谓词的整表删除");
            } else {
                facts.soft(
                    "delete_unverified_key",
                    score::UNCERTAIN_SHAPE,
                    "删除不可逆且未在提交前核实行数",
                );
            }
        }
        // DDL/GRANT/MERGE 等结构性写入：机制上无法回滚，交人工。
        _ => facts.hard("structural_write", score::IRREVERSIBLE, "结构或权限变更"),
    }
    if shape.has_side_clauses {
        facts.hard(
            "side_clause",
            score::UNCERTAIN_SHAPE,
            "带 RETURNING/ON CONFLICT/ON DUPLICATE 等附加语义",
        );
    }
    if guarantee == ConnectorGuarantee::Ordinary && shape.operation != "unknown" {
        // 普通执行路径没有事务内影响复核：行数、级联与触发器影响都只能事后观察。
        facts.soft(
            "indirect_effects_unverified",
            score::DETERMINED_ORDINARY,
            "该连接不支持有界提交，间接影响与真实行数未核实",
        );
    }
}

/// 出站结构事实：只有形状标签与计数，没有 SQL 文本、对象名或值。
fn sql_evidence_json(shape: &WriteShape, guarantee: ConnectorGuarantee) -> String {
    serde_json::json!({
        "operation": shape.operation,
        "single_statement": shape.single_statement,
        "tables": shape.tables,
        "has_predicate": shape.has_predicate,
        "key_equality": shape.key_equality,
        "literal_assignments": shape.literal_assignments,
        "literal_values": shape.literal_values,
        "value_rows": shape.value_rows,
        "side_clauses": shape.has_side_clauses,
        "bounded_commit": guarantee == ConnectorGuarantee::Bounded,
        "business_values": "omitted",
        "object_names": "omitted",
    })
    .to_string()
}

/// 解析 SQL 形状。解析失败或语句数不为 1 时给出"无法证明影响面"的形状，
/// 由评分函数落到硬信号上，绝不把"看不懂"当作安全。
fn write_shape(sql: &str, policy: &dyn SqlPolicy) -> WriteShape {
    let unknown = WriteShape {
        operation: "unknown",
        single_statement: false,
        tables: 0,
        has_predicate: false,
        key_equality: false,
        literal_assignments: false,
        literal_values: false,
        value_rows: 0,
        has_side_clauses: false,
    };
    let Ok(statements) = sqlparser::parser::Parser::parse_sql(policy.parser_dialect(), sql.trim())
    else {
        return unknown;
    };
    let [statement] = statements.as_slice() else {
        return unknown;
    };
    use sqlparser::ast::{Expr, SetExpr, Statement};
    let base = WriteShape {
        single_statement: true,
        ..unknown
    };
    match statement {
        Statement::Insert(insert) => {
            let value_rows = match insert.source.as_deref().map(|query| &*query.body) {
                Some(SetExpr::Values(values)) => values.rows.len(),
                _ => 0,
            };
            WriteShape {
                operation: "insert",
                literal_values: value_rows > 0,
                value_rows,
                // 表函数（如远端表）不是普通表写入，按多表/不可证明处理。
                tables: match insert.table {
                    sqlparser::ast::TableObject::TableName(_) => 1,
                    _ => 2,
                },
                has_side_clauses: insert.on.is_some()
                    || insert.returning.is_some()
                    || insert.output.is_some()
                    || insert.overwrite
                    || insert.replace_into,
                ..base
            }
        }
        Statement::Update(update) => WriteShape {
            operation: "update",
            has_predicate: update.selection.is_some(),
            key_equality: sql_equality_on_single_identifier(update.selection.as_ref()),
            literal_assignments: update
                .assignments
                .iter()
                .all(|assignment| matches!(assignment.value, Expr::Value(_))),
            tables: 1 + update.table.joins.len(),
            has_side_clauses: update.returning.is_some()
                || update.output.is_some()
                || update.from.is_some()
                || update.or.is_some(),
            ..base
        },
        Statement::Delete(delete) => WriteShape {
            operation: "delete",
            has_predicate: delete.selection.is_some(),
            key_equality: sql_equality_on_single_identifier(delete.selection.as_ref()),
            // 无赋值的语句天然满足"字面量赋值"，这里只用于统一形状。
            literal_assignments: true,
            tables: if delete.using.as_ref().is_some_and(|using| !using.is_empty()) {
                2
            } else {
                1
            },
            has_side_clauses: delete.returning.is_some()
                || delete.output.is_some()
                || !delete.tables.is_empty(),
            ..base
        },
        other => WriteShape {
            operation: sql_write_operation_name(other),
            ..base
        },
    }
}

/// 谓词形状：`<单列标识符> = <字面量或绑定占位>`。
///
/// 只认这一种形态的**语法**；"这一列到底是不是唯一键"由连接器在执行时核实
/// （见 `inspect_bounded_update`），本函数不代替那个证明。
fn sql_equality_on_single_identifier(selection: Option<&sqlparser::ast::Expr>) -> bool {
    use sqlparser::ast::{BinaryOperator, Expr};
    let Some(Expr::BinaryOp { left, op, right }) = selection else {
        return false;
    };
    if *op != BinaryOperator::Eq {
        return false;
    }
    let (Expr::Identifier(_), right) = (left.as_ref(), right.as_ref()) else {
        return false;
    };
    matches!(right, Expr::Value(_))
}

fn sql_write_operation_name(statement: &sqlparser::ast::Statement) -> &'static str {
    use sqlparser::ast::Statement;
    match statement {
        Statement::Merge { .. } => "merge",
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateRole(_)
        | Statement::CreatePolicy(_)
        | Statement::AlterTable(_)
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::CreateVirtualTable { .. } => "ddl",
        Statement::Drop { .. } | Statement::Truncate(_) => "destructive_ddl",
        Statement::Grant(_) | Statement::Revoke(_) => "privilege",
        _ => "unknown",
    }
}

