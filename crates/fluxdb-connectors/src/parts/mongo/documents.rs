// MongoDB 文档读取与数据库/集合的增删。

/// 列出可见数据库。系统库（admin/local/config）不展示。
fn mongo_list_databases(config: &ConnectionConfig) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    let client = mongo_connect(config)?;
    let names = mongo_block_on(30, "读取 MongoDB 数据库列表", async move {
        client.list_database_names().await
    })?;

    let mut names: Vec<String> = names
        .into_iter()
        .filter(|name| !MONGO_SYSTEM_DATABASES.contains(&name.as_str()))
        .collect();
    names.sort();
    // 超上限只截断展示，不报错：浏览器用途，用户可用搜索定位。
    names.truncate(MONGO_SIDEBAR_DATABASE_LIMIT);
    Ok(names
        .into_iter()
        .map(|name| database_object(config.id, &name))
        .collect())
}

/// 列出库内集合与视图。视图标记为 `ObjectKind::View`（只读）。
fn mongo_list_collections(
    config: &ConnectionConfig,
    connection_id: ConnectionId,
    database: &str,
) -> fluxdb_core::Result<Vec<ObjectSummary>> {
    let client = mongo_connect(config)?;
    let database_handle = client.database(database);

    // `listCollections` 一次拿到类型（collection / view / timeseries），比逐个探测省往返。
    let cursor = mongo_block_on(30, "读取 MongoDB 集合列表", async move {
        database_handle.list_collections().await
    })?;

    let mut objects = Vec::new();
    for spec in mongo_collect_cursor(cursor)? {
        if spec.name.starts_with("system.") {
            continue;
        }
        // 时间序列集合按普通集合处理（可读可写），只有视图只读。
        let kind = match spec.collection_type {
            CollectionType::View => ObjectKind::View,
            // 时间序列可读可写；未知的新类型也按普通集合处理（`CollectionType` 是 non_exhaustive）。
            _ => ObjectKind::Collection,
        };
        objects.push(ObjectSummary {
            path: ObjectPath {
                connection_id,
                database: Some(database.to_string()),
                schema: None,
                name: spec.name,
                kind,
            },
            rows: None,
            modified_at: None,
            comment: None,
            stable: None,
        });
    }
    objects.sort_by(|left, right| left.path.name.cmp(&right.path.name));
    Ok(objects)
}

/// 把驱动游标读空（游标本身按批拉取，单批失败即整体失败，不静默丢数据）。
fn mongo_collect_cursor<T>(cursor: mongodb::Cursor<T>) -> fluxdb_core::Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    mongo_runtime().block_on(async move {
        use futures_util::TryStreamExt as _;
        cursor.try_collect::<Vec<T>>().await.map_err(mongo_error)
    })
}

/// 执行 `find`。
///
/// 取 `limit + 1` 条判定 `has_more`，避免为了翻页额外做全表 count；
/// `total` 只在结果集不足一页时填充（此时已知总数），其余保持 `None`，不伪造统计。
fn mongo_find(
    config: &ConnectionConfig,
    database: &str,
    request: &MongoFindRequest,
    should_cancel: &dyn Fn() -> bool,
) -> fluxdb_core::Result<MongoDocumentsPage> {
    if request.collection.trim().is_empty() {
        return Err(Error::new(ErrorKind::Query, "请指定集合名"));
    }
    let filter = mongo_parse_object("Filter", &request.filter)?;
    let projection = mongo_parse_object("Projection", &request.projection)?;
    let sort = mongo_parse_object("Sort", &request.sort)?;
    mongo_validate_sort(&sort)?;

    let limit = request.limit.clamp(1, MONGO_MAX_DOCUMENTS_PER_QUERY);
    let client = mongo_connect(config)?;
    let collection = client.database(database).collection::<Document>(&request.collection);

    let mut find = collection.find(filter);
    if !projection.is_empty() {
        find = find.projection(projection);
    }
    if !sort.is_empty() {
        find = find.sort(sort);
    }
    let find = find
        .skip(request.skip)
        // 多取一条用于判定 has_more。
        .limit(limit.saturating_add(1) as i64)
        .batch_size(limit.min(200) as u32);

    let mut cursor = mongo_block_on(60, "MongoDB 查询", async move { find.await })?;

    let mut documents = Vec::new();
    let mut fetched: u64 = 0;
    loop {
        if should_cancel() {
            return Err(Error::new(ErrorKind::Cancelled, "查询已取消"));
        }
        let next = mongo_runtime().block_on(async {
            use futures_util::TryStreamExt as _;
            cursor.try_next().await
        });
        match next {
            Ok(Some(document)) => {
                fetched += 1;
                if fetched > limit {
                    // 多取到的那条只用于判定还有下一页，不返回。
                    break;
                }
                documents.push(MongoDocument {
                    id: mongo_document_id(&document),
                    json: mongo_json_text(Bson::Document(document)),
                });
            }
            Ok(None) => break,
            Err(error) => return Err(mongo_error(error)),
        }
    }

    let has_more = fetched > limit;
    // 不足一页时总数可确定（skip + 本页条数），无需再查。
    let total = (!has_more).then_some(request.skip + documents.len() as u64);
    Ok(MongoDocumentsPage {
        returned: documents.len() as u64,
        documents,
        total,
        has_more,
    })
}

/// 执行聚合管道。`pipeline` 必须是 Extended JSON **数组**文本，元素为各个 stage 文档。
///
/// 结果集上限与 `find` 一致（`MONGO_MAX_DOCUMENTS_PER_QUERY`），避免聚合把内存打满；
/// 超出部分不返回，由 `has_more` 告知 UI。
fn mongo_aggregate(
    config: &ConnectionConfig,
    path: &ObjectPath,
    pipeline: &str,
) -> fluxdb_core::Result<MongoDocumentsPage> {
    let database = path
        .database
        .as_deref()
        .filter(|database| !database.trim().is_empty())
        .ok_or_else(|| Error::new(ErrorKind::Query, "请指定数据库"))?;
    let collection = path.name.trim();
    if collection.is_empty() {
        return Err(Error::new(ErrorKind::Query, "请指定集合名"));
    }
    let text = pipeline.trim();
    // 空管道等价于「不筛选」，直接返回前 N 条。
    let stages: Vec<Document> = if text.is_empty() {
        Vec::new()
    } else {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
            Error::new(ErrorKind::Query, format!("聚合管道不是合法的 JSON：{error}"))
        })?;
        let value = Bson::try_from(value).map_err(|error| {
            Error::new(
                ErrorKind::Query,
                format!("聚合管道不是合法的 Extended JSON：{error}"),
            )
        })?;
        match value {
            Bson::Array(items) => items
                .into_iter()
                .map(|item| match item {
                    Bson::Document(stage) => Ok(stage),
                    _ => Err(Error::new(ErrorKind::Query, "聚合管道的每个阶段必须是对象")),
                })
                .collect::<fluxdb_core::Result<Vec<_>>>()?,
            _ => return Err(Error::new(ErrorKind::Query, "聚合管道必须是 Extended JSON 数组")),
        }
    };

    let limit = MONGO_MAX_DOCUMENTS_PER_QUERY;
    let client = mongo_connect(config)?;
    let database = database.to_string();
    let collection = collection.to_string();
    let cursor = mongo_block_on(60, "MongoDB 聚合", async move {
        client
            .database(&database)
            .collection::<Document>(&collection)
            .aggregate(stages)
            .await
    })?;
    let documents = mongo_collect_cursor(cursor)?;
    let has_more = documents.len() as u64 > limit;
    let documents = documents
        .into_iter()
        .take(limit as usize)
        .map(|document| MongoDocument {
            id: mongo_document_id(&document),
            json: mongo_json_text(Bson::Document(document)),
        })
        .collect::<Vec<_>>();
    Ok(MongoDocumentsPage {
        returned: documents.len() as u64,
        documents,
        total: None,
        has_more,
    })
}

/// 排序校验：值只能是 1 / -1（`$meta` 等文本排序暂不支持，显式拒绝）。
fn mongo_validate_sort(sort: &Document) -> fluxdb_core::Result<()> {
    for (field, value) in sort {
        let direction = match value {
            Bson::Int32(number) => *number as i64,
            Bson::Int64(number) => *number,
            Bson::Double(number) => *number as i64,
            _ => {
                return Err(Error::new(
                    ErrorKind::Query,
                    format!("Sort 字段 `{field}` 的方向必须是 1 或 -1"),
                ));
            }
        };
        if direction != 1 && direction != -1 {
            return Err(Error::new(
                ErrorKind::Query,
                format!("Sort 字段 `{field}` 的方向必须是 1 或 -1"),
            ));
        }
    }
    Ok(())
}

/// 创建集合。MongoDB 的库由首个集合隐式创建，因此这里也承担"建库"职责。
fn mongo_create_collection(
    config: &ConnectionConfig,
    database: &str,
    collection: &str,
) -> fluxdb_core::Result<()> {
    let database = database.trim();
    let collection = collection.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "请填写数据库名"));
    }
    if collection.is_empty() {
        return Err(Error::new(ErrorKind::Query, "请填写集合名"));
    }
    // MongoDB 命名的硬限制：库名不能含 `/\."$` 与空字符；集合名不能含 `$` 且不能为空。
    if database.contains(['/', '\\', '.', '"', '$', '\0']) {
        return Err(Error::new(
            ErrorKind::Query,
            "数据库名不能包含 / \\ . \" $ 或空字符",
        ));
    }
    if collection.contains(['$', '\0']) {
        return Err(Error::new(ErrorKind::Query, "集合名不能包含 $ 或空字符"));
    }

    let client = mongo_connect(config)?;
    let database = database.to_string();
    let collection = collection.to_string();
    mongo_block_on(30, "创建集合", async move {
        client
            .database(&database)
            .create_collection(collection)
            .await
    })
    .map(|_| ())
}

/// 删除集合。
fn mongo_drop_collection(
    config: &ConnectionConfig,
    database: &str,
    collection: &str,
) -> fluxdb_core::Result<()> {
    if database.trim().is_empty() || collection.trim().is_empty() {
        return Err(Error::new(ErrorKind::Query, "请指定数据库与集合"));
    }
    let client = mongo_connect(config)?;
    let database = database.to_string();
    let collection = collection.to_string();
    mongo_block_on(30, "删除集合", async move {
        client
            .database(&database)
            .collection::<Document>(&collection)
            .drop()
            .await
    })
    .map(|_| ())
}

/// 删除数据库（连带其中全部集合）。
fn mongo_drop_database(config: &ConnectionConfig, database: &str) -> fluxdb_core::Result<()> {
    let database = database.trim();
    if database.is_empty() {
        return Err(Error::new(ErrorKind::Query, "请指定数据库"));
    }
    if MONGO_SYSTEM_DATABASES.contains(&database) {
        return Err(Error::new(ErrorKind::Query, "系统库不允许删除"));
    }
    let client = mongo_connect(config)?;
    let database = database.to_string();
    mongo_block_on(60, "删除数据库", async move {
        client.database(&database).drop().await
    })
    .map(|_| ())
}

/// 执行数据库命令。接受 Extended JSON 对象文本（如 `{"ping": 1}`、`{"count": "orders"}`）。
fn mongo_run_command(
    config: &ConnectionConfig,
    database: &str,
    command: &str,
) -> fluxdb_core::Result<String> {
    let document = mongo_parse_object("命令", command)?;
    if document.is_empty() {
        return Err(Error::new(ErrorKind::Query, "命令不能为空"));
    }
    let client = mongo_connect(config)?;
    let database = if database.trim().is_empty() {
        "admin".to_string()
    } else {
        database.to_string()
    };
    mongo_block_on(60, "执行 MongoDB 命令", async move {
        client.database(&database).run_command(document).await
    })
    .map(mongo_command_text)
}
