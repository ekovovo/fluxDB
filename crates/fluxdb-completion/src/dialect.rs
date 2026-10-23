//! 方言抽象：语法差异（解析器、关键字表、标识符引用）集中在这里。
//!
//! 接入新数据库 = 新增一个实现 + 静态实例，引擎零改动。

use sqlparser::dialect::{
    Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
};

/// 方言需要向引擎提供的语法事实。
pub trait CompletionDialect: Sync {
    /// sqlparser 解析方言。无专用方言时用 Generic/最接近的方言。
    fn parser_dialect(&self) -> &'static dyn Dialect;

    /// 关键字候选表（大写原文，含多词如 `LEFT JOIN`）。
    fn keywords(&self) -> &'static [&'static str];

    /// 内置函数候选表。
    fn functions(&self) -> &'static [&'static str];

    /// 标识符引用字符：MySQL 反引号，其余双引号。
    fn identifier_quote(&self) -> char {
        '"'
    }

    /// 需要引用时按方言包装标识符。
    fn quote_ident(&self, name: &str) -> String {
        let quote = self.identifier_quote();
        format!("{quote}{name}{quote}")
    }

    /// 裸表名补全是否必须带 schema 限定（达梦默认 schema 可能不是查询所在 schema）。
    fn qualify_bare_table(&self) -> bool {
        false
    }

    /// 是否支持过程/函数补全。
    fn supports_routines(&self) -> bool {
        false
    }
}

/// 常见内置函数的参数骨架，供函数签名提示（`COUNT(|` → `COUNT(expr)`）使用。
///
/// 名字与各方言 [`CompletionDialect::functions`] 表同形（大写）；没列进来的函数不给
/// 签名，提示层回退到只显示函数名。`label` 用 `NAME(参数, …)` 形式，渲染层按 `()` +
/// 逗号切参数区间并高亮当前参数。
const BUILTIN_FUNCTION_SIGNATURES: &[(&str, &str)] = &[
    ("ABS", "ABS(number)"),
    ("AVG", "AVG(expr)"),
    ("CAST", "CAST(expr AS type)"),
    ("COALESCE", "COALESCE(expr, ...)"),
    ("CONCAT", "CONCAT(str, ...)"),
    ("COUNT", "COUNT(expr)"),
    ("DATE", "DATE(timevalue, modifier, ...)"),
    ("DATETIME", "DATETIME(timevalue, modifier, ...)"),
    ("DATE_TRUNC", "DATE_TRUNC(field, source)"),
    ("EXTRACT", "EXTRACT(field FROM source)"),
    ("GENERATE_SERIES", "GENERATE_SERIES(start, stop, step)"),
    ("IFNULL", "IFNULL(expr, fallback)"),
    ("INSTR", "INSTR(str, substr)"),
    ("JSON_EXTRACT", "JSON_EXTRACT(json, path)"),
    ("JSONB_EXTRACT_PATH", "JSONB_EXTRACT_PATH(json, path, ...)"),
    ("LENGTH", "LENGTH(str)"),
    ("LOWER", "LOWER(str)"),
    ("MAX", "MAX(expr)"),
    ("MIN", "MIN(expr)"),
    ("NOW", "NOW()"),
    ("NULLIF", "NULLIF(expr, value)"),
    ("RANDOM", "RANDOM()"),
    ("REPLACE", "REPLACE(str, from, to)"),
    ("ROUND", "ROUND(number, decimals)"),
    ("STRFTIME", "STRFTIME(format, timevalue, ...)"),
    ("SUBSTR", "SUBSTR(str, start, length)"),
    ("SUBSTRING", "SUBSTRING(str, start, length)"),
    ("SUM", "SUM(expr)"),
    ("TIME", "TIME(timevalue, modifier, ...)"),
    ("TO_CHAR", "TO_CHAR(value, format)"),
    ("TO_DATE", "TO_DATE(text, format)"),
    ("TOTAL", "TOTAL(expr)"),
    ("TRIM", "TRIM(str)"),
    ("UPPER", "UPPER(str)"),
];

/// 查内置函数签名；`name` 大小写不敏感，未收录返回 `None`。
pub fn builtin_function_signature(name: &str) -> Option<&'static str> {
    BUILTIN_FUNCTION_SIGNATURES
        .iter()
        .find(|(function, _)| function.eq_ignore_ascii_case(name))
        .map(|(_, signature)| *signature)
}

const GENERIC_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "DATABASE",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "CALL",
    "SHOW",
    "DESCRIBE",
    "EXPLAIN",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "IF EXISTS",
    "IF NOT EXISTS",
];

const SQLITE_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE",
    "ALTER",
    "DROP",
    "TABLE",
    "VIEW",
    "INDEX",
    "TRIGGER",
    "PRAGMA",
    "EXPLAIN",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "IF EXISTS",
    "IF NOT EXISTS",
];

const POSTGRES_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "ON",
    "USING",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "ILIKE",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "RETURNING",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "SCHEMA",
    "DATABASE",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "CALL",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "INTERSECT",
    "EXCEPT",
    "WITH",
    "WINDOW",
    "OVER",
    "PARTITION BY",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "IF EXISTS",
    "IF NOT EXISTS",
];

/// 达梦：在 PG 词表基础上补达梦常用语法（TOP、MERGE INTO、ROWNUM）。
const DAMENG_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "LIMIT",
    "TOP",
    "ROWNUM",
    "OFFSET",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "MERGE INTO",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "SCHEMA",
    "DATABASE",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "CALL",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "IF EXISTS",
    "IF NOT EXISTS",
];

/// SQL Server / T-SQL 常用语法。分页使用 `TOP` / `OFFSET ... FETCH`，
/// 对象名可用方括号引用；解析器使用 sqlparser 的 MSSQL 方言。
const SQLSERVER_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "CROSS APPLY",
    "OUTER APPLY",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "OFFSET",
    "FETCH NEXT",
    "TOP",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "MERGE INTO",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "SCHEMA",
    "DATABASE",
    "PROCEDURE",
    "FUNCTION",
    "TRIGGER",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "CONSTRAINT",
    "DEFAULT",
    "UNIQUE",
    "CHECK",
    "IDENTITY",
    "EXEC",
    "DECLARE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRY",
    "CATCH",
    "THROW",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "WITH",
    "OVER",
    "PARTITION BY",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "IF EXISTS",
    "IF NOT EXISTS",
];

const SQLSERVER_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "ISNULL",
    "NULLIF",
    "CONCAT",
    "SUBSTRING",
    "LEN",
    "LOWER",
    "UPPER",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "REPLACE",
    "ROUND",
    "GETDATE",
    "SYSDATETIME",
    "DATEADD",
    "DATEDIFF",
    "DATEPART",
    "CONVERT",
    "CAST",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "IIF",
    "TRY_CONVERT",
    "SCOPE_IDENTITY",
];

const GENERIC_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "IFNULL",
    "NULLIF",
    "CONCAT",
    "SUBSTRING",
    "LOWER",
    "UPPER",
    "TRIM",
    "LENGTH",
    "ROUND",
    "NOW",
    "CAST",
];

const SQLITE_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "TOTAL",
    "COALESCE",
    "IFNULL",
    "NULLIF",
    "LOWER",
    "UPPER",
    "TRIM",
    "LENGTH",
    "SUBSTR",
    "INSTR",
    "REPLACE",
    "ROUND",
    "ABS",
    "RANDOM",
    "DATE",
    "TIME",
    "DATETIME",
    "JULIANDAY",
    "STRFTIME",
    "JSON_EXTRACT",
    "HEX",
    "QUOTE",
    "CAST",
];

const POSTGRES_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "NULLIF",
    "CONCAT",
    "SUBSTRING",
    "LOWER",
    "UPPER",
    "TRIM",
    "LENGTH",
    "ROUND",
    "NOW",
    "DATE_TRUNC",
    "EXTRACT",
    "TO_CHAR",
    "TO_DATE",
    "GENERATE_SERIES",
    "JSONB_EXTRACT_PATH",
    "CAST",
];

/// MySQL / TiDB。
pub struct MySqlCompletionDialect;

impl CompletionDialect for MySqlCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &MySqlDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        GENERIC_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        GENERIC_FUNCTIONS
    }

    fn identifier_quote(&self) -> char {
        '`'
    }

    fn supports_routines(&self) -> bool {
        true
    }
}

/// SQLite。
pub struct SqliteCompletionDialect;

impl CompletionDialect for SqliteCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &SQLiteDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        SQLITE_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        SQLITE_FUNCTIONS
    }

    fn supports_routines(&self) -> bool {
        true
    }
}

/// PostgreSQL。
pub struct PostgresCompletionDialect;

impl CompletionDialect for PostgresCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &PostgreSqlDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        POSTGRES_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        POSTGRES_FUNCTIONS
    }

    fn supports_routines(&self) -> bool {
        true
    }
}

/// 达梦：sqlparser 无专用方言，用 PostgreSQL 方言解析（与既有实现一致）；
/// 裸表名必须带 schema 限定，避免落到登录用户同名 schema 的误解。
pub struct DamengCompletionDialect;

impl CompletionDialect for DamengCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &PostgreSqlDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        DAMENG_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        POSTGRES_FUNCTIONS
    }

    fn qualify_bare_table(&self) -> bool {
        true
    }
}

/// SQL Server：sqlparser 的 MSSQL 方言；方括号引用标识符。
pub struct SqlServerCompletionDialect;

impl CompletionDialect for SqlServerCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &MsSqlDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        SQLSERVER_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        SQLSERVER_FUNCTIONS
    }

    fn identifier_quote(&self) -> char {
        '['
    }

    fn quote_ident(&self, name: &str) -> String {
        format!("[{}]", name.replace(']', "]]"))
    }

    // SQL Server 的默认 schema 可被登录用户改掉，且库内可能有同名表；
    // 裸表名补全显式带 schema，避免依赖会话默认值。
    fn qualify_bare_table(&self) -> bool {
        true
    }

    fn supports_routines(&self) -> bool {
        true
    }
}

/// 兜底方言：未知数据库类型。
pub struct GenericCompletionDialect;

impl CompletionDialect for GenericCompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect {
        &GenericDialect {}
    }

    fn keywords(&self) -> &'static [&'static str] {
        GENERIC_KEYWORDS
    }

    fn functions(&self) -> &'static [&'static str] {
        GENERIC_FUNCTIONS
    }
}

pub static MYSQL: MySqlCompletionDialect = MySqlCompletionDialect;
pub static SQLITE: SqliteCompletionDialect = SqliteCompletionDialect;
pub static POSTGRES: PostgresCompletionDialect = PostgresCompletionDialect;
pub static DAMENG: DamengCompletionDialect = DamengCompletionDialect;
pub static SQLSERVER: SqlServerCompletionDialect = SqlServerCompletionDialect;
pub static GENERIC: GenericCompletionDialect = GenericCompletionDialect;
