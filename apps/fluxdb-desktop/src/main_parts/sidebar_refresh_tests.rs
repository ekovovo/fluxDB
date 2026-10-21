mod sidebar_refresh_tests {
    use super::*;

    fn connection_with_table() -> ConnectionState {
        let config = ConnectionConfig {
            id: ConnectionId(1),
            name: "refresh".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: Endpoint::SqliteFile {
                path: PathBuf::from(":memory:"),
                read_only: false,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
        };
        ConnectionState {
            config,
            connected: true,
            expanded: true,
            objects: vec![ObjectSummary {
                path: ObjectPath {
                    connection_id: ConnectionId(1),
                    database: Some("main".into()),
                    schema: None,
                    name: "SALES_REGION".into(),
                    kind: ObjectKind::Table,
                },
                rows: None,
                modified_at: None,
                comment: None,
                stable: None,
            }],
            redis_overview: Default::default(),
        }
    }

    fn visible_rows(loading: bool, loaded: bool) -> Vec<SidebarVisibleRow> {
        let connection = connection_with_table();
        let layout = SidebarLayout::for_connections(std::slice::from_ref(&connection.config));
        let database_key = database_tree_key(ConnectionId(1), "main");
        let group_key =
            object_group_tree_key_scoped(ConnectionId(1), "main", None, ObjectGroup::Tables);
        let loading_databases = loading.then(|| database_key.clone()).into_iter().collect();
        let loaded_database_children = loaded.then(|| database_key.clone()).into_iter().collect();

        flatten_sidebar_visible_rows(
            &[connection],
            &layout,
            &[],
            &BTreeSet::new(),
            &loading_databases,
            &loaded_database_children,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::from([(database_key, true)]),
            &BTreeMap::from([(group_key, true)]),
            "",
        )
    }

    fn has_table_group_and_row(rows: &[SidebarVisibleRow]) -> bool {
        let has_group = rows.iter().any(|row| {
            row.kind == SidebarRowKind::ObjectGroup
                && row.object_group == Some(ObjectGroup::Tables)
        });
        let has_table = rows.iter().any(|row| {
            row.kind == SidebarRowKind::Table
                && row
                    .object
                    .as_ref()
                    .is_some_and(|object| object.path.name == "SALES_REGION")
        });
        has_group && has_table
    }

    #[test]
    fn table_rows_stay_visible_while_refreshing_loaded_database() {
        assert!(has_table_group_and_row(&visible_rows(true, true)));
    }

    #[test]
    fn first_database_load_does_not_show_empty_table_group() {
        assert!(!has_table_group_and_row(&visible_rows(true, false)));
    }
}
