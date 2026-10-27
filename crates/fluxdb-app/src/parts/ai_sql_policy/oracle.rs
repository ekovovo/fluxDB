use fluxdb_completion::CompletionDialect as _;
use fluxdb_core::DatabaseKind;
use sqlparser::ast::Expr;
use sqlparser::dialect::{Dialect, OracleDialect};

use super::{ExplainMode, NamespaceScope, SqlPolicy};

pub static ORACLE: OraclePolicy = OraclePolicy;

pub struct OraclePolicy;

impl SqlPolicy for OraclePolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::Oracle
    }

    fn label(&self) -> &'static str {
        "oracle"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        &OracleDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
        &[
            // 文件、网络、外部程序与动态 SQL。
            "utl_file.fopen",
            "utl_file.fremove",
            "utl_file.frename",
            "utl_http.request",
            "utl_http.begin_request",
            "utl_tcp.open_connection",
            "utl_smtp.open_data",
            "utl_inaddr.get_host_address",
            "utl_inaddr.get_host_name",
            "dbms_pipe.pack_message",
            "dbms_pipe.send_message",
            "dbms_sql.execute",
            "dbms_sql.parse",
            "dbms_utility.exec_ddl_statement",
            // 调度、会话与统计。
            "dbms_scheduler.create_job",
            "dbms_scheduler.run_job",
            "dbms_scheduler.drop_job",
            "dbms_job.submit",
            "dbms_job.run",
            "dbms_session.set_role",
            "dbms_session.modify_package_state",
            "dbms_stats.gather_table_stats",
            "dbms_stats.gather_schema_stats",
            // 序列推进与闪回。
            "nextval",
            "currval",
            "dbms_flashback.enable_at_time",
            "dbms_flashback.disable",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
        &[
            // 账号、口令与角色：泄露主体与权限模型。
            "sys.user$",
            "sys.dba_users",
            "sys.dba_role_privs",
            "sys.dba_sys_privs",
            "sys.dba_tab_privs",
            // 会话/锁/等待：跨会话信息与可被终止的目标。
            "v$session",
            "v$lock",
            "v$transaction",
            "v$process",
            "gv$session",
            // 实例、配置与存储。
            "v$instance",
            "v$parameter",
            "v$database",
            "v$datafile",
            "v$tablespace",
            "v$logfile",
            // 审计与作业。
            "sys.aud$",
            "sys.registry$",
            "dba_jobs",
            "dba_scheduler_jobs",
        ]
    }

    fn extra_transaction_markers(&self) -> &'static [&'static str] {
        &["alter session", "set role", "set schema"]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::Schema
    }

    /// Oracle 的用户函数允许自治事务与隐式提交，包成员更无法从文本判断副作用。
    /// 只有能对上内置函数表才算无副作用；其余一律进入审批。
    fn known_functions(&self) -> Option<&'static [&'static str]> {
        // 直接复用补全方言的内置表，避免两份函数清单漂移。
        Some(fluxdb_completion::ORACLE.functions())
    }

    fn explain_mode(&self) -> ExplainMode {
        // Oracle 没有 `EXPLAIN <sql>` 语法：它只认 `EXPLAIN PLAN FOR`，而那条语句会写入
        // PLAN_TABLE，属于有副作用（且隐式提交）的写操作。默认的 Prefix 会给语句拼上
        // `EXPLAIN`，得到 Oracle 无法解析的 SQL，还会让模型误以为拿到了计划。
        ExplainMode::Unsupported
    }

    fn expression_side_effect(&self, expr: &Expr) -> Option<&'static str> {
        // `seq.NEXTVAL` 解析成复合标识符而不是函数调用，但会推进序列。
        let Expr::CompoundIdentifier(parts) = expr else {
            return None;
        };
        let last = parts.last()?.value.to_ascii_uppercase();
        if matches!(last.as_str(), "NEXTVAL" | "CURRVAL") {
            return Some("sequence_progression");
        }
        // `DBMS_RANDOM.VALUE` 这类无参包函数同样解析成复合标识符，
        // 不会进函数白名单；按包名判定，避免包成员被当成普通列读取。
        parts
            .first()
            .map(|part| part.value.to_ascii_uppercase())
            .filter(|package| package.starts_with("DBMS_") || package.starts_with("UTL_"))
            .map(|_| "package_member_access")
    }

    fn relation_scope_reason(&self, full_name: &str) -> Option<&'static str> {
        // `table@dblink` 会跨库读取，实际访问范围无法从语句文本证明。
        full_name.contains('@').then_some("database_link")
    }

    fn read_only_show_names(&self) -> &'static [&'static str] {
        &["PARAMETER"]
    }
}

#[cfg(test)]
mod tests {
    use super::ORACLE;
    // `explain_mode` 是 trait 方法，测试里调用前必须先引入 trait。
    use super::SqlPolicy as _;
    use crate::{SqlRisk, analyze_sql};

    #[test]
    fn plain_oracle_select_is_read_only() {
        for sql in [
            "SELECT * FROM HR.EMPLOYEES",
            "SELECT NVL(NAME, 'n/a') AS name FROM HR.EMPLOYEES WHERE ROWNUM <= 10",
            "WITH c AS (SELECT 1 AS n FROM DUAL) SELECT n FROM c",
            "SELECT TO_CHAR(HIRE_DATE, 'YYYY-MM-DD') FROM HR.EMPLOYEES",
        ] {
            assert_eq!(analyze_sql(sql, &ORACLE), SqlRisk::ReadOnly, "{sql}");
        }
    }

    #[test]
    fn oracle_locking_and_sequence_reads_are_not_read_only() {
        for sql in [
            "SELECT * FROM HR.EMPLOYEES FOR UPDATE",
            "SELECT EMP_SEQ.NEXTVAL FROM DUAL",
            "SELECT * FROM HR.EMPLOYEES WHERE ID = EMP_SEQ.NEXTVAL",
        ] {
            assert!(!analyze_sql(sql, &ORACLE).is_read_only(), "{sql}");
        }
    }

    #[test]
    fn oracle_user_functions_and_packages_need_authorization() {
        for sql in [
            "SELECT HR.AUDIT_LOG(1) FROM DUAL",
            "SELECT DBMS_RANDOM.VALUE FROM DUAL",
            "SELECT UTL_HTTP.REQUEST('http://127.0.0.1') FROM DUAL",
        ] {
            assert!(!analyze_sql(sql, &ORACLE).is_read_only(), "{sql}");
        }
    }

    #[test]
    fn oracle_dblink_and_dictionary_views_need_authorization() {
        for sql in [
            "SELECT * FROM HR.EMPLOYEES@REMOTE",
            "SELECT * FROM V$SESSION",
            "SELECT * FROM SYS.USER$",
        ] {
            assert!(!analyze_sql(sql, &ORACLE).is_read_only(), "{sql}");
        }
    }

    #[test]
    fn oracle_session_control_is_rejected() {
        for sql in ["COMMIT", "ROLLBACK", "SAVEPOINT sp1"] {
            assert!(
                matches!(analyze_sql(sql, &ORACLE), SqlRisk::Rejected { .. }),
                "{sql}"
            );
        }
    }

    #[test]
    fn oracle_has_no_side_effect_free_explain() {
        // `EXPLAIN PLAN FOR` 会写 PLAN_TABLE；不实现无副作用入口就别声称支持计划。
        assert_eq!(
            super::ORACLE.explain_mode(),
            super::ExplainMode::Unsupported
        );
    }

    #[test]
    fn oracle_alter_session_is_never_read_only() {
        // sqlparser 的 Oracle 方言不解析 `ALTER SESSION SET CURRENT_SCHEMA`，策略按未知语法
        // 降级为需授权；绝不能因为“解析失败”而放行为只读。
        assert!(!analyze_sql("ALTER SESSION SET CURRENT_SCHEMA = HR", &ORACLE).is_read_only());
    }

    #[test]
    fn oracle_writes_require_authorization() {
        for sql in [
            "UPDATE HR.EMPLOYEES SET SALARY = 1 WHERE ID = 1",
            "MERGE INTO HR.EMPLOYEES t USING HR.SRC s ON (t.ID = s.ID) WHEN MATCHED THEN UPDATE SET t.NAME = s.NAME",
            "TRUNCATE TABLE HR.EMPLOYEES",
            "DROP TABLE HR.EMPLOYEES",
        ] {
            assert!(!analyze_sql(sql, &ORACLE).is_read_only(), "{sql}");
        }
    }
}
