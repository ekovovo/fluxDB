//! SQL Server 表右键操作的显式真实库测试。
//!
//! 仅在提供 `FLUXDB_MSSQL_SMOKE=host:port:user:password` 时运行；测试只操作本次
//! 创建的随机表，并同时覆盖 `AppController` 生产派发与 Connector 元数据链路。

use fluxdb_app::{
    AppCommand, AppController, AppEvent, CreateTableState, ForeignKeyCheckMode, TabKind,
};
use fluxdb_connectors::connector_for;
use fluxdb_core::{
    ConnectionConfig, ConnectionDraft, ConnectionId, DatabaseKind, Endpoint, ObjectKind,
    ObjectPath, QueryExecutionOptions, QueryMode, QueryRequest, SecretRef, SqlDialect,
    SqlServerAdvancedOptions, SqlServerBasicOptions, SqlServerConnectionProfile,
    SqlServerEncryption, TableImportOptions,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::AtomicBool;

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
    let database =
        std::env::var("FLUXDB_MSSQL_DATABASE").unwrap_or_else(|_| DEFAULT_DATABASE.into());
    ConnectionConfig {
        id: ConnectionId(9_911),
        name: "SQL Server table actions".into(),
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
                database,
                schema: "dbo".into(),
                instance: String::new(),
                username: parts[2].into(),
                password: SecretRef::inline(parts[3]),
            },
            advanced: SqlServerAdvancedOptions {
                encryption: SqlServerEncryption::TrustServerCertificate,
                connect_timeout_secs: 10,
                query_timeout_secs: 30,
                application_name: "FluxDB SQL Server table actions".into(),
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

fn execute(config: &ConnectionConfig, text: &str) {
    let connector = connector_for(config).expect("SQL Server connector");
    let result = connector
        .execute(&QueryRequest {
            connection_id: config.id,
            database: Some(database_name(config)),
            schema: Some("dbo".into()),
            session_id: None,
            text: text.into(),
            mode: QueryMode::All,
            options: QueryExecutionOptions {
                continue_on_error: false,
                split_statements: true,
                ..QueryExecutionOptions::default()
            },
        })
        .expect("execute SQL Server SQL");
    if let Some(summary) = result.summaries.iter().find(|summary| !summary.success) {
        panic!("SQL Server SQL failed: {}\n{text}", summary.message);
    }
}

fn execute_best_effort(config: &ConnectionConfig, text: &str) {
    let Ok(connector) = connector_for(config) else {
        return;
    };
    let _ = connector.execute(&QueryRequest {
        connection_id: config.id,
        database: Some(database_name(config)),
        schema: Some("dbo".into()),
        session_id: None,
        text: text.into(),
        mode: QueryMode::All,
        options: QueryExecutionOptions::default(),
    });
}

fn qualified(table: &str) -> String {
    format!("[dbo].[{table}]")
}

fn drop_table(config: &ConnectionConfig, table: &str) {
    execute_best_effort(
        config,
        &format!("DROP TABLE IF EXISTS {}", qualified(table)),
    );
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_controller_load_rename_copy_truncate_import_design_and_drop() {
    let config = smoke_config();
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple().to_string().split_at(8).0
    );
    let source = format!("FLUXDB_ACTION_SOURCE_{suffix}");
    let renamed = format!("FLUXDB_ACTION_RENAMED_{suffix}");
    let copied = format!("FLUXDB_ACTION_COPY_{suffix}");
    let import_file = std::env::temp_dir().join(format!("fluxdb-sqlserver-import-{suffix}.csv"));
    let import_body = "NAME,AMOUNT,CREATED_AT\nimported,21.50,2026-03-04 05:06:07\n中文,22.75,2026-04-05 06:07:08\n";
    let mut file = std::fs::File::create(&import_file).expect("create import CSV");
    file.write_all(import_body.as_bytes())
        .expect("write import CSV");
    drop(file);

    for table in [&source, &renamed, &copied] {
        drop_table(&config, table);
    }

    let result = std::panic::catch_unwind(|| {
        let mut controller = AppController::new();
        let AppEvent::ConnectionCreated(created) =
            controller.dispatch(AppCommand::CreateConnection(ConnectionDraft {
                name: "SQL Server actions".into(),
                kind: DatabaseKind::SqlServer,
                endpoint: config.endpoint.clone(),
                options: BTreeMap::new(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
                sqlserver_profile: config.sqlserver_profile.clone(),
            }))
        else {
            panic!("CreateConnection must return ConnectionCreated");
        };
        let source_path = ObjectPath {
            connection_id: created.id,
            database: config
                .sqlserver_profile
                .as_ref()
                .map(|profile| profile.basic.database.clone()),
            schema: Some("dbo".into()),
            name: source.clone(),
            kind: ObjectKind::Table,
        };

        let mut create = CreateTableState::new(
            created.id,
            source_path.database.clone(),
            DatabaseKind::SqlServer,
        );
        create.schema = "dbo".into();
        create.table_name = source.clone();
        create.columns[0].name = "ID".into();
        create.columns[0].data_type = "int".into();
        create.columns[0].auto_increment = true;
        create.columns[0].nullable = false;
        create.columns[0].primary_key = true;
        create.add_column();
        let name = create.columns.last_mut().expect("NAME column");
        name.name = "NAME".into();
        name.data_type = "nvarchar".into();
        name.length = "80".into();
        name.nullable = false;
        create.add_column();
        let amount = create.columns.last_mut().expect("AMOUNT column");
        amount.name = "AMOUNT".into();
        amount.data_type = "decimal".into();
        amount.length = "18".into();
        amount.scale = "4".into();
        create.add_column();
        let created_at = create.columns.last_mut().expect("CREATED_AT column");
        created_at.name = "CREATED_AT".into();
        created_at.data_type = "datetime2".into();
        created_at.scale = "3".into();
        let create_sql = fluxdb_app::create_table_provider(DatabaseKind::SqlServer)
            .sql_preview(&create)
            .expect("SQL Server create table SQL");
        let app_config = controller
            .connection_configs()
            .into_iter()
            .find(|candidate| candidate.id == created.id)
            .expect("created config");
        execute(&app_config, &create_sql);
        execute(
            &app_config,
            &format!(
                "INSERT INTO {} ([NAME], [AMOUNT], [CREATED_AT]) VALUES (N'alpha', 12.3400, '2026-01-02T03:04:05.123');",
                qualified(&source)
            ),
        );

        let connector = connector_for(&app_config).expect("SQL Server connector");
        let page = connector
            .load_data(&source_path, 0, 10, &[], &[])
            .expect("load source data");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].values[1].display_label(), "alpha");
        assert_eq!(page.rows[0].values[2].display_label(), "12.3400");

        // 表右键「导出 → 导出数据」：预览必须真实返回行数与 SQL，不能落 Unsupported。
        let export = controller
            .preview_data_export(
                &source_path,
                &["ID".into(), "NAME".into()],
                &[],
                &[fluxdb_core::FilterSpec {
                    field: "NAME".into(),
                    op: fluxdb_core::FilterOp::Eq,
                    values: vec![fluxdb_core::CellValue::Text("alpha".into())],
                    enabled: true,
                }],
            )
            .expect("SQL Server export data preview");
        assert_eq!(export.row_count, 1);
        assert!(export.sql.contains("SELECT [ID], [NAME]"), "{}", export.sql);
        assert!(
            export.sql.contains("WHERE [NAME] = N'alpha'"),
            "{}",
            export.sql
        );

        // 生产控制器：加载 DDL、重命名、复制并保留数据。
        let source_ddl = controller
            .load_table_ddl(&source_path)
            .expect("controller loads SQL Server DDL");
        assert!(source_ddl.contains("CREATE TABLE"));
        let renamed_event = controller.dispatch(AppCommand::RenameTable {
            object: source_path.clone(),
            new_name: renamed.clone(),
        });
        assert!(
            matches!(renamed_event, AppEvent::TableRenamed { .. }),
            "{renamed_event:?}"
        );
        let renamed_path = ObjectPath {
            name: renamed.clone(),
            ..source_path.clone()
        };
        assert_eq!(
            connector
                .load_data(&renamed_path, 0, 10, &[], &[])
                .expect("load renamed data")
                .rows
                .len(),
            1
        );

        let copied_event = controller.dispatch(AppCommand::CopyTable {
            object: renamed_path.clone(),
            new_name: copied.clone(),
            copy_data: true,
        });
        assert!(
            matches!(copied_event, AppEvent::TableCopied { .. }),
            "{copied_event:?}"
        );
        let copied_path = ObjectPath {
            name: copied.clone(),
            ..source_path.clone()
        };
        let copied_page = connector
            .load_data(&copied_path, 0, 10, &[], &[])
            .expect("load copied data");
        assert_eq!(copied_page.rows.len(), 1);
        assert_eq!(copied_page.rows[0].values[1].display_label(), "alpha");

        // 导入走 AppController 的真实导入任务入口，向复制表追加带中文的行。
        let imported_event = controller.dispatch(AppCommand::ImportTableData {
            object: copied_path.clone(),
            source: import_file.clone(),
            options: TableImportOptions::new(),
            column_mapping: Vec::new(),
        });
        let AppEvent::TableDataImported { report, .. } = imported_event else {
            panic!("import must return TableDataImported: {imported_event:?}");
        };
        assert_eq!(report.imported_rows, 2);
        let imported_page = connector
            .load_data(&copied_path, 0, 10, &[], &[])
            .expect("load imported data");
        assert_eq!(imported_page.rows.len(), 3);
        assert!(imported_page.rows.iter().any(|row| {
            row.values[1] == fluxdb_core::CellValue::Text("中文".into())
                && row.values[2].display_label() == "22.7500"
        }));

        // 设计表要求列/索引/外键/DDL 全部来自 SQL Server 真实元数据。
        let design_event = controller.dispatch(AppCommand::OpenDesignTable(renamed_path.clone()));
        let AppEvent::TabOpened(tab_id) = design_event else {
            panic!("design table must open a tab: {design_event:?}");
        };
        let TabKind::CreateTable(design) = &controller
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .expect("design tab")
            .kind
        else {
            panic!("design tab kind");
        };
        assert!(design.columns.iter().any(|column| column.name == "NAME"));
        assert!(
            design
                .ddl_preview()
                .expect("design DDL")
                .contains("CREATE TABLE")
        );

        // 备份：对象级逻辑 SQL（DDL + INSERT），不是服务器端 BACKUP DATABASE。
        let backup_file = std::env::temp_dir().join(format!(
            "fluxdb-sqlserver-backup-{}.sql",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&backup_file);
        let backup_request = fluxdb_core::BackupRequest {
            config: app_config.clone(),
            database: database_name(&app_config),
            output: backup_file.clone(),
            execution: fluxdb_core::BackupExecution::SqlDump,
            tool: Default::default(),
            tool_version: None,
            scope: fluxdb_core::BackupScope::Objects(vec![fluxdb_core::BackupObjectRef {
                schema: Some("dbo".into()),
                kind: fluxdb_core::BackupObjectKind::Table,
                name: renamed.clone(),
            }]),
            include_schema: true,
            include_data: true,
            include_routines: false,
            single_transaction: false,
            lock_tables: false,
            include_owner: false,
            include_acl: false,
        };
        let backup = fluxdb_connectors::database_backup(DatabaseKind::SqlServer)
            .expect("SQL Server backup provider")
            .backup(&backup_request, &AtomicBool::new(false), &mut |_| {})
            .expect("SQL Server logical backup");
        assert!(backup.complete);
        assert_eq!(backup.objects, vec![format!("dbo.{renamed}")]);
        let backup_sql = std::fs::read_to_string(&backup_file).expect("backup SQL");
        assert!(backup_sql.contains("CREATE TABLE"));
        assert!(backup_sql.contains("N'alpha'"), "{backup_sql}");
        assert!(backup_sql.contains("SET IDENTITY_INSERT"));

        // 恢复要求目标对象不存在：在临时 schema 中重建，再清理。这里直接验证
        // SQL Server 恢复计划与执行链路，不依赖 UI 决策。
        let restore_schema = format!("fluxdb_restore_{}", std::process::id());
        execute(
            &app_config,
            &format!(
                "IF SCHEMA_ID(N'{restore_schema}') IS NULL EXEC(N'CREATE SCHEMA [{restore_schema}]');"
            ),
        );
        let restore_sql = backup_sql
            .replace("[dbo].", &format!("[{restore_schema}]."))
            .replace("N'dbo.", &format!("N'{restore_schema}."));
        let restore_file = std::env::temp_dir().join(format!(
            "fluxdb-sqlserver-restore-{}.sql",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&restore_file);
        std::fs::write(&restore_file, &restore_sql).expect("write restore SQL");
        let restore_request = fluxdb_core::RestoreRequest {
            config: app_config.clone(),
            source: restore_file.clone(),
            target: database_name(&app_config),
            create_target: false,
            tool: Default::default(),
            manifest: None,
            table_decisions: Vec::new(),
            options: Default::default(),
        };
        let provider = fluxdb_connectors::database_backup(DatabaseKind::SqlServer)
            .expect("SQL Server backup provider");
        let plan = provider
            .inspect_restore(&restore_request, &AtomicBool::new(false))
            .expect("SQL Server restore plan");
        assert_eq!(plan.format, fluxdb_core::BackupFormat::Sql);
        provider
            .restore(
                &restore_request,
                &plan,
                &AtomicBool::new(false),
                &mut |_| {},
            )
            .expect("SQL Server restore");
        let restored = ObjectPath {
            name: renamed.clone(),
            schema: Some(restore_schema.clone()),
            ..source_path.clone()
        };
        assert_eq!(
            connector
                .load_data(&restored, 0, 10, &[], &[])
                .expect("load restored data")
                .rows
                .len(),
            1
        );
        execute(
            &app_config,
            &format!("DROP TABLE [{restore_schema}].[{renamed}]; DROP SCHEMA [{restore_schema}];"),
        );
        let _ = std::fs::remove_file(&restore_file);
        let _ = std::fs::remove_file(&backup_file);

        // AI 只读入口：策略允许的 SELECT 正常返回；未识别的写语句即使绕过策略，
        // 也必须被服务端事务回滚并报告未提交。
        let read_only = connector
            .execute(&QueryRequest {
                connection_id: app_config.id,
                database: Some(database_name(&app_config)),
                schema: Some("dbo".into()),
                session_id: None,
                text: format!("SELECT TOP (1) [NAME] FROM {}", qualified(&renamed)),
                mode: QueryMode::All,
                options: QueryExecutionOptions {
                    read_only: true,
                    continue_on_error: false,
                    split_statements: false,
                    ..QueryExecutionOptions::default()
                },
            })
            .expect("read-only select");
        assert!(
            read_only.summaries.iter().all(|summary| summary.success),
            "{:?}",
            read_only.summaries
        );
        assert_eq!(read_only.results[0].rows.len(), 1);

        let blocked = connector
            .execute(&QueryRequest {
                connection_id: app_config.id,
                database: Some(database_name(&app_config)),
                schema: Some("dbo".into()),
                session_id: None,
                text: format!("UPDATE {} SET [NAME] = N'rolled-back'", qualified(&renamed)),
                mode: QueryMode::All,
                options: QueryExecutionOptions {
                    read_only: true,
                    continue_on_error: false,
                    split_statements: false,
                    ..QueryExecutionOptions::default()
                },
            })
            .expect("read-only update executes then rolls back");
        assert!(
            blocked.summaries.iter().any(|summary| !summary.success),
            "write in read-only session must not be reported as success"
        );
        let unchanged = connector
            .load_data(&renamed_path, 0, 10, &[], &[])
            .expect("reload after rollback");
        assert!(
            unchanged
                .rows
                .iter()
                .any(|row| row.values[1].display_label() == "alpha"),
            "UPDATE must have been rolled back"
        );

        // 清空复制表后清理两张测试表。
        let truncate_event = controller.dispatch(AppCommand::TruncateTable {
            object: copied_path.clone(),
            foreign_key_check: ForeignKeyCheckMode::Default,
            restart_identity: false,
        });
        assert!(
            matches!(truncate_event, AppEvent::TableTruncated(_)),
            "{truncate_event:?}"
        );
        assert!(
            connector
                .load_data(&copied_path, 0, 10, &[], &[])
                .expect("load truncated data")
                .rows
                .is_empty()
        );

        let drop_event = controller.dispatch(AppCommand::DropTable {
            object: copied_path,
            foreign_key_check: ForeignKeyCheckMode::Default,
        });
        assert!(
            matches!(drop_event, AppEvent::TableDropped(_)),
            "{drop_event:?}"
        );
        let drop_renamed = controller.dispatch(AppCommand::DropTable {
            object: renamed_path,
            foreign_key_check: ForeignKeyCheckMode::Default,
        });
        assert!(
            matches!(drop_renamed, AppEvent::TableDropped(_)),
            "{drop_renamed:?}"
        );
    });

    drop_table(&config, &copied);
    drop_table(&config, &renamed);
    drop_table(&config, &source);
    let _ = std::fs::remove_file(import_file);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE=host:port:user:password 指向可达的 SQL Server"]
fn sqlserver_database_completion_includes_unopened_schemas() {
    let config = smoke_config();
    let database = database_name(&config);
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let schemas = [
        format!("completion_a_{suffix}"),
        format!("completion_b_{suffix}"),
    ];
    let table = format!("FLUXDB_CATALOG_{suffix}");
    for schema in &schemas {
        execute(&config, &format!("CREATE SCHEMA [{schema}]"));
        execute(
            &config,
            &format!("CREATE TABLE [{schema}].[{table}] ([ID] int NOT NULL)"),
        );
    }

    let result = std::panic::catch_unwind(|| {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        // 只展开 A 并预热 A 的补全索引；B 从未加载到对象树。
        let event = controller.dispatch(AppCommand::LoadObjectChildren(ObjectPath {
            connection_id: config.id,
            database: Some(database.clone()),
            schema: Some(schemas[0].clone()),
            name: schemas[0].clone(),
            kind: ObjectKind::Schema,
        }));
        assert!(matches!(event, AppEvent::ObjectsLoaded(_, _)), "{event:?}");
        let query = format!("SELECT * FROM {table}");
        let warm = controller
            .query_completions_for_text(
                config.id,
                Some(database.clone()),
                Some(schemas[0].clone()),
                query.clone(),
                query.len(),
                true,
            )
            .expect("warm schema A completion");
        assert!(
            warm.items
                .iter()
                .any(|item| item.schema.as_deref() == Some(schemas[0].as_str()))
        );
        assert!(
            !controller.state().connections[0]
                .objects
                .iter()
                .any(|object| { object.path.schema.as_deref() == Some(schemas[1].as_str()) }),
            "schema B must remain unopened"
        );

        // 相同裸表名必须保留两种 schema 身份，不能被部分索引或去重吞掉。
        for _ in 0..2 {
            let completion = controller
                .query_completions_for_text(
                    config.id,
                    Some(database.clone()),
                    None,
                    query.clone(),
                    query.len(),
                    true,
                )
                .expect("database-wide completion");
            let tables = completion
                .items
                .iter()
                .filter(|item| item.kind == fluxdb_core::QueryCompletionKind::Table)
                .collect::<Vec<_>>();
            assert_eq!(tables.len(), 2, "{:?}", completion.items);
            for schema in &schemas {
                assert!(
                    tables.iter().any(|item| {
                        item.schema.as_deref() == Some(schema.as_str())
                            && item.insert_text == format!("[{schema}].[{table}]")
                    }),
                    "missing {schema}: {:?}",
                    completion.items
                );
            }
        }

        let query = format!("SELECT * FROM [{}].", schemas[1]);
        let qualified = controller
            .query_completions_for_text(
                config.id,
                Some(database.clone()),
                None,
                query.clone(),
                query.len(),
                true,
            )
            .expect("bracket-qualified completion");
        let tables = qualified
            .items
            .iter()
            .filter(|item| item.kind == fluxdb_core::QueryCompletionKind::Table)
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 1, "{:?}", qualified.items);
        assert_eq!(tables[0].schema.as_deref(), Some(schemas[1].as_str()));
        assert_eq!(tables[0].insert_text, format!("[{table}]"));
    });

    for schema in &schemas {
        execute(&config, &format!("DROP TABLE [{schema}].[{table}]"));
        execute(&config, &format!("DROP SCHEMA [{schema}]"));
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_copy_table_on_small_stack_worker() {
    let config = smoke_config();
    let source = format!("FLUXDB_STACK_{}", uuid::Uuid::new_v4().simple());
    let structure = format!("{source}_structure");
    let data = format!("{source}_data");
    eprintln!("small-stack fixture: {source}");
    execute(
        &config,
        &format!(
            "CREATE TABLE [dbo].[{source}] ([ID] int IDENTITY PRIMARY KEY, [NAME] nvarchar(40)); INSERT INTO [dbo].[{source}] ([NAME]) VALUES (N'stack regression');"
        ),
    );
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database_name(&config)),
        schema: Some("dbo".into()),
        name: source.clone(),
        kind: ObjectKind::Table,
    };
    let worker_config = config.clone();
    let worker_structure = structure.clone();
    let worker_data = data.clone();
    // macOS GCD 工作线程栈较小；libtest 的默认大栈会掩盖同步 TLS 握手的溢出。
    let worker = std::thread::Builder::new()
        .name("sqlserver-small-stack".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            let mut controller = AppController::new();
            controller.dispatch(AppCommand::ReplaceConnections(vec![worker_config.clone()]));
            for (new_name, copy_data) in [(worker_structure, false), (worker_data, true)] {
                let event = controller.dispatch(AppCommand::CopyTable {
                    object: object.clone(),
                    new_name: new_name.clone(),
                    copy_data,
                });
                assert!(matches!(event, AppEvent::TableCopied { .. }), "{event:?}");
                let connector = connector_for(&worker_config).expect("connector");
                let page = connector
                    .load_data(
                        &ObjectPath {
                            name: new_name,
                            ..object.clone()
                        },
                        0,
                        10,
                        &[],
                        &[],
                    )
                    .expect("copied table");
                assert_eq!(page.columns.len(), 2);
                assert_eq!(page.rows.len(), usize::from(copy_data));
                if copy_data {
                    assert_eq!(page.rows[0].values[0].display_label(), "1");
                    assert_eq!(page.rows[0].values[1].display_label(), "stack regression");
                }
            }
            let connector = connector_for(&worker_config).expect("connector");
            assert_eq!(
                connector
                    .load_data(&object, 0, 10, &[], &[])
                    .expect("source")
                    .rows
                    .len(),
                1
            );
        })
        .expect("small-stack worker");
    let result = worker.join();
    for table in [&data, &structure, &source] {
        drop_table(&config, table);
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_import_binary_hex_csv() {
    let config = smoke_config();
    let table = format!("FLUXDB_IMPORT_BINARY_{}", uuid::Uuid::new_v4().simple());
    let path = std::env::temp_dir().join(format!("fluxdb-sqlserver-binary-{table}.csv"));
    std::fs::write(&path, "ID,PAYLOAD\n1,0x00FF10\n2,NULL\n3,0x\n")
        .expect("write binary import CSV");
    eprintln!("binary import fixture: {table}");
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL, [PAYLOAD] varbinary(8) NULL)",
            qualified(&table)
        ),
    );
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database_name(&config)),
        schema: Some("dbo".into()),
        name: table.clone(),
        kind: ObjectKind::Table,
    };
    let result = std::panic::catch_unwind(|| {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        let event = controller.dispatch(AppCommand::ImportTableData {
            object: object.clone(),
            source: path.clone(),
            options: TableImportOptions::new(),
            column_mapping: Vec::new(),
        });
        let AppEvent::TableDataImported { report, .. } = event else {
            panic!("binary import failed: {event:?}");
        };
        assert_eq!(report.imported_rows, 3);
        let connector = connector_for(&config).expect("connector");
        let page = connector
            .load_data(&object, 0, 10, &[], &[])
            .expect("load binary rows");
        assert_eq!(page.rows.len(), 3);
        assert_eq!(
            page.rows[0].values[1],
            fluxdb_core::CellValue::Bytes(vec![0x00, 0xff, 0x10])
        );
        assert_eq!(page.rows[1].values[1], fluxdb_core::CellValue::Null);
        assert_eq!(
            page.rows[2].values[1],
            fluxdb_core::CellValue::Bytes(Vec::new())
        );
    });
    drop_table(&config, &table);
    let _ = std::fs::remove_file(path);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_copy_table_renames_foreign_key_constraint() {
    let config = smoke_config();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let parent = format!("FLUXDB_FK_PARENT_{suffix}");
    let child = format!("FLUXDB_FK_CHILD_{suffix}");
    let copied = format!("{child}_copy");
    let constraint = format!("FK_PARENT_{suffix}");
    for table in [&copied, &child, &parent] {
        drop_table(&config, table);
    }
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL PRIMARY KEY)",
            qualified(&parent)
        ),
    );
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL PRIMARY KEY, [PARENT_ID] int NOT NULL, CONSTRAINT {} FOREIGN KEY ([PARENT_ID]) REFERENCES {} ([ID]))",
            qualified(&child),
            fluxdb_core::SqlServerDialect.quote_identifier(&constraint),
            qualified(&parent)
        ),
    );
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database_name(&config)),
        schema: Some("dbo".into()),
        name: child.clone(),
        kind: ObjectKind::Table,
    };
    let result = std::panic::catch_unwind(|| {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        let connector = connector_for(&config).expect("connector");
        let event = controller.dispatch(AppCommand::CopyTable {
            object: object.clone(),
            new_name: copied.clone(),
            copy_data: false,
        });
        assert!(
            matches!(event, AppEvent::TableCopied { .. }),
            "copy with foreign key failed: {event:?}"
        );
        let copied_path = ObjectPath {
            name: copied.clone(),
            ..object
        };
        let foreign_keys = connector
            .list_foreign_keys(&copied_path)
            .expect("copied foreign keys");
        assert_eq!(foreign_keys.len(), 1, "{foreign_keys:?}");
        assert_eq!(
            foreign_keys[0].name,
            format!("{copied}_{constraint}"),
            "{foreign_keys:?}"
        );
        assert_eq!(foreign_keys[0].ref_table, parent);
        assert_eq!(foreign_keys[0].columns, vec!["PARENT_ID".to_string()]);
    });
    for table in [&copied, &child, &parent] {
        drop_table(&config, table);
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
#[ignore = "需要 FLUXDB_MSSQL_SMOKE 指向 SQL Server 测试库"]
fn sqlserver_copy_table_rolls_back_after_late_constraint_failure() {
    let config = smoke_config();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let parent = format!("FLUXDB_ATOMIC_PARENT_{suffix}");
    let child = format!("FLUXDB_ATOMIC_CHILD_{suffix}");
    let blocker = format!("FLUXDB_ATOMIC_BLOCKER_{suffix}");
    let copied = format!("{child}_copy");
    let source_constraint = format!("FK_ATOMIC_{suffix}");
    let copied_constraint = format!("{copied}_{source_constraint}");
    for table in [&copied, &blocker, &child, &parent] {
        drop_table(&config, table);
    }
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL PRIMARY KEY)",
            qualified(&parent)
        ),
    );
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL PRIMARY KEY, [PARENT_ID] int NOT NULL, CONSTRAINT {} FOREIGN KEY ([PARENT_ID]) REFERENCES {} ([ID]))",
            qualified(&child),
            fluxdb_core::SqlServerDialect.quote_identifier(&source_constraint),
            qualified(&parent)
        ),
    );
    execute(
        &config,
        &format!(
            "CREATE TABLE {} ([ID] int NOT NULL PRIMARY KEY, [PARENT_ID] int NOT NULL, CONSTRAINT {} FOREIGN KEY ([PARENT_ID]) REFERENCES {} ([ID]))",
            qualified(&blocker),
            fluxdb_core::SqlServerDialect.quote_identifier(&copied_constraint),
            qualified(&parent)
        ),
    );
    let object = ObjectPath {
        connection_id: config.id,
        database: Some(database_name(&config)),
        schema: Some("dbo".into()),
        name: child.clone(),
        kind: ObjectKind::Table,
    };
    let result = std::panic::catch_unwind(|| {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![config.clone()]));
        let event = controller.dispatch(AppCommand::CopyTable {
            object: object.clone(),
            new_name: copied.clone(),
            copy_data: false,
        });
        assert!(
            matches!(event, AppEvent::Failed(_)),
            "late constraint failure must be reported: {event:?}"
        );
        let connector = connector_for(&config).expect("connector");
        let tables = connector
            .list_objects(Some(&ObjectPath {
                connection_id: config.id,
                database: object.database.clone(),
                schema: None,
                name: "dbo".into(),
                kind: ObjectKind::Schema,
            }))
            .expect("schema objects");
        assert!(
            !tables.iter().any(|table| table.path.name == copied),
            "failed copy must roll back the partial target table"
        );
    });
    for table in [&copied, &blocker, &child, &parent] {
        drop_table(&config, table);
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
