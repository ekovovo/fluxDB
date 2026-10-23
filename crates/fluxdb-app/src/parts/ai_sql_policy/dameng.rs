use fluxdb_core::DatabaseKind;
use sqlparser::dialect::{Dialect, PostgreSqlDialect};

use super::{NamespaceScope, SqlPolicy};

pub static DAMENG: DamengPolicy = DamengPolicy;

pub struct DamengPolicy;

impl SqlPolicy for DamengPolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Dameng
    }

    fn label(&self) -> &'static str {
        "dameng"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        // 达梦与 Oracle/PG 的公共语法最接近，现有补全也使用 PostgreSQL 方言。
        &PostgreSqlDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
        &[
            // 阻塞/计时/会话锁。
            "sleep",
            "dbms_lock.sleep",
            "dbms_lock.allocate_unique",
            "dbms_lock.request",
            "dbms_lock.release",
            // 文件、网络与外部程序。
            "utl_file.fopen",
            "utl_file.fremove",
            "utl_file.frename",
            "utl_http.request",
            "utl_http.begin_request",
            "utl_inaddr.get_host_address",
            "utl_inaddr.get_host_name",
            "dbms_pipe.pack_message",
            "dbms_pipe.send_message",
            // 作业/调度与服务端状态。
            "dbms_scheduler.create_job",
            "dbms_scheduler.run_job",
            "dbms_scheduler.drop_job",
            "dbms_job.submit",
            "dbms_job.run",
            "dbms_job.remove",
            "dbms_session.set_role",
            "dbms_session.modify_package_state",
            "dbms_utility.exec_ddl_statement",
            "dbms_sql.execute",
            "dbms_sql.parse",
            // 统计/审计/备份管理。
            "dbms_stats.gather_table_stats",
            "dbms_stats.gather_schema_stats",
            "dbms_audit_mgmt",
            "dbms_backup_restore",
            "dbms_redefinition",
            // 序列推进与闪回。
            "nextval",
            "setval",
            "dbms_flashback.enable_at_time",
            "dbms_flashback.disable",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
        &[
            // 账号、角色与口令：泄露主体与权限模型。
            "sys.sysuser$",
            "sys.sysobjects",
            "sys.sysrole$",
            "sys.sysauth$",
            "sys.sysgrants",
            "sys.sysprops$",
            // 会话/锁/等待：跨会话信息与可被终止的目标。
            "sys.v$session",
            "sys.v$lock",
            "sys.v$lock_wait",
            "sys.v$transaction",
            "sys.v$thread",
            "sys.v$process",
            // 实例/配置/存储。
            "sys.v$instance",
            "sys.v$parameter",
            "sys.v$dm_instance",
            "sys.v$database",
            "sys.v$datafile",
            "sys.v$tablespace",
            "sys.v$logfile",
            "sys.v$rlog",
            "sys.v$backup",
            // 作业/调度/审计。
            "sys.sysjobs",
            "sys.sysjobsteps",
            "sys.sysjobhistory",
            "sys.sysauditors",
            // 其他会话可见的统计视图。
            "sys.v$sysstat",
            "sys.v$session_wait",
            "sys.v$session_event",
            "sys.v$sql_statistic",
        ]
    }

    fn extra_transaction_markers(&self) -> &'static [&'static str] {
        // 达梦支持 Oracle 风格的会话/角色切换，AI 单语句里一律拒绝。
        &["alter session", "set role", "set schema"]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::Schema
    }

    fn read_only_show_names(&self) -> &'static [&'static str] {
        // 达梦兼容部分 Oracle 的 SHOW 语法；这里只放行确定无副作用的几项。
        &["PARAMETER", "DATABASE", "INSTANCE"]
    }
}

#[cfg(test)]
mod tests {
    use super::DAMENG;
    use crate::{SqlRisk, analyze_sql};

    #[test]
    fn plain_dameng_select_is_read_only() {
        for sql in [
            "SELECT * FROM SYSDBA.CUSTOMERS",
            "SELECT NVL(NAME, 'n/a') AS name FROM SYSDBA.CUSTOMERS WHERE ROWNUM <= 10",
            "WITH c AS (SELECT 1 AS n FROM DUAL) SELECT n FROM c",
        ] {
            assert_eq!(analyze_sql(sql, &DAMENG), SqlRisk::ReadOnly, "{sql}");
        }
    }

    #[test]
    fn dameng_writes_and_ddl_require_authorization() {
        for sql in [
            "UPDATE SYSDBA.CUSTOMERS SET NAME = 'x' WHERE ID = 1",
            "DELETE FROM SYSDBA.CUSTOMERS WHERE ID = 1",
            "MERGE INTO SYSDBA.CUSTOMERS t USING SYSDBA.SRC s ON (t.ID = s.ID) WHEN MATCHED THEN UPDATE SET t.NAME = s.NAME",
            "TRUNCATE TABLE SYSDBA.CUSTOMERS",
            "DROP TABLE SYSDBA.CUSTOMERS",
            // SELECT INTO 会建表。
            "SELECT * INTO SYSDBA.CUSTOMERS_BAK FROM SYSDBA.CUSTOMERS",
        ] {
            assert!(
                matches!(
                    analyze_sql(sql, &DAMENG),
                    SqlRisk::RequiresAuthorization { .. }
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn dameng_dangerous_packages_and_privileged_views_are_flagged() {
        for sql in [
            "SELECT UTL_HTTP.REQUEST('http://127.0.0.1') FROM DUAL",
            "SELECT UTL_FILE.FOPEN('DIR', 'a.txt', 'w') FROM DUAL",
            "CALL DBMS_SCHEDULER.CREATE_JOB('J')",
            "SELECT * FROM SYS.SYSUSER$",
            "SELECT * FROM SYS.V$SESSION",
            "SELECT * FROM SYS.V$LOCK",
            "SELECT * FROM SYS.V$DATABASE",
        ] {
            assert!(
                !matches!(analyze_sql(sql, &DAMENG), SqlRisk::ReadOnly),
                "{sql} must not be auto-read"
            );
        }
    }

    #[test]
    fn dameng_session_control_is_rejected() {
        for sql in ["SET ROLE DBA", "COMMIT", "SAVEPOINT sp1"] {
            assert!(
                matches!(analyze_sql(sql, &DAMENG), SqlRisk::Rejected { .. }),
                "{sql}"
            );
        }
    }

    #[test]
    fn dameng_alter_session_is_never_read_only() {
        // sqlparser 的 PG 方言不解析 `ALTER SESSION SET SCHEMA`，策略按未知语法
        // 降级为需授权；绝不能因为“解析失败”而放行为只读。
        assert!(!analyze_sql("ALTER SESSION SET SCHEMA FLUXSALES", &DAMENG).is_read_only());
        // `SET SCHEMA` 在 PG 方言里可能解析成其他 Set 变体；同样必须非只读。
        assert!(!analyze_sql("SET SCHEMA SYSDBA", &DAMENG).is_read_only());
    }
}
