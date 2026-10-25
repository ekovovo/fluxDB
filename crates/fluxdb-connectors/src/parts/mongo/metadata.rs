// MongoDB 集合/视图元数据与索引读取。
//
// 统计口径对齐 `db.collection.stats()`：拿不到（视图、权限不足、分片元数据缺失）
// 就留 `None`，不用 0 或估算值冒充。

/// 读取集合/视图元数据。
fn mongo_collection_info(
    config: &ConnectionConfig,
    database: &str,
    collection: &str,
) -> fluxdb_core::Result<MongoCollectionInfo> {
    let client = mongo_connect(config)?;
    let database_handle = client.database(database);

    // 1) listCollections 拿类型与视图定义（视图拿不到 stats/count）。
    let filter = doc! {"name": collection};
    let list_handle = database_handle.clone();
    let cursor = mongo_block_on(30, "读取集合元数据", async move {
        list_handle.list_collections().filter(filter).await
    })?;
    let spec = mongo_collect_cursor(cursor)?.into_iter().next();
    let Some(spec) = spec else {
        return Err(Error::new(
            ErrorKind::Query,
            format!("集合 `{collection}` 不存在或无权访问"),
        ));
    };

    let mut info = MongoCollectionInfo {
        name: collection.to_string(),
        ..Default::default()
    };
    match spec.collection_type {
        CollectionType::View => {
            info.kind = MongoCollectionKind::View;
            info.view_on = spec.options.view_on;
            info.view_pipeline = spec.options.pipeline.map(|stages| {
                mongo_json_text(Bson::Array(
                    stages.into_iter().map(Bson::Document).collect(),
                ))
            });
            // 视图没有自己的统计：直接返回类型与定义。
            return Ok(info);
        }
        CollectionType::Timeseries => info.kind = MongoCollectionKind::Timeseries,
        // 未知的新类型按普通集合处理（`CollectionType` 是 non_exhaustive）。
        _ => info.kind = MongoCollectionKind::Collection,
    }

    // 2) stats 拿大小与索引数；权限不足时整段跳过，不阻断页面。
    if let Ok(stats) = mongo_block_on(30, "读取集合统计", async {
        database_handle.run_command(doc! {"collStats": collection}).await
    }) {
        info.avg_document_size = mongo_bson_u64(stats.get("avgObjSize"));
        info.data_size = mongo_bson_u64(stats.get("size"));
        info.storage_size = mongo_bson_u64(stats.get("storageSize"));
        info.index_count = mongo_bson_u64(stats.get("nindexes"));
    }

    // 3) 文档数：`estimatedDocumentCount` 不扫表，先给一个可用的近似值；
    //    精确计数在集合页里按需触发（`count_documents` 大集合代价高）。
    let collection_handle = database_handle.collection::<Document>(collection);
    info.document_count = mongo_block_on(30, "读取集合文档数", async move {
        collection_handle.estimated_document_count().await
    })
    .ok();

    Ok(info)
}

/// 列出集合索引。
fn mongo_indexes(
    config: &ConnectionConfig,
    database: &str,
    collection: &str,
) -> fluxdb_core::Result<Vec<MongoIndexInfo>> {
    let client = mongo_connect(config)?;
    let collection_handle = client.database(database).collection::<Document>(collection);
    let cursor = mongo_block_on(30, "读取索引列表", async move {
        collection_handle.list_indexes().await
    })?;

    let mut indexes = mongo_collect_cursor(cursor)?
        .into_iter()
        .map(|model| {
            let options = model.options.unwrap_or_default();
            MongoIndexInfo {
                name: options.name.clone().unwrap_or_default(),
                keys_json: mongo_json_text(Bson::Document(model.keys)),
                unique: options.unique.unwrap_or(false),
                sparse: options.sparse.unwrap_or(false),
                partial_filter: options
                    .partial_filter_expression
                    .map(|filter| mongo_json_text(Bson::Document(filter))),
                // `listIndexes` 不返回构建进度；缺字段视为已就绪，不臆造"构建中"状态。
                ready: true,
            }
        })
        .collect::<Vec<_>>();
    indexes.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(indexes)
}

/// 连接信息：「显示连接信息」弹框的数据。
///
/// 只汇总服务端真实返回的字段：`hello` 给拓扑/成员，`buildInfo` 给版本与发行版，
/// `listDatabases` 给库大小。任一命令失败只留空对应字段（不阻断弹框、不编造值）。
fn mongo_server_info(config: &ConnectionConfig) -> fluxdb_core::Result<MongoServerInfo> {
    let client = mongo_connect(config)?;
    let admin = client.database("admin");

    // 1) hello：拓扑判定与成员列表。
    let hello = mongo_block_on(20, "读取 MongoDB 拓扑信息", async {
        admin.run_command(doc! {"hello": 1}).await
    })
    .map_err(|error| {
        // 连 hello 都失败说明连接本身不可用，此时弹框无内容可展示。
        Error::new(error.kind, format!("读取 MongoDB 连接信息失败：{}", error.message))
    })?;

    let mut info = MongoServerInfo {
        topology: if hello.get_str("msg") == Ok("isdbgrid") {
            MongoTopology::Sharded
        } else if hello.get_str("setName").is_ok() {
            MongoTopology::ReplicaSet
        } else if hello.contains_key("isWritablePrimary") || hello.contains_key("ismaster") {
            MongoTopology::Standalone
        } else {
            MongoTopology::Unknown
        },
        replica_set_name: hello.get_str("setName").ok().map(str::to_string),
        primary: hello.get_str("primary").ok().map(str::to_string),
        hosts: hello
            .get_array("hosts")
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(|host| host.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        max_wire_version: hello.get_i32("maxWireVersion").ok(),
        uptime_secs: mongo_bson_u64(hello.get("uptime")),
        ..Default::default()
    };

    // 2) buildInfo：版本与发行版（`modules` 含 enterprise 才是企业版）。
    if let Ok(build) = mongo_block_on(20, "读取 MongoDB 版本信息", async {
        admin.run_command(doc! {"buildInfo": 1}).await
    }) {
        info.version = build.get_str("version").ok().map(str::to_string);
        let is_enterprise = build
            .get_array("modules")
            .map(|modules| modules.iter().any(|module| module.as_str() == Some("enterprise")))
            .unwrap_or(false);
        info.edition = Some(if is_enterprise { "Enterprise" } else { "Community" }.to_string());
    }

    // 3) listDatabases：库大小。权限不足时整段跳过。
    if let Ok(list) = mongo_block_on(20, "读取数据库列表", async {
        client.list_databases().await
    }) {
        info.databases = list
            .into_iter()
            .map(|database| MongoDatabaseStat {
                name: database.name,
                size_on_disk: Some(database.size_on_disk),
                empty: database.empty,
            })
            .collect();
    }

    Ok(info)
}

/// BSON 数值（int32/int64/double）转 u64；负数与非数值返回 `None`。
fn mongo_bson_u64(value: Option<&Bson>) -> Option<u64> {
    match value? {
        Bson::Int32(number) => u64::try_from(*number).ok(),
        Bson::Int64(number) => u64::try_from(*number).ok(),
        Bson::Double(number) if *number >= 0.0 => Some(*number as u64),
        _ => None,
    }
}
