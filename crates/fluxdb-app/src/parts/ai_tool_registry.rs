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

/// 工具产出结果的形状（设计 §15；参考 Pi `outputLimits` / Goose 大响应落盘）。
///
/// 截断应当发生在**产出**结果的地方：工具自己知道哪些内容可以省略、哪些是
/// 必须保留的结构。传输层只按这里声明的形状做兜底，不再对已裁好的载荷二次裁剪，
/// 也不去猜测未声明的结构。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutputShape {
    /// 结果由工具自己保证有界（如 `db.query` 只放样本行 + ResultRef）。
    /// `keep_rows` 是回执里保留的样本行数；`omit_field`/`rows_omitted_field`
    /// 是被省略行数的字段名，供兜底裁剪时优先丢弃前者、保留后者。
    Sample {
        keep_rows: usize,
        omit_field: String,
        rows_omitted_field: Option<String>,
    },
    /// 结果整体有意义，不得按字段裁剪；超限只保留结构与截断标记。
    /// 用于文件读取、Redis inspect 这类"内容本身就是产物"的工具。
    Opaque,
}

impl ToolOutputShape {
    /// 该形状下兜底裁剪时可优先丢弃的字段（大字段在前）。
    fn shed_order(&self) -> Vec<String> {
        match self {
            ToolOutputShape::Sample { omit_field, .. } => vec![omit_field.clone()],
            ToolOutputShape::Opaque => Vec::new(),
        }
    }
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
            // 未声明形状的工具按"内容本身是产物"处理：兜底裁剪只丢顶层字段，
            // 不做结构猜测，避免把 file.read 这类结果的语义裁坏。
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
    specs: Vec<ToolSpec>,
    /// 可执行实现（没有实现的条目只能 describe，不能 prepare 执行）。
    executable: HashMap<String, Box<dyn fluxdb_core::AiTool>>,
    max_argument_bytes: usize,
    hooks: HookPipeline,
    /// 本轮结果仓库（`standard_sql_catalog_with_results` 注入）。
    ///
    /// 目录是工具实现与结果仓库的同一次装配，回执登记溢写事实时按此查句柄，
    /// 不再要求调用方把同一个仓库另传一遍——两处各传一次迟早会漏。
    results: Option<Arc<Mutex<ResultStore>>>,
}

impl ToolCatalog {
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            executable: HashMap::new(),
            max_argument_bytes: DEFAULT_MAX_ARGUMENT_BYTES,
            hooks: HookPipeline::default(),
            results: None,
        }
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
        self.executable
            .retain(|id, _| !disabled_ids.contains(id));
    }

    pub fn with_hooks(mut self, hooks: HookPipeline) -> Self { self.hooks=hooks; self }

    /// 注册工具并显式声明其输出形状。
    ///
    /// 形状是**工具自己的语义**（"我产出的是样本"还是"内容本身就是产物"），
    /// 不能由通用层从 id 或描述反推，所以由注册处显式给出。
    pub fn add_tool_with_shape(
        &mut self,
        tool: Box<dyn fluxdb_core::AiTool>,
        shape: ToolOutputShape,
    ) -> fluxdb_core::Result<()> {
        let mut spec = ToolSpec::from_descriptor(tool.descriptor())?;
        spec.output_shape = shape;
        if self.describe(&spec.id).is_some(){return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"工具 ID 重复"));}
        self.specs.push(spec);
        self.register(tool)
    }

    /// 注册工具；未声明形状时按 [`ToolOutputShape::Opaque`] 处理。
    pub fn add_tool(&mut self, tool:Box<dyn fluxdb_core::AiTool>)->fluxdb_core::Result<()> {
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

    /// 把回执投影成**该工具声明的**输出上限内的内容（未注册工具返回受控错误）。
    ///
    /// 这是"output 大小限制"的生效点：调用方不应自行决定上限，避免某个工具忘记截断。
    /// 裁剪策略来自工具自己声明的 [`ToolOutputShape`]——已声明 `Sample` 的工具在
    /// 产出时裁过一次，这里只按同一顺序兜底，不重新解释它的结构。
    pub fn project(
        &self,
        id: &str,
        receipt: &fluxdb_core::ToolReceipt,
        result_ref: Option<ResultRef>,
    ) -> Result<ModelProjection, ToolError> {
        let spec = self
            .describe(id)
            .ok_or_else(|| ToolError::unknown_tool(id))?;
        Ok(project_receipt_with_shape(
            receipt,
            result_ref,
            spec.output_limit_bytes as usize,
            &spec.output_shape,
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
    /// 句柄归属：只有同 session/turn 才能读取，防止跨会话取数据。
    pub session_id: String,
    pub turn_id: String,
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

    /// spill 文件缺失或损坏：读取必须显式失败，不能返回一个假的空页或游标。
    fn damaged(message: impl Into<String>) -> Self {
        Self {
            code: "result_damaged",
            message: message.into(),
        }
    }
}

/// 结果仓库（T027）：内存保存首段，超出内存预算的条目写受控 spill 文件。
///
/// 仓库实例本身绑定一次会话（`for_turn`），因此跨 session/turn 的 ResultRef
/// 无法在此仓库读到数据；`read_page` 只读已取得的快照切片，不重新执行来源工具。
pub struct ResultStore {
    entries: HashMap<String, StoredResult>,
    next_id: u64,
    /// 单个结果的内存字节上限。
    pub byte_limit: usize,
    /// 句柄归属会话。
    owner_session_id: String,
    owner_turn_id: String,
    /// 受控落盘根；`None` 表示不 spill（超限条目回落内存，仍不丢数据）。
    spill_root: Option<std::path::PathBuf>,
}

struct StoredResult {
    result: ResultRef,
    items: Vec<serde_json::Value>,
    /// 超出内存预算、已写入受控 spill 文件的条目数；分页时按需回读。
    spilled_len: usize,
    /// spill 文件相对受控根的路径（无 spill 时为空）。
    spill_path: Option<std::path::PathBuf>,
    /// spill 文件相对受控根的路径字符串与内容指纹；溢写事实需与 `ai_results` 对账。
    spill_fact: Option<SpillFact>,
    status: ResultStatus,
}

/// 已落盘结果的可核验事实：产出工具、受控根内相对路径、字节数、内容哈希。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillFact {
    /// 产出该落盘结果的工具 ID。回执里的 `result_ref` 只是一次引用，
    /// 可能是别的工具（如 `result.read_page`）回显的来源句柄；
    /// 只有产出工具与本次调用的工具一致，这条事实才属于本次调用。
    pub tool_id: String,
    pub relative_path: String,
    pub byte_count: i64,
    pub content_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultStatus {
    Available,
    Released,
    Expired,
}

/// spill 落盘事实：相对路径 / 字节数 / 内容指纹，供 `ai_results` 登记与校验。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpillResult {
    /// spill 文件相对受控根的路径；未发生 spill 时为空。
    pub relative_path: Option<String>,
    pub byte_count: i64,
    pub content_hash: String,
}

impl SpillResult {
    /// 是否真的写了 spill 文件。
    pub fn spilled(&self) -> bool {
        self.relative_path.is_some()
    }
}

fn result_io_error(message: &str) -> impl Fn(std::io::Error) -> fluxdb_core::Error + '_ {
    move |error| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("{message}: {error}"),
        )
    }
}

impl ResultStore {
    /// 内存仓库（测试与无落盘场景）：超限条目回落内存，不写文件。
    pub fn new(byte_limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 1,
            byte_limit,
            owner_session_id: String::new(),
            owner_turn_id: String::new(),
            spill_root: None,
        }
    }

    /// 生产仓库：绑定会话归属；`spill_root` 为受控结果根目录。
    pub fn for_turn(
        byte_limit: usize,
        session_id: &str,
        turn_id: &str,
        spill_root: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 1,
            byte_limit,
            owner_session_id: session_id.to_string(),
            owner_turn_id: turn_id.to_string(),
            spill_root,
        }
    }

    /// 保存一次工具结果，返回句柄与 spill 事实。
    ///
    /// 内存预算内的条目留在内存；超出部分在有受控根时写入 `results/<uuid>.json`，
    /// 否则回落内存，保证模型可读到的条目不会因为没有目录而缺失。
    pub fn put(
        &mut self,
        tool_id: &str,
        now_ms: i64,
        items: impl IntoIterator<Item = serde_json::Value>,
    ) -> fluxdb_core::Result<(ResultRef, SpillResult)> {
        let mut memory = Vec::new();
        let mut overflow = Vec::new();
        let mut memory_bytes = 0usize;
        for item in items {
            let size = serde_json::to_string(&item).map(|text| text.len()).unwrap_or(0);
            if memory_bytes + size <= self.byte_limit {
                memory_bytes += size;
                memory.push(item);
            } else {
                overflow.push(item);
            }
        }
        let mut spill = SpillResult::default();
        let mut spilled_len = 0usize;
        if !overflow.is_empty() {
            let payload = serde_json::to_vec(&overflow).map_err(|error| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("结果序列化失败: {error}"),
                )
            })?;
            let content = String::from_utf8_lossy(&payload).into_owned();
            let hash = crate::source_hash(&[&content]);
            match self.spill_root.clone() {
                Some(root) => {
                    let dir = root.join("results");
                    std::fs::create_dir_all(&dir).map_err(result_io_error("创建结果目录失败"))?;
                    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
                    std::fs::write(&path, &payload).map_err(result_io_error("写结果 spill 文件失败"))?;
                    spill.relative_path = path
                        .strip_prefix(&root)
                        .ok()
                        .map(|rest| rest.to_string_lossy().into_owned());
                    spill.byte_count = payload.len() as i64;
                    spill.content_hash = hash;
                    spilled_len = overflow.len();
                }
                None => {
                    // 没有受控目录时不能丢数据：回填内存，避免模型读到不完整结果。
                    spilled_len = 0;
                    memory.append(&mut overflow);
                }
            }
        }
        let id = format!("result-{}", self.next_id);
        self.next_id += 1;
        let result = ResultRef {
            id: id.clone(),
            tool_id: tool_id.to_string(),
            created_at_ms: now_ms,
            session_id: self.owner_session_id.clone(),
            turn_id: self.owner_turn_id.clone(),
        };
        let spill_path = self
            .spill_root
            .as_ref()
            .zip(spill.relative_path.as_ref())
            .map(|(root, relative)| root.join(relative));
        self.entries.insert(
            id,
            StoredResult {
                result: result.clone(),
                items: memory,
                spilled_len,
                spill_path,
                spill_fact: spill
                    .relative_path
                    .as_ref()
                    .map(|relative_path| SpillFact {
                        tool_id: result.tool_id.clone(),
                        relative_path: relative_path.clone(),
                        byte_count: spill.byte_count,
                        content_hash: spill.content_hash.clone(),
                    }),
                status: ResultStatus::Available,
            },
        );
        Ok((result, spill))
    }

    /// 取句柄的 spill 事实；未溢写或句柄不存在返回 `None`。
    pub fn spill_fact(&self, id: &str) -> Option<SpillFact> {
        self.entries.get(id)?.spill_fact.clone()
    }

    /// 释放句柄（释放后读取必须报错，而不是返回空页）。
    pub fn release(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.status = ResultStatus::Released;
                true
            }
            None => false,
        }
    }

    /// 标记过期（TTL 由调用方判定；过期后读取必须报错）。
    pub fn expire(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.status = ResultStatus::Expired;
                true
            }
            None => false,
        }
    }

    /// 会话结束时释放该会话的全部句柄（memory 结果过期语义）。
    pub fn expire_session(&mut self) {
        for entry in self.entries.values_mut() {
            entry.status = ResultStatus::Expired;
        }
    }

    /// 读取一页：行数与字节双上限，返回截断标记与下一页游标。
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
        match entry.status {
            ResultStatus::Available => {}
            ResultStatus::Released => return Err(ResultError::expired(id)),
            ResultStatus::Expired => return Err(ResultError::unknown(id)),
        }
        if entry.result.session_id != self.owner_session_id
            || entry.result.turn_id != self.owner_turn_id
        {
            // 句柄不属于本仓库归属的会话：按未知处理，不泄露其它会话的数据。
            return Err(ResultError::unknown(id));
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
        let total_items = entry.items.len() + entry.spilled_len;
        if start > total_items {
            return Err(ResultError::bad_cursor(&format!("{id}:{start}")));
        }
        // spill 条目按需从受控文件回读；文件缺失或损坏必须显式报错，
        // 不能只报一个"还有更多"的游标让模型以为能读到数据。
        let spilled_items = match (entry.spill_path.as_ref(), entry.spilled_len) {
            (Some(_), 0) | (None, 0) => Vec::new(),
            (Some(path), expected) => {
                let raw = std::fs::read(path).map_err(|error| {
                    ResultError::damaged(format!("结果 spill 文件不可读: {error}"))
                })?;
                let decoded: Vec<serde_json::Value> = serde_json::from_slice(&raw).map_err(|error| {
                    ResultError::damaged(format!("结果 spill 文件已损坏: {error}"))
                })?;
                if decoded.len() != expected {
                    return Err(ResultError::damaged("结果 spill 文件条目数与登记不一致"));
                }
                decoded
            }
            (None, expected) => {
                return Err(ResultError::damaged(format!(
                    "结果缺少 spill 文件，无法读取剩余 {expected} 条"
                )));
            }
        };
        let limit = limit.max(1);
        let mut items = Vec::new();
        let mut used = 0usize;
        let mut index = start;
        while index < total_items && items.len() < limit {
            let item = match index < entry.items.len() {
                true => &entry.items[index],
                false => &spilled_items[index - entry.items.len()],
            };
            let size = serde_json::to_string(item).map(|text| text.len()).unwrap_or(0);
            if !items.is_empty() && used + size > max_bytes {
                break;
            }
            used += size;
            items.push(item.clone());
            index += 1;
        }
        let truncated = index < total_items;
        Ok(ResultPage {
            result: entry.result.clone(),
            items,
            truncated,
            next_cursor: truncated.then(|| format!("{id}:{index}")),
            total_items,
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
/// 不带形状声明的投影入口（供只需上限裁剪的调用方与测试使用）。
///
/// 等价于按 [`ToolOutputShape::Opaque`] 处理：不猜测结构，只在超限时按字段丢弃。
pub fn project_receipt(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    max_bytes: usize,
) -> ModelProjection {
    project_receipt_with_shape(receipt, result_ref, max_bytes, &ToolOutputShape::Opaque)
}

/// 把真实回执投影成有界内容（回执本身保持不变）。
///
/// 投影结果必须是**合法 JSON**：调用方（主循环）会把它原样嵌进
/// `{"tool":..,"truncated":..,"result":<投影>}` 信封发给模型，所以按字节硬切会
/// 截在字符串/数组中间，产出模型无法解析的半截 JSON。
///
/// 按工具声明的形状处理（参考 Pi `outputLimits`、Goose 大响应落盘）：
/// 工具在产出时已经裁过一次（`Sample` 形状），传输层只按同一字段顺序兜底；
/// 未声明形状时按 `Opaque` 处理，不做结构猜测。
pub fn project_receipt_with_shape(
    receipt: &fluxdb_core::ToolReceipt,
    result_ref: Option<ResultRef>,
    max_bytes: usize,
    shape: &ToolOutputShape,
) -> ModelProjection {
    let raw = receipt.result_json.as_str();
    if raw.len() <= max_bytes {
        return ModelProjection {
            text: raw.to_string(),
            truncated: false,
            result_ref,
        };
    }
    // 工具声明的 output 上限是硬约束：投影必须在 max_bytes 内，因此连受控错误对象
    // 也要按同样预算收敛，而不是"为了说明错误先超限"。
    let text = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(_)) => shrink_to_budget(raw, max_bytes, shape)
            .unwrap_or_else(|| too_large_marker(max_bytes, shape)),
        // 非 JSON 回执（受控夹具/简单文本结果）不做结构猜测：按字节截断后作为
        // JSON 字符串回传，保持"内容 + 截断标记"且仍是合法 JSON。
        _ => truncated_json_string(raw, max_bytes),
    };
    ModelProjection {
        text,
        truncated: true,
        result_ref,
    }
}

/// 在字节预算内丢弃字段，直到序列化结果装得下。
///
/// 丢弃顺序由工具声明的 [`ToolOutputShape::shed_order`] 决定（大字段优先），
/// 声明顺序之外的字段仍按"从末尾向前"兜底，这样结构、行数与截断标记等小事实
/// 留在上下文里。每次成功的候选都写入 `omitted_fields`，让模型知道哪些内容被省略
/// 而不是凭空消失。
fn shrink_to_budget(raw: &str, max_bytes: usize, shape: &ToolOutputShape) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object_mut()?;
    let shed = shape.shed_order();
    let keys: Vec<String> = object.keys().cloned().collect();
    // 声明要丢弃的字段排前面，其余保持"从末尾向前"。
    let mut order: Vec<String> = Vec::with_capacity(keys.len());
    order.extend(shed.iter().filter(|key| keys.contains(key)).cloned());
    order.extend(keys.iter().rev().filter(|key| !shed.contains(key)).cloned());

    let mut dropped: Vec<String> = Vec::new();
    for key in &order {
        let mut probe = object.clone();
        probe.insert(
            "omitted_fields".into(),
            serde_json::Value::Array(
                dropped
                    .iter()
                    .map(|name| serde_json::Value::String(name.clone()))
                    .collect(),
            ),
        );
        if let Ok(candidate) = serde_json::to_string(&probe)
            && candidate.len() <= max_bytes
        {
            return Some(candidate);
        }
        object.remove(key);
        dropped.push(key.clone());
    }
    None
}

/// 在给定预算内构造最小可解释的超限说明。
///
/// 优先给带 message 的完整说明；预算不够就退到只有错误码的最小对象，
/// 极端预算（0–1 字节）返回空串——工具声明的上限才是硬约束。
fn too_large_marker(max_bytes: usize, shape: &ToolOutputShape) -> String {
    // 样本形状的工具本来就能分页读取，提示应指向续读而不是让模型缩小查询。
    let hint = match shape {
        ToolOutputShape::Sample {
            rows_omitted_field, ..
        } => rows_omitted_field.as_ref().map_or_else(
            || "结果超过该工具的输出上限，请缩小查询范围".to_string(),
            |field| {
                format!(
                    "结果超过该工具的输出上限，完整数据请用 result.read_page 按 {field} 指定的句柄分页读取"
                )
            },
        ),
        ToolOutputShape::Opaque => {
            "结果超过该工具的输出上限，请缩小查询范围或读取范围".to_string()
        }
    };
    let full = serde_json::json!({
        "error": "result_too_large",
        "message": hint,
    })
    .to_string();
    if full.len() <= max_bytes {
        return full;
    }
    let compact = serde_json::json!({"error": "result_too_large"}).to_string();
    if compact.len() <= max_bytes {
        return compact;
    }
    String::new()
}

/// 非 JSON 内容按字节安全截断，并作为合法 JSON 字符串返回。
///
/// 截断只保证 UTF-8 字符边界；JSON 转义由 serde 负责，因此序列化结果可能比
/// 原始片段更长，这里按序列化后的实际长度回退到更短的前缀。
fn truncated_json_string(raw: &str, max_bytes: usize) -> String {
    let mut end = raw.len().min(max_bytes);
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut slice = &raw[..end];
    // 预留两个引号的预算。
    while slice.len() + 2 > max_bytes {
        let mut shorter = slice.len().saturating_sub(1);
        while shorter > 0 && !slice.is_char_boundary(shorter) {
            shorter -= 1;
        }
        slice = &slice[..shorter];
        if slice.is_empty() {
            break;
        }
    }
    serde_json::to_string(slice).unwrap_or_default()
}
