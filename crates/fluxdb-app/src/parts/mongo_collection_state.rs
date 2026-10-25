// MongoDB 集合工作台的标签页状态。
//
// 与 `DataEditorState`（行列模型）分开：MongoDB 的读取单位是整篇文档，且要保留 BSON 类型，
// 因此这里以 Extended JSON 文本承载查询参数与文档值，分节展示（文档 / 聚合 / 索引 / 集合信息）。

/// 集合工作台的分节页签。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MongoSection {
    #[default]
    Documents,
    Aggregation,
    Indexes,
    Info,
}

/// 文档结果的展示方式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MongoDisplayMode {
    /// 文档卡片（字段 + BSON 类型徽标，可折叠嵌套）。
    #[default]
    Documents,
    /// 表格（`_id` + 文档 JSON 两列）。
    Table,
    /// 整页 JSON（复用 JSON 组件）。
    Json,
}

/// 查询面板的可编辑文本。`filter`/`projection`/`sort` 均为 Extended JSON 对象文本。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MongoQueryDraft {
    pub filter: String,
    pub projection: String,
    pub sort: String,
    /// 本页条数（文本形式，提交时解析并夹到连接器上限）。
    pub limit: String,
}

impl MongoQueryDraft {
    /// 默认草稿：空 Filter 等价 `{}`，每页条数取设置里的数据表页大小。
    pub fn new(page_size: u64) -> Self {
        Self {
            filter: "{}".to_string(),
            projection: String::new(),
            sort: String::new(),
            limit: page_size.to_string(),
        }
    }

    /// 每个编辑项都能在分析器里逐字段定位错误，因此空文本一律按「未填写」处理。
    pub fn to_request(&self, collection: &str, offset: u64) -> fluxdb_core::MongoFindRequest {
        fluxdb_core::MongoFindRequest {
            collection: collection.to_string(),
            filter: self.filter.clone(),
            projection: self.projection.clone(),
            sort: self.sort.clone(),
            skip: offset,
            limit: self
                .limit
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|limit| *limit > 0)
                .unwrap_or(fluxdb_core::Pagination::DEFAULT_LIMIT),
        }
    }
}

/// 集合工作台标签页。
#[derive(Clone, Debug, PartialEq)]
pub struct MongoCollectionState {
    pub object: ObjectPath,
    /// 只读对象（视图）：禁止写入类操作，UI 据此隐藏/禁用入口。
    pub read_only: bool,
    pub section: MongoSection,
    pub display: MongoDisplayMode,
    /// 已生效的查询：刷新与输入框尚未创建时据此还原面板。
    pub applied_query: MongoQueryDraft,
    pub page: Option<fluxdb_core::MongoDocumentsPage>,
    pub pagination: Pagination,
    /// 每次查询/翻页递增；后台结果只回写与当前令牌一致的加载。
    pub load_generation: u64,
    pub loading: bool,
    pub error: Option<UserFacingError>,
    /// 右侧详情抽屉选中的文档下标（`None` 表示关闭）。
    pub selected_document: Option<usize>,
    pub info: LoadState<fluxdb_core::MongoCollectionInfo>,
    pub indexes: LoadState<Vec<fluxdb_core::MongoIndexInfo>>,
    /// 聚合管道文本（Extended JSON 数组）；空表示未编辑。
    pub aggregation_pipeline: String,
    /// 聚合结果（与文档页同样的分页结构，便于复用文档/表格/JSON 三种展示）。
    pub aggregation: LoadState<fluxdb_core::MongoDocumentsPage>,
    /// 聚合请求令牌：只回写与当前令牌一致的结果。
    pub aggregation_generation: u64,
}

impl MongoCollectionState {
    pub fn new(object: ObjectPath, read_only: bool, page_size: u64) -> Self {
        Self {
            object,
            read_only,
            section: MongoSection::Documents,
            display: MongoDisplayMode::Documents,
            applied_query: MongoQueryDraft::new(page_size),
            page: None,
            pagination: Pagination::new(0, page_size),
            load_generation: 0,
            loading: false,
            error: None,
            selected_document: None,
            info: LoadState::NotLoaded,
            indexes: LoadState::NotLoaded,
            aggregation_pipeline: String::new(),
            aggregation: LoadState::NotLoaded,
            aggregation_generation: 0,
        }
    }

    /// 默认聚合管道：统计集合总量。用户可改写后再执行，不做隐式字段猜测。
    pub fn default_aggregation_pipeline() -> String {
        "[\n  { \"$group\": { \"_id\": null, \"count\": { \"$sum\": 1 } } }\n]".to_string()
    }

    /// 开始一次聚合，返回本次令牌（与文档加载互不干扰）。
    pub fn begin_aggregation(&mut self) -> u64 {
        self.aggregation_generation = self.aggregation_generation.wrapping_add(1);
        self.aggregation = LoadState::Loading;
        self.aggregation_generation
    }

    pub fn accepts_aggregation(&self, generation: u64) -> bool {
        self.aggregation_generation == generation
    }

    /// 开始一次新的加载，返回本次令牌（旧结果据此丢弃）。
    pub fn begin_load(&mut self) -> u64 {
        self.load_generation = self.load_generation.wrapping_add(1);
        self.loading = true;
        self.error = None;
        self.load_generation
    }

    pub fn accepts_load(&self, generation: u64) -> bool {
        self.load_generation == generation
    }

    /// 当前页条数上限（与 `pagination.limit` 同源，避免查询草稿与分页器打架）。
    pub fn page_limit(&self) -> u64 {
        self.pagination.limit
    }
}
