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
/// 函数按 AST 标识符匹配，语法片段保留保守文本检查；这不是纯函数白名单，
/// 未知函数的只读保障仍须由数据库权限/只读事务承担。
const SIDE_EFFECT_MARKERS: &[&str] = &[
    "sleep(",
    "benchmark(",
    "get_lock(",
    "release_lock(",
    "release_all_locks(",
    "load_file(",
    "into outfile",
    "into dumpfile",
    "load data",
    "pg_sleep(",
    "pg_advisory_lock(",
    "pg_advisory_lock_shared(",
    "pg_try_advisory_lock(",
    "pg_try_advisory_lock_shared(",
    "pg_advisory_unlock(",
    "pg_advisory_unlock_shared(",
    "pg_advisory_unlock_all(",
    "pg_read_file(",
    "pg_ls_dir(",
    "dblink(",
    "lo_import(",
    "lo_export(",
    "lo_unlink(",
    "nextval(",
    "setval(",
    "copy ",
    // 通知/外部连接/文件/进程类副作用：命中即降级为需授权（宁可误判，不放过真实副作用）。
    "pg_notify(",
    "pg_read_binary_file(",
    "pg_write_file(",
    "dblink_exec(",
    "dblink_connect(",
    "pg_terminate_backend(",
    "pg_cancel_backend(",
    "pg_advisory_xact_lock(",
    "pg_advisory_xact_lock_shared(",
    "pg_try_advisory_xact_lock(",
    "pg_try_advisory_xact_lock_shared(",
    "set_config(",
    "load_extension(",
    "pg_reload_conf(",
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

/// 可在只读入口自动执行的已知纯函数（不承诺覆盖所有方言函数）。
///
/// 不在集合内的函数统一需授权：宿主不能证明用户定义/扩展函数没有副作用、
/// 文件访问、锁、通知或消耗型序列读取，宁可拒绝自动执行。
const PURE_FUNCTION_NAMES: &[&str] = &[
    "abs", "sign", "ceil", "ceiling", "floor", "round", "trunc", "mod", "power", "pow", "sqrt",
    "exp", "ln", "log", "greatest", "least", "coalesce", "nullif", "length", "char_length",
    "character_length", "lower", "upper", "trim", "btrim", "ltrim", "rtrim", "substring",
    "substr", "concat", "replace", "left", "right", "lpad", "rpad", "position", "strpos",
    "ascii", "chr", "hex", "md5", "count", "sum", "min", "max", "avg", "bool_and", "bool_or",
    "now", "current_date", "current_timestamp", "date_trunc", "date_part", "to_regclass",
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
        // 元数据读取（SHOW / DESCRIBE / EXPLAIN <table> / SQLite PRAGMA 只读子集）：
        // 这些语句不改数据也不改结构，与 Superset 的 `is_mutating()` 一致不算「写」；
        // 但泄露面大的子集（库/模式清单、状态变量、进程列表）不给只读结论，
        // 留一个理由降级为需授权，由用户逐次审批。
        other if classify_metadata_read(other, dialect, &mut reasons) => {}
        // 其余语句（INSERT/UPDATE/DELETE/DDL/GRANT/COPY/SET/…）都不是只读。
        other => {
            reasons.push(format!("non_read_statement:{}", variant_name(other)));
        }
    }

    // 使用标识符实际值识别 schema 限定/双引号函数名，不能靠格式化 SQL 中的 `name(`。
    let function_marker = sqlparser::ast::visit_expressions(statement, |expr| {
        if let sqlparser::ast::Expr::Function(function) = expr
            && let Some(name) = function.name.0.last().and_then(|part| part.as_ident())
            && let Some(marker) = SIDE_EFFECT_MARKERS.iter().find(|marker| {
                marker
                    .strip_suffix('(')
                    .is_some_and(|function_name| name.value.eq_ignore_ascii_case(function_name))
            })
        {
            return std::ops::ControlFlow::Break(*marker);
        }
        std::ops::ControlFlow::Continue(())
    });
    let marker = match function_marker {
        std::ops::ControlFlow::Break(marker) => Some(marker),
        std::ops::ControlFlow::Continue(()) => {
            SIDE_EFFECT_MARKERS.iter().copied().find(|marker| text.contains(marker))
        }
    };
    if let Some(marker) = marker {
        reasons.push(format!("side_effect_marker:{marker}"));
    }
    // 未知函数不能凭名字猜安全：不在纯函数集合内就降级为需授权。
    if sqlparser::ast::visit_expressions(statement, |expr| {
        if let sqlparser::ast::Expr::Function(function) = expr
            && let Some(name) = function.name.0.last().and_then(|part| part.as_ident())
            && !PURE_FUNCTION_NAMES
                .iter()
                .any(|pure| name.value.eq_ignore_ascii_case(pure))
        {
            return std::ops::ControlFlow::Break(name.value.to_ascii_lowercase());
        }
        std::ops::ControlFlow::Continue(())
    })
    .is_break()
    {
        reasons.push("unknown_function".into());
    }

    if reasons.is_empty() {
        SqlRisk::ReadOnly
    } else {
        SqlRisk::RequiresAuthorization { reasons }
    }
}

/// MySQL `SHOW <对象>` 的只读子集。MySQL 的这类语句解析成 `ShowVariable`，
/// 首关键字是动作名、其后是对象名，因此只匹配首关键字（对象名不做文本匹配）。
const MYSQL_READ_ONLY_SHOW_HEADS: &[&str] = &[
    "INDEX", "INDEXES", "KEYS", "COLUMNS", "FIELDS", "TRIGGERS",
];

/// PostgreSQL `SHOW <名称>` 的只读子集。PG 的这类语句后面不带对象参数，
/// 整体就是一个固定短语，因此按整段名称精确匹配。
const POSTGRES_READ_ONLY_SHOW_NAMES: &[&str] = &[
    "SEARCH_PATH",
    "SERVER_VERSION",
    "SERVER_VERSION_NUM",
    "TIME ZONE",
    "TRANSACTION ISOLATION LEVEL",
    "TRANSACTION READ ONLY",
    "DEFAULT TRANSACTION ISOLATION LEVEL",
    "DEFAULT TRANSACTION READ ONLY",
    "DEFAULT_TRANSACTION_ISOLATION_LEVEL",
    "DEFAULT_TRANSACTION_READ_ONLY",
    "TRANSACTION_ISOLATION_LEVEL",
    "CLIENT_ENCODING",
    "STANDARD_CONFORMING_STRINGS",
    "INTEGER_DATETIMES",
];

/// SQLite `PRAGMA <名称>` 的只读子集；对象查询可带对象参数，其余只能无值读取。
const SQLITE_READ_ONLY_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "database_list",
    "schema_version",
    "user_version",
    "data_version",
    "application_id",
    "freelist_count",
    "page_count",
    "encoding",
];

/// 判断一条非查询语句是否属于「元数据读取」。
///
/// 返回 `true` 表示已归类：`reasons` 为空即为只读，有理由则按需授权处理。
/// 返回 `false` 表示不是元数据语句，由调用方按普通非只读语句处理。
fn classify_metadata_read(
    statement: &sqlparser::ast::Statement,
    dialect: SqlDialectKind,
    reasons: &mut Vec<String>,
) -> bool {
    match statement {
        // `SHOW CREATE TABLE/VIEW/TRIGGER` 读 DDL 文本；不命中就按普通 SHOW 拒绝。
        sqlparser::ast::Statement::ShowCreate { obj_type, .. } => {
            use sqlparser::ast::ShowCreateObject;
            return if matches!(
                obj_type,
                ShowCreateObject::Table | ShowCreateObject::View | ShowCreateObject::Trigger
            ) {
                true
            } else {
                reasons.push("metadata_disclosure:show_create_object".into());
                true
            };
        }
        // `DESCRIBE t` / `DESC t` / `EXPLAIN t`：读表结构，不执行被解释语句。
        sqlparser::ast::Statement::ExplainTable { .. } => true,
        sqlparser::ast::Statement::ShowColumns { .. }
        | sqlparser::ast::Statement::ShowTables { .. }
        | sqlparser::ast::Statement::ShowViews { .. }
        | sqlparser::ast::Statement::ShowCharset(_)
        | sqlparser::ast::Statement::ShowCollation { .. } => true,
        // 库/模式清单会越过本轮目标作用域，状态变量与进程列表会泄露其他会话信息。
        sqlparser::ast::Statement::ShowDatabases { .. } => {
            reasons.push("metadata_disclosure:show_databases".into());
            true
        }
        sqlparser::ast::Statement::ShowSchemas { .. } => {
            reasons.push("metadata_disclosure:show_schemas".into());
            true
        }
        sqlparser::ast::Statement::ShowCatalogs { .. } => {
            reasons.push("metadata_disclosure:show_catalogs".into());
            true
        }
        sqlparser::ast::Statement::ShowStatus { .. } => {
            reasons.push("metadata_disclosure:show_status".into());
            true
        }
        sqlparser::ast::Statement::ShowVariables { .. } => {
            reasons.push("metadata_disclosure:show_variables".into());
            true
        }
        sqlparser::ast::Statement::ShowProcessList { .. } => {
            reasons.push("metadata_disclosure:show_processlist".into());
            true
        }
        sqlparser::ast::Statement::ShowVariable { variable } => {
            let head = variable
                .first()
                .map(|ident| ident.value.to_ascii_uppercase())
                .unwrap_or_default();
            let name = variable
                .iter()
                .map(|ident| ident.value.to_ascii_uppercase())
                .collect::<Vec<_>>()
                .join(" ");
            let read_only = match dialect {
                SqlDialectKind::MySql => MYSQL_READ_ONLY_SHOW_HEADS.contains(&head.as_str()),
                SqlDialectKind::Postgres => POSTGRES_READ_ONLY_SHOW_NAMES.contains(&name.as_str()),
                // SQLite 没有 SHOW；出现即语法不成立，不给只读结论。
                SqlDialectKind::Sqlite => false,
            };
            if !read_only {
                reasons.push(format!("metadata_disclosure:show_{}", head.to_lowercase()));
            }
            true
        }
        sqlparser::ast::Statement::Pragma { name, value, is_eq } => {
            // 赋值既支持 `x = y` 也支持 `x(y)`；仅对象查询允许括号内的对象参数。
            if dialect == SqlDialectKind::Sqlite && !*is_eq {
                let pragma = name
                    .0
                    .last()
                    .and_then(|part| part.as_ident())
                    .map(|ident| ident.value.to_ascii_lowercase())
                    .unwrap_or_default();
                if SQLITE_READ_ONLY_PRAGMAS.contains(&pragma.as_str()) {
                    return value.is_none()
                        || matches!(
                            pragma.as_str(),
                            "table_info" | "table_xinfo" | "table_list" | "index_list"
                                | "index_info" | "index_xinfo" | "foreign_key_list"
                        );
                }
                reasons.push(format!("metadata_disclosure:pragma_{pragma}"));
                return true;
            }
            false
        }
        _ => false,
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
            // 递归整段 CTE 查询（含嵌套 WITH），不能只看最外层 body：
            // `WITH outer AS (WITH inner AS (DELETE ...) SELECT ...) SELECT ...` 必须被识别。
            classify_query(&cte.query, reasons);
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
pub const DB_QUERY_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
        "parameters": { "type": "array", "maxItems": 1024, "items": { "$ref": "#/$defs/parameter" } },
    "max_rows": { "type": "integer", "minimum": 1, "maximum": 1000 },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 30000 }
  },
  "$defs": {
    "parameter": {
      "oneOf": [
        { "type": "object", "additionalProperties": false, "required": ["type"], "properties": { "type": { "const": "null" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "bool" }, "value": { "type": "boolean" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "i64" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "u64" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "f64" }, "value": { "type": "number" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "decimal" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "text" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "bytes" }, "value": { "type": "string" } } }
      ]
    }
  }
}"##;

/// `db.execute` 的输入 schema：修改专用入口（审批绑定 plan hash）。
pub const DB_EXECUTE_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
        "parameters": { "type": "array", "maxItems": 1024, "items": { "$ref": "#/$defs/parameter" } },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 30000 }
  },
  "$defs": {
    "parameter": {
      "oneOf": [
        { "type": "object", "additionalProperties": false, "required": ["type"], "properties": { "type": { "const": "null" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "bool" }, "value": { "type": "boolean" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "i64" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "u64" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "f64" }, "value": { "type": "number" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "decimal" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "text" }, "value": { "type": "string" } } },
        { "type": "object", "additionalProperties": false, "required": ["type", "value"], "properties": { "type": { "const": "bytes" }, "value": { "type": "string" } } }
      ]
    }
  }
}"##;

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

/// `db.search_objects` 的输入 schema（渐进元数据，不 dump 全库 DDL）。
pub const DB_SEARCH_OBJECTS_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "query": { "type": "string", "maxLength": 256 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
  }
}"#;

/// `db.describe_object` 的输入 schema。
///
/// object_ref 使用结构化对象定位，不接受 SQL 片段或连接字符串，避免把标识符
/// 拼接责任泄漏到 App 层。未指定 include 时只返回列、索引和外键摘要。
pub const DB_DESCRIBE_OBJECT_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "object_ref"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "object_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["name", "kind"],
      "properties": {
        "database": { "type": "string", "maxLength": 256 },
        "schema": { "type": "string", "maxLength": 256 },
        "name": { "type": "string", "minLength": 1, "maxLength": 256 },
        "kind": { "type": "string", "enum": ["table", "view"] }
      }
    },
    "include": {
      "type": "array",
      "maxItems": 6,
      "items": {
        "type": "string",
        "enum": ["columns", "indexes", "foreign_keys", "comment", "ddl"]
      }
    }
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

/// 构造标准 SQL 工具的定义。
///
/// - `db.query`：只读入口（Read）；
/// - `db.execute`：修改专用入口（Write，必须走审批）；
/// - `db.explain`：默认不执行（Read），`mode=execute` 时由策略层降级为需审批；
/// - `db.search_objects`/`db.describe_object`：只读元数据探索。
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
            id: "db.search_objects".into(),
            version: 1,
            description: "按名字模糊搜索表/视图（渐进元数据，只读）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: DB_SEARCH_OBJECTS_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "db.describe_object".into(),
            version: 1,
            description: "读取表或视图的列、索引、外键、注释和可选 DDL（只读）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: DB_DESCRIBE_OBJECT_INPUT_SCHEMA.into(),
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
