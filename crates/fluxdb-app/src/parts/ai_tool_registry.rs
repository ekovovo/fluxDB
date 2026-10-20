// 工具目录、schema 校验与结果协议（设计 §20，T019）。
//
// 三条硬规则：
// 1. **先校验再执行**：参数必须通过工具自己的 JSON Schema 校验，校验器只允许子集、
//    绝不做类型强转或"猜参数"；失败返回受控错误（code/retryable），不进入工具执行。
// 2. **发现不等于授权**：`discover`/`describe` 只返回描述与 schema，不授予任何执行权限，
//    也不因为"看见了"就允许调用。
// 3. **回执与投影分离**：`fluxdb_core::ToolReceipt` 是不可变事实；给模型/UI 的是有界的
//    `ModelProjection`（超限截断 + 标记），投影不修改回执。

/// 工具副作用类别（与设计 §20 的 EffectClass 一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectClass {
    PureRead,
    /// 只修改宿主内存中的页面草稿，不落盘、不触碰数据库。
    LocalUiMutation,
    Mutating,
    ExternalSideEffect,
}

/// 工具声明可省略的内容；预算层不按工具名称猜测语义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutputShape {
    /// 有序条目：只保留预算能容纳的完整前缀，不裁剪单个条目。
    Sample {
        omit_field: String,
        rows_omitted_field: Option<String>,
    },
    /// 整体结果、执行回执或未知结构；超限保留权威状态及快照引用，不随机删字段。
    Opaque,
    /// 主动分段读取；原工具控制范围与游标，通用层不得删内容后仍回显原续读位置。
    Read,
}

/// 工具目录项（描述 + schema + 限制）。
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub risk: fluxdb_core::ToolRisk,
    pub effect: EffectClass,
    pub timeout_ms: u64,
    pub output_limit_bytes: u64,
    /// 产出结果的形状；决定超限时兜底裁剪的策略。
    pub output_shape: ToolOutputShape,
}

impl ToolSpec {
    pub fn from_descriptor(descriptor: &fluxdb_core::ToolDescriptor) -> fluxdb_core::Result<Self> {
        let input_schema: serde_json::Value = serde_json::from_str(&descriptor.input_schema_json)
            .map_err(|e| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("工具 {} 的 input_schema 不是合法 JSON: {e}", descriptor.id),
            )
        })?;
        Ok(Self {
            id: descriptor.id.clone(),
            version: descriptor.version,
            description: descriptor.description.clone(),
            input_schema,
            output_schema: serde_json::json!({ "type": "object" }),
            risk: descriptor.risk,
            effect: match descriptor.risk {
                fluxdb_core::ToolRisk::Read => EffectClass::PureRead,
                fluxdb_core::ToolRisk::Write => EffectClass::Mutating,
                fluxdb_core::ToolRisk::Admin => EffectClass::ExternalSideEffect,
            },
            timeout_ms: 30_000,
            output_limit_bytes: 128 * 1024,
            // 未声明结构仍有通用预算，但不猜测哪些业务字段可以删除。
            output_shape: ToolOutputShape::Opaque,
        })
    }
}

/// 发现结果（**不含 schema、不授予执行权限**）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub risk: fluxdb_core::ToolRisk,
    pub effect: EffectClass,
}

/// 受控工具错误（设计 §20：code/message/retryability）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl ToolError {
    pub fn unknown_tool(id: &str) -> Self {
        Self {
            code: "unknown_tool",
            message: format!("未知工具: {id}"),
            retryable: false,
        }
    }

    pub fn invalid_arguments(reason: impl Into<String>) -> Self {
        Self {
            code: "invalid_arguments",
            message: reason.into(),
            // 参数错误通常可由模型修正，因此允许重试；但绝不是"猜参数"。
            retryable: true,
        }
    }

    pub fn too_large(reason: impl Into<String>) -> Self {
        Self {
            code: "arguments_too_large",
            message: reason.into(),
            retryable: true,
        }
    }
}

/// 已通过 schema 校验的参数（只能由校验器产出）。
#[derive(Clone, Debug)]
pub struct ValidatedArguments(serde_json::Value);

impl ValidatedArguments {
    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    pub fn to_json_string(&self) -> String {
        self.0.to_string()
    }
}

/// 参数体积上限（默认 256 KiB），防止把超大 JSON 透传进工具。
pub const DEFAULT_MAX_ARGUMENT_BYTES: usize = 256 * 1024;

/// 工具目录。
pub struct ToolCatalog {
    security: Option<Arc<ExecutionSecurity>>,
    specs: Vec<ToolSpec>,
    /// 可执行实现（没有实现的条目只能 describe，不能 prepare 执行）。
    executable: HashMap<String, Box<dyn fluxdb_core::AiTool>>,
    max_argument_bytes: usize,
    hooks: HookPipeline,
    /// 当前轮的快照仓库；SQL 条目和其他工具的输出片段共用同一授权生命周期。
    ///
    /// 目录是工具实现与结果仓库的同一次装配，回执登记溢写事实时按此查句柄，
    /// 不再要求调用方把同一个仓库另传一遍——两处各传一次迟早会漏。
    results: Option<Arc<Mutex<ResultStore>>>,
}

impl ToolCatalog {
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self {
            security: None,
            specs,
            executable: HashMap::new(),
            max_argument_bytes: DEFAULT_MAX_ARGUMENT_BYTES,
            hooks: HookPipeline::default(),
            results: None,
        }
    }

    pub fn with_execution_security(mut self, security: ExecutionSecurity) -> Self {
        self.security=Some(Arc::new(security));
        self
    }

    /// 关联本轮结果仓库：工具产出的大结果句柄可被查到溢写事实。
    pub fn with_results(mut self, results: Arc<Mutex<ResultStore>>) -> Self {
        self.results = Some(results);
        self
    }

    /// 本轮结果仓库；未装配（无结果句柄的工具目录）返回 `None`。
    pub fn results(&self) -> Option<&Arc<Mutex<ResultStore>>> {
        self.results.as_ref()
    }

    pub fn with_argument_limit(mut self, max_argument_bytes: usize) -> Self {
        self.max_argument_bytes = max_argument_bytes;
        self
    }

    /// 移除已被全局停用的工具条目（设置页「工具与 MCP → 工具」）。
    ///
    /// 被停用的工具同时从 `specs` 与可执行实现中移除：模型既发现不到，也
    /// 即使绕过发现直接发起调用也无法执行（`describe`/`executable` 都会落空）。
    pub fn without_tools(&mut self, disabled_ids: &std::collections::BTreeSet<String>) {
        if disabled_ids.is_empty() {
            return;
        }
        self.specs.retain(|spec| !disabled_ids.contains(&spec.id));
        self.executable.retain(|id, _| !disabled_ids.contains(id));
    }

    pub fn with_hooks(mut self, hooks: HookPipeline) -> Self {
        self.hooks = hooks;
        self
    }

    /// 注册工具并显式声明其输出形状。
    ///
    /// 形状是**工具自己的语义**（"我产出的是样本"还是"内容本身就是产物"），
    /// 不能由通用层从 id 或描述反推，所以由注册处显式给出。
    pub fn add_tool_with_shape(
        &mut self,
        tool: Box<dyn fluxdb_core::AiTool>,
        shape: ToolOutputShape,
    ) -> fluxdb_core::Result<()> {
        self.add_tool_with_shape_and_effect(tool, shape, None)
    }

    /// 注册工具并覆盖声明的效果类别；用于“风险按只读、但会改本地 UI 草稿”的页面动作。
    pub fn add_tool_with_shape_and_effect(
        &mut self,
        tool: Box<dyn fluxdb_core::AiTool>,
        shape: ToolOutputShape,
        effect: Option<EffectClass>,
    ) -> fluxdb_core::Result<()> {
        let mut spec = ToolSpec::from_descriptor(tool.descriptor())?;
        spec.output_shape = shape;
        if let Some(effect) = effect {
            spec.effect = effect;
        }
        if self.describe(&spec.id).is_some() {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "工具 ID 重复",
            ));
        }
        self.specs.push(spec);
        self.register(tool)
    }

    /// 注册工具；未声明形状时按 [`ToolOutputShape::Opaque`] 处理。
    pub fn add_tool(&mut self, tool: Box<dyn fluxdb_core::AiTool>) -> fluxdb_core::Result<()> {
        self.add_tool_with_shape(tool, ToolOutputShape::Opaque)
    }

    /// 注册可执行实现（按宿主 ID）。
    pub fn register(&mut self, tool: Box<dyn fluxdb_core::AiTool>) -> fluxdb_core::Result<()> {
        let id = tool.descriptor().id.clone();
        if !self.specs.iter().any(|spec| spec.id == id) {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("工具 {id} 没有对应的 ToolSpec，拒绝注册"),
            ));
        }
        self.executable.insert(id, tool);
        Ok(())
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    /// 工具发现：按关键字过滤（id/描述），返回**摘要**，不返回 schema、不授予权限。
    pub fn discover(&self, query: &str, limit: usize) -> Vec<ToolSummary> {
        let needle = query.trim().to_lowercase();
        self.specs
            .iter()
            .filter(|spec| {
                needle.is_empty()
                    || spec.id.to_lowercase().contains(&needle)
                    || spec.description.to_lowercase().contains(&needle)
            })
            .take(limit)
            .map(|spec| ToolSummary {
                id: spec.id.clone(),
                version: spec.version,
                description: spec.description.clone(),
                risk: spec.risk,
                effect: spec.effect,
            })
            .collect()
    }

    /// 具体 schema：调用方确认要用的工具后再取。
    pub fn describe(&self, id: &str) -> Option<&ToolSpec> {
        self.specs.iter().find(|spec| spec.id == id)
    }

    /// 校验参数（未知工具 / schema 不通过 → 受控错误；不做类型强转与"猜参数"）。
    pub fn prepare(&self, id: &str, arguments_json: &str) -> Result<ValidatedArguments, ToolError> {
        let spec = self
            .describe(id)
            .ok_or_else(|| ToolError::unknown_tool(id))?;
        if arguments_json.len() > self.max_argument_bytes {
            return Err(ToolError::too_large(format!(
                "参数超过 {} 字节上限",
                self.max_argument_bytes
            )));
        }
        let value: serde_json::Value = serde_json::from_str(arguments_json)
            .map_err(|e| ToolError::invalid_arguments(format!("参数不是合法 JSON: {e}")))?;
        validate_schema(&spec.input_schema, &value)
            .map_err(|reason| ToolError::invalid_arguments(reason))?;
        Ok(ValidatedArguments(value))
    }

    /// 取可执行实现（只有 `prepare` 通过后才应调用）。
    pub fn executable(&self, id: &str) -> Option<&dyn fluxdb_core::AiTool> {
        self.executable.get(id).map(|tool| tool.as_ref())
    }

    /// 回执不变；模型只获得按声明语义裁剪的投影及当前轮可用的续读引用。
    pub fn project(
        &self,
        id: &str,
        receipt: &fluxdb_core::ToolReceipt,
        result_ref: Option<ResultRef>,
    ) -> Result<ModelProjection, ToolError> {
        let spec = self
            .describe(id)
            .ok_or_else(|| ToolError::unknown_tool(id))?;
        let mut budget = ToolOutputBudget::for_spec(spec);
        let framing = serde_json::json!({
            "tool":id,"status":loop_tool_status_label(receipt.status),"truncated":false,"result":null,
        }).to_string();
        budget.max_bytes = budget
            .max_bytes
            .saturating_sub(framing.len().saturating_sub(4));
        budget.max_tokens = budget
            .max_tokens
            .saturating_sub(estimate_tokens(&framing) as usize);
        let mut reference = result_ref.or_else(|| {
            self.results
                .as_ref()?
                .lock()
                .ok()?
                .reference_for_call(&receipt.call_id)
        });
        let mut projection =
            project_receipt_with_budget(receipt, reference.clone(), budget, &spec.output_shape);
        if projection.truncated && reference.is_none() {
            if let Some(results) = &self.results {
                let mut store = results.lock().map_err(|_| ToolError {
                    code: "result_store_unavailable",
                    message: "结果仓库不可用".into(),
                    retryable: false,
                })?;
                reference = Some(store.put_output(receipt).map_err(|error| ToolError {
                    code: "result_snapshot_unavailable",
                    message: error.message,
                    retryable: false,
                })?);
                projection =
                    project_receipt_with_budget(receipt, reference, budget, &spec.output_shape);
            }
        }
        tracing::debug!(
            tool_id = id,
            source_bytes = receipt.result_json.len(),
            model_bytes = projection.text.len(),
            estimated_tokens = estimate_tokens(&projection.text),
            truncated = projection.truncated,
            "工具输出预算投影完成"
        );
        Ok(projection)
    }
}

/// 校验 JSON 值是否符合 schema 的**受支持子集**。
///
/// 支持：`$ref`（仅 `#/$defs/<name>`）、`type`、`required`、`properties`、
/// `additionalProperties: false`、`enum/const/oneOf`、`minLength/maxLength`、`minimum/maximum`、
/// `minItems/maxItems`、`items`。其余关键字忽略（不猜测语义）。
pub fn validate_schema(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), String> {
    validate_at(schema, value, schema, 0)
}

const MAX_SCHEMA_DEPTH: usize = 32;

fn validate_at(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    root: &serde_json::Value,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("schema 嵌套超过 {MAX_SCHEMA_DEPTH} 层"));
    }
    if let Some(variants) = schema.get("oneOf").and_then(|value| value.as_array()) {
        let action = value.get("action").and_then(|action| action.as_str());
        let mut matches = 0;
        let mut action_error = None;
        for variant in variants {
            match validate_at(variant, value, root, depth + 1) {
                Ok(()) => matches += 1,
                Err(error)
                    if action_error.is_none()
                        && action.is_some()
                        && variant
                            .get("properties")
                            .and_then(|properties| properties.get("action"))
                            .and_then(|action_schema| action_schema.get("const"))
                            .and_then(|expected| expected.as_str())
                            == action =>
                {
                    action_error = Some(error);
                }
                Err(_) => {}
            }
        }
        if matches == 0 {
            if let Some(error) = action_error {
                return Err(format!(
                    "参数不符合 action {} 的声明：{error}",
                    action.unwrap_or_default()
                ));
            }
            return Err("参数必须匹配且仅匹配一种声明形状".into());
        }
        if matches > 1 {
            return Err("参数必须匹配且仅匹配一种声明形状".into());
        }
    }
    if let Some(expected) = schema.get("const") {
        if value != expected {
            return Err("参数不匹配声明的固定取值".into());
        }
    }
    // $ref 解析（只支持本地 #/$defs/<name>）。
    if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| format!("不支持的 $ref: {reference}"))?;
        let target = root
            .get("$defs")
            .and_then(|defs| defs.get(name))
            .ok_or_else(|| format!("$ref 目标不存在: {reference}"))?;
        return validate_at(target, value, root, depth + 1);
    }

    if let Some(expected) = schema.get("type").and_then(|t| t.as_str()) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => return Err(format!("不支持的 type: {other}")),
        };
        if !matches {
            return Err(format!("参数类型不匹配：期望 {expected}，实际 {value}"));
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
        if !allowed.iter().any(|candidate| candidate == value) {
            return Err(format!("取值不在 enum 允许范围内: {value}"));
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(min) = schema.get("minLength").and_then(|m| m.as_u64()) {
            if (text.chars().count() as u64) < min {
                return Err(format!("字符串长度小于 {min}"));
            }
        }
        if let Some(max) = schema.get("maxLength").and_then(|m| m.as_u64()) {
            if (text.chars().count() as u64) > max {
                return Err(format!("字符串长度超过 {max}"));
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(|m| m.as_f64()) {
            if number < min {
                return Err(format!("数值小于最小值 {min}"));
            }
        }
        if let Some(max) = schema.get("maximum").and_then(|m| m.as_f64()) {
            if number > max {
                return Err(format!("数值超过最大值 {max}"));
            }
        }
    }

    if let Some(items) = value.as_array() {
        if let Some(min) = schema.get("minItems").and_then(|m| m.as_u64()) {
            if (items.len() as u64) < min {
                return Err(format!("数组元素少于 {min}"));
            }
        }
        if let Some(max) = schema.get("maxItems").and_then(|m| m.as_u64()) {
            if (items.len() as u64) > max {
                return Err(format!("数组元素超过 {max}"));
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate_at(item_schema, item, root, depth + 1)?;
            }
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for key in required.iter().filter_map(|k| k.as_str()) {
                if !object.contains_key(key) {
                    return Err(format!("缺少必填字段: {key}"));
                }
            }
        }
        let properties = schema.get("properties").and_then(|p| p.as_object());
        if let Some(properties) = properties {
            for (key, property_schema) in properties {
                if let Some(property_value) = object.get(key) {
                    validate_at(property_schema, property_value, root, depth + 1)?;
                }
            }
        }
        if schema.get("additionalProperties").and_then(|a| a.as_bool()) == Some(false) {
            if let Some(properties) = properties {
                for key in object.keys() {
                    if !properties.contains_key(key) {
                        return Err(format!("不允许的额外字段: {key}"));
                    }
                }
            }
        }
    }
    Ok(())
}
