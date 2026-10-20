// SQL 风险分类与只读保证（设计 §21，T023/T024）。
//
// 原则：
// - **不靠前缀猜安全**：`SELECT/WITH` 开头不等于只读（CTE 可写、SELECT INTO、锁定读）。
// - **只拦已知危险，未知放行**：借鉴 Apache Superset 的 `is_mutating()` + 函数/表 denylist
//   （见 `superset/sql/parse.py`、`superset/config.py` 的 `DISALLOWED_SQL_FUNCTIONS`/`DISALLOWED_SQL_TABLES`）：
//   结构上判定为读（SELECT/SHOW/DESCRIBE/EXPLAIN/只读 PRAGMA）且未命中已知危险函数/系统表时，
//   即可自动只读执行；用户自定义或扩展函数不逐个枚举，交由数据库权限与只读配置约束。
//   早先的「不在纯函数白名单内即需授权」会让每个漏列的常用函数（如 `date('now')`）都误伤只读查询，
//   因此改为 denylist。
// - **解析失败按高风险**：无法解析时给不出只读结论，走 RequiresAuthorization。
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

    /// denylist 键与系统表判定使用的稳定标签。
    fn as_str(self) -> &'static str {
        match self {
            SqlDialectKind::MySql => "mysql",
            SqlDialectKind::Postgres => "postgresql",
            SqlDialectKind::Sqlite => "sqlite",
        }
    }
}

/// 风险分类结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlRisk {
    /// 只读、单语句、无已知副作用 → 可以在只读事务中自动执行。
    ReadOnly,
    /// 需要授权/审批：写操作，或无法保证只读（未知语法、锁定读、危险函数/系统表）。
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

/// 有副作用/越权能力函数的 denylist（借鉴 Superset `DISALLOWED_SQL_FUNCTIONS`）。
///
/// 命中即降级为需授权。这是一份**黑名单**：其余函数（含用户自定义/扩展函数）默认按只读处理，
/// 由数据库账号权限与连接只读配置承担最终约束。比对按最后一段标识符、大小写不敏感，
/// 因此 `pg_catalog."pg_notify"`、`SETVAL` 等限定/带引号写法同样命中。
///
/// 只列真正有副作用、访问文件/网络/锁、改写服务端状态或可读取任意会话配置的函数；
/// `current_user`/`version()`/`database()` 这类纯身份/版本自省不在其列——fluxDB 已有的只读
/// 元数据入口（`SHOW server_version`、`SHOW search_path` 等）本就允许同类信息披露，此处保持一致。
const SQL_FUNCTION_DENYLIST: &[(&str, &[&str])] = &[
    (
        "postgresql",
        &[
            // 阻塞/计时：可长时间占用连接。
            "pg_sleep",
            "pg_sleep_for",
            "pg_sleep_until",
            // 进程/会话控制：可终止其他后端或重载服务端配置。
            "pg_terminate_backend",
            "pg_cancel_backend",
            "pg_reload_conf",
            // 文件/大对象：读写服务器文件系统或 pg_largeobject 目录。
            "pg_read_file",
            "pg_read_binary_file",
            "pg_write_file",
            "pg_ls_dir",
            "pg_ls_waldir",
            "pg_ls_logdir",
            "pg_ls_tmpdir",
            "pg_stat_file",
            "lo_import",
            "lo_export",
            "lo_unlink",
            "lo_create",
            "lo_creat",
            "lo_from_bytea",
            "lo_put",
            "lowrite",
            "lo_truncate",
            "lo_truncate64",
            // 咨询锁：获取/释放会改变跨会话可见的锁状态。
            "pg_advisory_lock",
            "pg_advisory_lock_shared",
            "pg_try_advisory_lock",
            "pg_try_advisory_lock_shared",
            "pg_advisory_unlock",
            "pg_advisory_unlock_shared",
            "pg_advisory_unlock_all",
            "pg_advisory_xact_lock",
            "pg_advisory_xact_lock_shared",
            "pg_try_advisory_xact_lock",
            "pg_try_advisory_xact_lock_shared",
            // 序列：取值会推进序列状态，影响后续调用者。
            "nextval",
            "setval",
            // 外部连接/通知/会话与集群范围配置写入。
            "dblink",
            "dblink_exec",
            "dblink_connect",
            "pg_notify",
            "set_config",
            // 服务端统计：会重置或改写统计视图。
            "pg_stat_reset",
            "pg_stat_reset_shared",
            "pg_stat_reset_single_table_counters",
            "pg_stat_reset_single_function_counters",
            "pg_stat_reset_replication_slot",
            "pg_clear_relation_stats",
            // WAL/复制/备份/恢复等管理操作：这些函数名不带写关键字，但会改写集群状态、
            // 推进 WAL、创建/删除复制槽或控制恢复，属于需授权的高影响能力。
            "pg_logical_emit_message",
            "pg_export_snapshot",
            "pg_switch_wal",
            "pg_rotate_logfile",
            "pg_promote",
            "pg_wal_replay_pause",
            "pg_wal_replay_resume",
            "pg_start_backup",
            "pg_stop_backup",
            "pg_backup_start",
            "pg_backup_stop",
            "pg_create_restore_point",
            "pg_drop_replication_slot",
            "pg_create_logical_replication_slot",
            "pg_create_physical_replication_slot",
            "pg_replication_slot_advance",
            "pg_replication_origin_create",
            "pg_replication_origin_drop",
            "pg_replication_origin_advance",
            "pg_import_system_collations",
            // 目录列举读取：可枚举服务端日志/数据目录内容。
            "pg_logdir_ls",
            // 非确定/会话状态：setseed 改会话随机种子；txid_current/pg_current_xact_id
            // 在当前事务尚未分配事务号时会分配一个并推进全局 XID 计数（非只读）。
            "setseed",
            "txid_current",
            "pg_current_xact_id",
            // 无界配置读取：可读出任意 GUC（超出固定的只读 SHOW 白名单）。
            "current_setting",
        ],
    ),
    (
        "mysql",
        &[
            // 阻塞/计时/命名锁：可长时间占用连接或改写锁状态。
            "sleep",
            "benchmark",
            "get_lock",
            "release_lock",
            "release_all_locks",
            // 文件访问。
            "load_file",
        ],
    ),
    (
        "sqlite",
        &[
            // 扩展加载与文件 I/O。
            "load_extension",
            "readfile",
            "writefile",
            "fts3_tokenizer",
            "edit",
        ],
    ),
];

/// 系统表/视图 denylist（借鉴 Superset `DISALLOWED_SQL_TABLES`）。
///
/// 命中会泄露全库元数据或其他会话信息的系统视图，降级为需授权。条目中带 `.` 的按
/// `schema.table` 精确匹配（qualifier 与表名都相等），裸名匹配任意 schema 下的同名表。
const SQL_TABLE_DENYLIST: &[(&str, &[&str])] = &[
    (
        "postgresql",
        &[
            "pg_stat_activity",
            "pg_roles",
            "pg_shadow",
            "pg_authid",
            "pg_settings",
            "pg_config",
            "pg_hba_file_rules",
            "pg_stat_ssl",
            "pg_stat_replication",
            "pg_stat_wal_receiver",
            "pg_user",
            "information_schema.tables",
            "information_schema.columns",
            "information_schema.schemata",
            "information_schema.views",
            "information_schema.routines",
            "information_schema.role_table_grants",
            "information_schema.role_column_grants",
            "information_schema.role_routine_grants",
            "information_schema.table_privileges",
            "information_schema.column_privileges",
            "information_schema.usage_privileges",
            "information_schema.key_column_usage",
            "information_schema.table_constraints",
            "information_schema.referential_constraints",
            "information_schema.view_table_usage",
            "information_schema.applicable_roles",
            "information_schema.enabled_roles",
        ],
    ),
    (
        "mysql",
        &[
            "mysql.user",
            "performance_schema.threads",
            "performance_schema.processlist",
        ],
    ),
    ("sqlite", &["sqlite_master", "sqlite_schema"]),
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

/// 函数名是否命中某方言的函数 denylist（按最后一段标识符、大小写不敏感）。
fn function_name_denied(dialect: SqlDialectKind, name: &str) -> bool {
    SQL_FUNCTION_DENYLIST
        .iter()
        .find(|(key, _)| *key == dialect.as_str())
        .is_some_and(|(_, names)| {
            names
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
        })
}

/// 对象名（`schema.table` / `table` / `db.table`）是否命中某方言的系统表 denylist。
fn relation_denied(dialect: SqlDialectKind, name: &sqlparser::ast::ObjectName) -> bool {
    use sqlparser::ast::ObjectNamePart;
    let Some((_, names)) = SQL_TABLE_DENYLIST
        .iter()
        .find(|(key, _)| *key == dialect.as_str())
    else {
        return false;
    };
    let parts: Vec<String> = name
        .0
        .iter()
        .filter_map(|part| match part {
            ObjectNamePart::Identifier(ident) => Some(ident.value.to_ascii_lowercase()),
            ObjectNamePart::Function(_) => None,
        })
        .collect();
    let Some(table) = parts.last() else {
        return false;
    };
    let qualifier = parts.iter().rev().nth(1);
    names.iter().any(|entry| match entry.split_once('.') {
        // 限定条目（`schema.table`）要求 schema 与表名都相等。
        Some((schema, wanted_table)) => {
            table == wanted_table && qualifier.is_some_and(|value| value == schema)
        }
        // 裸条目匹配任意 schema 下的同名表。
        None => table == entry,
    })
}

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

    // 函数 denylist：从 AST 函数标识符与 FROM 子句表函数两处收集名字。
    // 按标识符实际值匹配，schema 限定/带引号写法（`pg_catalog."nextval"`）同样命中。
    if let Some(name) = first_denied_function(statement, dialect) {
        reasons.push(format!("denied_function:{name}"));
    }

    // 锁定读：遍历整条语句里的所有 Query 节点（含标量子查询/派生表/CTE/EXPLAIN 内层），
    // 任一节点带 `lock_clause`（`FOR UPDATE/SHARE ...`）就降级。
    // 用 AST 而不是裸文本 `for update` 匹配：字面量 `SELECT 'for update'` 不能被误伤，
    // 嵌套子查询里的锁定读也不会被漏掉。`INTO OUTFILE`/`LOAD DATA` 这类越权语法在
    // sqlparser 里直接解析失败，已由上面的 unknown_syntax 兜底。
    if let Some(reason) = first_locking_query(statement) {
        reasons.push(reason.into());
    }

    // 系统表 denylist：整个表达式树里的关系引用（含 CTE/子查询/派生表）都要检查。
    if let Some(table) = first_denied_relation(statement, dialect) {
        reasons.push(format!("denied_table:{table}"));
    }

    if reasons.is_empty() {
        SqlRisk::ReadOnly
    } else {
        SqlRisk::RequiresAuthorization { reasons }
    }
}

/// 遍历整条语句，返回首个「锁定读」原因（`locking_read`/`for_clause`），无则 `None`。
///
/// 覆盖所有 `Query` 节点，因此标量子查询、派生表、集合查询、CTE 以及 EXPLAIN 内层语句里的
/// `FOR UPDATE`/`FOR SHARE`（`query.locks`）与 `query.for_clause` 都能被识别——只检查最外层
/// `Query` 会漏掉嵌套形态。这里不依赖文本 `for update` 匹配，字面量不受影响。
fn first_locking_query(statement: &sqlparser::ast::Statement) -> Option<&'static str> {
    use sqlparser::ast::{Query, Visit, Visitor};
    use std::ops::ControlFlow;

    struct LockScan;
    impl Visitor for LockScan {
        type Break = &'static str;

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<&'static str> {
            if !query.locks.is_empty() {
                return ControlFlow::Break("locking_read");
            }
            if query.for_clause.is_some() {
                return ControlFlow::Break("for_clause");
            }
            ControlFlow::Continue(())
        }
    }

    match statement.visit(&mut LockScan) {
        ControlFlow::Break(reason) => Some(reason),
        ControlFlow::Continue(()) => None,
    }
}

/// 遍历整条语句，返回首个命中方言函数 denylist 的函数名。
///
/// 同时覆盖 `Expr::Function`（普通函数调用）与 `FROM f(...)` 表函数——
/// 后者在 sqlparser 中是 `TableFactor::Table { args: Some(_) }` / `TableFactor::TableFunction`，
/// 不属于 `Expr::Function`，只扫表达式会漏掉（`SELECT * FROM my_srf(1)` 曾被误判为只读）。
fn first_denied_function(
    statement: &sqlparser::ast::Statement,
    dialect: SqlDialectKind,
) -> Option<String> {
    use sqlparser::ast::{Expr, TableFactor, Visit, Visitor};
    use std::ops::ControlFlow;

    struct DeniedFunctions {
        dialect: SqlDialectKind,
    }
    impl Visitor for DeniedFunctions {
        type Break = String;

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if let Expr::Function(function) = expr
                && let Some(name) = function.name.0.last().and_then(|part| part.as_ident())
                && function_name_denied(self.dialect, &name.value)
            {
                return ControlFlow::Break(name.value.to_ascii_lowercase());
            }
            ControlFlow::Continue(())
        }

        fn pre_visit_table_factor(&mut self, table_factor: &TableFactor) -> ControlFlow<String> {
            // FROM 位置的表函数：`Table` 带 args，或 `TABLE(expr)` 形态。
            let name = match table_factor {
                TableFactor::Table { name, args, .. } if args.is_some() => Some(name),
                TableFactor::TableFunction { expr, .. } => match expr {
                    Expr::Function(function) => Some(&function.name),
                    _ => None,
                },
                TableFactor::Function { name, .. } => Some(name),
                _ => None,
            };
            if let Some(name) = name
                && let Some(ident) = name.0.last().and_then(|part| part.as_ident())
                && function_name_denied(self.dialect, &ident.value)
            {
                return ControlFlow::Break(ident.value.to_ascii_lowercase());
            }
            ControlFlow::Continue(())
        }
    }

    match statement.visit(&mut DeniedFunctions { dialect }) {
        ControlFlow::Break(name) => Some(name),
        ControlFlow::Continue(()) => None,
    }
}

/// 遍历整条语句，返回首个命中方言系统表 denylist 的关系名。
fn first_denied_relation(
    statement: &sqlparser::ast::Statement,
    dialect: SqlDialectKind,
) -> Option<String> {
    use std::ops::ControlFlow;

    let mut found: Option<String> = None;
    let _ = sqlparser::ast::visit_relations(statement, |name| {
        if relation_denied(dialect, name) {
            found = Some(name.to_string().to_ascii_lowercase());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    found
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

/// `db.run` 的统一 SQL 入口：只读/写由内核分类，`mode=explain` 走计划。
pub const DB_RUN_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "sql"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sql": { "type": "string", "minLength": 1, "maxLength": 65536 },
    "parameters": { "type": "array", "maxItems": 1024, "items": { "$ref": "#/$defs/parameter" } },
    "max_rows": { "type": "integer", "minimum": 1, "maximum": 1000 },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 30000 },
    "mode": { "type": "string", "enum": ["auto", "explain"] }
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

/// 构造标准 SQL 工具的定义。
///
/// - `db.run`：统一执行入口（目录按最坏情况标 Write，单次调用按 SQL 分类）；
/// - `db.search_objects`/`db.describe_object`：只读元数据探索。
pub fn standard_sql_tool_specs() -> fluxdb_core::Result<Vec<ToolSpec>> {
    let descriptors = [
        fluxdb_core::ToolDescriptor {
            id: "db.run".into(),
            version: 1,
            description: "执行单条 SQL：只读语句直接返回结果，写语句需审批；mode=explain 只看计划。".into(),
            // 目录按最坏情况标注；单次调用的有效风险由 `effective_risk` 按 SQL 分类。
            // 只读执行策略下仍需保留该入口用于查询，见 `loop_tool_descriptors`。
            risk: fluxdb_core::ToolRisk::Write,
            input_schema_json: DB_RUN_INPUT_SCHEMA.into(),
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
    ];
    let mut specs = descriptors
        .iter()
        .map(ToolSpec::from_descriptor)
        .collect::<fluxdb_core::Result<Vec<_>>>()?;
    // 执行回执保留已取得行；通用层按预算裁完整前缀，不固定样本行数。
    for spec in &mut specs {
        if spec.id == "db.run" {
            spec.effect = crate::EffectClass::Mutating;
        }
        if spec.id == "db.run" {
            spec.output_shape = crate::ToolOutputShape::Sample {
                omit_field: "rows".into(),
                rows_omitted_field: Some("rows_omitted".into()),
            };
        }
    }
    Ok(specs)
}
