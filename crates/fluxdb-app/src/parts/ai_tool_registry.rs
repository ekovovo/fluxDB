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
    Mutating,
    ExternalSideEffect,
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
    specs: Vec<ToolSpec>,
    /// 可执行实现（没有实现的条目只能 describe，不能 prepare 执行）。
    executable: HashMap<String, Box<dyn fluxdb_core::AiTool>>,
    max_argument_bytes: usize,
    hooks: HookPipeline,
}

impl ToolCatalog {
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            executable: HashMap::new(),
            max_argument_bytes: DEFAULT_MAX_ARGUMENT_BYTES,
            hooks: HookPipeline::default(),
        }
    }

    pub fn with_argument_limit(mut self, max_argument_bytes: usize) -> Self {
        self.max_argument_bytes = max_argument_bytes;
        self
    }
    pub fn with_hooks(mut self, hooks: HookPipeline) -> Self { self.hooks=hooks; self }
    pub fn add_tool(&mut self, tool:Box<dyn fluxdb_core::AiTool>)->fluxdb_core::Result<()> {
        if self.describe(&tool.descriptor().id).is_some(){return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"工具 ID 重复"));}
        self.specs.push(ToolSpec::from_descriptor(tool.descriptor())?);
        self.register(tool)
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

    /// 把回执投影成**该工具声明的**输出上限内的内容（未注册工具返回受控错误）。
    ///
    /// 这是"output 大小限制"的生效点：调用方不应自行决定上限，避免某个工具忘记截断。
    pub fn project(
        &self,
        id: &str,
        receipt: &fluxdb_core::ToolReceipt,
        result_ref: Option<ResultRef>,
    ) -> Result<ModelProjection, ToolError> {
        let spec = self
            .describe(id)
            .ok_or_else(|| ToolError::unknown_tool(id))?;
        Ok(project_receipt(
            receipt,
            result_ref,
            spec.output_limit_bytes as usize,
        ))
    }
}

/// 校验 JSON 值是否符合 schema 的**受支持子集**。
///
/// 支持：`$ref`（仅 `#/$defs/<name>`）、`type`、`required`、`properties`、
/// `additionalProperties: false`、`enum`、`minLength/maxLength`、`minimum/maximum`、
/// `minItems/maxItems`、`items`。其余关键字忽略（不猜测语义）。
pub fn validate_schema(schema: &serde_json::Value, value: &serde_json::Value) -> Result<(), String> {
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

/// 结果引用（分页/过期的唯一句柄）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRef {
    pub id: String,
    pub tool_id: String,
    pub created_at_ms: i64,
}

/// 结果页：有界 + 截断标记 + 游标（不能用截断结果宣称全集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultPage {
    pub result: ResultRef,
    pub items: Vec<serde_json::Value>,
    /// 本次是否被行数/字节上限截断。
    pub truncated: bool,
    /// 继续读取的游标；`None` 表示已经到底。
    pub next_cursor: Option<String>,
    pub total_items: usize,
}

/// 结果读取错误（过期/已释放/未知/游标非法都可见）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultError {
    pub code: &'static str,
    pub message: String,
}

impl ResultError {
    fn unknown(id: &str) -> Self {
        Self {
            code: "unknown_result",
            message: format!("结果句柄不存在: {id}"),
        }
    }

    fn expired(id: &str) -> Self {
        Self {
            code: "result_expired",
            message: format!("结果句柄已过期: {id}"),
        }
    }

    fn bad_cursor(cursor: &str) -> Self {
        Self {
            code: "invalid_cursor",
            message: format!("结果游标非法: {cursor}"),
        }
    }
}

/// 结果仓库（本切片用内存实现；后续可换成分页持久化）。
pub struct ResultStore {
    entries: HashMap<String, StoredResult>,
    next_id: u64,
    /// 单个结果的字节上限。
    pub byte_limit: usize,
}

struct StoredResult {
    result: ResultRef,
    items: Vec<serde_json::Value>,
    released: bool,
}

impl ResultStore {
    pub fn new(byte_limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 1,
            byte_limit,
        }
    }

    /// 保存一次工具结果，返回句柄；超过字节上限的部分**不保存**（避免无界内存）。
    pub fn put(
        &mut self,
        tool_id: &str,
        now_ms: i64,
        items: Vec<serde_json::Value>,
    ) -> (ResultRef, bool) {
        let mut kept = Vec::new();
        let mut used = 0usize;
        let mut truncated = false;
        for item in items {
            let size = item.to_string().len();
            if used + size > self.byte_limit {
                truncated = true;
                break;
            }
            used += size;
            kept.push(item);
        }
        let id = format!("result-{}", self.next_id);
        self.next_id += 1;
        let result = ResultRef {
            id: id.clone(),
            tool_id: tool_id.to_string(),
            created_at_ms: now_ms,
        };
        self.entries.insert(
            id,
            StoredResult {
                result: result.clone(),
                items: kept,
                released: false,
            },
        );
        (result, truncated)
    }

    /// 释放句柄（释放后读取必须报错，而不是返回空页）。
    pub fn release(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.released = true;
                true
            }
            None => false,
        }
    }

    /// 标记过期（TTL 由调用方判定；过期后读取必须报错）。
    pub fn expire(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }

    /// 读取一页：有界（行数 + 字节），返回截断标记与下一页游标。
    pub fn read_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<ResultPage, ResultError> {
        let Some(entry) = self.entries.get(id) else {
            return Err(ResultError::unknown(id));
        };
        if entry.released {
            return Err(ResultError::expired(id));
        }
        let start = match cursor {
            None => 0usize,
            Some(raw) => {
                let prefix = format!("{id}:");
                let offset = raw
                    .strip_prefix(&prefix)
                    .ok_or_else(|| ResultError::bad_cursor(raw))?;
                offset
                    .parse::<usize>()
                    .map_err(|_| ResultError::bad_cursor(raw))?
            }
        };
        if start > entry.items.len() {
            return Err(ResultError::bad_cursor(&format!("{id}:{start}")));
        }
        let limit = limit.max(1);
        let mut items = Vec::new();
        let mut used = 0usize;
        let mut index = start;
        while index < entry.items.len() && items.len() < limit {
            let item = &entry.items[index];
            let size = item.to_string().len();
            if !items.is_empty() && used + size > max_bytes {
                break;
            }
            used += size;
            items.push(item.clone());
            index += 1;
        }
        let truncated = index < entry.items.len();
        Ok(ResultPage {
            result: entry.result.clone(),
            items,
            truncated,
            next_cursor: truncated.then(|| format!("{id}:{index}")),
            total_items: entry.items.len(),
        })
    }
}

/// 面向模型的**有界投影**：超限截断并标记，绝不修改原始回执。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelProjection {
    pub text: String,
    pub truncated: bool,
    pub result_ref: Option<ResultRef>,
}

/// 把真实回执投影成有界内容（回执本身保持不变）。
pub fn project_receipt(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    max_bytes: usize,
) -> ModelProjection {
    let raw = receipt.result_json.as_str();
    if raw.len() <= max_bytes {
        return ModelProjection {
            text: raw.to_string(),
            truncated: false,
            result_ref,
        };
    }
    // 按字符边界安全截断（避免产生半个 UTF-8 字符）。
    let mut end = max_bytes;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    ModelProjection {
        text: raw[..end].to_string(),
        truncated: true,
        result_ref,
    }
}
