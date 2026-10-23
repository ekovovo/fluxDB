//! SQL Server 真实连接集成测试。
//!
//! 仅在显式提供 `FLUXDB_MSSQL_SMOKE=host:port:user:password` 时运行；默认忽略，
//! 避免常规单元测试依赖 Podman 或外部数据库。测试只操作本次创建的随机对象，
//! 并在结束或 panic 展开时清理。

use fluxdb_connectors::connector_for;
use fluxdb_core::{
    AppliedChangeOutcome, CellUpdate, CellValue, ConnectionConfig, ConnectionId,
    CreateDatabaseRequest, DataChangeSet, DatabaseKind, Endpoint, FilterOp, FilterSpec, ObjectKind,
    ObjectPath, ParameterizedQueryRequest, QueryExecutionOptions, QueryMode, QueryRequest,
    QuerySessionId, Row, RowIdentity, RowUpdate, SecretRef, SortDirection, SortSpec, SqlParameter,
    SqlServerAdvancedOptions, SqlServerBasicOptions, SqlServerConnectionProfile,
    SqlServerEncryption, SqlServerPermissionState, SqlServerPermissionTarget,
    SqlServerPrincipalKind, SqlServerPrincipalRef, SqlServerRoleScope, SqlServerSecurityChange,
    SqlServerSecurityPlan,
};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const DEFAULT_DATABASE: &str = "fluxdb_smoke";

fn smoke_config() -> ConnectionConfig {
    let raw = std::env::var("FLUXDB_MSSQL_SMOKE")
        .expect("FLUXDB_MSSQL_SMOKE must be host:port:user:password");
    let parts = raw.splitn(4, ':').collect::<Vec<_>>();
    assert_eq!(
        parts.len(),
        4,
        "FLUXDB_MSSQL_SMOKE must be host:port:user:password"
    );
    let host = parts[0];
    let port = parts[1].parse::<u16>().expect("SQL Server port");
    let user = parts[2];
    let password = parts[3];
    let database =
        std::env::var("FLUXDB_MSSQL_DATABASE").unwrap_or_else(|_| DEFAULT_DATABASE.into());
    ConnectionConfig {
        id: ConnectionId(9_910),
        name: "SQL Server live".into(),
        kind: DatabaseKind::SqlServer,
        endpoint: Endpoint::Tcp {
            host: host.into(),
            port,
            database: Some(database.clone()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: Some(SqlServerConnectionProfile {
            basic: SqlServerBasicOptions {
                host: host.into(),
                port,
                database: database.clone(),
                schema: "dbo".into(),
                instance: String::new(),
                username: user.into(),
                password: SecretRef::inline(password),
            },
            advanced: SqlServerAdvancedOptions {
                encryption: SqlServerEncryption::TrustServerCertificate,
                connect_timeout_secs: 10,
                query_timeout_secs: 30,
                application_name: "FluxDB SQL Server live".into(),
            },
        }),
    }
}

fn database_name(config: &ConnectionConfig) -> String {
    config
        .sqlserver_profile
        .as_ref()
        .map(|profile| profile.basic.database.clone())
        .filter(|database| !database.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DATABASE.into())
}

fn object_path(config: &ConnectionConfig, table: &str, kind: ObjectKind) -> ObjectPath {
    ObjectPath {
        connection_id: config.id,
        database: Some(database_name(config)),
        schema: Some("dbo".into()),
        name: table.into(),
        kind,
    }
}

fn query_request(config: &ConnectionConfig, database: &str, text: &str) -> QueryRequest {
    QueryRequest {
        connection_id: config.id,
        database: Some(database.into()),
        schema: Some("dbo".into()),
        session_id: None,
        text: text.into(),
        mode: QueryMode::All,
        options: QueryExecutionOptions {
            continue_on_error: false,
            split_statements: true,
            ..QueryExecutionOptions::default()
        },
    }
}

fn scalar_text(page: &fluxdb_core::DataPage) -> String {
    page.rows
        .first()
        .and_then(|row| row.values.first())
        .map(CellValue::display_label)
        .unwrap_or_default()
}

fn execute(config: &ConnectionConfig, sql: &str) {
    let connector = connector_for(config).expect("SQL Server connector");
    let result = connector
        .execute(&query_request(config, &database_name(config), sql))
        .expect("execute SQL Server SQL");
    if let Some(summary) = result.summaries.iter().find(|summary| !summary.success) {
        panic!("SQL Server SQL failed: {}\n{sql}", summary.message);
    }
}

fn execute_best_effort(config: &ConnectionConfig, sql: &str) {
    let Ok(connector) = connector_for(config) else {
        return;
    };
    let _ = connector.execute(&query_request(config, &database_name(config), sql));
}

struct Fixture {
    config: ConnectionConfig,
    suffix: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let config = smoke_config();
        let suffix = format!(
            "{}_{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time")
                .as_nanos()
        );
        let fixture = Self {
            config,
            suffix: suffix.clone(),
        };
        execute(
            &fixture.config,
            &format!(
                "IF OBJECT_ID(N'dbo.FLUXDB_{name}_CHILD_{suffix}', N'U') IS NOT NULL DROP TABLE dbo.FLUXDB_{name}_CHILD_{suffix}; \
                 IF OBJECT_ID(N'dbo.FLUXDB_{name}_PARENT_{suffix}', N'U') IS NOT NULL DROP TABLE dbo.FLUXDB_{name}_PARENT_{suffix};"
            ),
        );
        fixture
    }

    fn table(&self, role: &str) -> String {
        format!("FLUXDB_LIVE_{role}_{}", self.suffix)
    }

    fn path(&self, table: &str) -> ObjectPath {
        object_path(&self.config, table, ObjectKind::Table)
    }

    fn create_parent_child(&self) -> (String, String) {
        let parent = self.table("PARENT");
        let child = self.table("CHILD");
        execute(
            &self.config,
            &format!(
                "CREATE TABLE [dbo].[{parent}] ( \
                    [ID] int IDENTITY(1,1) NOT NULL CONSTRAINT [PK_{parent}] PRIMARY KEY, \
                    [NAME] nvarchar(80) NOT NULL, \
                    [AMOUNT] decimal(18,4) NULL, \
                    [CREATED_AT] datetime2(3) NULL \
                ); \
                CREATE TABLE [dbo].[{child}] ( \
                    [ID] int NOT NULL CONSTRAINT [PK_{child}] PRIMARY KEY, \
                    [PARENT_ID] int NOT NULL, \
                    [NOTE] nvarchar(200) NULL, \
                    CONSTRAINT [FK_{child}_PARENT] FOREIGN KEY ([PARENT_ID]) \
                        REFERENCES [dbo].[{parent}] ([ID]) \
                ); \
                INSERT INTO [dbo].[{parent}] ([NAME], [AMOUNT], [CREATED_AT]) VALUES \
                    (N'alpha', 12.3400, '2026-01-02T03:04:05.123'), \
                    (N'中文', 56.7800, '2026-02-03T04:05:06.456'); \
                INSERT INTO [dbo].[{child}] ([ID], [PARENT_ID], [NOTE]) VALUES (10, 1, N'child');"
            ),
        );
        (parent, child)
    }

    fn cleanup(&self, tables: &[&str]) {
        for table in tables.iter().rev() {
            execute_best_effort(
                &self.config,
                &format!("DROP TABLE IF EXISTS [dbo].[{table}];"),
            );
        }
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_connection_objects_paging_filters_and_metadata() {
    let fixture = Fixture::new("METADATA");
    let (parent, child) = fixture.create_parent_child();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let connector = connector_for(&fixture.config).expect("SQL Server connector");
        connector
            .test_connection(&fixture.config)
            .expect("test SQL Server connection");

        let databases = connector.list_objects(None).expect("list databases");
        let database = databases
            .iter()
            .find(|object| {
                object.path.kind == ObjectKind::Database
                    && object
                        .path
                        .name
                        .eq_ignore_ascii_case(&database_name(&fixture.config))
            })
            .expect("configured database node");
        let schemas = connector
            .list_objects(Some(&database.path))
            .expect("list schemas");
        let schema = schemas
            .iter()
            .find(|object| object.path.kind == ObjectKind::Schema)
            .expect("schema node");
        let relations = connector
            .list_objects(Some(&schema.path))
            .expect("list relations");
        assert!(
            relations.iter().any(|object| {
                object.path.kind == ObjectKind::Table && object.path.name == parent
            })
        );
        assert!(relations.iter().any(|object| {
            object.path.kind == ObjectKind::Table
                && object.path.name == child
                && object.path.schema.as_deref() == Some("dbo")
        }));

        let parent_path = fixture.path(&parent);
        let first_page = connector
            .load_data(&parent_path, 0, 1, &[], &[])
            .expect("first page");
        assert_eq!(first_page.rows.len(), 1);
        assert!(first_page.has_more);
        assert_eq!(
            first_page.rows[0].values[1],
            CellValue::Text("alpha".into())
        );
        let second_page = connector
            .load_data(&parent_path, 1, 10, &[], &[])
            .expect("second page");
        assert_eq!(second_page.rows.len(), 1);
        assert_eq!(
            second_page.rows[0].values[1],
            CellValue::Text("中文".into())
        );
        assert_eq!(second_page.rows[0].values[2].display_label(), "56.7800");
        assert!(!second_page.has_more);

        let sorted = connector
            .load_data(
                &parent_path,
                0,
                10,
                &[SortSpec {
                    field: "ID".into(),
                    direction: SortDirection::Desc,
                }],
                &[],
            )
            .expect("sorted page");
        assert_eq!(sorted.rows[0].values[1], CellValue::Text("中文".into()));
        let filtered = connector
            .load_data(
                &parent_path,
                0,
                10,
                &[],
                &[FilterSpec {
                    field: "NAME".into(),
                    op: FilterOp::Eq,
                    values: vec![CellValue::Text("alpha".into())],
                    enabled: true,
                }],
            )
            .expect("filtered page");
        assert_eq!(filtered.rows.len(), 1);
        assert_eq!(filtered.rows[0].values[1], CellValue::Text("alpha".into()));

        let export = connector
            .preview_data_export(
                &parent_path,
                &["ID".into(), "NAME".into()],
                &[SortSpec {
                    field: "ID".into(),
                    direction: SortDirection::Desc,
                }],
                &[FilterSpec {
                    field: "NAME".into(),
                    op: FilterOp::StartsWith,
                    values: vec![CellValue::Text("a".into())],
                    enabled: true,
                }],
            )
            .expect("SQL Server export preview");
        assert_eq!(export.row_count, 1);
        assert!(export.sql.contains("[ID], [NAME]"), "{}", export.sql);
        assert!(
            export.sql.contains("WHERE [NAME] LIKE N'a%'"),
            "{}",
            export.sql
        );
        assert!(export.sql.contains("ORDER BY [ID] DESC"), "{}", export.sql);
        assert!(!export.sql.contains("@P"), "{}", export.sql);

        let indexes = connector.list_indexes(&parent_path).expect("indexes");
        assert!(indexes.iter().any(|index| index.is_primary));
        let foreign_keys = connector
            .list_foreign_keys(&fixture.path(&child))
            .expect("foreign keys");
        assert!(foreign_keys.iter().any(|foreign_key| {
            foreign_key.ref_table == parent
                && foreign_key.ref_schema.as_deref() == Some("dbo")
                && foreign_key.columns == vec!["PARENT_ID".to_string()]
        }));
        let ddl = connector.table_ddl(&parent_path).expect("table DDL");
        assert!(ddl.contains("CREATE TABLE"));
        assert!(ddl.contains("IDENTITY(1,1)"));
        assert!(ddl.contains("PRIMARY KEY"));

        let bound = connector.execute_parameterized(&ParameterizedQueryRequest {
            request: query_request(
                &fixture.config,
                &database_name(&fixture.config),
                "SELECT [NAME] FROM [dbo].[FLUXDB_LIVE_PARENT_PLACEHOLDER] WHERE [ID] = @P1",
            ),
            parameters: vec![],
        });
        assert!(bound.is_err(), "placeholder table must not exist");

        let bound = connector
            .execute_parameterized(&ParameterizedQueryRequest {
                request: query_request(
                    &fixture.config,
                    &database_name(&fixture.config),
                    &format!("SELECT [NAME] FROM [dbo].[{parent}] WHERE [ID] = @P1"),
                ),
                parameters: vec![SqlParameter::I64("1".into())],
            })
            .expect("parameterized query");
        assert_eq!(scalar_text(&bound.results[0]), "alpha");
        let marker = format!("x' OR 1=1 -- {}", fixture.suffix);
        let bound = connector
            .execute_parameterized(&ParameterizedQueryRequest {
                request: query_request(
                    &fixture.config,
                    &database_name(&fixture.config),
                    &format!("SELECT [NAME] FROM [dbo].[{parent}] WHERE [NAME] = @P1"),
                ),
                parameters: vec![SqlParameter::Text(marker.clone())],
            })
            .expect("parameterized literal");
        assert!(
            bound.results[0].rows.is_empty(),
            "parameter must be bound, not interpolated"
        );

        let view_sql = format!(
            "CREATE VIEW [dbo].[FLUXDB_LIVE_VIEW_{suffix}] AS SELECT [ID], [NAME] FROM [dbo].[{parent}]",
            suffix = fixture.suffix
        );
        execute(&fixture.config, &view_sql);
        let view = object_path(
            &fixture.config,
            &format!("FLUXDB_LIVE_VIEW_{}", fixture.suffix),
            ObjectKind::View,
        );
        let view_page = connector
            .load_data(&view, 0, 10, &[], &[])
            .expect("view data");
        assert_eq!(view_page.rows.len(), 2);
        let view_ddl = connector.table_ddl(&view).expect("view DDL");
        assert!(view_ddl.contains("CREATE VIEW"));
    }));
    execute_best_effort(
        &fixture.config,
        &format!(
            "DROP VIEW IF EXISTS [dbo].[FLUXDB_LIVE_VIEW_{}]",
            fixture.suffix
        ),
    );
    fixture.cleanup(&[&child, &parent]);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_create_and_drop_database_with_collation() {
    let config = smoke_config();
    let connector = connector_for(&config).expect("SQL Server connector");
    let name = format!(
        "FLUXDB_DB_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_nanos()
    );
    connector
        .create_database(&CreateDatabaseRequest {
            connection_id: config.id,
            name: name.clone(),
            charset: String::new(),
            collation: "Chinese_PRC_CI_AS".into(),
            owner: String::new(),
            template: String::new(),
            path: None,
        })
        .expect("create SQL Server database");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let databases = connector.list_objects(None).expect("list databases");
        assert!(
            databases
                .iter()
                .any(|object| object.path.name.eq_ignore_ascii_case(&name)),
            "created database must be visible"
        );
        execute(
            &config,
            &format!(
                "IF OBJECT_ID(N'[{name}].dbo.FLUXDB_DB_PROBE', N'U') IS NOT NULL \
                 DROP TABLE [{name}].dbo.FLUXDB_DB_PROBE; \
                 CREATE TABLE [{name}].dbo.FLUXDB_DB_PROBE ([ID] int NOT NULL); \
                 DROP TABLE [{name}].dbo.FLUXDB_DB_PROBE;"
            ),
        );
    }));
    connector
        .delete_database(config.id, &name)
        .expect("drop SQL Server database");
    let databases = connector.list_objects(None).expect("list databases");
    assert!(
        !databases
            .iter()
            .any(|object| object.path.name.eq_ignore_ascii_case(&name)),
        "dropped database must disappear"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_create_schema_in_selected_database() {
    let config = smoke_config();
    let connector = connector_for(&config).expect("SQL Server connector");
    let schema = format!(
        "FLUXDB_SCHEMA_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_nanos()
    );
    let database = database_name(&config);
    connector
        .create_schema(config.id, &database, &schema)
        .expect("create SQL Server schema");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let database_path = ObjectPath {
            connection_id: config.id,
            database: Some(database.clone()),
            schema: None,
            name: database.clone(),
            kind: ObjectKind::Database,
        };
        let schemas = connector
            .list_objects(Some(&database_path))
            .expect("list schemas");
        assert!(
            schemas
                .iter()
                .any(|object| object.path.kind == ObjectKind::Schema && object.path.name == schema),
            "created schema must be visible"
        );
        execute(
            &config,
            &format!(
                "CREATE TABLE [{schema}].[FLUXDB_SCHEMA_PROBE] ([ID] int NOT NULL); \
                 INSERT INTO [{schema}].[FLUXDB_SCHEMA_PROBE] ([ID]) VALUES (1);"
            ),
        );
        let schema_path = ObjectPath {
            connection_id: config.id,
            database: Some(database.clone()),
            schema: None,
            name: schema.clone(),
            kind: ObjectKind::Schema,
        };
        let relations = connector
            .list_objects(Some(&schema_path))
            .expect("list schema relations");
        assert!(
            relations
                .iter()
                .any(|object| object.path.name == "FLUXDB_SCHEMA_PROBE"),
            "table in created schema must be visible"
        );
    }));
    execute_best_effort(
        &config,
        &format!("DROP TABLE IF EXISTS [{schema}].[FLUXDB_SCHEMA_PROBE];"),
    );
    execute_best_effort(
        &config,
        &format!("IF SCHEMA_ID(N'{schema}') IS NOT NULL DROP SCHEMA [{schema}];"),
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_load_data_handles_sql_variant_without_panicking() {
    let fixture = Fixture::new("VARIANT");
    let table = fixture.table("VARIANT");
    execute(
        &fixture.config,
        &format!(
            "CREATE TABLE [dbo].[{table}] ( \
                [ID] int NOT NULL PRIMARY KEY, \
                [PAYLOAD] sql_variant NULL \
            ); \
            INSERT INTO [dbo].[{table}] ([ID], [PAYLOAD]) VALUES \
                (1, CAST(N'hello' AS sql_variant)), \
                (2, CAST(42 AS sql_variant)), \
                (3, NULL);"
        ),
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let connector = connector_for(&fixture.config).expect("SQL Server connector");
        let page = connector
            .load_data(&fixture.path(&table), 0, 10, &[], &[])
            .expect("load sql_variant table");
        assert_eq!(page.columns.len(), 2);
        assert_eq!(page.columns[1].type_name.as_deref(), Some("sql_variant"));
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.rows[0].values[1].display_label(), "hello");
        assert_eq!(page.rows[1].values[1].display_label(), "42");
        assert_eq!(page.rows[2].values[1], CellValue::Null);

        // 翻页同样不能带出驱动无法解码的隐藏列或 `sql_variant` 原始列。
        let second = connector
            .load_data(&fixture.path(&table), 1, 1, &[], &[])
            .expect("second page with sql_variant");
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.columns.len(), 2);
        assert_eq!(second.rows[0].values[1].display_label(), "42");

        let preview = connector
            .preview_data_export(&fixture.path(&table), &[], &[], &[])
            .expect("sql_variant export preview");
        assert_eq!(preview.row_count, 3);
        assert!(
            !preview.sql.contains("SELECT *"),
            "preview must list columns explicitly: {}",
            preview.sql
        );
        assert!(
            preview.sql.contains("CAST([PAYLOAD] AS nvarchar(max))"),
            "{}",
            preview.sql
        );

        // 编辑器执行用户 SQL 也会遇到 sql_variant；驱动必须解码而不是 abort。
        let raw = connector
            .execute(&query_request(
                &fixture.config,
                &database_name(&fixture.config),
                &format!("SELECT [ID], [PAYLOAD] FROM [dbo].[{table}] ORDER BY [ID]"),
            ))
            .expect("editor query with sql_variant");
        assert_eq!(raw.results[0].rows.len(), 3);
        assert_eq!(
            raw.results[0].columns[1].type_name.as_deref(),
            Some("sql_variant")
        );
        for row in &raw.results[0].rows {
            if let CellValue::Text(label) = &row.values[1] {
                assert!(label.starts_with("<sql_variant "), "{label}");
            }
        }
    }));
    fixture.cleanup(&[&table]);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_load_data_handles_clr_udt_without_panicking() {
    let config = smoke_config();
    let table = format!(
        "FLUXDB_UDT_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    execute_best_effort(&config, &format!("DROP TABLE IF EXISTS [dbo].[{table}]"));
    execute(
        &config,
        &format!(
            "CREATE TABLE [dbo].[{table}] ([ID] int NOT NULL, [SHAPE] geometry NULL); INSERT INTO [dbo].[{table}] ([ID], [SHAPE]) VALUES (1, geometry::STGeomFromText('POINT(1 2)', 4326));"
        ),
    );
    let result = std::panic::catch_unwind(|| {
        let connector = connector_for(&config).expect("connector");
        let page = connector
            .load_data(
                &object_path(&config, &table, ObjectKind::Table),
                0,
                10,
                &[],
                &[],
            )
            .expect("load geometry UDT");
        assert_eq!(page.rows.len(), 1);
        assert!(
            page.rows[0].values[1]
                .display_label()
                .contains("UDT 不支持解码"),
            "{:?}",
            page.rows[0].values[1]
        );
    });
    execute_best_effort(&config, &format!("DROP TABLE IF EXISTS [dbo].[{table}]"));
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_completion_lists_tables_columns_procedures_and_functions() {
    let config = smoke_config();
    let connector = connector_for(&config).expect("SQL Server connector");
    let database = database_name(&config);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_nanos()
    );
    let table = format!("FLUXDB_COMPLETION_{suffix}");
    let procedure = format!("FLUXDB_PROC_{suffix}");
    let function = format!("FLUXDB_FN_{suffix}");
    execute(
        &config,
        &format!(
            "CREATE TABLE [dbo].[{table}] ( \
                [ID] int IDENTITY(1,1) NOT NULL PRIMARY KEY, \
                [CUSTOMER_NAME] nvarchar(80) NOT NULL, \
                [AMOUNT] decimal(18,2) NULL \
            ); \
            EXEC(N'CREATE PROCEDURE [dbo].[{procedure}] @id int AS SELECT [ID] FROM [dbo].[{table}] WHERE [ID] = @id'); \
            EXEC(N'CREATE FUNCTION [dbo].[{function}] (@value int) RETURNS int AS BEGIN RETURN @value + 1 END');"
        ),
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let tables = connector
            .list_completion_tables(Some(&database), Some("dbo"), "FLUXDB_COMPLETION", 50)
            .expect("completion tables");
        assert!(
            tables.iter().any(|item| item.name == table
                && item.schema.as_deref() == Some("dbo")
                && item.kind == ObjectKind::Table),
            "{tables:?}"
        );

        let columns = connector
            .list_completion_columns(Some(&database), Some("dbo"), &table)
            .expect("completion columns");
        assert!(
            columns
                .iter()
                .any(|item| item.name == "CUSTOMER_NAME" && item.table == table),
            "{columns:?}"
        );
        assert!(
            columns
                .iter()
                .any(|item| item.name == "ID" && item.primary_key),
            "{columns:?}"
        );

        let routines = connector
            .list_completion_routines(Some(&database), Some("dbo"), "FLUXDB_", 100)
            .expect("completion routines");
        assert!(
            routines.iter().any(|item| item.name == procedure
                && item.kind == fluxdb_core::CompletionRoutineKind::Procedure
                && item.schema.as_deref() == Some("dbo")),
            "{routines:?}"
        );
        assert!(
            routines.iter().any(|item| item.name == function
                && item.kind == fluxdb_core::CompletionRoutineKind::Function
                && item
                    .signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("@value"))),
            "{routines:?}"
        );
    }));
    execute(
        &config,
        &format!(
            "DROP PROCEDURE IF EXISTS [dbo].[{procedure}]; \
             DROP FUNCTION IF EXISTS [dbo].[{function}]; \
             DROP TABLE IF EXISTS [dbo].[{table}];"
        ),
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_completion_tables_without_schema_covers_all_schemas() {
    let config = smoke_config();
    let connector = connector_for(&config).expect("SQL Server connector");
    let database = database_name(&config);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_nanos()
    );
    let schema_a = format!("FLUXA_{suffix}");
    let schema_b = format!("FLUXB_{suffix}");
    let table_a = format!("ORDERS_{suffix}");
    let table_b = format!("EMPLOYEES_{suffix}");
    execute(
        &config,
        &format!(
            "CREATE SCHEMA [{schema_a}]; CREATE SCHEMA [{schema_b}]; \
             CREATE TABLE [{schema_a}].[{table_a}] ([ID] int NOT NULL); \
             CREATE TABLE [{schema_b}].[{table_b}] ([ID] int NOT NULL);"
        ),
    );
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // 不传 schema：补全必须覆盖整个库，而不是只查登录默认 schema。
            let tables = connector
                .list_completion_tables(Some(&database), None, &suffix, 200)
                .expect("completion tables without schema");
            assert!(
                tables.iter().any(|table| table.name == table_a
                    && table.schema.as_deref() == Some(schema_a.as_str())),
                "{tables:?}"
            );
            assert!(
                tables.iter().any(|table| table.name == table_b
                    && table.schema.as_deref() == Some(schema_b.as_str())),
                "{tables:?}"
            );

            // 显式 schema 仍只返回该 schema。
            let scoped = connector
                .list_completion_tables(Some(&database), Some(&schema_a), &suffix, 200)
                .expect("completion tables scoped to schema");
            assert!(
                scoped
                    .iter()
                    .all(|table| table.schema.as_deref() == Some(schema_a.as_str())),
                "{scoped:?}"
            );
        }));
    execute(
        &config,
        &format!(
            "DROP TABLE IF EXISTS [{schema_a}].[{table_a}]; \
             DROP TABLE IF EXISTS [{schema_b}].[{table_b}]; \
             DROP SCHEMA IF EXISTS [{schema_a}]; DROP SCHEMA IF EXISTS [{schema_b}];"
        ),
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_apply_changes_insert_update_delete_and_identity() {
    let fixture = Fixture::new("CHANGES");
    let (parent, child) = fixture.create_parent_child();
    let result = std::panic::catch_unwind(|| {
        let connector = connector_for(&fixture.config).expect("SQL Server connector");
        let parent_path = fixture.path(&parent);
        let outcome = connector
            .apply_changes(&DataChangeSet {
                object: parent_path.clone(),
                inserts: vec![Row {
                    values: vec![
                        CellValue::Null,
                        CellValue::Text("inserted".into()),
                        CellValue::Text("7.5".into()),
                        CellValue::Null,
                    ],
                }],
                updates: vec![RowUpdate {
                    identity: RowIdentity {
                        values: [("ID".into(), CellValue::I64(1))].into(),
                    },
                    cells: vec![CellUpdate {
                        column: "NAME".into(),
                        value: CellValue::Text("updated".into()),
                    }],
                }],
                deletes: vec![RowIdentity {
                    values: [("ID".into(), CellValue::I64(2))].into(),
                }],
                insert_intents: None,
            })
            .expect("apply changes");
        assert!(matches!(outcome, AppliedChangeOutcome { .. }));

        let page = connector
            .load_data(&parent_path, 0, 10, &[], &[])
            .expect("reload");
        assert_eq!(page.rows.len(), 2);
        assert!(
            page.rows
                .iter()
                .any(|row| { row.values[1] == CellValue::Text("updated".into()) })
        );
        assert!(
            page.rows
                .iter()
                .any(|row| { row.values[1] == CellValue::Text("inserted".into()) })
        );
        assert!(
            !page
                .rows
                .iter()
                .any(|row| { row.values[0] == CellValue::I64(2) })
        );
        let generated_ids = page
            .rows
            .iter()
            .filter_map(|row| match row.values.first() {
                Some(CellValue::I64(id)) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(generated_ids.len(), 2);
        assert!(generated_ids.contains(&1));
        assert!(
            generated_ids.iter().any(|id| *id > 2),
            "identity insert must allocate a new id"
        );

        let child_path = fixture.path(&child);
        let error = connector
            .apply_changes(&DataChangeSet {
                object: child_path,
                inserts: vec![Row {
                    values: vec![
                        CellValue::I64(11),
                        CellValue::I64(9999),
                        CellValue::Text("orphan".into()),
                    ],
                }],
                updates: Vec::new(),
                deletes: Vec::new(),
                insert_intents: None,
            })
            .expect_err("foreign key must reject orphan insert");
        assert!(error.message.contains("插入失败"), "{error:?}");
    });
    fixture.cleanup(&[&child, &parent]);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_query_session_preserves_temp_table_across_executions() {
    let config = smoke_config();
    let connector = connector_for(&config).expect("connector");
    let session_id = QuerySessionId(70_001);
    let table = format!(
        "#FLUXDB_SESSION_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let mut request = query_request(
        &config,
        &database_name(&config),
        &format!("CREATE TABLE {table} ([V] int); INSERT INTO {table} VALUES (7);"),
    );
    request.session_id = Some(session_id);
    connector.execute(&request).expect("create temp table");

    request.text = format!("SELECT [V] FROM {table}");
    let page = connector
        .execute(&request)
        .expect("read temp table in same session")
        .results
        .into_iter()
        .next()
        .expect("result page");
    assert_eq!(scalar_text(&page), "7");

    request.session_id = Some(QuerySessionId(70_002));
    let isolated = connector.execute(&request).expect("isolated session query");
    assert!(
        isolated.summaries.iter().any(|summary| !summary.success),
        "temporary table must not leak into another query session"
    );
    fluxdb_connectors::sqlserver_close_query_session(config.id, session_id);
    fluxdb_connectors::sqlserver_close_query_session(config.id, QuerySessionId(70_002));
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_object_tree_lists_procedures_and_functions_with_ddl() {
    let config = smoke_config();
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let procedure = format!("FLUXDB_PROC_{suffix}");
    let function = format!("FLUXDB_FUNC_{suffix}");
    execute_best_effort(
        &config,
        &format!("DROP PROCEDURE IF EXISTS [dbo].[{procedure}]"),
    );
    execute_best_effort(
        &config,
        &format!("DROP FUNCTION IF EXISTS [dbo].[{function}]"),
    );
    execute(
        &config,
        &format!("CREATE PROCEDURE [dbo].[{procedure}] AS SELECT 1 AS [V]"),
    );
    execute(
        &config,
        &format!(
            "CREATE FUNCTION [dbo].[{function}](@v int) RETURNS int AS BEGIN RETURN @v + 1 END"
        ),
    );
    let result = std::panic::catch_unwind(|| {
        let connector = connector_for(&config).expect("connector");
        let schema = ObjectPath {
            connection_id: config.id,
            database: Some(database_name(&config)),
            schema: None,
            name: "dbo".into(),
            kind: ObjectKind::Schema,
        };
        let objects = connector
            .list_objects(Some(&schema))
            .expect("schema objects");
        let procedure_path = objects
            .iter()
            .find(|object| object.path.name == procedure)
            .map(|object| object.path.clone())
            .expect("procedure object");
        let function_path = objects
            .iter()
            .find(|object| object.path.name == function)
            .map(|object| object.path.clone())
            .expect("function object");
        assert_eq!(procedure_path.kind, ObjectKind::Procedure);
        assert_eq!(function_path.kind, ObjectKind::Function);
        assert!(
            connector
                .table_ddl(&procedure_path)
                .expect("procedure DDL")
                .contains("CREATE PROCEDURE")
        );
        assert!(
            connector
                .table_ddl(&function_path)
                .expect("function DDL")
                .contains("CREATE FUNCTION")
        );
    });
    execute_best_effort(
        &config,
        &format!("DROP PROCEDURE IF EXISTS [dbo].[{procedure}]"),
    );
    execute_best_effort(
        &config,
        &format!("DROP FUNCTION IF EXISTS [dbo].[{function}]"),
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_query_timeout_and_user_cancel_terminate_running_statement() {
    let mut config = smoke_config();
    config
        .sqlserver_profile
        .as_mut()
        .expect("profile")
        .advanced
        .query_timeout_secs = 1;
    let connector = connector_for(&config).expect("connector");
    let request = query_request(
        &config,
        &database_name(&config),
        "WAITFOR DELAY '00:00:05'; SELECT 1",
    );
    let started = std::time::Instant::now();
    let timed_out = connector.execute(&request).expect("timeout result");
    assert!(
        timed_out
            .summaries
            .iter()
            .any(|summary| !summary.success && summary.message.contains("查询超时")),
        "{timed_out:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));

    config
        .sqlserver_profile
        .as_mut()
        .expect("profile")
        .advanced
        .query_timeout_secs = 0;
    let connector = connector_for(&config).expect("connector");
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_signal = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        cancel_signal.store(true, Ordering::Relaxed);
    });
    let cancel_for_check = cancel.clone();
    let cancel_started = std::time::Instant::now();
    let cancelled = connector
        .execute_with_progress(&request, &mut |_| {}, &|| {
            cancel_for_check.load(Ordering::Relaxed)
        })
        .expect("cancel result");
    assert!(
        cancelled
            .summaries
            .iter()
            .any(|summary| !summary.success && summary.message.contains("查询已取消")),
        "{cancelled:?}"
    );
    assert!(cancel_started.elapsed() < std::time::Duration::from_secs(4));
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_security_plan_round_trip_and_transaction_rollback() {
    let fixture = Fixture::new("SECURITY");
    let login = fixture.table("SEC_LOGIN");
    let user = fixture.table("SEC_USER");
    let role = fixture.table("SEC_ROLE");
    let table = fixture.table("SEC_TABLE");
    let rollback_login = fixture.table("SEC_RB_LOGIN");
    let rollback_user = fixture.table("SEC_RB_USER");
    let password = format!("FluxDB!{}aA9", fixture.suffix);
    let database = database_name(&fixture.config);
    let connector = connector_for(&fixture.config).expect("SQL Server connector");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(
            &fixture.config,
            &format!(
                "CREATE TABLE [dbo].[{table}] ([ID] int NOT NULL PRIMARY KEY, [VALUE] nvarchar(40) NULL);"
            ),
        );
        let snapshot = connector
            .sqlserver_security_snapshot(fixture.config.id, &database)
            .expect("security snapshot");
        assert!(
            snapshot
                .logins
                .iter()
                .any(|item| item.name.eq_ignore_ascii_case(&snapshot.current_login))
        );

        let plan = SqlServerSecurityPlan {
            database: database.clone(),
            principal: login.clone(),
            changes: vec![
                SqlServerSecurityChange::CreateLogin {
                    name: login.clone(),
                    password: password.clone(),
                    default_database: Some(database.clone()),
                    disabled: false,
                },
                SqlServerSecurityChange::CreateUser {
                    name: user.clone(),
                    login: Some(login.clone()),
                    default_schema: Some("dbo".into()),
                },
                SqlServerSecurityChange::CreateDatabaseRole { name: role.clone() },
                SqlServerSecurityChange::AddRoleMember {
                    scope: SqlServerRoleScope::Database,
                    role: role.clone(),
                    member: user.clone(),
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: login.clone(),
                    target: SqlServerPermissionTarget::Server,
                    permission: "CONNECT SQL".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: user.clone(),
                    target: SqlServerPermissionTarget::Database,
                    permission: "CONNECT".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
                SqlServerSecurityChange::GrantPermission {
                    grantee: user.clone(),
                    target: SqlServerPermissionTarget::Object {
                        schema: "dbo".into(),
                        object: table.clone(),
                    },
                    permission: "SELECT".into(),
                    state: SqlServerPermissionState::Grant,
                    grant_option: false,
                    replace_existing: false,
                    cascade_revoke: false,
                },
            ],
        };
        let preview = connector
            .render_sqlserver_security_plan(fixture.config.id, &plan)
            .expect("masked preview");
        assert!(preview.iter().any(|sql| sql.contains("********")));
        assert!(preview.iter().all(|sql| !sql.contains(&password)));
        connector
            .apply_sqlserver_security_plan(fixture.config.id, &plan)
            .expect("apply security plan");

        let snapshot = connector
            .sqlserver_security_snapshot(fixture.config.id, &database)
            .expect("security snapshot after apply");
        assert!(snapshot.logins.iter().any(|item| item.name == login));
        assert!(snapshot.users.iter().any(|item| item.name == user));
        assert!(snapshot.database_roles.iter().any(|item| item.name == role));
        assert!(snapshot.has_database_role(&role, &user));

        let login_permissions = connector
            .sqlserver_principal_permissions(
                fixture.config.id,
                &database,
                &SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::Login,
                    name: login.clone(),
                },
            )
            .expect("login permissions");
        assert!(login_permissions.iter().any(|permission| {
            permission.permission == "CONNECT SQL"
                && permission.target == SqlServerPermissionTarget::Server
        }));
        let user_permissions = connector
            .sqlserver_principal_permissions(
                fixture.config.id,
                &database,
                &SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: user.clone(),
                },
            )
            .expect("user permissions");
        assert!(user_permissions.iter().any(|permission| {
            permission.permission == "SELECT"
                && permission.target
                    == SqlServerPermissionTarget::Object {
                        schema: "dbo".into(),
                        object: table.clone(),
                    }
        }));
        let object_target = SqlServerPermissionTarget::Object {
            schema: "dbo".into(),
            object: table.clone(),
        };
        connector
            .apply_sqlserver_security_plan(
                fixture.config.id,
                &SqlServerSecurityPlan {
                    database: database.clone(),
                    principal: user.clone(),
                    changes: vec![SqlServerSecurityChange::GrantPermission {
                        grantee: user.clone(),
                        target: object_target.clone(),
                        permission: "SELECT".into(),
                        state: SqlServerPermissionState::Grant,
                        grant_option: true,
                        replace_existing: false,
                        cascade_revoke: false,
                    }],
                },
            )
            .expect("grant option");
        let permissions_with_option = connector
            .sqlserver_principal_permissions(
                fixture.config.id,
                &database,
                &SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: user.clone(),
                },
            )
            .expect("permissions with grant option");
        assert!(permissions_with_option.iter().any(|permission| {
            permission.permission == "SELECT"
                && permission.target == object_target
                && permission.state == SqlServerPermissionState::GrantWithGrantOption
        }));

        connector
            .apply_sqlserver_security_plan(
                fixture.config.id,
                &SqlServerSecurityPlan {
                    database: database.clone(),
                    principal: user.clone(),
                    changes: vec![SqlServerSecurityChange::RevokeGrantOption {
                        grantee: user.clone(),
                        target: object_target.clone(),
                        permission: "SELECT".into(),
                        cascade: true,
                    }],
                },
            )
            .expect("remove grant option");
        let permissions_without_option = connector
            .sqlserver_principal_permissions(
                fixture.config.id,
                &database,
                &SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: user.clone(),
                },
            )
            .expect("permissions without grant option");
        assert!(permissions_without_option.iter().any(|permission| {
            permission.permission == "SELECT"
                && permission.target == object_target
                && permission.state == SqlServerPermissionState::Grant
        }));

        connector
            .apply_sqlserver_security_plan(
                fixture.config.id,
                &SqlServerSecurityPlan {
                    database: database.clone(),
                    principal: user.clone(),
                    changes: vec![SqlServerSecurityChange::GrantPermission {
                        grantee: user.clone(),
                        target: object_target.clone(),
                        permission: "SELECT".into(),
                        state: SqlServerPermissionState::Grant,
                        grant_option: true,
                        replace_existing: false,
                        cascade_revoke: false,
                    }],
                },
            )
            .expect("grant option before deny");
        connector
            .apply_sqlserver_security_plan(
                fixture.config.id,
                &SqlServerSecurityPlan {
                    database: database.clone(),
                    principal: user.clone(),
                    changes: vec![SqlServerSecurityChange::GrantPermission {
                        grantee: user.clone(),
                        target: object_target.clone(),
                        permission: "SELECT".into(),
                        state: SqlServerPermissionState::Deny,
                        grant_option: false,
                        replace_existing: true,
                        cascade_revoke: true,
                    }],
                },
            )
            .expect("replace grant with deny");
        let denied_permissions = connector
            .sqlserver_principal_permissions(
                fixture.config.id,
                &database,
                &SqlServerPrincipalRef {
                    kind: SqlServerPrincipalKind::User,
                    name: user.clone(),
                },
            )
            .expect("denied permissions");
        assert!(denied_permissions.iter().any(|permission| {
            permission.permission == "SELECT"
                && permission.target == object_target
                && permission.state == SqlServerPermissionState::Deny
        }));

        let self_protection = connector.apply_sqlserver_security_plan(
            fixture.config.id,
            &SqlServerSecurityPlan {
                database: database.clone(),
                principal: snapshot.current_login.clone(),
                changes: vec![SqlServerSecurityChange::SetLoginEnabled {
                    name: snapshot.current_login.clone(),
                    enabled: false,
                }],
            },
        );
        assert_eq!(
            self_protection
                .expect_err("must protect current login")
                .kind,
            fluxdb_core::ErrorKind::Permission
        );

        let rollback = SqlServerSecurityPlan {
            database: database.clone(),
            principal: rollback_login.clone(),
            changes: vec![
                SqlServerSecurityChange::CreateLogin {
                    name: rollback_login.clone(),
                    password: format!("{password}Rollback"),
                    default_database: None,
                    disabled: false,
                },
                SqlServerSecurityChange::CreateUser {
                    name: rollback_user.clone(),
                    login: Some(rollback_login.clone()),
                    default_schema: None,
                },
                SqlServerSecurityChange::CreateUser {
                    name: fixture.table("SEC_INVALID_LOGIN"),
                    login: Some(fixture.table("SEC_MISSING_LOGIN")),
                    default_schema: None,
                },
            ],
        };
        assert!(
            connector
                .apply_sqlserver_security_plan(fixture.config.id, &rollback)
                .is_err()
        );
        let after_rollback = connector
            .sqlserver_security_snapshot(fixture.config.id, &database)
            .expect("snapshot after rollback");
        assert!(
            !after_rollback
                .logins
                .iter()
                .any(|item| item.name == rollback_login)
        );
        assert!(
            !after_rollback
                .users
                .iter()
                .any(|item| item.name == rollback_user)
        );
    }));

    cleanup_sqlserver_security(
        &fixture.config,
        &role,
        &user,
        &login,
        &rollback_user,
        &rollback_login,
        &table,
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn cleanup_sqlserver_security(
    config: &ConnectionConfig,
    role: &str,
    user: &str,
    login: &str,
    rollback_user: &str,
    rollback_login: &str,
    table: &str,
) {
    for sql in [
        format!("ALTER ROLE [{role}] DROP MEMBER [{user}]"),
        format!("DROP ROLE IF EXISTS [{role}]"),
        format!("DROP USER IF EXISTS [{user}]"),
        format!("DROP USER IF EXISTS [{rollback_user}]"),
        format!("DROP TABLE IF EXISTS [dbo].[{table}]"),
        format!("DROP LOGIN [{login}]"),
        format!("DROP LOGIN [{rollback_login}]"),
    ] {
        execute_best_effort(config, &sql);
    }
}
