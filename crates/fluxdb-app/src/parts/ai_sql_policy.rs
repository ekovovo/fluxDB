// SQL 风险分类与只读保证（设计 §21，T023/T024）。
//
// 原则：
// - **不靠前缀猜安全**：`SELECT/WITH` 开头不等于只读（CTE 可写、SELECT INTO、锁定读、副作用函数）。
// - **未知/解析失败按高风险**：不给"自动安全读"结论，而是 RequiresAuthorization。
// - 客户端策略只是纵深的一层：调用方还必须叠加连接只读配置与数据库账号权限。
// - 多语句一律拒绝（AI 路径不做 split/continue_on_error）。

/// 方言（决定 sqlparser 方言与后续驱动）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlDialectKind {
    MySql,
    Postgres,
    Sqlite,
}

impl SqlDialectKind {
    fn parser_dialect(self) -> Box<dyn sqlparser::dialect::Dialect> {
        match self {
            SqlDialectKind::MySql => Box::new(sqlparser::dialect::MySqlDialect {}),
            SqlDialectKind::Postgres => Box::new(sqlparser::dialect::PostgreSqlDialect {}),
            SqlDialectKind::Sqlite => Box::new(sqlparser::dialect::SQLiteDialect {}),
        }
    }
}

/// 风险分类结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlRisk {
    /// 只读、单语句、无已知副作用 → 可以在只读事务中自动执行。
    ReadOnly,
    /// 需要授权/审批：写操作，或无法保证只读（未知语法、锁定读、副作用函数）。
    RequiresAuthorization { reasons: Vec<String> },
    /// 直接拒绝：多语句等结构性禁止。
    Rejected { reasons: Vec<String> },
}

impl SqlRisk {
    pub fn is_read_only(&self) -> bool {
        matches!(self, SqlRisk::ReadOnly)
    }

    pub fn reasons(&self) -> &[String] {
        match self {
            SqlRisk::ReadOnly => &[],
            SqlRisk::RequiresAuthorization { reasons } | SqlRisk::Rejected { reasons } => reasons,
        }
    }
}

/// 已知有副作用/越权能力的函数或语法片段（命中即降级为需授权）。
///
/// 采用"文本包含"而不是精确函数解析：宁可误判为需授权，也不放过真实副作用。
const SIDE_EFFECT_MARKERS: &[&str] = &[
    "sleep(",
    "benchmark(",
    "get_lock(",
    "release_lock(",
    "load_file(",
    "into outfile",
    "into dumpfile",
    "load data",
    "pg_sleep(",
    "pg_advisory_lock(",
    "pg_try_advisory_lock(",
    "pg_read_file(",
    "pg_ls_dir(",
    "dblink(",
    "lo_import(",
    "lo_export(",
    "nextval(",
    "setval(",
    "copy ",
    "for update",
    "for share",
    "lock in share mode",
    "skip locked",
    "nowait",
];

/// 事务/会话控制：AI 单次调用不允许。
const TRANSACTION_MARKERS: &[&str] = &[
    "begin",
    "start transaction",
    "commit",
    "rollback",
    "savepoint",
    "set session",
    "set local",
    "set ",
];

/// 分析一条 SQL。
pub fn analyze_sql(sql: &str, dialect: SqlDialectKind) -> SqlRisk {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return SqlRisk::Rejected {
            reasons: vec!["empty".into()],
        };
    }
    let statements =
        match sqlparser::parser::Parser::parse_sql(dialect.parser_dialect().as_ref(), trimmed) {
            Ok(statements) => statements,
            // 解析失败 = 无法判定，按高风险处理（不是"安全读"）。
            Err(_) => {
                return SqlRisk::RequiresAuthorization {
                    reasons: vec!["unknown_syntax".into()],
                };
            }
        };
    if statements.len() != 1 {
        return SqlRisk::Rejected {
            reasons: vec![format!("multi_statement:{}", statements.len())],
        };
    }
    let statement = &statements[0];
    let text = statement.to_string().to_lowercase();
    let mut reasons: Vec<String> = Vec::new();

    // 事务/会话控制语句（BEGIN/COMMIT/SET 等）直接拒绝：AI 路径不做事务编排。
    if reasons.is_empty()
        && TRANSACTION_MARKERS
            .iter()
            .any(|m| text.trim_start().starts_with(m.trim_end()))
    {
        return SqlRisk::Rejected {
            reasons: vec!["transaction_or_session_control".into()],
        };
    }

    match statement {
        // 普通 EXPLAIN（不执行）允许；EXPLAIN ANALYZE 会真实执行 → 需授权。
        sqlparser::ast::Statement::Explain {
            analyze,
            options,
            statement,
            ..
        } => {
            // `EXPLAIN ANALYZE` 会真实执行被解释语句；PostgreSQL 的括号写法
            // `EXPLAIN (ANALYZE, ...)` 在 sqlparser 中落到 `options` 而不是 `analyze` 布尔值，
            // 因此必须同时检查两者，否则会被误判为只读并自动执行。
            if *analyze || explain_options_request_analyze(options.as_deref()) {
                reasons.push("explain_analyze".into());
            }
            // EXPLAIN 继承被解释语句的只读性：对写/DDL 做 EXPLAIN ANALYZE 会产生副作用。
            match statement.as_ref() {
                sqlparser::ast::Statement::Query(query) => classify_query(query, &mut reasons),
                other => reasons.push(format!(
                    "explain_non_read_statement:{}",
                    variant_name(other)
                )),
            }
        }
        sqlparser::ast::Statement::Query(query) => classify_query(query, &mut reasons),
        // 其余语句（INSERT/UPDATE/DELETE/DDL/GRANT/COPY/SET/…）都不是只读。
        other => {
            reasons.push(format!("non_read_statement:{}", variant_name(other)));
        }
    }

    if let Some(marker) = SIDE_EFFECT_MARKERS.iter().find(|m| text.contains(**m)) {
        reasons.push(format!("side_effect_marker:{marker}"));
    }

    if reasons.is_empty() {
        SqlRisk::ReadOnly
    } else {
        SqlRisk::RequiresAuthorization { reasons }
    }
}

fn classify_query(query: &sqlparser::ast::Query, reasons: &mut Vec<String>) {
    if !query.locks.is_empty() {
        reasons.push("locking_read".into());
    }
    if query.for_clause.is_some() {
        reasons.push("for_clause".into());
    }
    classify_set_expr(&query.body, reasons);
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            classify_set_expr(&cte.query.body, reasons);
        }
    }
}

fn classify_set_expr(body: &sqlparser::ast::SetExpr, reasons: &mut Vec<String>) {
    match body {
        sqlparser::ast::SetExpr::Select(select) => {
            if select.into.is_some() {
                reasons.push("select_into".into());
            }
        }
        sqlparser::ast::SetExpr::Query(inner) => classify_query(inner, reasons),
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            classify_set_expr(left, reasons);
            classify_set_expr(right, reasons);
        }
        // CTE / 集合表达式里的修改语句。
        sqlparser::ast::SetExpr::Insert(_) => reasons.push("modifying_cte_insert".into()),
        sqlparser::ast::SetExpr::Update(_) => reasons.push("modifying_cte_update".into()),
        sqlparser::ast::SetExpr::Delete(_) => reasons.push("modifying_cte_delete".into()),
        sqlparser::ast::SetExpr::Merge(_) => reasons.push("modifying_cte_merge".into()),
        _ => {}
    }
}

/// `EXPLAIN (ANALYZE [, ...])` 是否显式请求执行式计划。
///
/// 只认名为 `analyze`（大小写不敏感）且未被显式关闭的选项；`ANALYZE false` 视为未请求。
fn explain_options_request_analyze(options: Option<&[sqlparser::ast::UtilityOption]>) -> bool {
    let Some(options) = options else {
        return false;
    };
    options.iter().any(|option| {
        option.name.value.eq_ignore_ascii_case("analyze")
            && !matches!(
                &option.arg,
                Some(sqlparser::ast::Expr::Value(sqlparser::ast::ValueWithSpan {
                    value: sqlparser::ast::Value::Boolean(false),
                    ..
                }))
            )
    })
}

/// 语句变体名（用于可见原因，不泄漏 SQL 文本）。
fn variant_name(statement: &sqlparser::ast::Statement) -> String {
    statement
        .to_string()
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_lowercase()
}

/// `db.query` 的输入 schema（设计 §21.3）。
pub const DB_QUERY_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
    "parameters": { "type": "array", "maxItems": 1024 },
    "max_rows": { "type": "integer", "minimum": 1, "maximum": 1000 },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 30000 }
  }
}"#;

/// `db.execute` 的输入 schema：修改专用入口（审批绑定 plan hash）。
pub const DB_EXECUTE_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
    "parameters": { "type": "array", "maxItems": 1024 },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 30000 }
  }
}"#;

/// `db.explain` 的输入 schema（默认不执行 ANALYZE）。
pub const DB_EXPLAIN_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
    "mode": { "type": "string", "enum": ["plan", "execute"] }
  }
}"#;

/// 按分类决定工具入口：只读走 `db.query`，其余必须走 `db.execute`（审批）。
pub fn route_for_sql(sql: &str, dialect: SqlDialectKind) -> (&'static str, SqlRisk) {
    let risk = analyze_sql(sql, dialect);
    let tool = match &risk {
        SqlRisk::ReadOnly => "db.query",
        SqlRisk::RequiresAuthorization { .. } | SqlRisk::Rejected { .. } => "db.execute",
    };
    (tool, risk)
}

/// 构造 `db.query` / `db.execute` / `db.explain` 三个标准 SQL 工具的定义。
///
/// - `db.query`：只读入口（Read）；
/// - `db.execute`：修改专用入口（Write，必须走审批）；
/// - `db.explain`：默认不执行（Read），`mode=execute` 时由策略层降级为需审批。
pub fn standard_sql_tool_specs() -> fluxdb_core::Result<Vec<ToolSpec>> {
    let descriptors = [
        fluxdb_core::ToolDescriptor {
            id: "db.query".into(),
            version: 1,
            description: "单条受控只读查询（驱动绑定参数、有界结果）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: DB_QUERY_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "db.execute".into(),
            version: 1,
            description: "单条修改语句（范围授权 + 审批绑定 plan hash）".into(),
            risk: fluxdb_core::ToolRisk::Write,
            input_schema_json: DB_EXECUTE_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "db.explain".into(),
            version: 1,
            description: "查询计划（默认不执行；执行式计划另需审批）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: DB_EXPLAIN_INPUT_SCHEMA.into(),
        },
    ];
    descriptors
        .iter()
        .map(ToolSpec::from_descriptor)
        .collect::<fluxdb_core::Result<Vec<_>>>()
}
