// 插件配置声明协议（v1）与宿主侧解析（设计 §24、§26）。
//
// 声明只描述「插件需要什么」：内置能力把声明写在代码注册表里，将来的外置插件把它放进
// manifest 的 `config_declaration`，宿主用同一个解析器读取声明、生成表单、解析候选项。
// 插件因此拿不到内部表、模型凭据和界面控件，宿主始终掌握「能选什么、怎么存、能不能跑」。
//
// v1 只有资源需求字段（当前实现 `model`）：插件声明所需的模型角色槽位，不写具体模型 id，
// 因此不存在「插件私自指向某个模型」的路径。标量参数留待协议主版本升级，
// `ai_plugin_instances.config_json` / `config_schema_version` 已为它预留。

use serde::{Deserialize, Serialize};

/// 宿主认识的声明协议标识；主版本不匹配直接拒绝，不做猜测兼容。
pub const PLUGIN_CONFIG_PROTOCOL: &str = "fluxdb.plugin.config.v1";

/// 声明字段的资源类型。v1 只有模型；未知类型由反序列化直接拒绝，不会静默忽略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    /// 模型档案：候选由宿主按角色槽位、用途、出站许可与凭据解析。
    Model,
}

/// 一个资源需求：插件只声明约束，具体资源由用户在宿主的槽位上选。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequirement {
    #[serde(rename = "type")]
    pub resource_type: ResourceType,
    /// 宿主里承担该资源的角色槽位（见 [`ModelRoleSlot`] 支持的取值）。
    pub role: String,
    /// 追加的协议要求（例如 `system-one`）；缺省表示不限制协议。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_protocol: Option<String>,
}

/// 一个可配置字段。`key` 是宿主回填选择与校验用的稳定机器键。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfigField {
    /// 稳定机器键：改名等同协议不兼容变更，不允许按展示文案定位。
    pub key: String,
    pub label: String,
    /// 弹框里的说明文字；只说契约与后果，不写当前调用场景。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// 必填字段未配置时插件不可用（不阻止用户先启用，只是不参与运行）。
    #[serde(default)]
    pub required: bool,
    pub resource: ResourceRequirement,
}

/// 插件的完整配置声明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfigDeclaration {
    pub protocol: String,
    /// 无字段表示插件不需要配置，宿主因此不显示「管理」入口。
    #[serde(default)]
    pub fields: Vec<PluginConfigField>,
}

impl PluginConfigDeclaration {
    /// 解析并校验声明；内置声明与 manifest 里的 `config_declaration` 走同一条路。
    pub fn parse(json: &str) -> fluxdb_core::Result<Self> {
        let declaration: Self = serde_json::from_str(json).map_err(|error| {
            declaration_error(&format!("不是有效声明：{error}"))
        })?;
        declaration.validate()?;
        Ok(declaration)
    }

    /// 校验协议版本、字段键与资源角色；未知/缺槽位的能力界限在这里拦下。
    fn validate(&self) -> fluxdb_core::Result<()> {
        if self.protocol != PLUGIN_CONFIG_PROTOCOL {
            return Err(declaration_error(&format!(
                "不支持的配置协议 {}（宿主只认识 {PLUGIN_CONFIG_PROTOCOL}）",
                self.protocol
            )));
        }
        let mut keys = std::collections::BTreeSet::new();
        for field in &self.fields {
            if !is_stable_key(&field.key) {
                return Err(declaration_error(&format!(
                    "字段键 `{}` 必须是 snake_case 机器键",
                    field.key
                )));
            }
            if !keys.insert(field.key.as_str()) {
                return Err(declaration_error(&format!("字段键 `{}` 重复", field.key)));
            }
            if field.label.trim().is_empty() {
                return Err(declaration_error(&format!(
                    "字段 `{}` 缺少标签",
                    field.key
                )));
            }
            field.model_role()?;
        }
        Ok(())
    }

    pub fn field(&self, key: &str) -> Option<&PluginConfigField> {
        self.fields.iter().find(|field| field.key == key)
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl PluginConfigField {
    /// 该字段要求的模型角色槽位。
    ///
    /// 插件只能引用宿主已经存在的槽位，不能自己发明模型绑定位置；角色缺失、未知或
    /// 没有对应槽位都按声明错误处理，避免「写了配置但运行时找不到落点」。
    pub fn model_role(&self) -> fluxdb_core::Result<ModelRoleSlot> {
        ModelRoleSlot::parse(&self.resource.role).ok_or_else(|| {
            declaration_error(&format!(
                "宿主没有角色 `{}` 的模型槽位（支持：{}）",
                self.resource.role,
                ModelRoleSlot::NAMES.join("、")
            ))
        })
    }
}

/// 宿主为模型角色提供的槽位：用户在这里选，宿主自己的角色槽位跟着变。
///
/// 槽位逐个对应 `ai_runtime_settings` 里的默认模型列；没有列的槽位不允许被插件声明，
/// 避免出现「配置项存在但无处落地」。槽位名是对外协议里的稳定机器键，因此用 `&str`
/// 解析而不是复用 Agent 绑定角色：`compaction_decision` 是宿主槽位，不是绑定角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRoleSlot {
    MainCandidate,
    Decision,
    /// 压缩决策槽位：压缩前在切点候选之间做选择的决策模型。
    CompactionDecision,
}

impl ModelRoleSlot {
    /// 宿主槽位名（协议 `resource.role` 的取值）。
    pub const NAMES: &'static [&'static str] =
        &["main_candidate", "decision", "compaction_decision"];

    pub fn parse(role: &str) -> Option<Self> {
        match role {
            "main_candidate" => Some(Self::MainCandidate),
            "decision" => Some(Self::Decision),
            "compaction_decision" => Some(Self::CompactionDecision),
            _ => None,
        }
    }

    /// 该槽位在路由候选过滤里对应的角色（用途/出站/凭据口径与 Agent 绑定一致）。
    fn candidate_role(self) -> CandidateRole {
        match self {
            Self::MainCandidate => CandidateRole::Main,
            Self::Decision => CandidateRole::Decision,
            Self::CompactionDecision => CandidateRole::CompactionDecision,
        }
    }
}

/// 宿主解析出的候选项：只有 ID 与公开标签，不含 endpoint、凭据或密文状态载荷。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginResourceChoice {
    pub id: String,
    pub label: String,
    /// 候选项的一句话说明（Provider / 模型标识 / 协议），帮助区分同名模型。
    pub detail: String,
}

/// 一个资源字段的解析结果：当前选择 + 候选项 + 被协议要求筛掉的数量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPluginField {
    /// 已保存的选择；`None` 表示未配置。
    pub selection: Option<String>,
    pub choices: Vec<PluginResourceChoice>,
    /// 用途/出站/凭据都合格、仅协议不符的模型数量：用来解释「为什么没有候选项」。
    pub protocol_rejected: usize,
}

impl ResolvedPluginField {
    /// 选择是否有效：已保存且仍在候选里。
    pub fn selection_is_usable(&self) -> bool {
        match self.selection.as_deref() {
            Some(id) => self.choices.iter().any(|choice| choice.id == id),
            None => false,
        }
    }

    pub fn choice_label(&self, id: &str) -> Option<&str> {
        self.choices
            .iter()
            .find(|choice| choice.id == id)
            .map(|choice| choice.label.as_str())
    }
}

/// 解析一个资源字段：按角色槽位读出当前选择，按声明的协议要求筛出候选项。
pub fn resolve_field(
    conn: &rusqlite::Connection,
    field: &PluginConfigField,
) -> fluxdb_core::Result<ResolvedPluginField> {
    let slot = field.model_role()?;
    let policy = egress_policy(conn)?;
    let required_protocol = field.resource.api_protocol.as_deref();
    // 候选口径与 Agent 绑定共用 candidate_check：启停、用途、出站许可、凭据一个都不放过。
    let models = fluxdb_storage::ai_platform::models::list_models_page(conn, 500, 0)?;
    let mut choices = Vec::new();
    let mut protocol_rejected = 0usize;
    for model in models {
        if candidate_check(&model, slot.candidate_role(), policy).is_err() {
            continue;
        }
        match required_protocol {
            Some(required) if model.api_protocol != required => {
                protocol_rejected += 1;
            }
            _ => choices.push(PluginResourceChoice {
                id: model.id.clone(),
                label: model.display_name.clone(),
                detail: format!("{} · {}", model.provider, model.api_protocol),
            }),
        }
    }
    let selection = load_slot_selection(conn, slot)?;
    Ok(ResolvedPluginField {
        selection,
        choices,
        protocol_rejected,
    })
}

/// 候选过滤口径的说明：由声明推导，**不让插件自述**过滤规则。
///
/// 插件只声明需求；「为什么列表里是这几个模型」是宿主的事实，必须跟着宿主规则变。
pub fn field_hint(field: &PluginConfigField) -> String {
    let usage = match field.model_role() {
        Ok(ModelRoleSlot::Decision) => "决策模型",
        Ok(ModelRoleSlot::CompactionDecision) => "压缩决策模型",
        Ok(ModelRoleSlot::MainCandidate) => "生成模型",
        Err(_) => "模型",
    };
    match field.resource.api_protocol.as_deref() {
        Some(protocol) => format!("只列出已启用、允许出站、凭据可用的 {protocol} {usage}。"),
        None => format!("只列出已启用、允许出站、凭据可用的{usage}。"),
    }
}

/// 已保存的选择不再是候选项时的脱敏原因（缺凭据、停用、用途不符、协议不符或已删除）。
pub fn selection_rejection(
    conn: &rusqlite::Connection,
    field: &PluginConfigField,
    model_id: &str,
) -> String {
    let Ok(slot) = field.model_role() else {
        return "配置声明无效".to_string();
    };
    let Ok(policy) = egress_policy(conn) else {
        return "模型状态无法确认".to_string();
    };
    let Ok(model) = fluxdb_storage::ai_platform::models::load_model(conn, model_id) else {
        return "所选模型已被删除或不可用".to_string();
    };
    if candidate_check(&model, slot.candidate_role(), policy).is_err() {
        return "所选模型当前不可用（已停用、用途不符、禁止出站或缺少凭据）".to_string();
    }
    if let Some(required) = field.resource.api_protocol.as_deref() {
        if model.api_protocol != required {
            return format!("所选模型不支持 {required} 协议");
        }
    }
    "所选模型已不是该插件的候选项".to_string()
}

/// 保存一次配置提交：逐字段按声明校验，然后一次 CAS 写回所有受影响的角色槽位。
///
/// 所有字段一起提交，避免多字段插件出现「一半新配置、一半旧配置」；`resource_id = None`
/// 表示清空该字段（插件随即回到「未配置」，不会退回自动挑选）。
pub fn save_selection(
    conn: &rusqlite::Connection,
    declaration: &PluginConfigDeclaration,
    selections: &[AiPluginFieldSelection],
    expected_revision: i64,
    now_ms: i64,
) -> fluxdb_core::Result<i64> {
    use fluxdb_storage::ai_platform::settings::{self, RuntimeSettings};

    let mut seen = std::collections::BTreeSet::new();
    let mut main_model_id = None;
    let mut decision_model_id = None;
    let mut compaction_decision_model_id = None;
    for selection in selections {
        let field = declaration.field(&selection.key).ok_or_else(|| {
            declaration_error(&format!("字段 `{}` 不在插件声明里", selection.key))
        })?;
        if !seen.insert(field.key.as_str()) {
            return Err(declaration_error(&format!("字段 `{}` 重复提交", field.key)));
        }
        if let Some(id) = selection.resource_id.as_deref() {
            let resolved = resolve_field(conn, field)?;
            if !resolved.choices.iter().any(|choice| choice.id == id) {
                return Err(declaration_error(&format!(
                    "选择的模型不满足插件要求：{}",
                    selection_rejection(conn, field, id)
                )));
            }
        }
        let target = match field.model_role()? {
            ModelRoleSlot::MainCandidate => &mut main_model_id,
            ModelRoleSlot::Decision => &mut decision_model_id,
            ModelRoleSlot::CompactionDecision => &mut compaction_decision_model_id,
        };
        if target.is_some() {
            return Err(declaration_error(&format!(
                "字段 `{}` 与另一个字段占用同一个模型槽位",
                field.key
            )));
        }
        *target = Some(selection.resource_id.clone());
    }
    let current = settings::load_runtime_settings(conn)?;
    let next = RuntimeSettings {
        default_agent_id: current.default_agent_id.clone(),
        default_model_profile_id: main_model_id.unwrap_or(current.default_model_profile_id),
        default_decision_model_profile_id: decision_model_id
            .unwrap_or(current.default_decision_model_profile_id),
        compaction_decision_model_profile_id: compaction_decision_model_id
            .unwrap_or(current.compaction_decision_model_profile_id),
        max_active_turns: current.max_active_turns,
        revision: current.revision,
    };
    let saved = settings::save_runtime_settings(conn, now_ms, &next, expected_revision)?;
    // 槽位变化后立刻重新装配：内置 Jev 是当前唯一按角色槽位建 system 绑定的能力。
    reconcile_jev_builtins(conn, now_ms)?;
    Ok(saved.revision)
}

/// 实例的配置声明。
///
/// 内置能力以代码注册表为准（升级后立即生效，不依赖数据库里的旧副本）；其他来源读
/// manifest 的 `config_declaration`，这是外置插件接入配置的唯一入口。两条路径共用同一个
/// 解析器与校验，内置插件不是什么特权路径。
pub fn instance_declaration(
    conn: &rusqlite::Connection,
    instance_id: &str,
) -> fluxdb_core::Result<Option<PluginConfigDeclaration>> {
    let Some(source) = fluxdb_storage::ai_platform::plugins::load_plugin_instance_source(
        conn,
        instance_id,
    )?
    else {
        return Ok(None);
    };
    if source.runtime_kind == "builtin" {
        return BuiltinRegistry::default()
            .config_declaration(instance_id)
            .map(PluginConfigDeclaration::parse)
            .transpose();
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&source.manifest_json).map_err(|error| {
            declaration_error(&format!("manifest 不是有效 JSON：{error}"))
        })?;
    match manifest.get("config_declaration") {
        None => Ok(None),
        Some(value) => Ok(Some(PluginConfigDeclaration::parse(&value.to_string())?)),
    }
}

fn load_slot_selection(
    conn: &rusqlite::Connection,
    slot: ModelRoleSlot,
) -> fluxdb_core::Result<Option<String>> {
    let runtime = fluxdb_storage::ai_platform::settings::load_runtime_settings(conn)?;
    Ok(match slot {
        ModelRoleSlot::MainCandidate => runtime.default_model_profile_id,
        ModelRoleSlot::Decision => runtime.default_decision_model_profile_id,
        ModelRoleSlot::CompactionDecision => runtime.compaction_decision_model_profile_id,
    })
}

/// 当前全局模型出站闸门；模型候选过滤必须与路由共用同一来源。
pub(crate) fn egress_policy(conn: &rusqlite::Connection) -> fluxdb_core::Result<EgressPolicy> {
    Ok(EgressPolicy {
        enabled: fluxdb_storage::ai_platform::settings::load_privacy_policy(conn)?
            .model_egress_enabled,
    })
}

fn declaration_error(message: &str) -> fluxdb_core::Error {
    fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        format!("插件配置声明无效：{message}"),
    )
}

/// 字段键只允许 snake_case 机器键：它同时是对外协议里的稳定标识。
fn is_stable_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
