// 长尾工具发现与动态加载。
//
// toolbox 只负责发现和选择，不直接执行被选中的领域工具。未加载的长尾工具不会
// 出现在 Provider tools 列表中；加载只影响本轮可见性，不授予执行权限。

const TOOLBOX_SEARCH_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "query": { "type": "string", "maxLength": 256 },
    "domain": { "type": "string", "maxLength": 64 },
    "offset": { "type": "integer", "minimum": 0, "maximum": 10000 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 20 }
  }
}"#;

const TOOLBOX_LOAD_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["ids"],
  "properties": {
    "ids": {
      "type": "array",
      "minItems": 1,
      "maxItems": 5,
      "items": { "type": "string", "minLength": 1, "maxLength": 128 }
    }
  }
}"#;

const MAX_LOADED_TOOLS_PER_CALL: usize = 5;

/// 动态配置动作都属于同一领域能力；设置页用这个稳定 ID 控制整组动作。
pub const CONFIG_PROPOSE_CAPABILITY: &str = "config.propose";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolboxEntry {
    id: String,
    version: u32,
    domain: String,
    title: String,
    description: String,
    capability: String,
    required_fields: Vec<String>,
    risk: fluxdb_core::ToolRisk,
    effect: EffectClass,
}

/// 当前 Turn 的 toolbox 目录与已加载集合。
pub struct ToolboxState {
    entries: Vec<ToolboxEntry>,
    managed_ids: BTreeSet<String>,
    loaded: Mutex<BTreeSet<String>>,
}

impl ToolboxState {
    fn new(
        specs: &[ToolSpec],
        globally_disabled: &BTreeSet<String>,
        agent_allowed_tools: &[String],
        read_only: bool,
    ) -> Self {
        let managed_ids = specs
            .iter()
            .filter(|spec| spec.toolbox.is_some())
            .map(|spec| spec.id.clone())
            .collect();
        let mut entries = specs
            .iter()
            .filter_map(|spec| {
                let metadata = spec.toolbox.as_ref()?;
                if read_only && spec.risk != fluxdb_core::ToolRisk::Read {
                    return None;
                }
                if globally_disabled.contains(&spec.id) {
                    return None;
                }
                if globally_disabled.contains(&metadata.capability) {
                    return None;
                }
                if !agent_allowed_tools.is_empty()
                    && !agent_allowed_tools
                        .iter()
                        .any(|id| id == &metadata.capability)
                {
                    return None;
                }
                let required_fields = spec
                    .input_schema
                    .get("required")
                    .and_then(|required| required.as_array())
                    .map(|required| {
                        required
                            .iter()
                            .filter_map(|field| field.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ToolboxEntry {
                    id: spec.id.clone(),
                    version: spec.version,
                    domain: metadata.domain.clone(),
                    title: metadata.title.clone(),
                    description: spec.description.clone(),
                    capability: metadata.capability.clone(),
                    required_fields,
                    risk: spec.risk,
                    effect: spec.effect,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            (&left.domain, &left.title, &left.id).cmp(&(&right.domain, &right.title, &right.id))
        });
        Self {
            entries,
            managed_ids,
            loaded: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn is_managed(&self, id: &str) -> bool {
        self.managed_ids.contains(id)
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.loaded
            .lock()
            .map(|loaded| loaded.contains(id))
            .unwrap_or(false)
    }

    /// 全局开关在目录装配后才读取，因此需要在工具箱冻结后再次移除已停用的动作。
    fn remove_disabled(&self, disabled: &BTreeSet<String>) {
        if let Ok(mut loaded) = self.loaded.lock() {
            loaded.retain(|id| !disabled.contains(id));
        }
    }

    fn search(
        &self,
        query: &str,
        domain: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<serde_json::Value>, usize, Option<usize>) {
        let needle = query.trim().to_ascii_lowercase();
        let domain = domain.map(str::trim).filter(|value| !value.is_empty());
        let matches = self
            .entries
            .iter()
            .filter(|entry| {
                domain.is_none_or(|domain| entry.domain.eq_ignore_ascii_case(domain))
                    && (needle.is_empty()
                        || entry.id.to_ascii_lowercase().contains(&needle)
                        || entry.title.to_ascii_lowercase().contains(&needle)
                        || entry.description.to_ascii_lowercase().contains(&needle))
            })
            .collect::<Vec<_>>();
        let total = matches.len();
        let items = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "version": entry.version,
                    "domain": entry.domain,
                    "title": entry.title,
                    "description": entry.description,
                    "required_fields": entry.required_fields,
                    "risk": format!("{:?}", entry.risk).to_ascii_lowercase(),
                    "effect": format!("{:?}", entry.effect).to_ascii_lowercase(),
                    "loaded": self.is_loaded(&entry.id),
                })
            })
            .collect();
        let next_offset = offset
            .saturating_add(limit)
            .checked_sub(0)
            .filter(|next| *next < total);
        (items, total, next_offset)
    }

    fn load(&self, ids: &[String]) -> Result<Vec<String>, String> {
        if ids.is_empty() {
            return Err("至少选择一个工具".into());
        }
        if ids.len() > MAX_LOADED_TOOLS_PER_CALL {
            return Err(format!(
                "单次最多加载 {MAX_LOADED_TOOLS_PER_CALL} 个工具"
            ));
        }
        let mut unique = BTreeSet::new();
        for id in ids {
            if !self.entries.iter().any(|entry| &entry.id == id) {
                return Err(format!("工具 {id} 不存在或当前无权使用"));
            }
            unique.insert(id.clone());
        }
        let loaded = unique.into_iter().collect::<Vec<_>>();
        let mut state = self
            .loaded
            .lock()
            .map_err(|_| "toolbox 状态不可用".to_string())?;
        state.extend(loaded.iter().cloned());
        Ok(loaded)
    }

    fn domain_counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for entry in &self.entries {
            *counts.entry(entry.domain.clone()).or_insert(0) += 1;
        }
        counts
    }
}

struct ToolboxSearchTool {
    descriptor: fluxdb_core::ToolDescriptor,
    state: Arc<ToolboxState>,
}

impl ToolboxSearchTool {
    fn new(state: Arc<ToolboxState>) -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "toolbox.search".into(),
                version: 1,
                description: "搜索当前可用的长尾操作。只返回摘要、所需字段名和风险，不返回参数 Schema；选择后调用 toolbox.load 加载 1-5 个操作。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: TOOLBOX_SEARCH_INPUT_SCHEMA.into(),
            },
            state,
        }
    }
}

impl fluxdb_core::AiTool for ToolboxSearchTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let started_at_ms = now_ms();
            let result = serde_json::from_str::<serde_json::Value>(&arguments_json)
                .map_err(|error| format!("参数不是合法 JSON: {error}"))
                .and_then(|value| {
                    let query = value
                        .get("query")
                        .and_then(|query| query.as_str())
                        .unwrap_or("");
                    let domain = value.get("domain").and_then(|domain| domain.as_str());
                    let offset =
                        usize::try_from(value.get("offset").and_then(|v| v.as_u64()).unwrap_or(0))
                            .map_err(|_| "offset 超出范围".to_string())?;
                    let limit = value
                        .get("limit")
                        .and_then(|limit| limit.as_u64())
                        .unwrap_or(10)
                        .clamp(1, 20) as usize;
                    Ok(self.state.search(query, domain, offset, limit))
                });
            let (status, result_json, error_code) = match result {
                Ok((items, total, next_offset)) => (
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    serde_json::json!({
                        "items": items,
                        "total": total,
                        "next_offset": next_offset,
                        "domains": self.state.domain_counts(),
                        "note": "发现不等于授权；选择后调用 toolbox.load 才会在下一轮出现完整 Schema。"
                    }),
                    None,
                ),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error": error, "retryable": false}),
                    Some("toolbox_search_failed".to_string()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: result_json.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms,
                finished_at_ms: now_ms(),
            })
        })
    }
}

struct ToolboxLoadTool {
    descriptor: fluxdb_core::ToolDescriptor,
    state: Arc<ToolboxState>,
}

impl ToolboxLoadTool {
    fn new(state: Arc<ToolboxState>) -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "toolbox.load".into(),
                version: 1,
                description: "从 toolbox.search 结果中选择 1-5 个工具。下一轮模型请求会加入这些工具的完整参数 Schema；加载不授予权限。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: TOOLBOX_LOAD_INPUT_SCHEMA.into(),
            },
            state,
        }
    }
}

impl fluxdb_core::AiTool for ToolboxLoadTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let started_at_ms = now_ms();
            let result = serde_json::from_str::<serde_json::Value>(&arguments_json)
                .map_err(|error| format!("参数不是合法 JSON: {error}"))
                .and_then(|value| {
                    let ids = value
                        .get("ids")
                        .and_then(|ids| ids.as_array())
                        .ok_or_else(|| "缺少 ids".to_string())?
                        .iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect::<Vec<_>>();
                    self.state.load(&ids)
                });
            let (status, result_json, error_code) = match result {
                Ok(loaded) => (
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    serde_json::json!({
                        "loaded": loaded,
                        "note": "这些工具将在下一轮模型请求中可见；仍需通过 Schema、权限和审批校验。"
                    }),
                    None,
                ),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error": error, "retryable": false}),
                    Some("toolbox_load_failed".to_string()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: result_json.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms,
                finished_at_ms: now_ms(),
            })
        })
    }
}

/// 标记内置长尾操作。配置动作在注册时自行带元数据；这里补齐通用操作。
pub fn mark_builtin_long_tail_tools(catalog: &mut ToolCatalog) -> fluxdb_core::Result<()> {
    for (id, domain, title, capability) in [
        (
            "plan.update",
            "planning",
            "Update execution plan",
            "plan.update",
        ),
        (
            "er.propose_relationships",
            "relationship",
            "Propose ER relationships",
            "er.propose_relationships",
        ),
        (
            "artifact.create",
            "artifact",
            "Create report draft",
            "artifact.create",
        ),
        (
            "artifact.add_block",
            "artifact",
            "Add report block",
            "artifact.add_block",
        ),
        (
            "artifact.finalize",
            "artifact",
            "Finalize report",
            "artifact.finalize",
        ),
        (
            "diagnostics.read_turns",
            "diagnostics",
            "Read recent turn diagnostics",
            "diagnostics.read_turns",
        ),
    ] {
        if catalog.describe(id).is_some() {
            catalog.mark_toolbox_tool(
                id,
                ToolboxMetadata {
                    domain: domain.into(),
                    title: title.into(),
                    capability: capability.into(),
                },
            )?;
        }
    }
    Ok(())
}

/// 装配 toolbox 自身并冻结本轮长尾目录。
pub fn install_ai_toolbox(
    catalog: &mut ToolCatalog,
    globally_disabled: &BTreeSet<String>,
    agent_allowed_tools: &[String],
    read_only: bool,
) -> fluxdb_core::Result<()> {
    let state = Arc::new(ToolboxState::new(
        catalog.specs(),
        globally_disabled,
        agent_allowed_tools,
        read_only,
    ));
    if !globally_disabled.contains("toolbox.search")
        && agent_allows_capability(agent_allowed_tools, "toolbox.search")
    {
        catalog.add_tool_with_shape_and_effect(
            Box::new(ToolboxSearchTool::new(Arc::clone(&state))),
            ToolOutputShape::Opaque,
            Some(EffectClass::PureRead),
        )?;
    }
    if !globally_disabled.contains("toolbox.load")
        && agent_allows_capability(agent_allowed_tools, "toolbox.load")
    {
        catalog.add_tool_with_shape_and_effect(
            Box::new(ToolboxLoadTool::new(Arc::clone(&state))),
            ToolOutputShape::Opaque,
            Some(EffectClass::PureRead),
        )?;
    }
    catalog.set_toolbox_state(state);
    Ok(())
}

fn agent_allows_capability(agent_allowed_tools: &[String], id: &str) -> bool {
    agent_allowed_tools.is_empty() || agent_allowed_tools.iter().any(|allowed| allowed == id)
}

#[cfg(test)]
mod ai_toolbox_tests {
    use super::*;

    fn spec(id: &str, title: &str) -> ToolSpec {
        let mut spec = ToolSpec::from_descriptor(&fluxdb_core::ToolDescriptor {
            id: id.into(),
            version: 1,
            description: title.into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: serde_json::json!({
                "type": "object",
                "required": ["value"],
                "properties": {"value": {"type": "string"}}
            })
            .to_string(),
        })
        .expect("spec");
        spec.toolbox = Some(ToolboxMetadata {
            domain: "config".into(),
            title: title.into(),
            capability: CONFIG_PROPOSE_CAPABILITY.into(),
        });
        spec
    }

    #[test]
    fn search_returns_summaries_without_schema_and_load_controls_visibility() {
        let state = ToolboxState::new(
            &[spec("config.model.create", "Create model")],
            &BTreeSet::new(),
            &[],
            false,
        );
        let (items, total, next) = state.search("model", None, 0, 10);
        assert_eq!(total, 1);
        assert_eq!(next, None);
        assert_eq!(state.domain_counts()["config"], 1);
        assert_eq!(items[0]["required_fields"], serde_json::json!(["value"]));
        assert!(items[0].get("input_schema").is_none());
        assert!(!state.is_loaded("config.model.create"));
        state
            .load(&["config.model.create".into()])
            .expect("load tool");
        assert!(state.is_loaded("config.model.create"));
    }

    #[test]
    fn load_rejects_unauthorized_unknown_and_too_many_ids() {
        let state = ToolboxState::new(
            &[spec("config.model.create", "Create model")],
            &BTreeSet::new(),
            &["other.capability".into()],
            false,
        );
        assert!(state
            .load(&["config.model.create".into()])
            .is_err());
        let all = ToolboxState::new(
            &[spec("config.model.create", "Create model")],
            &BTreeSet::new(),
            &[],
            false,
        );
        assert!(all.load(&["unknown.tool".into()]).is_err());
        assert!(all
            .load(&[
                "config.model.create".into(),
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into()
            ])
            .is_err());
    }

    #[test]
    fn loaded_schema_changes_provider_visibility_only_after_load() {
        let mut catalog = ToolCatalog::new(vec![
            spec("config.model.create", "Create model"),
            spec("config.agent.update", "Update agent"),
        ]);
        install_ai_toolbox(
            &mut catalog,
            &BTreeSet::new(),
            &["config.propose".into()],
            false,
        )
        .expect("toolbox");
        assert!(!catalog.is_model_visible("config.model.create"));
        assert!(catalog
            .describe("config.model.create")
            .expect("spec")
            .input_schema
            .get("properties")
            .is_some());

        let state = catalog.toolbox.as_ref().expect("state").clone();
        state
            .load(&["config.model.create".into()])
            .expect("load");
        assert!(catalog.is_model_visible("config.model.create"));
        assert!(!catalog.is_model_visible("config.agent.update"));
    }

    #[test]
    fn toolbox_tools_respect_global_and_agent_restrictions() {
        let mut catalog = ToolCatalog::new(vec![spec(
            "config.model.create",
            "Create model",
        )]);
        install_ai_toolbox(
            &mut catalog,
            &BTreeSet::from(["toolbox.search".to_string()]),
            &["toolbox.load".to_string(), "config.propose".to_string()],
            false,
        )
        .expect("toolbox");
        assert!(catalog.describe("toolbox.search").is_none());
        assert!(catalog.describe("toolbox.load").is_some());

        let mut catalog =
            ToolCatalog::new(vec![spec("config.model.create", "Create model")]);
        install_ai_toolbox(
            &mut catalog,
            &BTreeSet::new(),
            &["config.propose".to_string()],
            false,
        )
        .expect("toolbox");
        assert!(catalog.describe("toolbox.search").is_none());
        assert!(catalog.describe("toolbox.load").is_none());
    }

    #[test]
    fn search_and_load_are_hidden_without_the_domain_capability() {
        let mut catalog = ToolCatalog::new(vec![spec(
            "config.model.create",
            "Create model",
        )]);
        install_ai_toolbox(
            &mut catalog,
            &BTreeSet::new(),
            &["toolbox.inspect".to_string()],
            false,
        )
        .expect("toolbox");
        assert!(catalog.describe("toolbox.inspect").is_none());
        assert!(catalog.describe("toolbox.load").is_none());
        let state = catalog.toolbox.as_ref().expect("state").clone();
        assert!(state.search("model", None, 0, 10).0.is_empty());
    }
}
