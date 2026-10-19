// ER 关系能力来源（物理外键 + 表字段 + 本地逻辑关系目录）。
//
// 分层：本文件在 fluxdb-app（应用层）。AI 的关系/路径工具只依赖本 trait，不直接接触
// ER 页面状态：ER 页面是否打开、画布上哪些节点可见，都不参与关系判定（否则「隐藏节点」
// 会被误读成「没有关系」）。桌面的 `AppController` 与对外 MCP 进程各自提供实现：
// 桌面复用既有 ER 目录缓存与关系服务实例（连接修订、结构失效、乐观锁全部沿用），MCP
// 独立进程自建一份缓存——它没有 UI 会话，也就不存在跨进程刷新问题。
//
// 缓存键仍带连接修订（见 `ErCatalogCache`）：连接被改写或删除后旧关系索引不会命中，
// 关系查询因此不会读到旧连接的结构。

/// AI 读取 ER 关系所需的最小能力集合。
///
/// 返回值如实表达加载状态（`ErLoadStatus`），调用方必须把它当作覆盖范围的一部分返回，
/// 不能把「未加载到」解释成「没有关系」。
pub trait ErRelationshipSource: Send + Sync {
    /// 读取整库范围的物理外键边（含加载状态）。
    fn foreign_keys(
        &self,
        config: &ConnectionConfig,
        database: &str,
        schema: Option<&str>,
    ) -> fluxdb_core::Result<ErRelationSnapshot>;

    /// 按结构化表身份按需加载字段（只请求缺失项，命中缓存不重复查询）。
    fn table_columns(
        &self,
        config: &ConnectionConfig,
        tables: &[ErTableRef],
    ) -> fluxdb_core::Result<ErColumnBatch>;

    /// 取该作用域的本地逻辑关系目录服务；未接入 ER 存储时为 `None`。
    fn local_service(&self, scope_key: &str) -> Option<Arc<ErModelService>>;
}

/// 生产实现：复用 ER 元数据缓存与按作用域复用的关系服务实例。
pub struct ErRelationshipAccess {
    catalog: Arc<Mutex<ErCatalogCache>>,
    services: Arc<Mutex<BTreeMap<String, Arc<ErModelService>>>>,
    storage: fluxdb_storage::FileStorage,
}

impl ErRelationshipAccess {
    /// 桌面装配：与 `AppController` 共享同一份缓存与服务表，避免出现第二份目录视图。
    pub fn shared(
        catalog: Arc<Mutex<ErCatalogCache>>,
        services: Arc<Mutex<BTreeMap<String, Arc<ErModelService>>>>,
        storage: fluxdb_storage::FileStorage,
    ) -> Self {
        Self {
            catalog,
            services,
            storage,
        }
    }

    /// 独立进程装配（对外 MCP）：自建缓存与服务表。
    pub fn standalone(storage: fluxdb_storage::FileStorage) -> Self {
        Self {
            catalog: Arc::new(Mutex::new(ErCatalogCache::default())),
            services: Arc::new(Mutex::new(BTreeMap::new())),
            storage,
        }
    }

    /// 按作用域复用关系服务实例：同一目录的所有写操作仍在服务内串行，
    /// 不会因为调用方换了（工具每轮重新装配）而绕过乐观锁。
    fn service(&self, scope_key: &str) -> Option<Arc<ErModelService>> {
        let mut services = self.services.lock().ok()?;
        if let Some(service) = services.get(scope_key) {
            return Some(service.clone());
        }
        let service = Arc::new(ErModelService::new(
            scope_key.to_string(),
            Box::new(FileErRelationshipStore::new(
                self.storage.clone(),
                scope_key.to_string(),
            )),
        ));
        services.insert(scope_key.to_string(), service.clone());
        Some(service)
    }
}

impl ErRelationshipSource for ErRelationshipAccess {
    fn foreign_keys(
        &self,
        config: &ConnectionConfig,
        database: &str,
        schema: Option<&str>,
    ) -> fluxdb_core::Result<ErRelationSnapshot> {
        let connector = connector_for(config)?;
        er_relations_core(
            &self.catalog,
            config,
            database,
            schema,
            connector.as_ref(),
        )
    }

    fn table_columns(
        &self,
        config: &ConnectionConfig,
        tables: &[ErTableRef],
    ) -> fluxdb_core::Result<ErColumnBatch> {
        let connector = connector_for(config)?;
        er_columns_core(&self.catalog, config, tables, connector.as_ref())
    }

    fn local_service(&self, scope_key: &str) -> Option<Arc<ErModelService>> {
        self.service(scope_key)
    }
}

impl AppController {
    /// AI 关系工具的能力来源；未注册 ER 模型存储时为 `None`（工具不进入目录）。
    pub fn er_relationship_source(&self) -> Option<Arc<dyn ErRelationshipSource>> {
        let storage = self.er_model_storage.clone()?;
        Some(Arc::new(ErRelationshipAccess::shared(
            std::sync::Arc::clone(&self.er_catalog),
            std::sync::Arc::clone(&self.er_model_services),
            storage,
        )))
    }
}