mod tabs_workspace_tests {
    use std::collections::BTreeSet;

    use fluxdb_app::{AppCommand, AppController, AppEvent};
    use fluxdb_core::{
        ConnectionConfig, ConnectionId, DatabaseKind, Endpoint, ObjectKind, ObjectPath,
    };

    use crate::{
        FirstLevelScope, TabWorkspace, WorkspaceTabGroup, first_level_has_second_row,
        first_level_tab_entries, first_level_tab_groups, tab_context_menu_close_targets,
        tab_first_level_scope, table_tab_entries,
    };

    #[test]
    fn redis_databases_share_a_connection_workspace_and_remain_separate_tabs() {
        let mut controller = AppController::new();
        controller.dispatch(AppCommand::ReplaceConnections(vec![
            ConnectionConfig {
                id: ConnectionId(10),
                name: "docker-redis".into(),
                kind: DatabaseKind::Redis,
                endpoint: Endpoint::Tcp {
                    host: "redis.invalid".into(),
                    port: 6379,
                    database: None,
                },
                options: Default::default(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
                mongo_profile: None,
                sqlserver_profile: None,
            },
            ConnectionConfig {
                id: ConnectionId(11),
                name: "sqlite".into(),
                kind: DatabaseKind::Sqlite,
                endpoint: Endpoint::SqliteFile {
                    path: std::env::temp_dir().join("fluxdb-tab-workspace-test.sqlite"),
                    read_only: true,
                },
                options: Default::default(),
                redis_profile: None,
                mysql_profile: None,
                postgres_profile: None,
                dameng_profile: None,
                mongo_profile: None,
                sqlserver_profile: None,
            },
        ]));

        let AppEvent::TabOpened(db0_tab) =
            controller.dispatch(AppCommand::OpenDataEditor(ObjectPath {
                connection_id: ConnectionId(10),
                database: Some("0".into()),
                schema: None,
                name: "0".into(),
                kind: ObjectKind::RedisDb,
            }))
        else {
            panic!("open Redis DB 0 tab")
        };
        let AppEvent::TabOpened(db1_tab) =
            controller.dispatch(AppCommand::OpenDataEditor(ObjectPath {
                connection_id: ConnectionId(10),
                database: Some("1".into()),
                schema: None,
                name: "1".into(),
                kind: ObjectKind::RedisDb,
            }))
        else {
            panic!("open Redis DB 1 tab")
        };
        let AppEvent::TabOpened(sqlite_tab) =
            controller.dispatch(AppCommand::OpenObjectList(Some(ObjectPath {
                connection_id: ConnectionId(11),
                database: Some("main".into()),
                schema: None,
                name: "main".into(),
                kind: ObjectKind::Database,
            })))
        else {
            panic!("open SQLite database tab")
        };

        let state = controller.state();
        let redis_scope = FirstLevelScope::RedisConnection(ConnectionId(10));
        assert_eq!(
            tab_first_level_scope(
                state
                    .tabs
                    .iter()
                    .find(|tab| tab.id == db0_tab)
                    .expect("Redis DB 0 tab")
            ),
            Some(redis_scope.clone())
        );
        assert_eq!(
            tab_first_level_scope(
                state
                    .tabs
                    .iter()
                    .find(|tab| tab.id == db1_tab)
                    .expect("Redis DB 1 tab")
            ),
            Some(redis_scope.clone())
        );
        assert!(first_level_has_second_row(Some(&redis_scope)));

        let groups = first_level_tab_groups(state);
        assert_eq!(
            groups
                .iter()
                .filter(|group| matches!(group, WorkspaceTabGroup::RedisConnection { .. }))
                .count(),
            1
        );
        let redis_entry = first_level_tab_entries(&groups, Some(&redis_scope), "")
            .into_iter()
            .find(|entry| entry.title == "docker-redis")
            .expect("Redis connection entry");
        assert_eq!(redis_entry.id, db0_tab);
        assert_eq!(
            table_tab_entries(state, Some(&redis_scope), "", &[], &BTreeSet::new())
                .into_iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![db0_tab, db1_tab]
        );
        let scopes = state
            .tabs
            .iter()
            .map(|tab| (tab.id, tab_first_level_scope(tab)))
            .collect::<Vec<_>>();
        assert_eq!(
            tab_context_menu_close_targets(scopes.clone(), db0_tab, true),
            vec![db0_tab, db1_tab]
        );
        assert_eq!(
            tab_context_menu_close_targets(scopes, db0_tab, false),
            vec![db1_tab]
        );

        let sqlite_tab = state
            .tabs
            .iter()
            .find(|tab| tab.id == sqlite_tab)
            .expect("SQLite tab");
        assert_eq!(
            tab_first_level_scope(sqlite_tab),
            Some(FirstLevelScope::Database(TabWorkspace {
                connection_id: ConnectionId(11),
                database: "main".into(),
            }))
        );
        assert_eq!(
            groups
                .iter()
                .filter(|group| matches!(group, WorkspaceTabGroup::Database { .. }))
                .count(),
            1
        );
    }
}
