// Included in crate-root scope by ../lib.rs; grouped by SQL completion index responsibility.

const COMPLETION_INDEX_TTL_SECONDS: u64 = 30 * 60;
const COMPLETION_PREFIX_LIMIT: usize = 24;
const COMPLETION_WARMUP_BATCH_SIZE: usize = 50;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct DbKey {
    connection_id: ConnectionId,
    database: Option<String>,
    schema: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct TableKey {
    connection_id: ConnectionId,
    database: Option<String>,
    schema: Option<String>,
    table: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct PrefixKey {
    connection_id: ConnectionId,
    database: Option<String>,
    schema: Option<String>,
    prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct TableId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct ColumnId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct RoutineId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct TriggerId(usize);

#[derive(Clone, Debug)]
struct IndexedColumnRef {
    source: ColumnRef,
    lower_column: String,
    column_tokens: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MetadataEntryKind {
    Table,
}

impl MetadataEntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Table => "Table",
        }
    }
}

/// 检索条目携带的领域对象。新增 Skill、知识文档时在这里加变体，
/// 复用同一套作用域失效、排序与有界检索逻辑。
#[derive(Clone, Debug)]
enum MetadataEntryPayload {
    Table(TableRef),
}

#[derive(Clone, Debug)]
struct IndexedMetadataEntry {
    kind: MetadataEntryKind,
    title: String,
    title_lower: String,
    description: Option<String>,
    description_lower: Option<String>,
    /// 列名、列注释等附属关键词的拼接文本；命中后再回表取具体命中项。
    keywords_lower: String,
    payload: MetadataEntryPayload,
}

#[derive(Clone, Debug)]
struct MetadataSearchHit {
    kind: MetadataEntryKind,
    title: String,
    description: Option<String>,
    matched_keywords: Vec<String>,
    payload: MetadataEntryPayload,
}

/// 通用本地元数据索引。
///
/// 当前承载表、列、例程与触发器，供 SQL 补全、对象详情和 AI 找表共用；
/// 检索与持久化都只依赖 `TableRef` / `ColumnRef` 这类元数据，不保存数据行。
/// 未来技能、知识文档等结构化条目应扩在这里，而不是再建一套独立索引。
#[derive(Clone, Debug, Default)]
struct CompletionIndex {
    tables_by_db: BTreeMap<DbKey, Vec<TableId>>,
    columns_by_table: BTreeMap<TableKey, Vec<ColumnId>>,
    columns_by_db: BTreeMap<DbKey, Vec<ColumnId>>,
    column_prefix_index: BTreeMap<PrefixKey, Vec<ColumnId>>,
    tables: Vec<TableRef>,
    columns: Vec<IndexedColumnRef>,
    /// 通用检索条目：标题、描述、附属关键词与领域 payload；表只是当前的一种条目。
    metadata_entries: BTreeMap<TableKey, IndexedMetadataEntry>,
    /// 例程（含签名 identity arguments）与触发器同样进索引并随快照持久化：
    /// 同名重载按签名分条，避免缓存/持久化时把重载合并成一个候选（§8.4）。
    routines_by_db: BTreeMap<DbKey, Vec<RoutineId>>,
    routines: Vec<RoutineRef>,
    triggers_by_db: BTreeMap<DbKey, Vec<TriggerId>>,
    triggers: Vec<TriggerRef>,
    dirty_databases: BTreeSet<DbKey>,
    dirty_tables: BTreeSet<TableKey>,
    metas: BTreeMap<DbKey, CompletionIndexMeta>,
    /// T051：正在后台刷新的库，用于 stale-while-refresh 的去重，避免每次按键重复触发刷新线程。
    refresh_inflight: BTreeSet<DbKey>,
    /// 已从盘上合并过全部作用域快照的连接；AI 描述式找表只在首次查询时付一次目录 IO。
    persisted_loaded_connections: BTreeSet<ConnectionId>,
}

impl CompletionIndex {
    fn db_key(
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> DbKey {
        DbKey {
            connection_id,
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
        }
    }

    /// 表索引键：名称按 catalog 原样持有，不做大小写折叠。
    ///
    /// PG 允许 `"Foo"` 与 `"foo"` 并存（未加引号才折叠为小写），折叠会合并两个真实对象
    /// 并互相覆盖列（§8.4）。大小写差异在匹配阶段按方言处理，不在存储阶段合并。
    fn table_key(
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) -> TableKey {
        TableKey {
            connection_id,
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
            table: table.to_string(),
        }
    }

    fn insert_tables(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        tables: Vec<CompletionTable>,
        db_kind: DatabaseKind,
    ) {
        let db_key = Self::db_key(connection_id, database, schema);
        let old_keys = self.metadata_entry_keys_for_scope(&db_key);
        let mut table_ids = Vec::new();
        let mut new_keys = BTreeSet::new();
        for table in tables {
            let source = TableRef {
                database: table.database.clone().or_else(|| database.map(str::to_string)),
                schema: table.schema.clone().or_else(|| schema.map(str::to_string)),
                name: table.name,
                kind: table.kind,
                rows: None,
                // 注释随表元数据进索引与快照，供补全项文档提示（§8.4）。
                comment: table.comment,
            };
            let table_id = TableId(self.tables.len());
            self.tables.push(source.clone());
            table_ids.push(table_id);
            self.index_table_metadata(
                connection_id,
                db_key.database.as_deref(),
                db_key.schema.as_deref(),
                &source,
            );
            new_keys.insert(Self::table_key(
                connection_id,
                source.database.as_deref().or(db_key.database.as_deref()),
                source.schema.as_deref().or(db_key.schema.as_deref()),
                &source.name,
            ));
        }
        for old_key in old_keys {
            if !new_keys.contains(&old_key) {
                self.metadata_entries.remove(&old_key);
            }
        }
        self.tables_by_db.insert(db_key.clone(), table_ids);
        self.touch_meta(db_key, db_kind);
    }

    fn metadata_entry_keys_for_scope(&self, db_key: &DbKey) -> Vec<TableKey> {
        let mut keys = self
            .metadata_entries
            .keys()
            .filter(|key| Self::metadata_key_matches_scope(key, db_key))
            .cloned()
            .collect::<BTreeSet<_>>();
        keys.extend(
            self.tables_by_db
            .get(db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.tables.get(id.0))
            .map(|table| {
                Self::table_key(
                    db_key.connection_id,
                    table.database.as_deref().or(db_key.database.as_deref()),
                    table.schema.as_deref().or(db_key.schema.as_deref()),
                    &table.name,
                )
            }),
        );
        keys.into_iter().collect()
    }

    /// 作用域键的 `None` 表示“未指定”，覆盖其下具体库/schema 的检索条目。
    /// 整库刷新时必须一并清掉旧 schema 条目，否则删表/改名后仍会被搜到。
    fn metadata_key_matches_scope(key: &TableKey, scope: &DbKey) -> bool {
        key.connection_id == scope.connection_id
            && scope
                .database
                .as_ref()
                .is_none_or(|database| key.database.as_ref() == Some(database))
            && scope
                .schema
                .as_ref()
                .is_none_or(|schema| key.schema.as_ref() == Some(schema))
    }

    fn index_table_metadata(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &TableRef,
    ) {
        let table_key = Self::table_key(
            connection_id,
            table.database.as_deref().or(database),
            table.schema.as_deref().or(schema),
            &table.name,
        );
        // 列可能先于表清单进入索引；刷新表清单时保留已有列关键词。
        let keywords_lower = self
            .metadata_entries
            .get(&table_key)
            .map(|entry| entry.keywords_lower.clone())
            .unwrap_or_default();
        self.metadata_entries.insert(
            table_key,
            IndexedMetadataEntry {
                kind: MetadataEntryKind::Table,
                title: table.name.clone(),
                title_lower: table.name.to_ascii_lowercase(),
                description: table.comment.clone(),
                description_lower: table
                    .comment
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .filter(|comment| !comment.is_empty()),
                keywords_lower,
                payload: MetadataEntryPayload::Table(table.clone()),
            },
        );
    }

    fn replace_table_columns(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
        columns: Vec<CompletionColumn>,
        db_kind: DatabaseKind,
    ) {
        let table_key = Self::table_key(connection_id, database, schema, table);
        let db_key = Self::db_key(connection_id, database, schema);
        self.remove_table_column_ids(&table_key);

        // 列自身可能带更具体的库/schema（PG search_path）；记录所有可能对应检索键的作用域，
        // 稍后直接按键刷新，避免逐表扫描整条连接的检索条目。
        let mut search_scopes =
            BTreeSet::from([(database.map(str::to_string), schema.map(str::to_string))]);
        for column in &columns {
            search_scopes.insert((
                column
                    .database
                    .clone()
                    .or_else(|| database.map(str::to_string)),
                column
                    .schema
                    .clone()
                    .or_else(|| schema.map(str::to_string)),
            ));
        }
        let mut column_ids = Vec::new();
        for (index, column) in columns.into_iter().enumerate() {
            let source = ColumnRef {
                database: database.map(str::to_string),
                schema: schema.map(str::to_string),
                table: column.table.clone(),
                column: column.name,
                type_name: column.type_name,
                nullable: column.nullable,
                primary_key: column.primary_key,
                ordinal_position: Some((index + 1) as u32),
                comment: column.comment,
            };
            let column_id = self.push_column(source);
            column_ids.push(column_id);
            self.columns_by_db
                .entry(db_key.clone())
                .or_default()
                .push(column_id);
            self.index_column_prefixes(&db_key, column_id);
        }

        self.columns_by_table.insert(table_key, column_ids);
        for (database, schema) in search_scopes {
            self.refresh_metadata_keywords(
                connection_id,
                database.as_deref(),
                schema.as_deref(),
                table,
            );
        }
        self.touch_meta(db_key, db_kind);
        self.clear_dirty_table_names_matching(connection_id, database, schema, table);
    }

    /// 用当前列索引重建某表的检索文本；列变更后只更新这一张表，避免全库重扫。
    fn refresh_metadata_keywords(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) {
        let text = self
            .table_columns_for_search(connection_id, database, schema, table)
            .into_iter()
            .fold(String::new(), |mut text, column| {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&column.name.to_ascii_lowercase());
                if let Some(comment) = column.comment.filter(|comment| !comment.is_empty()) {
                    text.push(' ');
                    text.push_str(&comment.to_ascii_lowercase());
                }
                text
            });
        let exact = Self::table_key(connection_id, database, schema, table);
        // 列按整库 (库, None) 收录时，检索条目可能带具体 schema（PG search_path）；
        // 同步该库下同名表的条目，避免列关键词只对其中一种键生效。
        let keys = if schema.is_none() {
            self.metadata_entries
                .keys()
                .filter(|key| {
                    key.connection_id == connection_id
                        && key.database.as_deref() == database
                        && key.table.eq_ignore_ascii_case(table)
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            vec![exact]
        };
        for key in keys {
            if let Some(entry) = self.metadata_entries.get_mut(&key) {
                entry.keywords_lower = text.clone();
            }
        }
    }

    /// 一批只清理一次旧列引用，避免共享前缀被逐表重复扫描。
    fn replace_table_columns_batch(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        columns: BTreeMap<String, Vec<CompletionColumn>>,
        db_kind: DatabaseKind,
    ) {
        let mut old_ids = BTreeSet::new();
        for table in columns.keys() {
            let key = Self::table_key(connection_id, database, schema, table);
            if let Some(ids) = self.columns_by_table.remove(&key) {
                old_ids.extend(ids);
            }
        }
        self.remove_column_ids(&old_ids);
        for (table, columns) in columns {
            self.replace_table_columns(connection_id, database, schema, &table, columns, db_kind);
        }
    }

    /// 清除指定库/schema 下「按名称匹配（忽略大小写）」的表级 dirty 标记。
    ///
    /// 存储键按 catalog 原名（§8.4 不折叠、不合并），但 dirty 标记来自 DDL 文本，
    /// 各客户端方言对未加引号标识符的折叠规则不同（PG 折小写、MySQL 视配置），
    /// 故清除与匹配都按忽略大小写进行：多清一点只是多刷一次，不会漏刷。
    /// 失效指定库/schema 的例程与触发器索引（表与列不受影响）。
    ///
    /// 例程/触发器无法按表名精确刷新，DDL 或 TTL 过期后整体失效，下次补全按需重取（§8.4）。
    fn clear_routines_and_triggers(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) {
        let db_key = Self::db_key(connection_id, database, schema);
        self.routines_by_db.remove(&db_key);
        self.triggers_by_db.remove(&db_key);
    }

    /// 清除指定库/schema 的**全部**表级 dirty 标记（库级 dirty 标记保留，由整库刷新负责）。
    fn clear_dirty_tables(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) {
        self.dirty_tables.retain(|key| {
            !(key.connection_id == connection_id
                && key.database.as_deref() == database
                && key.schema.as_deref() == schema)
        });
    }

    fn clear_dirty_table_names_matching(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) {
        self.dirty_tables.retain(|key| {
            !(key.connection_id == connection_id
                && key.database.as_deref() == database
                && key.schema.as_deref() == schema
                && key.table.eq_ignore_ascii_case(table))
        });
    }

    /// 写入某 scope 的例程索引（整批替换）。
    ///
    /// 去重键含签名：PG 同名但 identity arguments 不同的重载是不同候选，必须分别保留。
    fn insert_routines(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        routines: Vec<CompletionRoutine>,
        db_kind: DatabaseKind,
    ) {
        let db_key = Self::db_key(connection_id, database, schema);
        let mut seen = BTreeSet::new();
        let mut routine_ids = Vec::new();
        for routine in routines {
            let reference = RoutineRef {
                database: database.map(str::to_string),
                schema: routine.schema.or_else(|| schema.map(str::to_string)),
                name: routine.name,
                kind: routine.kind,
                signature: routine.signature,
            };
            let dedupe_key = (
                reference.schema.clone(),
                reference.name.clone(),
                reference.signature.clone(),
            );
            if !seen.insert(dedupe_key) {
                continue;
            }
            let routine_id = RoutineId(self.routines.len());
            self.routines.push(reference);
            routine_ids.push(routine_id);
        }
        self.routines_by_db.insert(db_key.clone(), routine_ids);
        self.touch_meta(db_key, db_kind);
    }

    fn database_routines(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> Vec<CompletionRoutine> {
        let db_key = Self::db_key(connection_id, database, schema);
        self.routines_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.routines.get(id.0))
            .map(|reference| CompletionRoutine {
                schema: reference.schema.clone(),
                name: reference.name.clone(),
                kind: reference.kind,
                signature: reference.signature.clone(),
            })
            .collect()
    }

    fn insert_snapshot(&mut self, snapshot: CompletionIndexSnapshot) {
        if snapshot.meta.app_index_version != COMPLETION_INDEX_VERSION {
            return;
        }
        let db_key = Self::db_key(
            snapshot.connection_id,
            snapshot.database.as_deref(),
            snapshot.schema.as_deref(),
        );
        self.clear_database(&db_key);
        let table_ids = snapshot
            .tables
            .into_iter()
            .map(|table| {
                let table_id = TableId(self.tables.len());
                self.tables.push(table.clone());
                self.index_table_metadata(
                    snapshot.connection_id,
                    snapshot.database.as_deref(),
                    snapshot.schema.as_deref(),
                    &table,
                );
                table_id
            })
            .collect::<Vec<_>>();
        self.tables_by_db.insert(db_key.clone(), table_ids);
        let mut search_columns: BTreeMap<(Option<String>, Option<String>, String), Vec<String>> =
            BTreeMap::new();
        for column in snapshot.columns {
            let database = column
                .database
                .clone()
                .or_else(|| snapshot.database.clone());
            let schema = column.schema.clone().or_else(|| snapshot.schema.clone());
            let table = column.table.clone();
            let table_key = Self::table_key(
                snapshot.connection_id,
                database.as_deref(),
                schema.as_deref(),
                &table,
            );
            let mut searchable = column.column.to_ascii_lowercase();
            if let Some(comment) = column.comment.as_deref().filter(|text| !text.is_empty()) {
                searchable.push(' ');
                searchable.push_str(&comment.to_ascii_lowercase());
            }
            let column_id = self.push_column(column);
            self.columns_by_table
                .entry(table_key)
                .or_default()
                .push(column_id);
            self.columns_by_db
                .entry(db_key.clone())
                .or_default()
                .push(column_id);
            self.index_column_prefixes(&db_key, column_id);
            search_columns
                .entry((database, schema, table))
                .or_default()
                .push(searchable);
        }
        for ((database, schema, table), columns) in search_columns {
            let table_key = Self::table_key(
                snapshot.connection_id,
                database.as_deref(),
                schema.as_deref(),
                &table,
            );
            if let Some(entry) = self.metadata_entries.get_mut(&table_key) {
                entry.keywords_lower = columns.join(" ");
            }
        }
        let routine_ids = snapshot
            .routines
            .into_iter()
            .map(|routine| {
                let routine_id = RoutineId(self.routines.len());
                self.routines.push(routine);
                routine_id
            })
            .collect::<Vec<_>>();
        self.routines_by_db.insert(db_key.clone(), routine_ids);
        let trigger_ids = snapshot
            .triggers
            .into_iter()
            .map(|trigger| {
                let trigger_id = TriggerId(self.triggers.len());
                self.triggers.push(trigger);
                trigger_id
            })
            .collect::<Vec<_>>();
        self.triggers_by_db.insert(db_key.clone(), trigger_ids);
        self.metas.insert(db_key, snapshot.meta);
    }

    fn snapshot(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        db_kind: DatabaseKind,
    ) -> CompletionIndexSnapshot {
        let db_key = Self::db_key(connection_id, database, schema);
        let tables = self
            .tables_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.tables.get(id.0).cloned())
            .collect::<Vec<_>>();
        let columns = self
            .columns_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.columns.get(id.0).map(|column| column.source.clone()))
            .collect::<Vec<_>>();
        let routines = self
            .routines_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.routines.get(id.0).cloned())
            .collect::<Vec<_>>();
        let triggers = self
            .triggers_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.triggers.get(id.0).cloned())
            .collect::<Vec<_>>();
        let mut meta = self
            .metas
            .get(&db_key)
            .cloned()
            .unwrap_or_else(|| empty_completion_meta(db_kind));
        meta.app_index_version = COMPLETION_INDEX_VERSION;
        meta.db_kind = db_kind;
        meta.table_count = tables.len();
        meta.table_fingerprints = table_fingerprints(&columns);
        CompletionIndexSnapshot {
            connection_id,
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
            tables,
            columns,
            routines,
            triggers,
            meta,
        }
    }

    fn clear_database(&mut self, db_key: &DbKey) {
        self.tables_by_db.remove(db_key);
        self.columns_by_db.remove(db_key);
        self.routines_by_db.remove(db_key);
        self.triggers_by_db.remove(db_key);
        self.metas.remove(db_key);
        self.dirty_databases.remove(db_key);
        self.columns_by_table.retain(|key, _| {
            key.connection_id != db_key.connection_id
                || key.database != db_key.database
                || key.schema != db_key.schema
        });
        self.metadata_entries.retain(|key, _| {
            key.connection_id != db_key.connection_id
                || key.database != db_key.database
                || key.schema != db_key.schema
        });
        self.dirty_tables.retain(|key| {
            key.connection_id != db_key.connection_id
                || key.database != db_key.database
                || key.schema != db_key.schema
        });
        self.rebuild_prefix_index();
    }

    fn mark_dirty(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) {
        let db_key = Self::db_key(connection_id, database, schema);
        self.dirty_databases.insert(db_key.clone());
        self.metas
            .entry(db_key)
            .and_modify(|meta| meta.dirty = true);
    }

    fn mark_table_dirty(
        &mut self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) {
        let db_key = Self::db_key(connection_id, database, schema);
        let table_key = Self::table_key(connection_id, database, schema, table);
        self.dirty_tables.insert(table_key);
        self.metas
            .entry(db_key)
            .and_modify(|meta| meta.dirty = true);
    }

    fn has_database_index(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> bool {
        let db_key = Self::db_key(connection_id, database, schema);
        self.tables_by_db.contains_key(&db_key) || self.columns_by_db.contains_key(&db_key)
    }

    fn is_database_dirty(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> bool {
        let db_key = Self::db_key(connection_id, database, schema);
        self.dirty_databases.contains(&db_key)
    }

    fn dirty_table_names(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> Vec<String> {
        let db_key = Self::db_key(connection_id, database, schema);
        self.dirty_tables
            .iter()
            .filter(|key| {
                key.connection_id == db_key.connection_id
                    && key.database == db_key.database
                    && key.schema == db_key.schema
            })
            .map(|key| key.table.clone())
            .collect()
    }

    fn is_dirty_or_expired(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        now: u64,
    ) -> bool {
        let db_key = Self::db_key(connection_id, database, schema);
        if self.dirty_databases.contains(&db_key) {
            return true;
        }
        if self.dirty_tables.iter().any(|key| {
            key.connection_id == db_key.connection_id
                && key.database == db_key.database
                && key.schema == db_key.schema
        }) {
            return true;
        }
        let Some(meta) = self.metas.get(&db_key) else {
            return true;
        };
        meta.dirty || now.saturating_sub(meta.last_verified_at) >= meta.ttl_seconds
    }

    fn table_columns(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) -> Vec<CompletionColumn> {
        let table_key = Self::table_key(connection_id, database, schema, table);
        self.columns_by_table
            .get(&table_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.columns.get(id.0))
            .map(indexed_column_to_completion)
            .collect()
    }

    fn database_tables(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
    ) -> Vec<CompletionTable> {
        let db_key = Self::db_key(connection_id, database, schema);
        self.tables_by_db
            .get(&db_key)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.tables.get(id.0))
            .map(|table| CompletionTable {
                database: table.database.clone(),
                schema: table.schema.clone(),
                name: table.name.clone(),
                kind: table.kind,
                comment: table.comment.clone(),
            })
            .collect()
    }

    /// 收集该连接在索引中出现过的 (database, schema) 名称，用于骨架级 schema 补全（P1.5）。
    fn database_schemas(&self, connection_id: ConnectionId) -> Vec<(Option<String>, Option<String>)> {
        let mut schemas = BTreeSet::new();
        for (key, table_ids) in &self.tables_by_db {
            if key.connection_id != connection_id {
                continue;
            }
            if table_ids.is_empty() {
                schemas.insert((key.database.clone(), key.schema.clone()));
                continue;
            }
            for table_id in table_ids {
                let Some(table) = self.tables.get(table_id.0) else {
                    continue;
                };
                schemas.insert((
                    table.database.clone().or_else(|| key.database.clone()),
                    table.schema.clone().or_else(|| key.schema.clone()),
                ));
            }
        }
        schemas.into_iter().collect()
    }

    /// 通用元数据检索：按关键词在表名、表注释、字段名/注释中匹配，返回有界候选。
    ///
    /// 只读取本地索引（不访问数据库）；多个关键词（空格/标点分隔）按任一命中处理。
    /// 同一关键词可能命中多张表，按名称精确 > 名称包含 > 注释命中 > 字段命中的顺序
    /// 排序，并保留命中的字段，供上层让 Jev/模型在少量候选里选择。
    /// 只搜未过期/未标记 dirty 的作用域，避免把已删除对象当成事实。
    fn search_fresh_metadata(
        &self,
        connection_id: ConnectionId,
        query: &str,
        limit: usize,
        now: u64,
    ) -> Vec<MetadataSearchHit> {
        let needles = metadata_search_needles(query);
        if needles.primary.is_empty() || limit == 0 {
            return Vec::new();
        }
        let matched =
            self.search_fresh_metadata_with_needles(connection_id, query, &needles.primary, limit, now);
        if !matched.is_empty() || needles.fallback.is_empty() {
            return matched;
        }
        // “订单增长率”这类整句通常不会完整出现在表名/注释里：退一步用中文二元词
        // 命中“订单”“增长”，比直接判定不存在更符合描述式找表。
        self.search_fresh_metadata_with_needles(connection_id, query, &needles.fallback, limit, now)
    }

    fn search_fresh_metadata_with_needles(
        &self,
        connection_id: ConnectionId,
        query: &str,
        needles: &[String],
        limit: usize,
        now: u64,
    ) -> Vec<MetadataSearchHit> {
        let full_needle = query.trim().to_ascii_lowercase();
        let mut matched = Vec::new();
        // 表键按 (连接, 库, schema, 表) 排序，连续的同作用域只需判断一次有效期，
        // 避免大库每张表都遍历 dirty 集合。
        let mut scope: Option<(Option<String>, Option<String>, bool)> = None;
        for (table_key, entry) in &self.metadata_entries {
            if table_key.connection_id != connection_id {
                continue;
            }
            let fresh = match &scope {
                Some((database, schema, fresh))
                    if *database == table_key.database && *schema == table_key.schema =>
                {
                    *fresh
                }
                _ => {
                    let fresh = self.is_search_scope_fresh(
                        connection_id,
                        table_key.database.as_deref(),
                        table_key.schema.as_deref(),
                        now,
                    );
                    scope = Some((table_key.database.clone(), table_key.schema.clone(), fresh));
                    fresh
                }
            };
            if !fresh {
                continue;
            }
            let name_hit = metadata_matches(&entry.title_lower, needles);
            let comment_hit = entry
                .description_lower
                .as_deref()
                .is_some_and(|comment| metadata_matches(comment, needles));
            let columns_hit = metadata_matches(&entry.keywords_lower, needles);
            if !name_hit && !comment_hit && !columns_hit {
                continue;
            }
            let hits = needles
                .iter()
                .filter(|needle| {
                    entry.title_lower.contains(needle.as_str())
                        || entry
                            .description_lower
                            .as_deref()
                            .is_some_and(|comment| comment.contains(needle.as_str()))
                        || entry.keywords_lower.contains(needle.as_str())
                })
                .count();
            let columns = if columns_hit {
                self.table_columns_for_search(
                    connection_id,
                    table_key.database.as_deref(),
                    table_key.schema.as_deref(),
                    &entry.title,
                )
                .into_iter()
                .filter(|column| {
                    metadata_matches(&column.name.to_ascii_lowercase(), needles)
                        || column.comment.as_deref().is_some_and(|comment| {
                            metadata_matches(&comment.to_ascii_lowercase(), needles)
                        })
                })
                .map(|column| column.name)
                .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let rank = if entry.title_lower == full_needle {
                0
            } else if name_hit {
                1
            } else if comment_hit {
                2
            } else {
                3
            };
            matched.push((rank, hits, entry.clone(), columns));
        }
        matched.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.title.len().cmp(&right.2.title.len()))
                .then_with(|| left.2.title.cmp(&right.2.title))
        });
        matched
            .into_iter()
            .take(limit)
            .map(|(_, _, entry, matched_keywords)| MetadataSearchHit {
                kind: entry.kind,
                title: entry.title,
                description: entry.description,
                matched_keywords,
                payload: entry.payload,
            })
            .collect()
    }

    /// 检索条目的有效期按作用域判断：表自带 schema 时同时受该 schema 与整库
    /// (库, None) 两个作用域约束，任一已建作用域 dirty 或 TTL 过期都视为不可用，
    /// 避免 DDL 整库失效后仍拿父作用域的旧表当事实；都没有元信息时不可信。
    fn is_search_scope_fresh(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        now: u64,
    ) -> bool {
        let mut scopes = BTreeSet::from([Self::db_key(connection_id, database, schema)]);
        if schema.is_some() {
            scopes.insert(Self::db_key(connection_id, database, None));
        }
        let mut has_meta = false;
        for scope in scopes {
            if !self.metas.contains_key(&scope) {
                continue;
            }
            has_meta = true;
            if self.is_dirty_or_expired(
                connection_id,
                scope.database.as_deref(),
                scope.schema.as_deref(),
                now,
            ) {
                return false;
            }
        }
        has_meta
    }

    /// 按表检索时取列：先按完整身份取；预热批次可能把列挂在无 schema 的桶下，
    /// 再按 (database, None) 兜底一次，避免 PostgreSQL 列关键词找不到表。
    fn table_columns_for_search(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
    ) -> Vec<CompletionColumn> {
        let columns = self.table_columns(connection_id, database, schema, table);
        if !columns.is_empty() || schema.is_none() {
            return columns;
        }
        self.table_columns(connection_id, database, None, table)
    }

    fn persisted_scopes_loaded(&self, connection_id: ConnectionId) -> bool {
        self.persisted_loaded_connections.contains(&connection_id)
    }

    fn mark_persisted_scopes_loaded(&mut self, connection_id: ConnectionId) {
        self.persisted_loaded_connections.insert(connection_id);
    }

    /// 连接是否有未过期的作用域，可作为 AI 找表的可信来源。
    fn has_fresh_connection_index(&self, connection_id: ConnectionId, now: u64) -> bool {
        self.tables_by_db.iter().any(|(key, tables)| {
            key.connection_id == connection_id
                && !tables.is_empty()
                && !self.is_dirty_or_expired(
                    connection_id,
                    key.database.as_deref(),
                    key.schema.as_deref(),
                    now,
                )
        })
    }

    /// 连接内是否有检索条目落在不可信作用域（过期、dirty 或无元信息）。
    /// 命中结果仍可用，但覆盖不完整，调用方应实时补齐后再报告精确命中数。
    fn has_search_scope_gaps(&self, connection_id: ConnectionId, now: u64) -> bool {
        self.metadata_entries.keys().any(|key| {
            key.connection_id == connection_id
                && !self.is_search_scope_fresh(
                    connection_id,
                    key.database.as_deref(),
                    key.schema.as_deref(),
                    now,
                )
        })
    }

    fn push_column(&mut self, source: ColumnRef) -> ColumnId {
        let lower_column = source.column.to_ascii_lowercase();
        let column_tokens = completion_tokens(&source.column);
        let indexed = IndexedColumnRef {
            source,
            lower_column,
            column_tokens,
        };
        let column_id = ColumnId(self.columns.len());
        self.columns.push(indexed);
        column_id
    }

    fn remove_table_column_ids(&mut self, table_key: &TableKey) {
        if let Some(search) = self.metadata_entries.get_mut(table_key) {
            search.keywords_lower.clear();
        }
        let Some(old_ids) = self.columns_by_table.remove(table_key) else {
            return;
        };
        let old_ids = old_ids.into_iter().collect::<BTreeSet<_>>();
        self.remove_column_ids(&old_ids);
    }

    fn remove_column_ids(&mut self, old_ids: &BTreeSet<ColumnId>) {
        if old_ids.is_empty() {
            return;
        }
        // 快照的列 schema 可以比补全桶更具体（例如 PostgreSQL search_path）。
        // 通过旧列 ID 找到实际所在桶，不能从表键猜测桶的 schema。
        let mut affected_databases = Vec::new();
        for (db_key, ids) in &mut self.columns_by_db {
            let previous_len = ids.len();
            ids.retain(|id| !old_ids.contains(id));
            if ids.len() != previous_len {
                affected_databases.push(db_key.clone());
            }
        }
        // 只移除旧列实际占用的前缀，不再为替换一张表重建所有数据库的索引。
        let mut prefixes = BTreeSet::new();
        for id in old_ids {
            if let Some(column) = self.columns.get(id.0) {
                for token in std::iter::once(column.lower_column.as_str())
                    .chain(column.column_tokens.iter().map(String::as_str))
                {
                    prefixes.extend(token_prefixes(token));
                }
            }
        }
        for db_key in affected_databases {
            for prefix in &prefixes {
                let key = PrefixKey {
                    connection_id: db_key.connection_id,
                    database: db_key.database.clone(),
                    schema: db_key.schema.clone(),
                    prefix: prefix.clone(),
                };
                if let Some(ids) = self.column_prefix_index.get_mut(&key) {
                    ids.retain(|id| !old_ids.contains(id));
                    if ids.is_empty() {
                        self.column_prefix_index.remove(&key);
                    }
                }
            }
        }
    }

    fn rebuild_prefix_index(&mut self) {
        self.column_prefix_index.clear();
        let entries = self
            .columns_by_db
            .iter()
            .flat_map(|(db_key, ids)| ids.iter().map(|id| (db_key.clone(), *id)))
            .collect::<Vec<_>>();
        for (db_key, id) in entries {
            self.index_column_prefixes(&db_key, id);
        }
    }

    fn index_column_prefixes(&mut self, db_key: &DbKey, column_id: ColumnId) {
        let Some(column) = self.columns.get(column_id.0) else {
            return;
        };
        let mut prefixes = BTreeSet::new();
        for token in std::iter::once(column.lower_column.as_str())
            .chain(column.column_tokens.iter().map(String::as_str))
        {
            for prefix in token_prefixes(token) {
                prefixes.insert(prefix);
            }
        }
        for prefix in prefixes {
            self.column_prefix_index
                .entry(PrefixKey {
                    connection_id: db_key.connection_id,
                    database: db_key.database.clone(),
                    schema: db_key.schema.clone(),
                    prefix,
                })
                .or_default()
                .push(column_id);
        }
    }

    fn touch_meta(&mut self, db_key: DbKey, db_kind: DatabaseKind) {
        let now = unix_timestamp_secs();
        let meta = self
            .metas
            .entry(db_key.clone())
            .or_insert_with(|| empty_completion_meta(db_kind));
        meta.app_index_version = COMPLETION_INDEX_VERSION;
        meta.db_kind = db_kind;
        meta.last_indexed_at = now;
        meta.last_verified_at = now;
        meta.dirty = false;
        self.dirty_databases.remove(&db_key);
    }

    /// T051：标记某库进入后台刷新。返回 false 表示已在刷新中（去重，不再触发新线程）。
    fn begin_refresh(&mut self, db_key: DbKey) -> bool {
        self.refresh_inflight.insert(db_key)
    }

    /// T051：后台刷新结束（无论成功或失败）后清除刷新标记。
    fn end_refresh(&mut self, db_key: &DbKey) {
        self.refresh_inflight.remove(db_key);
    }
}

fn completion_tokens(value: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    let lower = value.to_ascii_lowercase();
    tokens.insert(lower.clone());
    for part in lower.split(|ch: char| ch == '_' || ch == '-' || ch.is_whitespace()) {
        if !part.is_empty() {
            tokens.insert(part.to_string());
        }
    }
    for part in camel_case_tokens(value) {
        tokens.insert(part);
    }
    tokens.into_iter().collect()
}

struct MetadataSearchNeedles {
    /// 完整短语与显式分隔的词，优先精确匹配。
    primary: Vec<String>,
    /// 中文长词的二元切分；只在 primary 无命中时启用，用于描述式找表。
    fallback: Vec<String>,
}

/// 通用元数据检索关键词：保留完整短语，并把空格/常见分隔符切开。
///
/// 中文描述通常没有空格，完整短语仍可命中；英文注释和字段名可按词分别命中。
fn metadata_search_needles(query: &str) -> MetadataSearchNeedles {
    let phrase = query.trim().to_ascii_lowercase();
    if phrase.is_empty() {
        return MetadataSearchNeedles {
            primary: Vec::new(),
            fallback: Vec::new(),
        };
    }
    let mut primary = BTreeSet::from([phrase]);
    for part in query
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, ',' | '，' | ';' | '；' | '、' | '/' | '|')
        })
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
    {
        primary.insert(part);
    }
    let mut fallback = BTreeSet::new();
    for part in &primary {
        let chars = part.chars().collect::<Vec<_>>();
        if chars.len() < 3 || !chars.iter().any(|ch| is_cjk(*ch)) {
            continue;
        }
        for window in chars.windows(2) {
            let token = window.iter().collect::<String>();
            if token.chars().all(is_cjk) {
                fallback.insert(token);
            }
        }
    }
    fallback.retain(|token| !primary.contains(token));
    MetadataSearchNeedles {
        primary: primary.into_iter().collect(),
        fallback: fallback.into_iter().collect(),
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
    )
}

fn metadata_matches(haystack: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn camel_case_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_uppercase() && !current.is_empty() {
            tokens.push(current.to_ascii_lowercase());
            current.clear();
        }
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(current.to_ascii_lowercase());
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(current.to_ascii_lowercase());
    }
    tokens
}

fn token_prefixes(token: &str) -> Vec<String> {
    let token = token.trim().to_ascii_lowercase();
    let chars = token.chars().collect::<Vec<_>>();
    let max = chars.len().min(COMPLETION_PREFIX_LIMIT);
    (1..=max)
        .map(|len| chars.iter().take(len).collect::<String>())
        .collect()
}

fn indexed_column_to_completion(column: &IndexedColumnRef) -> CompletionColumn {
    CompletionColumn {
        table: column.source.table.clone(),
        database: None,
        schema: None,
        name: column.source.column.clone(),
        type_name: column.source.type_name.clone(),
        nullable: column.source.nullable,
        primary_key: column.source.primary_key,
        comment: column.source.comment.clone(),
        stable: None,
    }
}

fn empty_completion_meta(db_kind: DatabaseKind) -> CompletionIndexMeta {
    CompletionIndexMeta {
        app_index_version: COMPLETION_INDEX_VERSION,
        db_kind,
        last_indexed_at: 0,
        last_verified_at: 0,
        ttl_seconds: COMPLETION_INDEX_TTL_SECONDS,
        dirty: false,
        table_count: 0,
        table_fingerprints: Vec::new(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletionSnapshotSignature {
    table_count: usize,
    column_count: usize,
    fingerprints: Vec<TableFingerprint>,
}

impl CompletionSnapshotSignature {
    fn from_snapshot(snapshot: &CompletionIndexSnapshot) -> Self {
        Self {
            table_count: snapshot.tables.len(),
            column_count: snapshot.columns.len(),
            fingerprints: table_fingerprints(&snapshot.columns),
        }
    }
}

fn table_fingerprints(columns: &[ColumnRef]) -> Vec<TableFingerprint> {
    let mut grouped: BTreeMap<(Option<String>, Option<String>, String), Vec<&ColumnRef>> =
        BTreeMap::new();
    for column in columns {
        grouped
            .entry((
                column.database.clone(),
                column.schema.clone(),
                column.table.clone(),
            ))
            .or_default()
            .push(column);
    }
    grouped
        .into_iter()
        .map(|((database, schema, table), columns)| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            table.hash(&mut hasher);
            for column in columns {
                column.column.hash(&mut hasher);
                column.type_name.hash(&mut hasher);
                column.nullable.hash(&mut hasher);
                column.primary_key.hash(&mut hasher);
                column.ordinal_position.hash(&mut hasher);
                column.comment.hash(&mut hasher);
            }
            TableFingerprint {
                database,
                schema,
                table,
                fingerprint: hasher.finish(),
            }
        })
        .collect()
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
