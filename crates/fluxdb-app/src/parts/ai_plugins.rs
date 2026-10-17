// 内置能力注册表与宿主能力契约（设计 §23、T032）。
//
// 这里登记的是随应用编译的能力，不是可执行的第三方插件加载器。插件实例、
// Agent 绑定和启停状态仍由 storage 管理；运行时只从这个注册表解析已知能力。

/// 设计 §23 中稳定的内置能力边界。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BuiltinCapability {
    ContextProvider,
    ToolProvider,
    DecisionProvider,
    ContextProcessor,
    SecurityAudit,
    ArtifactProducer,
    Progress,
    AgentHook,
    Telemetry,
}

/// 上下文提供能力的最小输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextRequest {
    pub scope: String,
    pub summary_only: bool,
}

/// 上下文提供能力只能返回受宿主预算约束的公开内容。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    pub content: String,
    pub truncated: bool,
}

pub trait ContextProvider: Send + Sync {
    fn provide(
        &self,
        request: ContextRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, ContextSnapshot>;
}

/// 上下文处理能力收到宿主已经筛选的条目，只能返回条目 ID 和处理方式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPlanInput {
    pub item_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPlan {
    pub selected_item_ids: Vec<String>,
}

pub trait ContextProcessor: Send + Sync {
    fn plan(
        &self,
        input: ContextPlanInput,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, ContextPlan>;
}

/// 产物能力错误：`code` 直接成为工具回执的 `error_code`，不被吞成"未知失败"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactError {
    pub code: &'static str,
    pub message: String,
}

impl ArtifactError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 转成宿主错误；保留 code 语义，避免调用方只能看到笼统的内部错误。
    pub fn into_core(self) -> fluxdb_core::Error {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("{}: {}", self.code, self.message),
        )
    }
}

impl From<fluxdb_core::Error> for ArtifactError {
    fn from(error: fluxdb_core::Error) -> Self {
        let code = if error.kind == fluxdb_core::ErrorKind::Cancelled {
            "cancelled"
        } else {
            "artifact_store_error"
        };
        Self::new(code, error.message)
    }
}

/// 产物能力描述：本插件能产出的 renderer/类型与上限（设计 §23）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactProducerDescriptor {
    pub instance_id: &'static str,
    /// 当前目录快照中可用的 renderer ID；实际操作仍须检查对应实现与启停状态。
    pub renderers: Vec<String>,
    pub kinds: Vec<fluxdb_core::ArtifactKind>,
    /// 单个图表最多取多少个来源数据点。
    pub max_points: usize,
}

/// 一个来源在渲染时的公开事实（数据来自结果快照，不是模型提供的数字）。
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactSourceSummary {
    /// 模型声明并由宿主解析过的结果句柄；为 `None` 表示只声明了工具调用来源。
    pub declared_ref: Option<String>,
    /// 宿主结果行 ID（`ai_results.id`）。
    pub result_id: String,
    pub tool_call_id: Option<String>,
    /// 来源结果行的生成时间；行 ID 与该时间共同标识不可变快照版本。
    pub snapshot_at_ms: i64,
    /// 来源结果的总条目数与是否被抽样（spill）。
    pub rows: usize,
    pub truncated: bool,
}

/// 已提交的快照文件事实（相对受控根的路径 + 字节数 + 内容指纹）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactFileFact {
    pub relative_path: String,
    pub byte_count: i64,
    pub content_hash: String,
}

/// 产物操作的可见结果（工具回执与卡片都从这里投影，不另造事实）。
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactOutcome {
    pub artifact_id: String,
    /// `draft` / `staged` / `ready` / `failed`。
    pub status: String,
    pub revision: i64,
    pub kind: String,
    pub title: String,
    pub renderer: String,
    pub metric: Option<fluxdb_core::ArtifactMetric>,
    /// 来源被抽样/数据点被截断时为 true；产物不得把抽样结果说成全集。
    pub truncated: bool,
    /// 真实结果里取出的数据点预览（图表块）。
    pub points: Vec<serde_json::Value>,
    /// 产物当前的完整块列表（与声明时的块形状一致），供模型确认已有哪些块、避免重复追加。
    pub blocks: Vec<fluxdb_core::ArtifactBlock>,
    pub sources: Vec<ArtifactSourceSummary>,
    pub file: Option<ArtifactFileFact>,
}

/// 产物能力边界（设计 §23、§27）：工具只把已通过 schema 校验的声明交给插件，
/// 插件负责字段/来源校验、渲染与提交；来源句柄与受控文件由宿主注入的依赖解析。
pub trait ArtifactProducer: Send + Sync {
    fn descriptor(&self) -> &ArtifactProducerDescriptor;
    /// 新建草稿：校验形状与来源可解析性，写入产物与来源事实。
    fn create(&self, spec: fluxdb_core::ArtifactSpec, cancel: &fluxdb_core::CancelFlag) -> Result<ArtifactOutcome, ArtifactError>;
    /// 追加一个块：块只引用已声明来源的字段，字段必须真实存在。
    fn add_block(
        &self,
        artifact_id: &str,
        block: fluxdb_core::ArtifactBlock,
        cancel: &fluxdb_core::CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError>;
    /// 完成：重新校验来源与块、渲染快照并提交；renderer 失败不改写来源结果。
    fn finalize(&self, artifact_id: &str, cancel: &fluxdb_core::CancelFlag) -> Result<ArtifactOutcome, ArtifactError>;
}

/// 一个随应用注册的内置能力描述。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinDescriptor {
    pub instance_id: &'static str,
    pub package_key: &'static str,
    pub display_name: &'static str,
    /// 设计 §23.2「负责内容」：只描述能力范围，不声称当前实现已覆盖全部条目。
    pub summary: &'static str,
    pub capabilities: &'static [BuiltinCapability],
    pub mandatory: bool,
    /// 无实现或已下线的目录占位行：保留实例与页面行，但不登记任何能力、不可启停。
    ///
    /// 与 `mandatory` 的区别是"没有实现"而不是"内核强制"：置灰行只在设置页占位，
    /// 运行时不解析任何能力；`summary` 可写明该行尚未实现的用途。
    pub retired: bool,
    /// 配置声明（协议见 [`PluginConfigDeclaration`]）；空串表示该能力没有可配置项，
    /// 宿主因此不显示「管理」入口。外置插件把同一份声明放进 manifest 的
    /// `config_declaration`，两条路径由同一个解析器处理。
    pub config_declaration: &'static str,
}

const CONTEXT_UI: &[BuiltinCapability] = &[BuiltinCapability::ContextProvider];
const FILES: &[BuiltinCapability] = &[BuiltinCapability::ContextProvider];
/// 已下线能力的占位条目：不登记任何能力。
const RETIRED: &[BuiltinCapability] = &[];
const JEV: &[BuiltinCapability] = &[BuiltinCapability::DecisionProvider];
const COMPACTION: &[BuiltinCapability] = &[BuiltinCapability::ContextProcessor];
const SECURITY: &[BuiltinCapability] = &[BuiltinCapability::SecurityAudit];
const BI: &[BuiltinCapability] = &[BuiltinCapability::ArtifactProducer];
const PROGRESS: &[BuiltinCapability] = &[BuiltinCapability::Progress];
const TELEMETRY: &[BuiltinCapability] =
    &[BuiltinCapability::AgentHook, BuiltinCapability::Telemetry];

/// 全局实例与 Agent 绑定共同决定内置产物能力是否可用，不控制通用工具的注册。
fn builtin_bi_enabled(conn: &rusqlite::Connection, session_id: &str) -> fluxdb_core::Result<bool> {
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .iter()
        .any(|instance| {
            instance.id == BI_INSTANCE_ID
                && instance.enabled
                && instance.runtime_kind == "builtin"
                && instance.package_key == "fluxdb.output.bi"
        });
    if !enabled {
        return Ok(false);
    }
    let session = fluxdb_storage::ai_platform::sessions::load_session(conn, session_id)?;
    let bindings =
        fluxdb_storage::ai_platform::plugins::list_agent_plugin_bindings(conn, &session.agent_id)?;
    Ok(!bindings
        .iter()
        .any(|binding| binding.plugin_instance_id == BI_INSTANCE_ID && !binding.enabled))
}

/// Jev 的决策模型配置字段键；声明与运行期共用，改名等同协议不兼容变更。
pub const JEV_DECISION_MODEL_FIELD: &str = "decision_model";

/// Jev 的配置声明（协议见 [`PluginConfigDeclaration`]）。
///
/// 用户显式选择一个支持 System One 协议的决策模型，未选择就不参与运行；
/// 不按「唯一合格候选」替用户挑模型，也不把模型 id 写进插件自己的 JSON。
const JEV_CONFIG_DECLARATION: &str = r#"{
  "protocol": "fluxdb.plugin.config.v2",
  "fields": [
    {
      "key": "decision_model",
      "label": "决策模型",
      "help": "未选择时 Jev 不参与运行，基础 Agent 不受影响。",
      "required": true,
      "resource": {
        "type": "model",
        "role": "decision",
        "api_protocol": "system-one"
      }
    }
  ]
}"#;

/// Jev 的配置声明（解析一次后复用）。
///
/// 注册表构造时已校验过同一份文本，这里的 `expect` 只在「改声明忘了跑校验」时才会触发。
pub fn jev_config_declaration() -> &'static PluginConfigDeclaration {
    static DECLARATION: std::sync::OnceLock<PluginConfigDeclaration> = std::sync::OnceLock::new();
    DECLARATION.get_or_init(|| {
        PluginConfigDeclaration::parse(JEV_CONFIG_DECLARATION)
            .expect("Jev 配置声明必须通过注册表自洽检查")
    })
}

/// 上下文压缩的压缩决策模型字段键；声明与运行期共用，改名等同协议不兼容变更。
pub const COMPACTION_DECISION_MODEL_FIELD: &str = "compaction_decision_model";

/// 调用前安全审计的配置声明（协议见 [`PluginConfigDeclaration`]）。
///
/// 规则全部是**可选项**：不填即不拦，插件的存在不改变基础授权链的行为。
/// 填写后作用于每一次工具调用的最终计划，命中即按声明的方式拒绝或转人工。
const SECURITY_AUDIT_CONFIG_DECLARATION: &str = r#"{
  "protocol": "fluxdb.plugin.config.v2",
  "fields": [
    {
      "key": "denied_tables",
      "group": "数据范围", "kind": "text_list",
      "label": "禁止访问的表",
      "help": "读写都拒绝。一行一个：裸名匹配任意 schema，schema.表名 只匹配该 schema。",
      "required": false,
      "placeholder": "例如 audit_logs 或 public.secrets"
    },
    {
      "key": "allowed_tables",
      "group": "数据范围", "kind": "text_list",
      "label": "允许访问的表",
      "help": "填了就变成白名单：不在名单里的表一律拒绝，语句无法解析出表名时也拒绝。留空表示不限制。",
      "required": false,
      "placeholder": "例如 orders\norder_items"
    },
    {
      "key": "protected_tables",
      "group": "数据范围", "kind": "text_list",
      "label": "只读保护的表",
      "help": "允许查询，但任何写入都拒绝。一行一个表名。",
      "required": false,
      "placeholder": "例如 customers"
    },
    {
      "key": "denied_columns",
      "group": "数据范围", "kind": "text_list",
      "label": "禁止访问的列",
      "help": "语句里出现这些列即拒绝。裸列名匹配任意表，表名.列名 只匹配该表。",
      "required": false,
      "placeholder": "例如 api_token 或 app_users.password_hash"
    },
    {
      "key": "forbid_ddl",
      "group": "写入与结构", "kind": "switch",
      "label": "禁止结构变更",
      "help": "打开后拒绝 CREATE / ALTER / DROP / TRUNCATE / GRANT 等结构与管理语句。",
      "required": false,
      "default": "true"
    },
    {
      "key": "write_requires_where",
      "group": "写入与结构", "kind": "switch",
      "label": "写操作必须带条件",
      "help": "打开后，没有 WHERE 的 UPDATE / DELETE 转为待审批，而不是直接拒绝。",
      "required": false,
      "default": "true"
    },
    {
      "key": "denied_redis_commands",
      "group": "Redis", "kind": "text_list",
      "label": "禁止的 Redis 命令",
      "help": "一行一个命令名，不区分大小写。覆盖内核拒绝列表之外的额外命令。",
      "required": false,
      "placeholder": "例如 KEYS\nFLUSHALL"
    },
    {
      "key": "denied_redis_key_prefixes",
      "group": "Redis", "kind": "text_list",
      "label": "禁止的 Redis Key 前缀",
      "help": "命令触碰任一以此开头的 Key 即拒绝；前缀按字面匹配。",
      "required": false,
      "placeholder": "例如 prod:session:"
    }
  ]
}"#;

/// 调用前安全审计的配置声明（解析一次后复用）。
pub fn security_audit_config_declaration() -> &'static PluginConfigDeclaration {
    static DECLARATION: std::sync::OnceLock<PluginConfigDeclaration> = std::sync::OnceLock::new();
    DECLARATION.get_or_init(|| {
        PluginConfigDeclaration::parse(SECURITY_AUDIT_CONFIG_DECLARATION)
            .expect("调用前安全审计配置声明必须通过注册表自洽检查")
    })
}

/// 上下文压缩的配置声明（协议见 [`PluginConfigDeclaration`]）。
///
/// 声明的是**压缩决策**槽位：压缩前由该决策模型在宿主给出的合法切点之间选择保留多少历史；
/// 摘要正文仍由生成模型（Agent 的 `compaction` 绑定）写。未选择时压缩照常按确定性切点运行，
/// 只是没有这一层建议，因此字段非必填。
const COMPACTION_CONFIG_DECLARATION: &str = r#"{
  "protocol": "fluxdb.plugin.config.v2",
  "fields": [
    {
      "key": "compaction_decision_model",
      "label": "压缩决策模型",
      "help": "未选择时按确定性切点压缩，压缩与摘要照常运行；选择后仅影响哪些历史进入摘要。",
      "required": false,
      "resource": {
        "type": "model",
        "role": "compaction_decision",
        "api_protocol": "system-one"
      }
    }
  ]
}"#;

/// 上下文压缩的配置声明（解析一次后复用）。
pub fn compaction_config_declaration() -> &'static PluginConfigDeclaration {
    static DECLARATION: std::sync::OnceLock<PluginConfigDeclaration> = std::sync::OnceLock::new();
    DECLARATION.get_or_init(|| {
        PluginConfigDeclaration::parse(COMPACTION_CONFIG_DECLARATION)
            .expect("上下文压缩配置声明必须通过注册表自洽检查")
    })
}

/// 内置插件实例的唯一能力目录；数据库与 Redis 能力由工具目录管理。
pub const BUILTIN_DESCRIPTORS: &[BuiltinDescriptor] = &[
    BuiltinDescriptor {
        instance_id: "builtin.context-ui",
        package_key: "fluxdb.context.ui",
        display_name: "当前界面与 @ 上下文",
        summary: "当前页面、@ 对象摘要和主动细节读取",
        capabilities: CONTEXT_UI,
        mandatory: true,
        retired: false,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.files",
        package_key: "fluxdb.context.files",
        display_name: "文件与附件",
        summary: "附件、授权目录、有界文件读取",
        capabilities: FILES,
        mandatory: false,
        retired: false,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.router",
        package_key: "fluxdb.model.router",
        display_name: "模型路由",
        summary: "",
        capabilities: RETIRED,
        mandatory: false,
        retired: true,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.jev",
        package_key: "fluxdb.decision.jev",
        display_name: "Jev 决策优化",
        summary: "可选分类、路由与相关性建议",
        capabilities: JEV,
        mandatory: false,
        retired: false,
        config_declaration: JEV_CONFIG_DECLARATION,
    },
    BuiltinDescriptor {
        instance_id: "builtin.compaction",
        package_key: "fluxdb.context.compaction",
        display_name: "上下文压缩",
        summary: "保护条目、卸载、摘要与 checkpoint",
        capabilities: COMPACTION,
        mandatory: false,
        retired: false,
        config_declaration: COMPACTION_CONFIG_DECLARATION,
    },
    BuiltinDescriptor {
        instance_id: "builtin.security-audit",
        package_key: "fluxdb.security.audit",
        display_name: "调用前安全审计",
        summary: "调用前确定性风险/授权检查与审计",
        capabilities: SECURITY,
        mandatory: true,
        retired: false,
        config_declaration: SECURITY_AUDIT_CONFIG_DECLARATION,
    },
    BuiltinDescriptor {
        instance_id: "builtin.budget",
        package_key: "fluxdb.usage.budget",
        display_name: "AI 用量统计",
        summary: "会话 Token 用量统计看板（待实现）",
        capabilities: RETIRED,
        mandatory: false,
        retired: true,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.bi",
        package_key: "fluxdb.output.bi",
        display_name: "BI 与报表输出",
        summary: "提供内置图表、表格与报告生成；关闭不隐藏产物工具，已完成报告仍可查看和导出",
        capabilities: BI,
        mandatory: false,
        retired: false,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.progress",
        package_key: "fluxdb.progress.plan",
        display_name: "计划与执行进度",
        summary: "公开计划、工具进展与检查点说明",
        capabilities: PROGRESS,
        mandatory: false,
        retired: false,
        config_declaration: "",
    },
    BuiltinDescriptor {
        instance_id: "builtin.telemetry",
        package_key: "fluxdb.lifecycle.telemetry",
        display_name: "生命周期诊断",
        summary: "脱敏的阶段耗时/错误/诊断汇总",
        capabilities: TELEMETRY,
        mandatory: false,
        retired: false,
        config_declaration: "",
    },
];

/// 内置能力在目录中的稳定顺序（设计 §23.2 表格顺序）。
///
/// 展示层按此排序，避免落到数据库的按名称排序（中文名会把 BI 排到最前）。
/// 未知 package 返回 `None`，由调用方决定排在何处。
pub fn builtin_catalog_order(package_key: &str) -> Option<usize> {
    BUILTIN_DESCRIPTORS
        .iter()
        .position(|descriptor| descriptor.package_key == package_key)
}

/// 只允许应用内置的注册表；未知 package/runtime 不会被当作能力执行。
#[derive(Clone, Debug)]
pub struct BuiltinRegistry {
    descriptors: &'static [BuiltinDescriptor],
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        Self::new().expect("内置能力目录必须保持自洽")
    }
}

impl BuiltinRegistry {
    pub fn new() -> fluxdb_core::Result<Self> {
        let mut ids = std::collections::BTreeSet::new();
        let mut package_keys = std::collections::BTreeSet::new();
        for descriptor in BUILTIN_DESCRIPTORS {
            // 能力目录与存储层的种子目录各自独立声明，这里逐行核对身份字段，
            // 让两份清单漂移在启动时暴露，而不是让某个内置行静默缺失或改名。
            let seeded = fluxdb_storage::ai_platform::plugins::BUILTIN_PLUGINS
                .iter()
                .find(|plugin| plugin.instance_id == descriptor.instance_id)
                .ok_or_else(|| {
                    fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        format!("内置能力 {} 未在存储层插件注册表中登记", descriptor.instance_id),
                    )
                })?;
            if seeded.package_key != descriptor.package_key
                || seeded.display_name != descriptor.display_name
            {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!(
                        "内置能力 {} 与存储层插件注册表不一致",
                        descriptor.instance_id
                    ),
                ));
            }
            if !ids.insert(descriptor.instance_id)
                || !package_keys.insert(descriptor.package_key)
            {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "内置能力目录包含重复 ID",
                ));
            }
            // 占位行表示能力已下线：必须没有能力与配置入口，避免"行还在、能力却在跑"。
            if descriptor.retired {
                if !descriptor.capabilities.is_empty()
                    || !descriptor.config_declaration.is_empty()
                {
                    return Err(fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "已下线的内置能力不得登记能力或配置项",
                    ));
                }
            } else if descriptor.capabilities.is_empty() {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "内置能力目录包含空能力项",
                ));
            }
            // 内置声明与 manifest 声明同协议：目录自洽检查顺带把声明解析一遍，
            // 写错的声明在启动时就暴露，而不是等用户打开管理弹框。
            if !descriptor.config_declaration.is_empty() {
                PluginConfigDeclaration::parse(descriptor.config_declaration)?;
            }
        }
        Ok(Self {
            descriptors: BUILTIN_DESCRIPTORS,
        })
    }

    pub fn descriptors(&self) -> &'static [BuiltinDescriptor] {
        self.descriptors
    }

    pub fn find_by_package(&self, package_key: &str) -> Option<&'static BuiltinDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.package_key == package_key)
    }

    pub fn find_by_instance(&self, instance_id: &str) -> Option<&'static BuiltinDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.instance_id == instance_id)
    }

    /// 该内置能力的配置声明；`None` 表示没有可配置项（宿主不显示「管理」入口）。
    pub fn config_declaration(&self, instance_id: &str) -> Option<&'static str> {
        self.find_by_instance(instance_id)
            .map(|descriptor| descriptor.config_declaration)
            .filter(|declaration| !declaration.is_empty())
    }

    /// 返回当前已启用且属于本注册表的能力，未知实例被忽略并记录诊断。
    pub fn enabled_capabilities(
        &self,
        instances: &[fluxdb_storage::ai_platform::plugins::PluginInstanceView],
    ) -> std::collections::BTreeSet<BuiltinCapability> {
        instances
            .iter()
            .filter(|instance| instance.enabled && instance.runtime_kind == "builtin")
            .filter_map(|instance| {
                let descriptor = self.find_by_package(&instance.package_key);
                if descriptor.is_none() {
                    tracing::warn!(
                        instance_id = %instance.id,
                        package_key = %instance.package_key,
                        "忽略未知内置插件实例"
                    );
                }
                descriptor
            })
            .flat_map(|descriptor| descriptor.capabilities.iter().copied())
            .collect()
    }

    pub fn can_disable(&self, instance_id: &str) -> bool {
        self.find_by_instance(instance_id)
            .is_some_and(|descriptor| !descriptor.mandatory && !descriptor.retired)
    }

    /// 该实例是否为已下线的目录占位行（没有能力、不可启停）。
    pub fn is_retired(&self, instance_id: &str) -> bool {
        self.find_by_instance(instance_id)
            .is_some_and(|descriptor| descriptor.retired)
    }
}
