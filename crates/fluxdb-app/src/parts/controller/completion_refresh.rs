/// T051：后台刷新 worker——取 index 中已有的（旧）表名，在线拉取这些表的列并写回 index。
/// 与同步 warm 共用同一批 free connector 查询函数，不依赖控制器 `&self`，故可由独立线程执行。
/// 返回 (刷新表数, 刷新列数)。刷新仅对成功拉取的列写回，失败时旧候选保留。
fn refresh_index_columns_in_background(
    index: &Mutex<CompletionIndex>,
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: Option<&str>,
    schema: Option<&str>,
) -> fluxdb_core::Result<(usize, usize)> {
    // T052：按影响范围限制刷新对象。库级 dirty → 刷新整库；
    // 仅部分表 dirty（非库级）→ 只刷新那些表，避免每次 DDL 都重刷整库。
    let (mut table_names, database_wide): (Vec<String>, bool) = {
        let Ok(guard) = index.lock() else {
            return Ok((0, 0));
        };
        let database_wide = guard.is_database_dirty(connection_id, database, schema);
        let dirty_tables = guard
            .dirty_table_names(connection_id, database, schema)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let tables = guard.database_tables(connection_id, database, schema);
        let large_database = tables.len() >= COMPLETION_METADATA_LIMIT as usize;
        let table_names = tables
            .into_iter()
            .filter(|table| matches!(table.kind, ObjectKind::Table | ObjectKind::View))
            // 存储键按 catalog 原名（§8.4 不折叠、不合并），而 dirty 标记来自 DDL 文本，
            // 各方言对未加引号标识符的折叠规则不同，故这里按忽略大小写匹配：多刷可接受，漏刷不行。
            .filter(|table| {
                database_wide
                    || dirty_tables.is_empty()
                    || dirty_tables
                        .iter()
                        .any(|dirty| dirty.eq_ignore_ascii_case(&table.name))
            })
            .filter(|table| {
                !large_database
                    || guard
                        .columns_by_table
                        .contains_key(&CompletionIndex::table_key(
                            connection_id,
                            database,
                            schema,
                            &table.name,
                        ))
                    || dirty_tables
                        .iter()
                        .any(|dirty| dirty.eq_ignore_ascii_case(&table.name))
            })
            .map(|table| table.name)
            .collect::<Vec<_>>();
        (table_names, database_wide)
    };
    // 触发后台刷新即说明该 scope 的元数据整体已过期（dirty 或 TTL）：例程/触发器不像列那样
    // 能按表名精确刷新，直接失效该 scope 的例程/触发器索引，下次补全按需重新拉取并写回（§8.4）。
    if let Ok(mut guard) = index.lock() {
        guard.clear_routines_and_triggers(connection_id, database, schema);
    }
    // 库级失效（DDL / 表操作）时同时重取表清单：新建、重命名、删除的表才能及时进出候选，
    // 否则索引里的表名只在冷启动时建立，删掉的表会一直被建议（§8.4 DDL 后刷新）。
    if database_wide {
        let tables = list_completion_tables_for_connection_with_cancel(
            config,
            database,
            schema,
            "",
            i64::MAX as u64,
            &|| false,
        )?;
        if let Ok(mut guard) = index.lock() {
            let names = tables
                .iter()
                .map(|table| table.name.clone())
                .collect::<BTreeSet<_>>();
            // 删除/重命名后同时移除旧列，防止全库列补全继续返回已删除对象。
            let removed = guard
                .columns_by_table
                .keys()
                .filter(|key| {
                    key.connection_id == connection_id
                        && key.database.as_deref() == database
                        && key.schema.as_deref() == schema
                        && !names.contains(&key.table)
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in removed {
                guard.remove_table_column_ids(&key);
            }
            if tables.len() < COMPLETION_METADATA_LIMIT as usize {
                table_names = names.into_iter().collect();
            } else {
                table_names.retain(|table| names.contains(table));
            }
            guard.insert_tables(connection_id, database, schema, tables, config.kind);
            // 表清单刷新完成不等于列刷新完成，失败时必须保留失效状态。
            guard.mark_dirty(connection_id, database, schema);
        }
    }
    if table_names.is_empty() {
        if let Ok(mut guard) = index.lock() {
            guard.clear_dirty_tables(connection_id, database, schema);
            guard.touch_meta(
                CompletionIndex::db_key(connection_id, database, schema),
                config.kind,
            );
        }
        return Ok((0, 0));
    }
    // 逐批成功才清除对应表的失效标记，中途失败不能把未刷新的表误判为新鲜。
    if let Ok(mut guard) = index.lock() {
        for table in &table_names {
            guard.mark_table_dirty(connection_id, database, schema, table);
        }
    }
    let mut refreshed_columns = 0;
    for chunk in table_names.chunks(COMPLETION_WARMUP_BATCH_SIZE) {
        let started = std::time::Instant::now();
        let columns = list_completion_columns_for_tables_for_connection_with_cancel(
            config,
            database,
            schema,
            chunk,
            &|| false,
        )?;
        let column_count = columns.len();
        refreshed_columns += column_count;
        let query_ms = started.elapsed().as_millis() as u64;
        // 按表名精确分组（不折叠大小写）：同一批次内，PG 端已按 search_path 只返回每个表名
        // 首个可见 schema 的列，故表名在批内唯一；折叠会让 `"Foo"` 与 `"foo"` 互相覆盖列（§8.4）。
        let mut by_table: BTreeMap<String, Vec<CompletionColumn>> = BTreeMap::new();
        for column in columns {
            by_table
                .entry(column.table.clone())
                .or_default()
                .push(column);
        }
        for table in chunk {
            by_table.entry(table.clone()).or_default();
        }
        let waiting = std::time::Instant::now();
        let lock_us;
        let lock_wait_us;
        {
            let Ok(mut guard) = index.lock() else {
                continue;
            };
            lock_wait_us = waiting.elapsed().as_micros() as u64;
            let locked = std::time::Instant::now();
            guard.replace_table_columns_batch(
                connection_id,
                database,
                schema,
                by_table,
                config.kind,
            );
            if database_wide {
                guard.mark_dirty(connection_id, database, schema);
            }
            lock_us = locked.elapsed().as_micros() as u64;
        }
        tracing::debug!(target: "gdb_sql_completion", op = "index_refresh_batch", table_count = chunk.len(), column_count, query_ms, lock_us, lock_wait_us, elapsed_ms = started.elapsed().as_millis() as u64, "补全索引分批刷新完成");
    }
    if let Ok(mut guard) = index.lock() {
        guard.touch_meta(
            CompletionIndex::db_key(connection_id, database, schema),
            config.kind,
        );
    }
    Ok((table_names.len(), refreshed_columns))
}

impl AppController {
    /// 保留可用列索引，只失效目标作用域的远程查询缓存；刷新期间继续使用旧候选。
    fn invalidate_completion_metadata(
        &self,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        tables: Option<&BTreeSet<String>>,
    ) {
        if let Ok(mut index) = self.completion_index.lock() {
            index.clear_routines_and_triggers(connection_id, database, schema);
            if let Some(tables) = tables {
                for table in tables {
                    index.mark_table_dirty(connection_id, database, schema, table);
                }
            } else {
                index.mark_dirty(connection_id, database, schema);
            }
        }
        if let Ok(mut cache) = self.completion_cache.lock() {
            cache.tables.remove(&CompletionTablesKey {
                connection_id,
                database: database.map(str::to_string),
                schema: schema.map(str::to_string),
            });
            cache.routines.remove(&CompletionRoutinesKey {
                connection_id,
                database: database.map(str::to_string),
                schema: schema.map(str::to_string),
            });
            cache.triggers.remove(&CompletionTriggersKey {
                connection_id,
                database: database.map(str::to_string),
                schema: schema.map(str::to_string),
            });
            let keep = |key: &CompletionColumnsKey| {
                key.connection_id != connection_id
                    || key.database.as_deref() != database
                    || key.schema.as_deref() != schema
                    || tables.is_some_and(|tables| {
                        !tables
                            .iter()
                            .any(|table| table.eq_ignore_ascii_case(&key.table))
                    })
            };
            cache.columns.retain(|key, _| keep(key));
            cache.foreign_keys.retain(|key, _| keep(key));
        }
        tracing::debug!(target: "gdb_sql_completion", op = "index_invalidate", ?connection_id, database, schema, database_wide = tables.is_none(), "DDL 成功，补全元数据已标记失效");
    }
}

