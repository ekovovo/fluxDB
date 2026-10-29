use fluxdb_app::{AppCommand, AppController, AppEvent};
use fluxdb_connectors::connector_for;
use fluxdb_core::{
    ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ObjectKind, ObjectPath,
    QueryExecutionOptions, QueryMode, QueryRequest,
};
use std::collections::BTreeMap;

fn execute(config: &ConnectionConfig, text: &str) {
    let result = connector_for(config)
        .expect("SQLite connector")
        .execute(&QueryRequest {
            connection_id: config.id,
            database: None,
            schema: None,
            session_id: None,
            text: text.into(),
            mode: QueryMode::All,
            options: QueryExecutionOptions {
                continue_on_error: false,
                split_statements: true,
                ..QueryExecutionOptions::default()
            },
        })
        .expect("SQLite execute");
    if let Some(summary) = result.summaries.iter().find(|summary| !summary.success) {
        panic!("SQLite SQL failed: {}\n{}", summary.message, text);
    }
}

#[test]
fn sqlite_design_table_loads_metadata_before_installing_tab() {
    let dir = std::env::temp_dir().join(format!(
        "fluxdb-sqlite-design-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let database = dir.join("design.sqlite");
    let config = ConnectionConfig {
        id: ConnectionId(9_904),
        name: "SQLite design".into(),
        kind: DatabaseKind::Sqlite,
        endpoint: Endpoint::SqliteFile {
            path: database,
            read_only: false,
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        mongo_profile: None,
        sqlserver_profile: None,
        oracle_profile: None,
    };
    execute(
        &config,
        "CREATE TABLE items (ID INTEGER PRIMARY KEY, NAME TEXT NOT NULL)",
    );
    execute(&config, "CREATE INDEX idx_items_name ON items(NAME)");
    execute(
        &config,
        "CREATE TRIGGER items_ai AFTER INSERT ON items BEGIN UPDATE items SET NAME = NAME WHERE ID = NEW.ID; END",
    );

    let object = ObjectPath {
        connection_id: config.id,
        database: None,
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    let mut controller = AppController::new();
    controller.dispatch(AppCommand::ReplaceConnections(vec![config]));

    let create = match controller.dispatch(AppCommand::LoadDesignTable(object)) {
        AppEvent::DesignTableLoaded(Ok(create)) => create,
        AppEvent::DesignTableLoaded(Err(error)) => {
            panic!("load SQLite design metadata: {}", error.message)
        }
        event => panic!("unexpected design load event: {event:?}"),
    };
    assert_eq!(create.table_name, "items");
    assert_eq!(create.columns.len(), 2);
    assert_eq!(create.indexes.len(), 1);
    assert_eq!(create.triggers.len(), 1);

    let tab_id = match controller.dispatch(AppCommand::FinishOpenDesignTable { result: Ok(create) })
    {
        AppEvent::TabOpened(tab_id) => tab_id,
        event => panic!("unexpected finish design event: {event:?}"),
    };
    assert_eq!(controller.state().active_tab, Some(tab_id));
    assert_eq!(controller.state().tabs.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}
