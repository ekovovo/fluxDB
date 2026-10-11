// 内置能力注册表与宿主能力契约（设计 §23、T032）。
//
// 这里登记的是随应用编译的能力，不是可执行的第三方插件加载器。插件实例、
// Agent 绑定和启停状态仍由 storage 管理；运行时只从这个注册表解析已知能力。

/// 设计 §23 中稳定的内置能力边界。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BuiltinCapability {
    ContextProvider,
    ToolProvider,
    ModelRouter,
    DecisionProvider,
    ContextProcessor,
    SecurityAudit,
    UsageBudget,
    KnowledgeSkills,
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

/// 路由能力只能引用宿主提供的候选 ID，不能提供 endpoint 或凭据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRouteInput {
    pub task_summary: String,
    pub candidate_model_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRouteAdvice {
    pub model_id: Option<String>,
    pub reason: Option<String>,
}

pub trait ModelRouter: Send + Sync {
    fn advise(
        &self,
        input: ModelRouteInput,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, ModelRouteAdvice>;
}

/// 产物能力只接收宿主分配的来源引用；文件创建和提交由宿主完成。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactBuildRequest {
    pub artifact_type: String,
    pub source_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactBuildResult {
    pub artifact_id: String,
}

pub trait ArtifactProducer: Send + Sync {
    fn build(
        &self,
        request: ArtifactBuildRequest,
        cancel: fluxdb_core::CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, ArtifactBuildResult>;
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
}

const CONTEXT_UI: &[BuiltinCapability] = &[BuiltinCapability::ContextProvider];
const DATABASE: &[BuiltinCapability] = &[BuiltinCapability::ToolProvider];
const FILES: &[BuiltinCapability] = &[BuiltinCapability::ContextProvider];
const ROUTER: &[BuiltinCapability] = &[BuiltinCapability::ModelRouter];
const JEV: &[BuiltinCapability] = &[BuiltinCapability::DecisionProvider];
const COMPACTION: &[BuiltinCapability] = &[BuiltinCapability::ContextProcessor];
const SECURITY: &[BuiltinCapability] = &[BuiltinCapability::SecurityAudit];
const BUDGET: &[BuiltinCapability] = &[BuiltinCapability::UsageBudget];
const KNOWLEDGE: &[BuiltinCapability] = &[BuiltinCapability::KnowledgeSkills];
const BI: &[BuiltinCapability] = &[BuiltinCapability::ArtifactProducer];
const PROGRESS: &[BuiltinCapability] = &[BuiltinCapability::Progress];
const TELEMETRY: &[BuiltinCapability] =
    &[BuiltinCapability::AgentHook, BuiltinCapability::Telemetry];

/// 12 个内置插件实例的唯一能力目录。
pub const BUILTIN_DESCRIPTORS: &[BuiltinDescriptor] = &[
    BuiltinDescriptor {
        instance_id: "builtin.context-ui",
        package_key: "fluxdb.context.ui",
        display_name: "当前界面与 @ 上下文",
        summary: "当前页面、@ 对象摘要和主动细节读取",
        capabilities: CONTEXT_UI,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.database",
        package_key: "fluxdb.tools.database",
        display_name: "数据库与 Redis 工具",
        summary: "关系型探索、SQL、Redis 标准工具",
        capabilities: DATABASE,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.files",
        package_key: "fluxdb.context.files",
        display_name: "文件与附件",
        summary: "附件、授权目录、有界文件读取",
        capabilities: FILES,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.router",
        package_key: "fluxdb.model.router",
        display_name: "模型路由",
        summary: "默认模型、经济与高能力候选路由",
        capabilities: ROUTER,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.jev",
        package_key: "fluxdb.decision.jev",
        display_name: "Jev 决策优化",
        summary: "可选分类、路由与相关性建议",
        capabilities: JEV,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.compaction",
        package_key: "fluxdb.context.compaction",
        display_name: "上下文压缩",
        summary: "保护条目、卸载、摘要与 checkpoint",
        capabilities: COMPACTION,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.security-audit",
        package_key: "fluxdb.security.audit",
        display_name: "调用前安全审计",
        summary: "调用前确定性风险/授权检查与审计",
        capabilities: SECURITY,
        mandatory: true,
    },
    BuiltinDescriptor {
        instance_id: "builtin.budget",
        package_key: "fluxdb.usage.budget",
        display_name: "Token 用量与执行保护",
        summary: "Token 用量与执行保护事件",
        capabilities: BUDGET,
        mandatory: true,
    },
    BuiltinDescriptor {
        instance_id: "builtin.knowledge",
        package_key: "fluxdb.knowledge.skills",
        display_name: "业务知识与 Skills",
        summary: "已确认业务知识与按需专业说明",
        capabilities: KNOWLEDGE,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.bi",
        package_key: "fluxdb.output.bi",
        display_name: "BI 与报表输出",
        summary: "图表、表格、报告的创建与渲染说明",
        capabilities: BI,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.progress",
        package_key: "fluxdb.progress.plan",
        display_name: "计划与执行进度",
        summary: "公开计划、工具进展与检查点说明",
        capabilities: PROGRESS,
        mandatory: false,
    },
    BuiltinDescriptor {
        instance_id: "builtin.telemetry",
        package_key: "fluxdb.lifecycle.telemetry",
        display_name: "生命周期诊断",
        summary: "脱敏的阶段耗时/错误/诊断汇总",
        capabilities: TELEMETRY,
        mandatory: false,
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
            if !ids.insert(descriptor.instance_id)
                || !package_keys.insert(descriptor.package_key)
            {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "内置能力目录包含重复 ID",
                ));
            }
            if descriptor.capabilities.is_empty() {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "内置能力目录包含空能力项",
                ));
            }
        }
        if BUILTIN_DESCRIPTORS.len() != 12 {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "内置能力目录数量与设计契约不一致",
            ));
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
            .is_some_and(|descriptor| !descriptor.mandatory)
    }
}
