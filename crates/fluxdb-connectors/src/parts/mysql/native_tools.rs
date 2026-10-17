//! MySQL/MariaDB 原生工具版本与备份参数；不启动进程，不接触 UI。

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MySqlClientVersion {
    pub major: u32,
    pub minor: u32,
    pub mariadb: bool,
}

/// 旧客户端的 Ver 10.13 是工具版本，实际发行版本在 Distrib 后；MariaDB 同理。
pub fn mysql_client_version(output: &str) -> Option<MySqlClientVersion> {
    let version = output
        .split_once("Distrib ")
        .or_else(|| output.split_once("Ver "))?
        .1;
    let mut parts = version.split(|ch: char| !ch.is_ascii_digit());
    Some(MySqlClientVersion {
        major: parts.next()?.parse().ok()?,
        minor: parts.next()?.parse().ok()?,
        mariadb: output.to_ascii_lowercase().contains("mariadb"),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct MySqlDumpOptions {
    pub include_schema: bool,
    pub include_data: bool,
    pub include_routines: bool,
    pub single_transaction: bool,
    pub lock_tables: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MySqlDumpInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
pub fn mysql_dump_invocation(
    program: &str,
    version: &MySqlClientVersion,
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    database: &str,
    tables: &[String],
    options: MySqlDumpOptions,
) -> MySqlDumpInvocation {
    let mut args = vec![
        "--protocol=tcp".into(),
        "-h".into(),
        host.into(),
        "-P".into(),
        port.to_string(),
        "-u".into(),
        user.into(),
        "--default-character-set=utf8mb4".into(),
        "--hex-blob".into(),
        "--no-tablespaces".into(),
    ];
    if options.single_transaction {
        args.push("--single-transaction".into());
        args.push("--skip-lock-tables".into());
    } else if options.lock_tables {
        args.push("--lock-all-tables".into());
    } else {
        args.push("--skip-lock-tables".into());
    }
    if !options.include_data {
        args.push("--no-data".into());
    }
    if !options.include_schema {
        args.push("--no-create-info".into());
    }
    args.push(
        if options.include_schema {
            "--triggers"
        } else {
            "--skip-triggers"
        }
        .into(),
    );
    if options.include_schema && options.include_routines {
        args.push("--routines".into());
    }
    // MySQL 8 默认采集的列统计不适用于旧服务端/TiDB；MariaDB 不认识此参数。
    if !version.mariadb && version.major >= 8 {
        args.push("--column-statistics=0".into());
    }
    // 单库 SQL 恢复不应修改目标实例全局 GTID 状态；MariaDB 没有该选项。
    if !version.mariadb && (version.major > 5 || version.major == 5 && version.minor >= 6) {
        args.push("--set-gtid-purged=OFF".into());
    }
    // 终止选项解析，数据库/表名即使以 '-' 开头也只能作为名称传入。
    args.push("--".into());
    args.push(database.into());
    args.extend_from_slice(tables);
    MySqlDumpInvocation {
        program: program.into(),
        args,
        env: vec![("MYSQL_PWD".into(), password.into())],
    }
}
