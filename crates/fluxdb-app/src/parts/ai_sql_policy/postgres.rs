use fluxdb_core::DatabaseKind;
use sqlparser::dialect::{Dialect, PostgreSqlDialect};

use super::{NamespaceScope, SqlPolicy};

pub static POSTGRES: PostgresPolicy = PostgresPolicy;

pub struct PostgresPolicy;

impl SqlPolicy for PostgresPolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Postgres
    }

    fn label(&self) -> &'static str {
        "postgresql"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        &PostgreSqlDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
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
            // 序列：取值会推进序列状态。
            "nextval",
            "setval",
            // 外部连接/通知/会话与集群范围配置写入。
            "dblink",
            "dblink_exec",
            "dblink_connect",
            "pg_notify",
            "set_config",
            // 服务端统计重置。
            "pg_stat_reset",
            "pg_stat_reset_shared",
            "pg_stat_reset_single_table_counters",
            "pg_stat_reset_single_function_counters",
            "pg_stat_reset_replication_slot",
            "pg_clear_relation_stats",
            // WAL/复制/备份/恢复。
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
            "pg_logdir_ls",
            // 会话/事务状态写入。
            "setseed",
            "txid_current",
            "pg_current_xact_id",
            "current_setting",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
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
        ]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::Schema
    }

    fn read_only_show_names(&self) -> &'static [&'static str] {
        &[
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
        ]
    }
}
