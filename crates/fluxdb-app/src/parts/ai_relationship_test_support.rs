// ER 关系工具的测试支撑：内存目录与内存元数据源。
//
// 单测覆盖的是「汇合/去重/可用性判定/提交约束」这些应用层行为，不应为了验证纯投影
// 去连真实数据库；因此这里注入假的 `ErRelationshipSource`，生产代码路径（同一套
// 汇合与判定函数）完全复用，不为测试另开一条实现。

/// 单测连接配置：只用于解析目标与授权判定，不会建立真实连接。
pub fn er_test_connection() -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId(1),
        name: "测试库".into(),
        kind: DatabaseKind::MySql,
        endpoint: Endpoint::Tcp {
            host: "localhost".into(),
            port: 3306,
            database: Some("sales".into()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
        dameng_profile: None,
        sqlserver_profile: None,
        oracle_profile: None,
    }
}

pub fn test_table(name: &str) -> ErTableRef {
    ErTableRef {
        database: "sales".into(),
        schema: None,
        name: name.into(),
    }
}

pub fn test_column(name: &str, type_name: &str) -> ErColumn {
    ErColumn {
        name: name.into(),
        type_name: Some(type_name.into()),
        primary_key: false,
        nullable: false,
        stable: None,
    }
}

pub fn test_edge(from: &str, to: &str, constraint: &str, from_column: &str, to_column: &str) -> ErForeignKeyEdge {
    let (from_reference, to_reference) = (test_table(from), test_table(to));
    ErForeignKeyEdge {
        name: constraint.into(),
        from_table: from_reference.display(),
        from_column: from_column.into(),
        to_table: to_reference.display(),
        to_column: to_column.into(),
        from_reference,
        to_reference,
    }
}

/// 内存关系目录：生产实现走 FileStorage，这里用同一 trait 的最小实现。
pub struct MemoryRelationshipStore {
    relationships: std::sync::Mutex<Vec<ErRelationship>>,
}

impl MemoryRelationshipStore {
    pub fn new(relationships: Vec<ErRelationship>) -> Self {
        Self {
            relationships: std::sync::Mutex::new(relationships),
        }
    }
}

impl ErRelationshipStore for MemoryRelationshipStore {
    fn load(&self, _scope_key: &str) -> fluxdb_core::Result<Vec<ErRelationship>> {
        Ok(self
            .relationships
            .lock()
            .expect("目录锁可用")
            .clone())
    }

    fn save(&self, _scope_key: &str, rels: &[ErRelationship]) -> fluxdb_core::Result<()> {
        *self.relationships.lock().expect("目录锁可用") = rels.to_vec();
        Ok(())
    }
}

/// 内存元数据源：外键边、字段与本地目录全部由测试给定。
pub struct FakeErSource {
    pub edges: Vec<ErForeignKeyEdge>,
    pub foreign_status: ErLoadStatus,
    pub foreign_error: bool,
    /// 表展示名 → 列。
    pub columns: BTreeMap<String, Vec<ErColumn>>,
    pub logical: Vec<ErRelationship>,
    /// 按作用域缓存服务实例：工具写入后必须能从同一个源读到，否则测不出真实落库行为。
    services: std::sync::Mutex<BTreeMap<String, Arc<ErModelService>>>,
}

impl FakeErSource {
    pub fn new() -> Self {
        Self {
            edges: Vec::new(),
            foreign_status: ErLoadStatus::Loaded,
            foreign_error: false,
            columns: BTreeMap::new(),
            logical: Vec::new(),
            services: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_column(mut self, table: &str, columns: Vec<ErColumn>) -> Self {
        self.columns.insert(table.into(), columns);
        self
    }

    pub fn with_edges(mut self, edges: Vec<ErForeignKeyEdge>) -> Self {
        self.edges = edges;
        self
    }

    pub fn with_logical(mut self, logical: Vec<ErRelationship>) -> Self {
        self.logical = logical;
        self
    }

    pub fn with_foreign_status(mut self, status: ErLoadStatus) -> Self {
        self.foreign_status = status;
        self
    }

    pub fn with_foreign_error(mut self) -> Self {
        self.foreign_error = true;
        self
    }
}

impl Default for FakeErSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ErRelationshipSource for FakeErSource {
    fn foreign_keys(
        &self,
        _config: &ConnectionConfig,
        _database: &str,
        _schema: Option<&str>,
    ) -> fluxdb_core::Result<ErRelationSnapshot> {
        if self.foreign_error {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Connection,
                "外键读取失败",
            ));
        }
        Ok(ErRelationSnapshot {
            edges: self.edges.clone(),
            status: self.foreign_status,
        })
    }

    fn table_columns(
        &self,
        _config: &ConnectionConfig,
        tables: &[ErTableRef],
    ) -> fluxdb_core::Result<ErColumnBatch> {
        Ok(ErColumnBatch {
            tables: tables
                .iter()
                .map(|table| {
                    let columns = self.columns.get(&table.name).cloned().unwrap_or_default();
                    let status = if self.columns.contains_key(&table.name) {
                        ErLoadStatus::Loaded
                    } else {
                        ErLoadStatus::NotLoaded
                    };
                    (table.display(), columns, status)
                })
                .collect(),
        })
    }

    fn local_service(&self, scope_key: &str) -> Option<Arc<ErModelService>> {
        let mut services = self.services.lock().expect("服务锁可用");
        Some(
            services
                .entry(scope_key.to_string())
                .or_insert_with(|| {
                    Arc::new(ErModelService::new(
                        scope_key.to_string(),
                        Box::new(MemoryRelationshipStore::new(self.logical.clone())),
                    ))
                })
                .clone(),
        )
    }
}

/// 单测 resolver：只登记一个连接，对象白名单由测试决定。
pub fn test_resolver() -> Arc<MapSqlResolver> {
    Arc::new(MapSqlResolver::new([er_test_connection()]))
}

pub fn test_resolver_with_scope(names: &[&str]) -> Arc<MapSqlResolver> {
    let target = AiSqlTarget {
        connection_id: ConnectionId(1),
        database: Some("sales".into()),
        schema: None,
    };
    Arc::new(
        MapSqlResolver::new([er_test_connection()])
            .with_sql_object_scope(&target, names.iter().map(|name| name.to_string()).collect()),
    )
}

/// 构造一条已确认、无物理约束的本地关系（`usage()` 的基础候选）。
pub fn confirmed_logical(
    left: &str,
    right: &str,
    role: &str,
    pairs: &[(&str, &str)],
) -> ErRelationship {
    let unknown = || fluxdb_core::ErCardinality {
        min: fluxdb_core::ErCardinalityBound::Unknown,
        max: fluxdb_core::ErCardinalityBound::Unknown,
    };
    ErRelationship {
        id: format!("local-{role}"),
        revision: 1,
        left_entity: fluxdb_core::er_entity_id(&test_table(left)),
        right_entity: fluxdb_core::er_entity_id(&test_table(right)),
        role: role.into(),
        column_pairs: pairs
            .iter()
            .map(|(left_column, right_column)| fluxdb_core::ErColumnPair {
                left_column: fluxdb_core::er_column_id(&test_table(left), left_column),
                right_column: fluxdb_core::er_column_id(&test_table(right), right_column),
            })
            .collect(),
        required_filters: Vec::new(),
        match_cardinality: fluxdb_core::ErMatchCardinality {
            left_to_right: unknown(),
            right_to_left: unknown(),
            basis: fluxdb_core::ErCardinalityBasis::Unknown,
        },
        origin: fluxdb_core::ErRelationshipOrigin::User,
        review: fluxdb_core::ErRelationshipReview {
            state: ErReviewState::Confirmed,
            confirmed_revision: Some(1),
            confirmed_by: Some("user".into()),
        },
        enforcement: fluxdb_core::ErRelationshipEnforcement {
            kind: fluxdb_core::ErEnforcementKind::None,
            constraint_ref: None,
            enforced: None,
        },
        validity: fluxdb_core::ErValidity {
            state: fluxdb_core::ErValidityState::Current,
            reason: None,
        },
        description: None,
        evidence_refs: Vec::new(),
    }
}

/// 读取单测库 `sales` 作用域的关系目录（与工具写入用的是同一个服务实例）。
pub fn er_stored(source: &FakeErSource) -> Vec<ErRelationship> {
    let scope_key = fluxdb_core::er_relationship_scope_key(ConnectionId(1), "sales", None);
    source
        .local_service(&scope_key)
        .expect("本地目录可用")
        .list()
        .expect("读取目录")
}

/// 查询作用域：默认库 `sales`、无 schema。
pub fn test_scope(config: &ConnectionConfig) -> ErRelationshipScope {
    ErRelationshipScope {
        target: AiSqlTarget {
            connection_id: config.id,
            database: Some("sales".into()),
            schema: None,
        },
        config: config.clone(),
        database: "sales".into(),
        schema: None,
        scope_key: fluxdb_core::er_relationship_scope_key(config.id, "sales", None),
    }
}
