// SQL 补全接线：把 `fluxdb-completion` 引擎接到现有索引、缓存与 UI 通道。
//
// 本文件只做适配，不做语义判断：方言映射、`MetadataSource` 实现、候选转换与
// 个性化同分重排。语法/槽位/排序全部由引擎负责。

use std::cell::RefCell;

use fluxdb_completion::{
    Candidate, CandidateKind, ColumnMeta, CompletionDialect, CompletionRequest, ForeignKeyMeta,
    MetadataSource, RoutineKind, RoutineMeta, TableKey as EngineTableKey, TableKind, TableMeta,
};

/// 连接类型 → 补全方言（TiDB 与 MySQL 语法一致；Mongo/Redis 走通用词表）。
fn app_completion_dialect(kind: DatabaseKind) -> &'static dyn CompletionDialect {
    match kind {
        DatabaseKind::MySql | DatabaseKind::TiDb => &fluxdb_completion::MYSQL,
        DatabaseKind::Sqlite => &fluxdb_completion::SQLITE,
        DatabaseKind::Postgres => &fluxdb_completion::POSTGRES,
        DatabaseKind::Dameng => &fluxdb_completion::DAMENG,
        DatabaseKind::MongoDb | DatabaseKind::Redis => &fluxdb_completion::GENERIC,
    }
}

/// 一次补全请求内的元数据适配：列与外键按表缓存（同一请求会重复查询同一张表）。
struct IndexMetadataSource<'a> {
    controller: &'a AppController,
    config: &'a ConnectionConfig,
    connection_id: ConnectionId,
    database: Option<String>,
    /// 连接级 schema 作用域（PG search_path 首项）。
    schema: Option<String>,
    /// 达梦默认 schema：该 schema 无对象时回退全库。
    fallback_schema: Option<String>,
    should_cancel: &'a dyn Fn() -> bool,
    columns: RefCell<std::collections::HashMap<EngineTableKey, Vec<ColumnMeta>>>,
    foreign_keys: RefCell<std::collections::HashMap<EngineTableKey, Vec<ForeignKeyMeta>>>,
}

impl<'a> IndexMetadataSource<'a> {
    fn new(
        controller: &'a AppController,
        config: &'a ConnectionConfig,
        connection_id: ConnectionId,
        database: Option<&str>,
        schema: Option<&str>,
        fallback_schema: Option<String>,
        should_cancel: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            controller,
            config,
            connection_id,
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
            fallback_schema,
            should_cancel,
            columns: RefCell::new(std::collections::HashMap::new()),
            foreign_keys: RefCell::new(std::collections::HashMap::new()),
        }
    }

    fn cancelled(&self) -> bool {
        (self.should_cancel)()
    }

    fn fetch_tables(&self, schema: Option<&str>) -> Vec<TableMeta> {
        if self.cancelled() {
            return Vec::new();
        }
        match self.controller.indexed_completion_tables_with_cancel(
            self.config,
            self.connection_id,
            self.database.as_deref(),
            schema,
            self.should_cancel,
        ) {
            Ok(tables) => tables
                .into_iter()
                .map(|table| TableMeta {
                    schema: table.schema,
                    name: table.name,
                    kind: match table.kind {
                        ObjectKind::View => TableKind::View,
                        _ => TableKind::Table,
                    },
                    comment: table.comment,
                })
                .collect(),
            Err(error) => {
                tracing::warn!(
                    target: "gdb_sql_completion",
                    error = %error,
                    "table metadata unavailable; keeping local candidates"
                );
                Vec::new()
            }
        }
    }

    fn fetch_columns(&self, key: &EngineTableKey, schema: Option<&str>) -> Vec<ColumnMeta> {
        if self.cancelled() {
            return Vec::new();
        }
        match self.controller.indexed_table_completion_columns_with_cancel(
            self.config,
            self.connection_id,
            key.database.as_deref().or(self.database.as_deref()),
            schema,
            &key.table,
            self.should_cancel,
        ) {
            Ok(columns) => columns
                .into_iter()
                .map(|column| ColumnMeta {
                    table: column.table,
                    name: column.name,
                    type_name: column.type_name,
                    nullable: column.nullable,
                    primary_key: column.primary_key,
                    comment: column.comment,
                })
                .collect(),
            Err(error) => {
                tracing::warn!(
                    target: "gdb_sql_completion",
                    table = %key.table,
                    error = %error,
                    "column metadata unavailable"
                );
                Vec::new()
            }
        }
    }

    /// 表名 → schema：AST 里常只有裸表名，索引按 schema 分桶。
    fn resolve_schema(&self, table: &str) -> Option<String> {
        self.tables(self.schema.as_deref())
            .into_iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(table))?
            .schema
    }
}

impl MetadataSource for IndexMetadataSource<'_> {
    fn tables(&self, schema: Option<&str>) -> Vec<TableMeta> {
        let mut tables = self.fetch_tables(schema);
        // 达梦默认 schema 不是可靠的会话默认值：对象树里已展开的其他 schema
        // 也要并入候选（插入文本由方言带 schema 限定）；默认 schema 无对象时
        // 直接回退全库。
        if schema.is_some() && schema == self.fallback_schema.as_deref() {
            if tables.is_empty() {
                tables = self.fetch_tables(None);
            }
            let mut seen = tables
                .iter()
                .map(|table| {
                    (
                        table.schema.clone(),
                        table.name.to_ascii_lowercase(),
                    )
                })
                .collect::<std::collections::HashSet<_>>();
            for table in loaded_completion_tables(
                &self.controller.state,
                self.connection_id,
                self.database.as_deref(),
                None,
            ) {
                let key = (table.schema.clone(), table.name.to_ascii_lowercase());
                if seen.insert(key) {
                    tables.push(TableMeta {
                        schema: table.schema,
                        name: table.name,
                        kind: match table.kind {
                            ObjectKind::View => TableKind::View,
                            _ => TableKind::Table,
                        },
                        comment: table.comment,
                    });
                }
            }
        }
        tables
    }

    fn schemas(&self) -> Vec<String> {
        if self.cancelled() {
            return Vec::new();
        }
        self.controller
            .completion_schemas(self.config, self.connection_id, self.database.as_deref())
            .map(|schemas| {
                schemas
                    .into_iter()
                    .filter_map(|(database, schema)| schema.or(database))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn columns(&self, key: &EngineTableKey) -> Vec<ColumnMeta> {
        if let Some(cached) = self.columns.borrow().get(key).cloned() {
            return cached;
        }
        let mut columns = self.fetch_columns(key, key.schema.as_deref());
        // 裸表名且默认桶未命中时，按表名回查所属 schema。
        if columns.is_empty()
            && key.schema.is_none()
            && let Some(schema) = self.resolve_schema(&key.table)
        {
            columns = self.fetch_columns(key, Some(&schema));
        }
        self.columns.borrow_mut().insert(key.clone(), columns.clone());
        columns
    }

    fn foreign_keys(&self, key: &EngineTableKey) -> Vec<ForeignKeyMeta> {
        if let Some(cached) = self.foreign_keys.borrow().get(key).cloned() {
            return cached;
        }
        let schema = key.schema.as_deref().or(self.schema.as_deref());
        let foreign_keys = if self.cancelled() {
            Vec::new()
        } else {
            self.controller
                .completion_foreign_keys_with_cancel(
                    self.config,
                    self.connection_id,
                    key.database.as_deref().or(self.database.as_deref()),
                    schema,
                    &key.table,
                    self.should_cancel,
                )
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        target: "gdb_sql_completion",
                        table = %key.table,
                        error = %error,
                        "foreign key metadata unavailable; keeping other join candidates"
                    );
                    Vec::new()
                })
                .into_iter()
                .map(|foreign_key| ForeignKeyMeta {
                    columns: if foreign_key.columns.is_empty() {
                        vec![foreign_key.column]
                    } else {
                        foreign_key.columns
                    },
                    ref_schema: foreign_key.ref_schema,
                    ref_table: foreign_key.ref_table,
                    ref_columns: if foreign_key.ref_columns.is_empty() {
                        vec![foreign_key.ref_column]
                    } else {
                        foreign_key.ref_columns
                    },
                })
                .collect()
        };
        self.foreign_keys
            .borrow_mut()
            .insert(key.clone(), foreign_keys.clone());
        foreign_keys
    }

    fn routines(&self, schema: Option<&str>) -> Vec<RoutineMeta> {
        if self.cancelled() {
            return Vec::new();
        }
        let schema = schema.or(self.schema.as_deref());
        self.controller
            .indexed_completion_routines_with_cancel(
                self.config,
                self.connection_id,
                self.database.as_deref(),
                schema,
                self.should_cancel,
            )
            .unwrap_or_else(|error| {
                tracing::warn!(
                    target: "gdb_sql_completion",
                    error = %error,
                    "routine metadata unavailable"
                );
                Vec::new()
            })
            .into_iter()
            .map(|routine| RoutineMeta {
                schema: routine.schema,
                name: routine.name,
                signature: routine.signature,
                kind: match routine.kind {
                    CompletionRoutineKind::Function => RoutineKind::Function,
                    CompletionRoutineKind::Procedure => RoutineKind::Procedure,
                },
            })
            .collect()
    }
}

impl AppController {
    /// 新引擎入口：词法 + 容错解析 + 槽位 → 候选，再套宿主的引号与个性化排序。
    fn query_completions_from_engine(
        &self,
        editor: &QueryEditorState,
        cursor: usize,
        explicit: bool,
        should_cancel: &dyn Fn() -> bool,
    ) -> fluxdb_core::Result<QueryCompletionResult> {
        let started = std::time::Instant::now();
        let config = self
            .connection_config(editor.connection_id)
            .ok_or_else(|| Error::new(ErrorKind::Connection, "连接不存在"))?;
        let database = editor.database.as_deref();
        self.maybe_refresh_expired_index_in_background(config, editor.connection_id, database);

        let cursor = cursor.min(editor.text.len());
        if should_cancel() {
            return Ok(QueryCompletionResult {
                replace_start: cursor,
                replace_end: cursor,
                items: Vec::new(),
            });
        }

        // 达梦默认 schema 由连接配置提供；其余方言沿用编辑器 schema 作用域。
        let fallback_schema = default_completion_schema(config);
        let schema = editor.schema.as_deref().or(fallback_schema.as_deref());
        let source = IndexMetadataSource::new(
            self,
            config,
            editor.connection_id,
            database,
            schema,
            fallback_schema.clone(),
            should_cancel,
        );
        let request = CompletionRequest {
            text: &editor.text,
            cursor,
            explicit,
            database,
            schema,
        };
        let dialect = app_completion_dialect(config.kind);
        let output = fluxdb_completion::complete(&request, dialect, &source);

        let mut items = output
            .items
            .into_iter()
            .map(|candidate| completion_item_from_engine(candidate, database))
            .collect::<Vec<_>>();
        qualify_ambiguous_tables(&mut items, database);

        // 表/列/模式按方言补引号，与旧行为一致；函数仅在必要时引用。
        let reserved = |word: &str| {
            dialect
                .keywords()
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(word))
        };
        for item in &mut items {
            quote_completion_insert_text(item, config.kind, &reserved);
        }
        apply_personalized_order(&mut items, &self.recency);

        let result = QueryCompletionResult {
            replace_start: output.replace_start,
            replace_end: output.replace_end,
            items,
        };
        tracing::debug!(
            target: "gdb_sql_perf",
            op = "app_completion",
            elapsed_us = started.elapsed().as_micros() as u64,
            text_bytes = editor.text.len(),
            cursor,
            item_count = result.items.len(),
            top_candidates = %completion_top_candidates(&result.items, 8),
        );
        Ok(result)
    }
}

/// 引擎候选 → 宿主补全项。表/视图的 detail 展示所属 namespace，注释进文档。
fn completion_item_from_engine(
    candidate: Candidate,
    database: Option<&str>,
) -> QueryCompletionItem {
    let Candidate {
        label,
        insert_text,
        kind,
        detail,
        documentation,
        filter_text,
        sort_text,
        schema,
    } = candidate;
    let mut insert_text_format = InsertTextFormat::PlainText;
    let kind = match kind {
        CandidateKind::Keyword => QueryCompletionKind::Keyword,
        CandidateKind::Snippet => {
            insert_text_format = InsertTextFormat::Snippet;
            QueryCompletionKind::Snippet
        }
        CandidateKind::Schema => QueryCompletionKind::Schema,
        CandidateKind::Table => QueryCompletionKind::Table,
        CandidateKind::View => QueryCompletionKind::View,
        CandidateKind::Column => QueryCompletionKind::Column,
        CandidateKind::Function => QueryCompletionKind::Function,
        CandidateKind::Procedure => QueryCompletionKind::Procedure,
    };
    let (detail, documentation) = if matches!(
        kind,
        QueryCompletionKind::Table | QueryCompletionKind::View
    ) {
        let namespace = schema.as_deref().map(|schema| match database {
            Some(database) => format!("{database}.{schema}"),
            None => schema.to_string(),
        });
        (namespace, documentation.or(detail))
    } else {
        (detail, documentation)
    };
    QueryCompletionItem {
        label,
        insert_text,
        kind,
        detail,
        documentation,
        filter_text,
        sort_text,
        insert_text_format,
        schema,
    }
}

/// 同名表出现在多个 schema/库时加上 namespace，避免点错对象。
fn qualify_ambiguous_tables(items: &mut [QueryCompletionItem], database: Option<&str>) {
    let mut owners: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for item in items.iter() {
        if !matches!(
            item.kind,
            QueryCompletionKind::Table | QueryCompletionKind::View
        ) {
            continue;
        }
        // 只统计真正带 namespace 的对象：CTE / 派生表（无 schema）不是跨 schema 的同名
        // 对象，拿连接库名当它的 namespace 会把它误标成 `库.名`。
        if let Some(namespace) = item.schema.clone() {
            owners
                .entry(item.label.to_ascii_lowercase())
                .or_default()
                .insert(namespace);
        }
    }
    for item in items.iter_mut() {
        if !matches!(
            item.kind,
            QueryCompletionKind::Table | QueryCompletionKind::View
        ) || item.label.contains('.')
        {
            continue;
        }
        let Some(namespace) = item
            .schema
            .clone()
            .or_else(|| database.map(str::to_string))
        else {
            continue;
        };
        if owners
            .get(&item.label.to_ascii_lowercase())
            .is_some_and(|namespaces| namespaces.len() > 1)
        {
            let qualified = format!("{namespace}.{}", item.label);
            item.label = qualified.clone();
            item.insert_text = qualified;
        }
    }
}

/// 个性化只做同分内重排：不改变语义分层的先后。
fn apply_personalized_order(items: &mut Vec<QueryCompletionItem>, recency: &Mutex<RecencyFrequency>) {
    let Ok(recency) = recency.lock() else {
        return;
    };
    let personal = items
        .iter()
        .map(|item| recency.score(&item.label))
        .collect::<Vec<_>>();
    if personal.iter().all(|score| *score == 0) {
        return;
    }
    let mut order = items
        .iter()
        .zip(personal)
        .enumerate()
        .map(|(index, (item, personal))| {
            let score = item
                .sort_text
                .as_deref()
                .and_then(|sort_text| sort_text.split_once(':'))
                .and_then(|(score, _)| score.parse::<i32>().ok())
                .unwrap_or(0);
            (index, score, personal)
        })
        .collect::<Vec<_>>();
    order.sort_by_key(|&(_, score, personal)| (-score, -personal));
    let reordered = order
        .into_iter()
        .map(|(index, _, _)| items[index].clone())
        .collect();
    *items = reordered;
}
