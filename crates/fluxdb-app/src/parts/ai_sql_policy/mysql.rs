use fluxdb_core::DatabaseKind;
use sqlparser::dialect::{Dialect, MySqlDialect};

use super::{NamespaceScope, SqlPolicy};

pub static MYSQL: MySqlPolicy = MySqlPolicy;

pub struct MySqlPolicy;

impl SqlPolicy for MySqlPolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::MySql
    }

    fn label(&self) -> &'static str {
        "mysql"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        &MySqlDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
        &[
            // 阻塞/计时/命名锁：可长时间占用连接或改写锁状态。
            "sleep",
            "benchmark",
            "get_lock",
            "release_lock",
            "release_all_locks",
            // 文件访问。
            "load_file",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
        &[
            "mysql.user",
            "performance_schema.threads",
            "performance_schema.processlist",
        ]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::Database
    }

    fn read_only_show_heads(&self) -> &'static [&'static str] {
        &["INDEX", "INDEXES", "KEYS", "COLUMNS", "FIELDS", "TRIGGERS"]
    }
}
