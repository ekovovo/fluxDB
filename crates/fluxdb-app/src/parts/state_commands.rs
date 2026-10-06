#[derive(Clone, Debug, PartialEq)]
pub enum AppCommand {
    AiConversation(AiConversationCommand),
    PrepareBackup { request: BackupRequest, method: BackupMethod },
    RunBackup(BackupRequest),
    PrepareRestore(RestoreRequest),
    ProbeRestore(RestoreRequest),
    RunRestore { request: RestoreRequest, plan: RestorePlan },
    LoadConnections,
    ReplaceConnections(Vec<ConnectionConfig>),
    ReplaceSidebarLayout(SidebarLayout),
    CreateConnection(ConnectionDraft),
    UpdateConnection(ConnectionConfig),
    CreateConnectionGroup(String),
    RenameConnectionGroup {
        group_id: ConnectionGroupId,
        name: String,
    },
    ToggleConnectionGroup(ConnectionGroupId),
    DeleteConnectionGroup(ConnectionGroupId),
    MoveConnectionToGroup {
        connection_id: ConnectionId,
        group_id: ConnectionGroupId,
    },
    MoveConnectionToTopLevel(ConnectionId),
    TestConnection(ConnectionConfig),
    /// 从「连接串 / 云资源标识」自动发现一条 Redis 连接（URI 导入 / 云自动发现统一入口）。
    /// `provider` 为云提供商标识（`azure` / `redis-cloud` / 空表示普通连接串导入）。
    DiscoverRedisConnection {
        provider: String,
        connection_string: String,
    },
    OpenConnection(ConnectionId),
    ToggleConnectionExpanded(ConnectionId),
    DisconnectConnection(ConnectionId),
    DisconnectDatabase {
        connection_id: ConnectionId,
        database: String,
    },
    DeleteConnection(ConnectionId),
    CreateDatabase(CreateDatabaseRequest),
    CreateSchema {
        connection_id: ConnectionId,
        /// 目标数据库（PG 的 schema 属于库）；空串表示回退连接维护库。
        database: String,
        schema: String,
    },
    /// 角色管理（T26）：列表 + CRUD（PG 集群级角色）。
    LoadPgRoles(ConnectionId),
    CreatePgRole {
        connection_id: ConnectionId,
        name: String,
        can_login: bool,
        password: Option<String>,
    },
    AlterPgRolePassword {
        connection_id: ConnectionId,
        name: String,
        password: String,
    },
    RenamePgRole {
        connection_id: ConnectionId,
        old_name: String,
        new_name: String,
    },
    DropPgRole {
        connection_id: ConnectionId,
        name: String,
    },
    DeleteDatabase {
        connection_id: ConnectionId,
        database: String,
    },
    LoadObjectChildren(ObjectPath),
    RefreshObject(Option<ObjectPath>),
    /// 侧边栏「刷新连接树」：只重拉**已展开**连接的第一层对象（库 / Schema / RedisDb）。
    /// 不写 `connected` / `expanded`，也不触碰已加载的表 / 视图行（详见 `replace_connection_level0`）。
    RefreshConnectionTree,
    OpenObjectList(Option<ObjectPath>),
    OpenDataEditor(ObjectPath),
    /// 打开某数据库的备份列表 tab（传入数据库 ObjectPath）。
    OpenBackupList(ObjectPath),
    /// 打开某数据库的 ER 关系图 tab（画布原型；Path 定位到库/表所在连接与 schema）。
    OpenErDiagram(ObjectPath),
    /// ER 逻辑关系目录操作；实际文件读写由桌面端后台任务调用 dispatch。
    LoadErRelationships { scope_key: String },
    CreateErRelationship { scope_key: String, relationship: ErRelationship },
    UpdateErRelationship { scope_key: String, relationship: ErRelationship, expected_revision: u64 },
    /// 仅结构身份机械重绑，服务层原子保留原确认；普通编辑仍走 Update 并失效确认。
    RebindErRelationship { scope_key: String, relationship: ErRelationship, expected_revision: u64 },
    ConfirmErRelationship { scope_key: String, id: String, expected_revision: u64, by: String },
    RejectErRelationship { scope_key: String, id: String, expected_revision: u64 },
    DeleteErRelationship { scope_key: String, id: String, expected_revision: u64 },
    CopyTable {
        object: ObjectPath,
        new_name: String,
        copy_data: bool,
    },
    DropTable {
        object: ObjectPath,
        foreign_key_check: ForeignKeyCheckMode,
    },
    TruncateTable {
        object: ObjectPath,
        foreign_key_check: ForeignKeyCheckMode,
        /// PG：是否 RESTART IDENTITY（默认 false = CONTINUE IDENTITY）。
        restart_identity: bool,
    },
    LoadDataPage(TabId),
    SetDataPagePagination {
        tab_id: TabId,
        offset: u64,
        limit: u64,
    },
    LoadDataPageWithSort {
        tab_id: TabId,
        sort: Vec<SortSpec>,
        filters: Vec<FilterSpec>,
    },
    LoadRedisKey {
        tab_id: TabId,
        key: String,
    },
    /// 惰性补齐 Redis Key 列表元信息：给定当前可见的一批键名，批量取回类型/值/大小/TTL。
    /// 对应 [`AppEvent::RedisKeyMetadataLoaded`]；空数组时直接成功返回空结果（不再发请求）。
    LoadRedisKeyMetadata {
        tab_id: TabId,
        keys: Vec<String>,
    },
    /// 拉取 Redis 连接级运行概览（版本 / 内存 / CPU），刷新底栏连接状态摘要。
    LoadRedisOverview(ConnectionId),
    /// 拉取 Redis 服务端版本，作为字段级 TTL 编辑等能力开关。
    /// 结果经 [`AppEvent::RedisServerVersionLoaded`] 回传；解析/命令失败统一回 `None`。
    LoadRedisServerVersion(ConnectionId),
    FinishRedisKeyRefresh {
        tab_id: TabId,
        key: String,
        result: std::result::Result<Row, UserFacingError>,
    },
    ApplyRedisKeyValue {
        tab_id: TabId,
        key: String,
        new_key: String,
        ttl: Option<String>,
        value: Option<String>,
    },
    AddRedisStreamEntry {
        tab_id: TabId,
        key: String,
        id: String,
        fields: Vec<(String, String)>,
        /// 追加后按 `MAXLEN ~ n` 近似裁剪；None 表示不裁剪。
        maxlen: Option<u64>,
    },
    DeleteRedisStreamEntry {
        tab_id: TabId,
        key: String,
        entry_id: String,
    },
    DeleteRedisSetMember {
        tab_id: TabId,
        key: String,
        member: String,
    },
    AddRedisSetMember {
        tab_id: TabId,
        key: String,
        member: String,
    },
    LoadRedisSetMembers {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
    },
    LoadRedisHashFields {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
    },
    SetRedisHashField {
        tab_id: TabId,
        key: String,
        field: String,
        value: String,
        /// 该字段 TTL 的处置方式：保留原值 / 清除 / 设为指定秒数。
        ttl: RedisHashFieldTtl,
    },
    /// 完整值弹框保存：携带完整原始值（可能 >1MB 被截断标记），放行截断 guard。
    /// 行内编辑走 [`AppCommand::SetRedisHashField`]（default 拒截断）。
    SetRedisHashFieldRaw {
        tab_id: TabId,
        key: String,
        field: String,
        value: String,
        ttl: RedisHashFieldTtl,
    },
    /// 仅更新 Hash 字段 TTL，不触碰字段值（纯 TTL 命令，Redis 7.4+ 字段级 TTL）。
    /// 用于大字段（>1MB 被截断）的 TTL 编辑：行内 `SetRedisHashField` 会重写 value，
    /// 对截断片段回写是危险的，必须走纯 TTL 路径。
    SetRedisHashFieldTtl {
        tab_id: TabId,
        key: String,
        field: String,
        ttl: RedisHashFieldTtl,
    },
    /// 完整值弹框懒加载：读取 hash 字段完整原始值（不截断），
    /// 结果经 [`AppEvent::RedisHashFieldFullValueLoaded`] 回传。
    LoadRedisHashFieldFull {
        tab_id: TabId,
        key: String,
        field: String,
    },
    /// String 详情值加载：`full=false` 取前 4999 字节预览（STRLEN+GETRANGE），
    /// `full=true` 取完整值（GET）。结果经 [`AppEvent::RedisStringValueLoaded`] 回传。
    LoadRedisStringValue {
        tab_id: TabId,
        key: String,
        full: bool,
    },
    /// String / JSON 值下载：取原始字节供导出文件。结果经 [`AppEvent::RedisStringValueDownloaded`] 回传。
    DownloadRedisStringValue {
        tab_id: TabId,
        key: String,
    },
    RenameRedisHashField {
        tab_id: TabId,
        key: String,
        old_field: String,
        new_field: String,
        value: String,
    },
    DeleteRedisHashField {
        tab_id: TabId,
        key: String,
        field: String,
    },
    LoadRedisZSetMembers {
        tab_id: TabId,
        key: String,
        query: String,
        cursor: String,
    },
    AddRedisZSetMember {
        tab_id: TabId,
        key: String,
        member: String,
        score: String,
    },
    UpdateRedisZSetScore {
        tab_id: TabId,
        key: String,
        member: String,
        score: String,
    },
    DeleteRedisZSetMember {
        tab_id: TabId,
        key: String,
        member: String,
    },
    LoadRedisListItems {
        tab_id: TabId,
        key: String,
        /// 按「包含」过滤元素；空串表示不过滤。
        query: String,
        cursor: String,
    },
    /// 分页拉取 Stream 条目：cursor 为空取最新一页，非空表示「加载更多」续页。
    LoadRedisStreamEntries {
        tab_id: TabId,
        key: String,
        /// 可选的时间范围（毫秒时间戳），下推给 XREVRANGE 由服务端过滤。
        since_ms: Option<u64>,
        until_ms: Option<u64>,
        cursor: String,
    },
    /// 读取 Stream 的消费者组概览（只读）。
    LoadRedisStreamGroups {
        tab_id: TabId,
        key: String,
    },
    PushRedisListItems {
        tab_id: TabId,
        key: String,
        items: Vec<String>,
        head: bool,
    },
    /// 新建一个 Redis Key（对齐 RedisInsight AddKey）。`request` 携带类型化字段，
    /// 后端据类型执行 SET/HSET/SADD/ZADD/RPUSH/XADD/JSON.SET 并应用可选 TTL。
    /// 成功后回传 [`AppEvent::RedisKeyCreated`]。
    CreateRedisKey {
        tab_id: TabId,
        request: RedisAddKeyRequest,
    },
    SetRedisListItem {
        tab_id: TabId,
        key: String,
        index: usize,
        expected_old: Option<String>,
        value: String,
    },
    DeleteRedisListItem {
        tab_id: TabId,
        key: String,
        index: usize,
        expected_old: Option<String>,
    },
    /// 从 List 头部/尾部按数量弹出元素（LPOP/RPOP，对齐 RedisInsight Remove elements）。
    PopRedisListItems {
        tab_id: TabId,
        key: String,
        /// true 从头弹出（LPOP），false 从尾弹出（RPOP）。
        head: bool,
        /// 弹出数量；1 全版本可用，>1 需 Redis ≥ 6.2。
        count: usize,
    },
    FinishDataPageLoad {
        tab_id: TabId,
        result: std::result::Result<DataPage, UserFacingError>,
    },
    ToggleTableInfo {
        tab_id: TabId,
        tab: TableInfoTab,
    },
    CloseTableInfo(TabId),
    SelectTableInfoTab {
        tab_id: TabId,
        tab: TableInfoTab,
    },
    LoadTableInfo {
        tab_id: TabId,
        tab: TableInfoTab,
    },
    FinishTableInfoLoad {
        tab_id: TabId,
        tab: TableInfoTab,
        result: std::result::Result<TableInfoResult, UserFacingError>,
    },
    SetTableInfoSearch {
        tab_id: TabId,
        search: String,
    },
    SetTableInfoWidth {
        tab_id: TabId,
        width: f32,
    },
    ToggleDdlWrap(TabId),
    HighlightDataColumn {
        tab_id: TabId,
        column: String,
    },
    EditDataCell {
        tab_id: TabId,
        row: usize,
        column: usize,
        value: CellValue,
    },
    LoadBinaryPreview {
        tab_id: TabId,
        row: usize,
        column: usize,
    },
    DownloadBinaryCell {
        tab_id: TabId,
        row: usize,
        column: usize,
    },
    UpdateBinaryCell {
        tab_id: TabId,
        row: usize,
        column: usize,
        payload: BinaryUpdatePayload,
    },
    SetBinaryCellNull {
        tab_id: TabId,
        row: usize,
        column: usize,
    },
    ReplaceBinaryCellFromFile {
        tab_id: TabId,
        row: usize,
        column: usize,
        path: PathBuf,
    },
    OpenCellDetail {
        tab_id: TabId,
        row: usize,
        column: usize,
    },
    CloseCellDetail(TabId),
    StartCellDetailEdit(TabId),
    UpdateCellDetailEditValue {
        tab_id: TabId,
        value: String,
    },
    CancelCellDetailEdit(TabId),
    SaveCellDetailEdit(TabId),
    SetCellDetailNull(TabId),
    RestoreCellDetailOriginalValue(TabId),
    InsertDataRow {
        tab_id: TabId,
        result_index: Option<usize>,
        after_row: Option<usize>,
    },
    CloneDataRow {
        tab_id: TabId,
        result_index: Option<usize>,
        row: usize,
        after_row: Option<usize>,
    },
    DeleteDataRow {
        tab_id: TabId,
        result_index: Option<usize>,
        row: usize,
    },
    DiscardDataChanges(TabId),
    ApplyDataChanges(TabId),
    ApplyDataChangesWithView {
        tab_id: TabId,
        sort: Vec<SortSpec>,
        filters: Vec<FilterSpec>,
    },
    OpenQueryEditor(ConnectionId),
    OpenUserAdmin(ConnectionId),
    OpenSettings,
    OpenQueryEditorInDatabase {
        connection_id: ConnectionId,
        database: Option<String>,
        /// 编辑器初始 schema 作用域（PG）；MySQL/TiDB 恒为 None。
        schema: Option<String>,
    },
    OpenRedisWorkbench {
        connection_id: ConnectionId,
        database: u32,
    },
    OpenRedisCli {
        connection_id: ConnectionId,
        database: u32,
    },
    OpenRedisPubSub {
        connection_id: ConnectionId,
        database: u32,
    },
    OpenCreateTable {
        connection_id: ConnectionId,
        database: Option<String>,
        /// schema 作用域（PG）；None/空表示用连接默认 search_path。
        schema: Option<String>,
    },
    OpenDesignTable(ObjectPath),
    RenameTable {
        object: ObjectPath,
        new_name: String,
    },
    StartCreateTableApply(TabId),
    ApplyCreateTable(TabId),
    FinishCreateTableApply {
        tab_id: TabId,
        result: std::result::Result<(), UserFacingError>,
    },
    SetCreateTableField {
        tab_id: TabId,
        field: CreateTableField,
        value: String,
    },
    SetCreateTableOptionField {
        tab_id: TabId,
        field: CreateTableOptionField,
        value: String,
    },
    ToggleCreateTablePartitionEnabled(TabId),
    SetCreateTablePartitionField {
        tab_id: TabId,
        field: CreateTablePartitionField,
        value: String,
    },
    SelectCreateTableTab {
        tab_id: TabId,
        create_tab: CreateTableTab,
    },
    SelectCreateTableColumn {
        tab_id: TabId,
        column_id: u64,
    },
    AddCreateTableColumn(TabId),
    MoveCreateTableColumnUp {
        tab_id: TabId,
        column_id: u64,
    },
    MoveCreateTableColumnDown {
        tab_id: TabId,
        column_id: u64,
    },
    RemoveCreateTableColumn {
        tab_id: TabId,
        column_id: u64,
    },
    SetCreateTableColumnField {
        tab_id: TabId,
        column_id: u64,
        field: CreateTableColumnField,
        value: String,
    },
    ToggleCreateTableColumnFlag {
        tab_id: TabId,
        column_id: u64,
        flag: CreateTableColumnFlag,
    },
    SelectCreateTableIndex {
        tab_id: TabId,
        index_id: u64,
    },
    AddCreateTableIndex(TabId),
    MoveCreateTableIndexUp {
        tab_id: TabId,
        index_id: u64,
    },
    MoveCreateTableIndexDown {
        tab_id: TabId,
        index_id: u64,
    },
    RemoveCreateTableIndex {
        tab_id: TabId,
        index_id: u64,
    },
    SetCreateTableIndexField {
        tab_id: TabId,
        index_id: u64,
        field: CreateTableIndexField,
        value: String,
    },
    AddCreateTableIndexColumn {
        tab_id: TabId,
        index_id: u64,
    },
    MoveCreateTableIndexColumnUp {
        tab_id: TabId,
        index_id: u64,
        column_index: usize,
    },
    MoveCreateTableIndexColumnDown {
        tab_id: TabId,
        index_id: u64,
        column_index: usize,
    },
    RemoveCreateTableIndexColumn {
        tab_id: TabId,
        index_id: u64,
        column_index: usize,
    },
    SetCreateTableIndexColumnField {
        tab_id: TabId,
        index_id: u64,
        column_index: usize,
        field: CreateTableIndexColumnField,
        value: String,
    },
    SelectCreateTableCheck {
        tab_id: TabId,
        check_id: u64,
    },
    AddCreateTableCheck(TabId),
    MoveCreateTableCheckUp {
        tab_id: TabId,
        check_id: u64,
    },
    MoveCreateTableCheckDown {
        tab_id: TabId,
        check_id: u64,
    },
    RemoveCreateTableCheck {
        tab_id: TabId,
        check_id: u64,
    },
    SetCreateTableCheckField {
        tab_id: TabId,
        check_id: u64,
        field: CreateTableCheckField,
        value: String,
    },
    ToggleCreateTableCheckNotEnforced {
        tab_id: TabId,
        check_id: u64,
    },
    SelectCreateTableTrigger {
        tab_id: TabId,
        trigger_id: u64,
    },
    AddCreateTableTrigger(TabId),
    MoveCreateTableTriggerUp {
        tab_id: TabId,
        trigger_id: u64,
    },
    MoveCreateTableTriggerDown {
        tab_id: TabId,
        trigger_id: u64,
    },
    RemoveCreateTableTrigger {
        tab_id: TabId,
        trigger_id: u64,
    },
    SetCreateTableTriggerField {
        tab_id: TabId,
        trigger_id: u64,
        field: CreateTableTriggerField,
        value: String,
    },
    SetCreateTableTriggerEvent {
        tab_id: TabId,
        trigger_id: u64,
        event: CreateTableTriggerEvent,
    },
    SelectCreateTableForeignKey {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    AddCreateTableForeignKey(TabId),
    MoveCreateTableForeignKeyUp {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    MoveCreateTableForeignKeyDown {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    RemoveCreateTableForeignKey {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    SetCreateTableForeignKeyField {
        tab_id: TabId,
        foreign_key_id: u64,
        field: CreateTableForeignKeyField,
        value: String,
    },
    StartCreateTableReferenceColumnsLoad {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    LoadCreateTableReferenceColumns {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    FinishCreateTableReferenceColumnsLoad {
        tab_id: TabId,
        foreign_key_id: u64,
        result: std::result::Result<Vec<String>, UserFacingError>,
    },
    AddCreateTableForeignKeyColumn {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    MoveCreateTableForeignKeyColumnUp {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    MoveCreateTableForeignKeyColumnDown {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    RemoveCreateTableForeignKeyColumn {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    SetCreateTableForeignKeyColumn {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
        value: String,
    },
    AddCreateTableForeignKeyReferencedColumn {
        tab_id: TabId,
        foreign_key_id: u64,
    },
    MoveCreateTableForeignKeyReferencedColumnUp {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    MoveCreateTableForeignKeyReferencedColumnDown {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    RemoveCreateTableForeignKeyReferencedColumn {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
    },
    SetCreateTableForeignKeyReferencedColumn {
        tab_id: TabId,
        foreign_key_id: u64,
        column_index: usize,
        value: String,
    },

    UpdateQueryText {
        tab_id: TabId,
        text: String,
    },
    RequestQueryCompletions {
        tab_id: TabId,
        request_seq: u64,
        cursor: usize,
        explicit: bool,
    },
    WarmCompletionIndex {
        connection_id: ConnectionId,
        database: Option<String>,
    },
    FormatQuerySql(TabId),
    CompressQuerySql(TabId),
    MarkQuerySaved {
        tab_id: TabId,
        title: String,
        origin: QueryOrigin,
    },
    StartQueryExecution(TabId),
    /// 请求停止当前标签正在执行的查询（工具栏「停止」）。置位该标签的取消标志，
    /// 执行线程在下一次检查点向服务端发送取消（PG CancelToken），不会强杀执行线程。
    CancelQueryExecution(TabId),
    ExecuteQuery(TabId),
    ExecuteQueryText {
        tab_id: TabId,
        text: String,
    },
    ExecuteQueryTextWithOptions {
        tab_id: TabId,
        text: String,
        options: QueryExecutionOptions,
    },
    FinishQueryExecution {
        tab_id: TabId,
        result: std::result::Result<QueryExecutionResult, UserFacingError>,
    },
    UpdateRedisWorkbenchText {
        tab_id: TabId,
        text: String,
    },
    /// Redis Workbench 执行的准备步：只置运行态并按需清空顶部草稿，不做网络 I/O。
    ///
    /// `execution_id` 给定时重跑结果区该条记录（保留顶部草稿），为 None 时执行顶部草稿。
    /// UI 紧接着在后台线程派发 [`AppCommand::RunRedisWorkbench`]，再用
    /// [`AppCommand::FinishRedisWorkbenchExecution`] 把结果落回主线程。
    BeginRedisWorkbenchExecution {
        tab_id: TabId,
        execution_id: Option<u64>,
    },
    /// 执行 Redis 命令：在后台线程的控制器副本上跑，只读标签页作用域，不改动任何标签页状态。
    RunRedisWorkbench {
        tab_id: TabId,
        execution_id: Option<u64>,
    },
    FinishRedisWorkbenchExecution {
        tab_id: TabId,
        /// 本次实际执行的命令文本：失败入历史时要用，不能取执行期间的草稿。
        text: String,
        /// 一次运行按「一条命令一条记录」产出多条执行记录。
        result: std::result::Result<Vec<CommandWorkbenchExecution>, UserFacingError>,
    },
    /// 删除结果区某条执行记录，只删当前一条。
    DeleteRedisWorkbenchRecord {
        tab_id: TabId,
        execution_id: u64,
    },
    ClearRedisWorkbenchResults(TabId),
    /// 切换结果区某条执行记录的折叠/展开（仅 UI 展示态，会话内有效）。
    ToggleRedisWorkbenchRecordCollapse { tab_id: TabId, execution_id: u64 },
    /// 切换结果区某条子命令的 `Text / JSON` 展示（仅 JSON 命令结果显示切换；按执行项维度记忆）。
    ToggleRedisWorkbenchJsonView {
        tab_id: TabId,
        execution_id: u64,
        command_index: usize,
    },
    /// 删除 Redis Workbench「历史」抽屉中指定 scope 的一条历史记录。
    DeleteRedisWorkbenchHistory {
        scope: WorkbenchHistoryScope,
        id: u64,
    },
    /// 清空 Redis Workbench「历史」抽屉中指定 scope 的全部历史。
    ClearRedisWorkbenchHistory { scope: WorkbenchHistoryScope },
    FinishQueryResultPageRefresh {
        tab_id: TabId,
        result_index: usize,
        page_index: usize,
        result: std::result::Result<QueryExecutionResult, UserFacingError>,
    },
    ActivateQueryResultEditor {
        tab_id: TabId,
        page_index: Option<usize>,
    },
    LoadUserAdminUsers(TabId),
    StartUserAdminUsersLoad(TabId),
    FinishUserAdminUsersLoad {
        tab_id: TabId,
        result: std::result::Result<Vec<DatabaseUserIdentity>, UserFacingError>,
    },
    LoadUserAdminGrants {
        tab_id: TabId,
        user: DatabaseUserIdentity,
    },
    StartUserAdminGrantsLoad {
        tab_id: TabId,
        user: DatabaseUserIdentity,
    },
    FinishUserAdminGrantsLoad {
        tab_id: TabId,
        user: DatabaseUserIdentity,
        result: std::result::Result<Vec<String>, UserFacingError>,
    },
    LoadUserAdminMemberGrants {
        tab_id: TabId,
        role: DatabaseUserIdentity,
    },
    StartUserAdminMemberGrantsLoad {
        tab_id: TabId,
        role: DatabaseUserIdentity,
    },
    FinishUserAdminMemberGrantsLoad {
        tab_id: TabId,
        role: DatabaseUserIdentity,
        result: std::result::Result<Vec<UserRoleMember>, UserFacingError>,
    },
    SelectUserAdminUser {
        tab_id: TabId,
        user: DatabaseUserIdentity,
    },
    BeginUserAdminCreateUser(TabId),
    /// 结束「新建」态（取消新建或提交完成），回到选中既有对象。
    EndUserAdminCreateUser(TabId),
    // ===== PG 用户与角色工作台（改版）=====
    /// 切换选中 PG 角色（UI 在有草稿时先弹确认，确认后走 DiscardPgDraftAndSelect）。
    SelectPgRole {
        tab_id: TabId,
        name: String,
    },
    /// 挂起「切换角色」等待用户确认丢弃草稿。
    SetPgRoleSwitchPending {
        tab_id: TabId,
        target: String,
    },
    /// 取消切换（留在当前角色，草稿保留）。
    PgCancelSwitchRole(TabId),
    /// 丢弃当前草稿并切换到目标角色（切换确认框确认后）。
    DiscardPgDraftAndSelect {
        tab_id: TabId,
        name: String,
    },
    /// 开始「新建角色」草稿（角色名留空待填，不自动误建）。
    PgBeginCreateRole(TabId),
    /// 取消/关闭当前草稿（仅清理本地草稿，不写库）。
    PgCancelDraft(TabId),
    SetPgDraftName {
        tab_id: TabId,
        name: String,
    },
    SetPgDraftCanLogin {
        tab_id: TabId,
        can_login: bool,
    },
    /// 布尔属性草稿切换（SUPERUSER/CREATEDB/CREATEROLE/INHERIT/REPLICATION/BYPASSRLS）。
    SetPgDraftAttr {
        tab_id: TabId,
        field: PgDraftAttrField,
        value: bool,
    },
    SetPgDraftConnectionLimit {
        tab_id: TabId,
        value: String,
    },
    /// 密码有效期操作：Keep/Clear(infinity)/At(绝对时间)。
    SetPgDraftValidUntil {
        tab_id: TabId,
        op: PgValidUntilOp,
    },
    /// 密码操作：Keep/Set/Clear。
    SetPgDraftPasswordOp {
        tab_id: TabId,
        op: PgPasswordOp,
    },
    /// 录入新密码明文（仅内存；写入 Set 操作）。
    SetPgDraftPassword {
        tab_id: TabId,
        password: String,
    },
    /// 开始加载 PG 角色列表（权威 pg_roles 数据）。
    StartUserAdminPgRolesLoad(TabId),
    LoadUserAdminPgRoles(TabId),
    FinishUserAdminPgRolesLoad {
        tab_id: TabId,
        result: Result<Vec<PgRole>, UserFacingError>,
    },
    /// 加载全量成员关系（双向展示用；同时探测服务端成员选项版本支持）。
    StartPgMembershipsLoad(TabId),
    LoadPgMemberships(TabId),
    FinishPgMembershipsLoad {
        tab_id: TabId,
        result: Result<Vec<PgRoleMembership>, UserFacingError>,
        member_options_supported: bool,
    },
    /// 成员关系草稿：授予 role → member（含 ADMIN/INHERIT/SET 选项）。
    PgMembershipGrant {
        tab_id: TabId,
        role: String,
        member: String,
        admin: bool,
        inherit: bool,
        set: bool,
    },
    /// 成员关系草稿：撤销 role ← member。
    PgMembershipRevoke {
        tab_id: TabId,
        role: String,
        member: String,
    },
    /// 移除一条成员关系草稿变更。
    PgMembershipRemoveEdit {
        tab_id: TabId,
        index: usize,
    },
    /// 权限页：切换目标数据库（一期同批只允许一个数据库）。
    SetPgGrantDatabase {
        tab_id: TabId,
        database: String,
    },
    /// 开始加载权限页目标列表（数据库/schema/对象/函数）。
    StartPgGrantTargetsLoad(TabId),
    LoadPgGrantTargets {
        tab_id: TabId,
        database: String,
    },
    FinishPgGrantTargetsLoad {
        tab_id: TabId,
        result: Result<PgGrantTargetLists, UserFacingError>,
    },
    /// 对象授权草稿：授予/撤销/仅取消可再授权。
    PgToggleGrant {
        tab_id: TabId,
        privilege: String,
        scope: PgObjectGrantScope,
        /// Grant（可带 grant_option）/ Revoke / RevokeGrantOption。
        op: PgGrantEditOp,
        grant_option: bool,
    },
    /// 移除一条对象授权草稿变更。
    PgRemoveGrantEdit {
        tab_id: TabId,
        index: usize,
    },
    /// 生成脱敏 SQL 预览（渲染与执行共用 connector 规则）。
    StartPgPlanPreview(TabId),
    LoadPgPlanPreview(TabId),
    FinishPgPlanPreview {
        tab_id: TabId,
        result: Result<Vec<String>, UserFacingError>,
    },
    /// 保存：以单事务应用全部草稿变更（App 内部构建计划并执行）。
    StartPgPlanApply(TabId),
    ApplyPgRolePlan(TabId),
    FinishPgRolePlanApply {
        tab_id: TabId,
        plan: PgRoleSavePlan,
        result: Result<Vec<String>, UserFacingError>,
    },
    /// 删除角色确认框。
    PgBeginDeleteRole(TabId),
    PgCancelDeleteRole(TabId),
    /// 角色列表筛选：all / login / nologin / predefined。
    SetPgRoleFilter {
        tab_id: TabId,
        filter: String,
    },
    /// PostgreSQL 对象权限：设置授权目标（种类/schema/对象/签名）。
    SetUserAdminPgGrantTarget {
        tab_id: TabId,
        kind: PgGrantObjectKind,
        schema: String,
        object: String,
        signature: String,
    },
    /// 读取当前授权目标的对象权限（owner/默认/显式条目 + 选中角色生效权限）。
    LoadUserAdminPgObjectGrants(TabId),
    /// 置对象权限面板为「加载中」。
    StartUserAdminPgObjectGrantsLoad(TabId),
    /// 对象权限读取完成回填（Ok 为对象读模型+生效权限；Err 为失败）。
    FinishUserAdminPgObjectGrantsLoad {
        tab_id: TabId,
        target_fingerprint: String,
        result: Result<(PgObjectGrants, Vec<PgEffectivePrivilege>), UserFacingError>,
    },
    SelectUserAdminDetailTab {
        tab_id: TabId,
        detail_tab: UserAdminDetailTab,
    },
    SetUserAdminSearch {
        tab_id: TabId,
        search: String,
    },
    SetUserAdminPrivilegeDatabase {
        tab_id: TabId,
        database: String,
    },
    SetUserAdminPrivilegeTable {
        tab_id: TabId,
        table: String,
    },
    ToggleUserAdminPrivilege {
        tab_id: TabId,
        privilege: String,
    },
    SetUserAdminGrantOption {
        tab_id: TabId,
        enabled: bool,
    },
    AddUserAdminPrivilegeRow {
        tab_id: TabId,
        database: String,
    },
    SetUserAdminPrivilegeRowDatabase {
        tab_id: TabId,
        row_id: u64,
        database: String,
    },
    ToggleUserAdminPrivilegeRowPrivilege {
        tab_id: TabId,
        row_id: u64,
        privilege: String,
    },
    SetUserAdminPrivilegeRowGrantOption {
        tab_id: TabId,
        row_id: u64,
        enabled: bool,
    },
    SetUserAdminCreateUser {
        tab_id: TabId,
        user: String,
    },
    SetUserAdminCreateHost {
        tab_id: TabId,
        host: String,
    },
    SetUserAdminAuthPlugin {
        tab_id: TabId,
        plugin: String,
    },
    SetUserAdminPasswordExpiryPolicy {
        tab_id: TabId,
        policy: String,
    },
    SetUserAdminCreatePassword {
        tab_id: TabId,
        password: String,
    },
    SetUserAdminNewPassword {
        tab_id: TabId,
        password: String,
    },
    SetUserAdminMaxQueriesPerHour {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminMaxUpdatesPerHour {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminMaxConnectionsPerHour {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminMaxUserConnections {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminSslType {
        tab_id: TabId,
        ssl_type: String,
    },
    SetUserAdminSslCipher {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminSslIssuer {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminSslSubject {
        tab_id: TabId,
        value: String,
    },
    SetUserAdminRoleMembershipGranted {
        tab_id: TabId,
        role: DatabaseUserIdentity,
        granted: bool,
    },
    SetUserAdminRoleMembershipDefault {
        tab_id: TabId,
        role: DatabaseUserIdentity,
        default_role: bool,
    },
    SetUserAdminRoleMemberGranted {
        tab_id: TabId,
        member: DatabaseUserIdentity,
        granted: bool,
    },
    PreviewUserAdminSql {
        tab_id: TabId,
        sql: String,
        danger: bool,
    },
    ClearUserAdminPendingSql(TabId),
    /// MySQL：打开「删除用户」确认弹框（针对当前选中的已有用户）。
    BeginUserAdminDeleteUser(TabId),
    /// MySQL：关闭「删除用户」确认弹框（取消）。
    CancelUserAdminDeleteUser(TabId),
    StartUserAdminSqlApply(TabId),
    ApplyUserAdminSql {
        tab_id: TabId,
        sql: String,
    },
    FinishUserAdminSqlApply {
        tab_id: TabId,
        result: std::result::Result<(), UserFacingError>,
    },
    CloseTab(TabId),
    CloseTabs(Vec<TabId>),
    ConfirmCloseDirtyTab(TabId),
    CancelCloseDirtyTab(TabId),
    ActivateTab(TabId),
    /// 取消激活当前标签（active_tab 置空 → 显示首页/启动页）。已打开的标签保留在标签栏，
    /// 仅取消聚焦，非破坏性；与「关闭全部标签」(CloseTabs) 语义不同。
    DeactivateTab,
    PinTab(TabId),
    ReplaceQueryHistory(Vec<QueryHistoryEntry>),
    SaveSettings(Settings),
    /// 刷新设置页「大模型 / 工具与MCP」列表（模型、Agent、插件实例、MCP Client）。
    RefreshAiSettings,
    /// 新建或保存模型档案（含 Key 的 Keep/Set/Clear）。
    SaveAiModel(AiModelSaveInput),
    /// 删除无引用的模型（对象级 CAS，UI 在后台执行）。
    DeleteAiModel { id: String, expected_revision: i64 },
    /// 将后台删除结果合并到主窗口状态，不覆盖其他页面的并发变化。
    CompleteAiModelDelete { id: String, result: std::result::Result<(), String> },
    /// 新建或保存 Agent 配置（提示词直接随 Agent 保存）。
    SaveAiAgent(AiAgentSaveInput),
    /// 切换模型启用状态（对象级 CAS）。
    SetAiModelEnabled {
        id: String,
        enabled: bool,
        expected_revision: i64,
    },
    /// 切换内置插件实例启停（对象级 CAS）。
    SetAiPluginInstanceEnabled {
        id: String,
        enabled: bool,
        expected_revision: i64,
    },
    /// 保存默认主模型 / 默认决策模型选择（运行设置单例 CAS）。
    SaveAiDefaultModels {
        default_model_id: Option<String>,
        default_decision_model_id: Option<String>,
        expected_revision: i64,
    },
    /// 测试模型连接（真实协议请求并记录结果，不改配置 revision）。
    TestAiModel {
        id: String,
    },
    /// 拉取 Provider 的可用模型列表（模型 ID 下拉用）。
    ///
    /// `api_key` 仅为本次请求的内存载荷：不落库、不写日志、不回显，命令返回即丢弃。
    FetchAiModels {
        provider: String,
        endpoint: Option<String>,
        endpoint_is_full_url: bool,
        api_key: String,
    },
    SetTabDirty { tab_id: TabId, dirty: bool },
    ClearCompletionIndexCache,
}

