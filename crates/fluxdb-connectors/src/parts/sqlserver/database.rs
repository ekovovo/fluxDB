/// SQL Server 新建/删除数据库。
///
/// 这两类 DDL 必须在 `master` 执行，且不能放进用户库的默认 schema 作用域；
/// 名称与排序规则都先做校验，再按 T-SQL 标识符/字面量引用。
const SQLSERVER_MASTER_DATABASE: &str = "master";

fn sqlserver_create_schema(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
    schema: &str,
) -> fluxdb_core::Result<()> {
    if config.kind != DatabaseKind::SqlServer {
        return Err(Error::new(ErrorKind::Connection, "连接类型不支持新建 schema"));
    }
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "连接不匹配"));
    }
    let schema = schema.trim();
    if schema.is_empty() {
        return Err(Error::new(ErrorKind::Query, "schema 名称不能为空"));
    }
    if schema.chars().count() > 128
        || schema
            .chars()
            .any(|ch| ch == '\0' || ch.is_control())
    {
        return Err(Error::new(ErrorKind::Query, "schema 名称不合法"));
    }
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "缺少 SQL Server 数据库上下文"));
    }
    let mut profile = sqlserver_profile(config)?;
    profile.basic.database = database.to_string();
    let mut client = sqlserver_connect_profile(profile)?;
    let sql = format!(
        "CREATE SCHEMA {}",
        fluxdb_core::SqlServerDialect.quote_identifier(schema)
    );
    sqlserver_block_on(async {
        let result = sqlserver_run_batch(&mut client, &sql).await;
        sqlserver_close(client).await;
        result
    })?;
    Ok(())
}

fn sqlserver_create_database(
    config: &ConnectionConfig,
    request: &CreateDatabaseRequest,
) -> fluxdb_core::Result<()> {
    if config.kind != DatabaseKind::SqlServer {
        return Err(Error::new(ErrorKind::Connection, "连接类型不支持新建数据库"));
    }
    if config.id != request.connection_id {
        return Err(Error::new(ErrorKind::Connection, "连接不匹配"));
    }
    let name = request.name.trim();
    if name.is_empty() {
        return Err(Error::new(ErrorKind::Query, "数据库名称不能为空"));
    }
    let mut profile = sqlserver_profile(config)?;
    profile.basic.database = SQLSERVER_MASTER_DATABASE.to_string();
    let mut client = sqlserver_connect_profile(profile)?;

    let mut sql = format!(
        "CREATE DATABASE {}",
        fluxdb_core::SqlServerDialect.quote_identifier(name)
    );
    let collation = request.collation.trim();
    if !collation.is_empty() {
        // COLLATE 接受排序规则名，不能按普通标识符引用；只允许字母/数字/下划线，
        // 避免把空格、分号或注释带进 DDL。
        if !collation
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(Error::new(ErrorKind::Query, "排序规则名称不合法"));
        }
        sql.push_str(&format!(" COLLATE {collation}"));
    }
    sqlserver_block_on(async {
        let result = sqlserver_run_batch(&mut client, &sql).await;
        sqlserver_close(client).await;
        result
    })?;
    Ok(())
}

fn sqlserver_delete_database(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
) -> fluxdb_core::Result<()> {
    if config.kind != DatabaseKind::SqlServer {
        return Err(Error::new(ErrorKind::Connection, "连接类型不支持删除数据库"));
    }
    if config.id != connection_id {
        return Err(Error::new(ErrorKind::Connection, "连接不匹配"));
    }
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "数据库名称不能为空"));
    }
    if database.eq_ignore_ascii_case(SQLSERVER_MASTER_DATABASE)
        || database.eq_ignore_ascii_case("model")
        || database.eq_ignore_ascii_case("msdb")
        || database.eq_ignore_ascii_case("tempdb")
    {
        return Err(Error::new(ErrorKind::Permission, "不能删除 SQL Server 系统数据库"));
    }
    let mut profile = sqlserver_profile(config)?;
    profile.basic.database = SQLSERVER_MASTER_DATABASE.to_string();
    let mut client = sqlserver_connect_profile(profile)?;
    let quoted = fluxdb_core::SqlServerDialect.quote_identifier(database);
    // 先切 SINGLE_USER 断开其他会话，否则有活动连接时 DROP DATABASE 会失败。
    let sql = format!(
        "ALTER DATABASE {quoted} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {quoted};"
    );
    sqlserver_block_on(async {
        let result = sqlserver_run_batch(&mut client, &sql).await;
        sqlserver_close(client).await;
        result
    })?;
    Ok(())
}
