#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    AiConversationLoaded(AiConversationView),
    /// AI 设置列表已刷新。
    AiSettingsLoaded,
    /// 模型启停状态已变化（id、启用、revision）。
    AiModelEnabled {
        id: String,
        enabled: bool,
        revision: i64,
    },
    /// 内置插件实例启停状态已变化。
    AiPluginInstanceEnabled {
        id: String,
        enabled: bool,
        revision: i64,
    },
    /// AI 设置操作失败（CAS 冲突、存储未初始化等）；UI 用主窗口 show_message 展示。
    AiSettingsError(String),
    /// 模型已保存（id、新 revision）。
    AiModelSaved {
        id: String,
        revision: i64,
    },
    AiModelDeleted {
        id: String,
    },
    /// Agent 已保存（id、新 revision）。
    AiAgentSaved {
        id: String,
        revision: i64,
    },
    AiAgentDeleted {
        id: String,
    },
    /// 会话已重命名（id、新标题、新 revision）。
    AiSessionRenamed {
        id: String,
        title: String,
        revision: i64,
    },
    AiSessionArchived {
        id: String,
        revision: i64,
    },
    AiSessionRestored {
        id: String,
        revision: i64,
    },
    /// 默认模型选择已保存（运行设置新 revision）。
    AiDefaultModelsSaved {
        revision: i64,
    },
    /// 模型连接探测结果（status 见 `test_ai_model`）。
    AiModelTested {
        id: String,
        status: String,
    },
    /// 已拉取到 Provider 的模型 ID 列表（已排序去重）。
    AiModelsFetched {
        models: Vec<String>,
        capabilities: Vec<AiDiscoveredModel>,
    },
    /// 拉取模型列表失败（message 为脱敏后的可读原因）。
    AiModelsFetchFailed {
        message: String,
    },
    ErRelationshipsLoaded {
        scope_key: String,
        relationships: Vec<ErRelationship>,
    },
    ErRelationshipChanged {
        scope_key: String,
        relationship: ErRelationship,
    },
    ErRelationshipDeleted {
        scope_key: String,
        id: String,
    },
    BackupPrepared(BackupRequest),
    BackupCompleted(BackupManifest),
    RestorePrepared {
        request: RestoreRequest,
        plan: RestorePlan,
    },
    RestoreObjectsProbed(Vec<RestoreObjectProbe>),
    RestoreCompleted(RestoreOutcome),
    ConnectionsLoaded(Vec<ConnectionConfig>),
    SidebarLayoutChanged(SidebarLayout),
    ConnectionCreated(ConnectionConfig),
    ConnectionUpdated(ConnectionConfig),
    ConnectionGroupCreated(ConnectionGroup),
    ConnectionGroupDeleted(ConnectionGroupId),
    ConnectionDeleted(ConnectionId),
    DatabaseDeleted {
        connection_id: ConnectionId,
        database: String,
    },
    ConnectionTested(ConnectionId, fluxdb_core::Result<()>),
    /// 自动发现 / 连接串导入成功，弹框可据此预填一条 Redis 连接草稿。
    RedisConnectionDiscovered(ConnectionDraft),
    ObjectsLoaded(Option<ObjectPath>, Vec<ObjectSummary>),
    /// schema 新建成功；UI 据此失效该库 schema 缓存并重取，使新 schema 出现在树。
    SchemaCreated {
        connection_id: ConnectionId,
        schema: String,
    },
    /// PG 角色列表加载完成（T26）。
    PgRolesLoaded(ConnectionId, Vec<fluxdb_core::PgRole>),
    /// PG 角色变更成功（CRUD 后 UI 重取列表）。
    PgRoleChanged(ConnectionId),
    /// PG 对象权限读取完成（对象读模型 + 选中角色生效权限）；Err 为读取失败。
    UserAdminPgObjectGrantsLoaded(
        TabId,
        Result<(PgObjectGrants, Vec<PgEffectivePrivilege>), UserFacingError>,
    ),
    /// PG 对象权限授予/撤销成功（UI 重新读取该对象权限）。
    UserAdminPgGrantsChanged(TabId),
    /// PG 角色列表加载完成回填（改版工作台）。
    UserAdminPgRolesLoaded(TabId, Vec<PgRole>),
    /// PG 成员关系加载完成回填（含服务端是否支持成员级 INHERIT/SET 选项）。
    UserAdminPgMembershipsLoaded(TabId, Vec<PgRoleMembership>, bool),
    /// PG 权限目标列表加载完成回填。
    UserAdminPgGrantTargetsLoaded(TabId, PgGrantTargetLists),
    /// PG 变更计划脱敏预览生成完成。
    UserAdminPgPlanPreview(TabId, Vec<String>),
    /// PG 变更计划应用完成（含计划本体，供 Finish 回填；Err 为失败/结果不确定）。
    UserAdminPgRolePlanFinished(TabId, PgRoleSavePlan, Result<Vec<String>, UserFacingError>),
    /// PG 变更计划应用成功（UI 刷新角色列表并清空草稿）。
    UserAdminPgRolePlanApplied(TabId),
    DataLoaded(TabId, DataPage),
    /// 惰性补齐无用的事件：元信息已由控制器合并进 `editor.page`，只用于通知 UI 续补下一批可见行。
    RedisKeyMetadataLoaded {
        tab_id: TabId,
    },
    RedisSetMembersLoaded {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
        members: Vec<String>,
        next_cursor: String,
        total: usize,
    },
    RedisHashFieldsLoaded {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
        fields: Vec<(String, String, String)>,
        next_cursor: String,
        total: usize,
    },
    /// 完整值弹框懒加载结果：对应 [`AppCommand::LoadRedisHashFieldFull`]，返回非截断原始值。
    RedisHashFieldFullValueLoaded {
        tab_id: TabId,
        field: String,
        value: String,
    },
    /// String 详情值加载结果：对应 [`AppCommand::LoadRedisStringValue`]。
    RedisStringValueLoaded {
        tab_id: TabId,
        key: String,
        value: String,
        len: u64,
        loaded_all: bool,
    },
    /// String / JSON 值下载结果：对应 [`AppCommand::DownloadRedisStringValue`]，返回原始字节。
    RedisStringValueDownloaded {
        tab_id: TabId,
        key: String,
        bytes: Vec<u8>,
    },
    RedisZSetMembersLoaded {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
        members: Vec<(String, String)>,
        next_cursor: String,
        total: usize,
    },
    RedisListItemsLoaded {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
        items: Vec<(usize, String)>,
        next_cursor: String,
        total: usize,
    },
    RedisStreamGroupsLoaded {
        tab_id: TabId,
        key: String,
        /// (组名, 消费者数, 未确认数, 最后投递 ID, [(消费者名, 未确认数, 空闲毫秒)])
        groups: Vec<(String, u64, u64, String, Vec<(String, u64, u64)>)>,
    },
    RedisStreamEntriesLoaded {
        tab_id: TabId,
        key: String,
        cursor: String,
        /// 一页条目：(流 ID, 由 ID 推导的本地时间, 字段列表)，顺序为从新到旧。
        entries: Vec<(String, String, Vec<(String, String)>)>,
        next_cursor: String,
        total: usize,
    },
    /// Redis 连接级运行概览拉取结果，供底栏连接状态摘要展示。
    RedisOverviewLoaded(ConnectionId, ConnectionOverview),
    /// Redis 服务端版本加载结果；`None` 表示非 Redis 连接 / 解析失败 / 命令失败（能力未知）。
    RedisServerVersionLoaded(ConnectionId, Option<RedisServerVersion>),
    /// 新建 Key 成功：`tab_id` 为目标库键列表页，`object` 为目标库（`RedisDb`）路径，
    /// `key` 为新键名，供 UI 关闭抽屉、刷新列表并打开新键详情。
    RedisKeyCreated {
        tab_id: TabId,
        object: ObjectPath,
        key: String,
    },
    BinaryPreviewLoaded {
        tab_id: TabId,
        row: usize,
        column: usize,
        preview: BinaryPreviewResponse,
    },
    BinaryCellDownloaded {
        tab_id: TabId,
        row: usize,
        column: usize,
        bytes: Vec<u8>,
    },
    QueryCompletionsLoaded(TabId, u64, QueryCompletionResult),
    CompletionIndexWarmed(ConnectionId, Option<String>),
    QueryFinished(TabId, QueryExecutionResult),
    /// Redis Workbench 后台执行完成：一次运行按「一条命令一条记录」产出多条结果。
    /// 失败也用本事件回传，让实际执行的命令文本随结果一起回到主线程写历史。
    RedisWorkbenchCommandsRan {
        tab_id: TabId,
        text: String,
        result: std::result::Result<Vec<CommandWorkbenchExecution>, UserFacingError>,
    },
    CreateTableApplied(TabId),
    TableRenamed {
        object: ObjectPath,
        new_name: String,
    },
    TableCopied {
        object: ObjectPath,
        new_name: String,
    },
    TableDropped(ObjectPath),
    TableTruncated(ObjectPath),
    CreateTableReferenceColumnsLoaded {
        tab_id: TabId,
        foreign_key_id: u64,
        columns: Vec<String>,
    },
    QueryResultPageRefreshed {
        tab_id: TabId,
        result_index: usize,
        page_index: usize,
        page: DataPage,
    },
    TabOpened(TabId),
    TabClosed(TabId),
    /// 已置位某标签的查询取消标志（可能仍在等语句收尾）；UI 据此提示「已请求停止」。
    QueryCancelRequested(TabId),
    TabCloseRequested(TabId),
    TabCloseCancelled(TabId),
    TabActivated(TabId),
    UserAdminUsersLoaded(TabId, Vec<DatabaseUserIdentity>),
    UserAdminGrantsLoaded(TabId, Vec<String>),
    UserAdminMemberGrantsLoaded(TabId, Vec<UserRoleMember>),
    UserAdminSqlApplied(TabId),
    TableInfoLoaded(TabId, TableInfoTab, TableInfoResult),
    TableInfoChanged(TabId, TableInfoTab),
    SettingsSaved,
    /// Redis Workbench 命令历史已变化（新增/删除/清空），供桌面层做持久化 diff 与刷新。
    RedisWorkbenchHistoryChanged,
    Failed(UserFacingError),
}
