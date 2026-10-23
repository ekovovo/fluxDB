use fluxdb_core::DatabaseKind;
use sqlparser::dialect::{Dialect, SQLiteDialect};

use super::{NamespaceScope, SqlPolicy};

pub static SQLITE: SqlitePolicy = SqlitePolicy;

pub struct SqlitePolicy;

impl SqlPolicy for SqlitePolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Sqlite
    }

    fn label(&self) -> &'static str {
        "sqlite"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        &SQLiteDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
        &[
            "load_extension",
            "readfile",
            "writefile",
            "fts3_tokenizer",
            "edit",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
        &["sqlite_master", "sqlite_schema"]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::AttachedDatabase
    }

    fn read_only_pragmas(&self) -> &'static [&'static str] {
        &[
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
        ]
    }
}
