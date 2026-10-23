use fluxdb_core::DatabaseKind;
use sqlparser::dialect::{Dialect, MsSqlDialect};

use super::{ExplainMode, NamespaceScope, SqlPolicy};

pub static SQLSERVER: SqlServerPolicy = SqlServerPolicy;

pub struct SqlServerPolicy;

impl SqlPolicy for SqlServerPolicy {
    fn kind(&self) -> DatabaseKind {
        DatabaseKind::SqlServer
    }

    fn label(&self) -> &'static str {
        "sqlserver"
    }

    fn parser_dialect(&self) -> &'static dyn Dialect {
        &MsSqlDialect {}
    }

    fn function_denylist(&self) -> &'static [&'static str] {
        &[
            // 操作系统命令、注册表与文件系统访问。
            "xp_cmdshell",
            "xp_regread",
            "xp_regwrite",
            "xp_regdeletekey",
            "xp_regdeletevalue",
            "xp_dirtree",
            "xp_fileexist",
            "xp_subdirs",
            "xp_availablemedia",
            "xp_terminate_process",
            "xp_servicecontrol",
            "xp_startmail",
            "xp_sendmail",
            // OLE Automation：可在 SQL Server 服务账号下创建进程/写文件。
            "sp_oacreate",
            "sp_oamethod",
            "sp_oagetproperty",
            "sp_oasetproperty",
            "sp_oadestroy",
            // 即席动态 SQL 与跨实例访问。
            "sp_executesql",
            "sp_prepare",
            "sp_execute",
            "openrowset",
            "openquery",
            "opendatasource",
            "openxml",
            // 服务端状态/配置写入。
            "sp_configure",
            "sp_addextendedproperty",
            "sp_updateextendedproperty",
            "sp_dropextendedproperty",
            "sp_addlinkedserver",
            "sp_addlinkedsrvlogin",
            "sp_dropserver",
            "sp_who",
            "sp_who2",
            "sp_helpdb",
            // 阻塞/计时：可长时间占用连接。
            "waitfor",
        ]
    }

    fn relation_denylist(&self) -> &'static [&'static str] {
        &[
            // 登录、凭据与服务端主体。
            "sys.sql_logins",
            "sys.server_principals",
            "sys.server_role_members",
            "sys.credentials",
            "sys.crypt_properties",
            "sys.symmetric_keys",
            "sys.asymmetric_keys",
            "sys.certificates",
            "master.sys.sql_logins",
            "master.sys.server_principals",
            "master.sys.credentials",
            // 会话/请求。
            "sys.dm_exec_sessions",
            "sys.dm_exec_requests",
            "sys.dm_exec_connections",
            "sys.dm_exec_sql_text",
            "sys.dm_tran_locks",
            "sys.sysprocesses",
            "sys.dm_os_waiting_tasks",
            // 服务端/实例配置。
            "sys.configurations",
            "sys.dm_server_services",
            "sys.dm_server_registry",
            "sys.dm_os_sys_info",
            "sys.dm_os_sys_memory",
            "sys.dm_os_volume_stats",
            "sys.dm_server_audit_status",
        ]
    }

    fn extra_transaction_markers(&self) -> &'static [&'static str] {
        &["use ", "set showplan", "set statistics"]
    }

    fn namespace_scope(&self) -> NamespaceScope {
        NamespaceScope::SchemaAndDatabase
    }

    fn explain_mode(&self) -> ExplainMode {
        ExplainMode::Showplan
    }
}
