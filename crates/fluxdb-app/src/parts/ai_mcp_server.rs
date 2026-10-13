// 对外 MCP Server 的共享执行服务（设计 §20.4–20.6、§28.4–28.8，T054/T059/T065）。
//
// 内部 Agent 与外部 MCP Client 走**同一条执行服务**——相同的 schema 校验、
// 风险分类、强制审计、授权、连接器与回执；只换 Caller/Owner，不复制第二套实现，
// 也不创建假 Turn。协议层（stdio/JSON-RPC）在 `fluxdb-mcp` 二进制，UI 在桌面端，
// 两者都经由本模块对外部请求执行。

// 本文件以 include! 引入 crate 根作用域：BTreeSet/Arc/Mutex 及内核类型（AuditOutcome 等）
// 已在 lib.rs 顶部导入，这里不重复 use（重复导入会触发 E0252）。

/// 13 个对外 MCP 工具的稳定映射：MCP 名称 ↔ 内部 stable ToolId。
///
/// 外部用普通下划线名称方便 CLI 精确授权；内部不重复建第二实现。
/// `fluxdb_connections_list`/`fluxdb_operation_status`/`fluxdb_operation_cancel`
/// 由服务内实现（内部 id 为空），其余映射到既有工具 ID。
pub const MCP_TOOL_MAPPINGS: &[(&str, &str)] = &[
    ("fluxdb_connections_list", ""),
    ("fluxdb_search_objects", "db.search_objects"),
    ("fluxdb_describe_object", "db.describe_object"),
    ("fluxdb_query", "db.query"),
    ("fluxdb_explain", "db.explain"),
    ("fluxdb_execute", "db.execute"),
    ("fluxdb_redis_scan", "redis.scan"),
    ("fluxdb_redis_inspect", "redis.inspect"),
    ("fluxdb_redis_execute", "redis.execute"),
    ("fluxdb_redis_subscribe", "redis.subscribe"),
    ("fluxdb_redis_server_info", "redis.server_info"),
    ("fluxdb_result_page", "result.read_page"),
    ("fluxdb_operation_status", ""),
    ("fluxdb_operation_cancel", ""),
];

/// MCP 工具名 → 内部 ToolId；服务内实现的工具返回 `None`。
pub fn mcp_tool_internal_id(mcp_name: &str) -> Option<&'static str> {
    MCP_TOOL_MAPPINGS
        .iter()
        .find(|(name, _)| *name == mcp_name)
        .map(|(_, internal)| *internal)
        .filter(|internal| !internal.is_empty())
}

/// 内部 ToolId → MCP 工具名。
pub fn mcp_tool_name(internal_id: &str) -> Option<&'static str> {
    MCP_TOOL_MAPPINGS
        .iter()
        .find(|(_, internal)| *internal == internal_id)
        .map(|(name, _)| *name)
}

/// 资源类型（决定暴露哪些工具，不做跨类型放行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpResourceKind {
    Relational,
    Redis,
}

impl McpResourceKind {
    fn from_db_kind(kind: fluxdb_core::DatabaseKind) -> Self {
        match kind {
            fluxdb_core::DatabaseKind::Redis => McpResourceKind::Redis,
            _ => McpResourceKind::Relational,
        }
    }

    pub fn grant_kind(self) -> fluxdb_storage::ai_platform::mcp_server::GrantResourceKind {
        match self {
            McpResourceKind::Relational => fluxdb_storage::ai_platform::mcp_server::GrantResourceKind::Sql,
            McpResourceKind::Redis => fluxdb_storage::ai_platform::mcp_server::GrantResourceKind::Redis,
        }
    }
}

/// 由连接配置推导资源类型（无配置时按关系型处理）。
pub fn resource_kind_for_config(
    config: Option<&fluxdb_core::ConnectionConfig>,
) -> McpResourceKind {
    config
        .map(|c| McpResourceKind::from_db_kind(c.kind))
        .unwrap_or(McpResourceKind::Relational)
}

/// 解析后的资源句柄：绑定 grant ID/revision 与连接配置 revision。
///
/// 句柄不是模型提交的 URL/库名；模型只给 `target` 字符串，服务按已授权资源解析，
/// 授权被修改/禁用后返回 STALE_RESOURCE，不静默换绑另一个库。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpResourceHandle {
    pub grant_id: String,
    pub grant_revision: i64,
    pub target: String,
    pub label: String,
    pub kind: McpResourceKind,
    pub connection_id: fluxdb_core::ConnectionId,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub read_only: bool,
}

/// 把一条资源授权转换为对外可见的资源句柄（不含 endpoint/凭据）。
pub fn resource_handle(
    grant: &fluxdb_storage::ai_platform::mcp_server::ResourceGrantView,
    config: Option<&fluxdb_core::ConnectionConfig>,
) -> McpResourceHandle {
    let kind = resource_kind_for_config(config);
    let connection_id = fluxdb_core::ConnectionId(grant.connection_id as u64);
    let (target, database, schema) = match kind {
        McpResourceKind::Redis => {
            let database = if grant.database_name.trim().is_empty() {
                "0".to_string()
            } else {
                grant.database_name.clone()
            };
            (
                format!("conn:{}/db:{}", grant.connection_id, database),
                Some(database),
                None,
            )
        }
        McpResourceKind::Relational => {
            let mut target = format!("conn:{}", grant.connection_id);
            if !grant.database_name.is_empty() {
                target.push_str(&format!("/db:{}", grant.database_name));
            }
            if !grant.namespace.is_empty() {
                target.push_str(&format!("/schema:{}", grant.namespace));
            }
            (
                target,
                (!grant.database_name.is_empty()).then(|| grant.database_name.clone()),
                (!grant.namespace.is_empty()).then(|| grant.namespace.clone()),
            )
        }
    };
    let label = if grant.database_name.is_empty() {
        format!("connection:{}", grant.connection_id)
    } else {
        format!("{}/{}", grant.connection_id, grant.database_name)
    };
    McpResourceHandle {
        grant_id: grant.id.clone(),
        grant_revision: grant.revision,
        target,
        label,
        kind,
        connection_id,
        database,
        schema,
        read_only: !grant.allow_write,
    }
}

/// 由 Redis 授权对象规则推导 Key 范围（空规则不放行；见 §28.6.1）。
pub fn redis_scope_from_grant(
    grant: &fluxdb_storage::ai_platform::mcp_server::ResourceGrantView,
) -> std::result::Result<crate::RedisKeyScope, String> {
    let database = grant
        .database_name
        .trim()
        .parse::<u32>()
        .map_err(|_| "Redis 授权 database_name 必须是 0-15 的整数".to_string())?;
    let value: serde_json::Value = serde_json::from_str(&grant.object_policy_json)
        .map_err(|_| "Redis 授权对象规则不是合法 JSON".to_string())?;
    if value.get("all_keys").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(crate::RedisKeyScope::AllKeys { database });
    }
    let prefixes = value
        .get("key_prefixes")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if prefixes.is_empty() {
        return Err("Redis 授权必须显式声明 all_keys 或非空 key_prefixes".into());
    }
    Ok(crate::RedisKeyScope::Prefixes { database, prefixes })
}

/// 对外可见的一个工具条目（`tools/list` 投影）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpToolView {
    /// 对外 MCP 名称。
    pub name: String,
    /// 内部工具版本（供审计与状态查询）。
    pub internal_id: Option<String>,
    pub version: u32,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub risk: fluxdb_core::ToolRisk,
}

/// 由 Server Profile 与资源授权装配对外工具集。
///
/// - `allowed_tool_ids` 为空 → 不开放任何工具（**不解释为全部**）；
/// - 写工具（`db.execute`/`redis.execute`）只在 Profile 非 `disabled` 且授权允许写时出现；
/// - 按资源类型过滤：没有关系型授权就不出现 SQL 工具，没有 Redis 授权就不出现 Redis 工具；
/// - `tools/list` 可见性不是授权：调用时仍再检查一次（见 [`McpExecutionService::authorize_call`]）。
pub fn visible_tools(
    profile: &fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
    grants: &[fluxdb_storage::ai_platform::mcp_server::ResourceGrantView],
) -> fluxdb_core::Result<Vec<McpToolView>> {
    let mut has_relational = false;
    let mut has_redis = false;
    let mut has_writable = false;
    for grant in grants.iter().filter(|g| g.enabled && g.allow_read) {
        // 关系型与 Redis 的区分靠授权对象的形状：SQL 用 all_objects/table_allowlist，
        // Redis 用 all_keys/key_prefixes；一条 grant 只会匹配一种。
        match classify_grant(grant) {
            Some(McpResourceKind::Relational) => has_relational = true,
            Some(McpResourceKind::Redis) => has_redis = true,
            None => {}
        }
        if grant.allow_write {
            has_writable = true;
        }
    }

    let mut catalog = ToolCatalog::new(Vec::new());
    let _ = &mut catalog;
    let mut views = Vec::new();
    // SQL 工具：只有存在关系型授权时才列出。
    if has_relational {
        for spec in standard_sql_tool_specs()? {
            if spec.risk == fluxdb_core::ToolRisk::Write && !has_writable {
                continue;
            }
            if let Some(view) = tool_view_from_spec(&spec)? {
                views.push(view);
            }
        }
    }
    // Redis 工具：只有存在 Redis 授权时才列出。
    if has_redis {
        for spec in standard_redis_tool_specs()? {
            if spec.risk == fluxdb_core::ToolRisk::Write && !has_writable {
                continue;
            }
            if let Some(view) = tool_view_from_spec(&spec)? {
                views.push(view);
            }
        }
    }
    // 公共工具：connections.list / result.read_page / operation.status / operation.cancel。
    views.push(connections_list_view()?);
    if has_relational {
        // result.read_page 与列出的查询工具配套（结果句柄只在服务内产生）。
        if let Some(view) = tool_view_from_internal("result.read_page", &crate::result_read_page_spec()?)? {
            views.push(view);
        }
    }
    views.push(operation_status_view()?);
    views.push(operation_cancel_view()?);

    // 最后按 allowed_tool_ids 收窄：空表示一个都不开放。
    let allowed: BTreeSet<&str> = profile.allowed_tool_ids.iter().map(String::as_str).collect();
    views.retain(|view| allowed.contains(view.name.as_str()));
    // 写工具在 disabled 模式下不列出（即使列表里写了也不公开）。
    if profile.write_mode == fluxdb_storage::ai_platform::mcp_server::WriteMode::Disabled {
        views.retain(|view| view.risk != fluxdb_core::ToolRisk::Write);
    }
    Ok(views)
}

/// 判断一条授权属于哪种资源（按对象规则形状，不按连接类型猜测）。
fn classify_grant(grant: &fluxdb_storage::ai_platform::mcp_server::ResourceGrantView) -> Option<McpResourceKind> {
    let value: serde_json::Value = serde_json::from_str(&grant.object_policy_json).ok()?;
    let object = value.as_object()?;
    let has = |key: &str| object.get(key).is_some();
    if has("all_keys") || has("key_prefixes") {
        Some(McpResourceKind::Redis)
    } else if has("all_objects") || has("table_allowlist") {
        Some(McpResourceKind::Relational)
    } else {
        None
    }
}

fn tool_view_from_spec(spec: &crate::ToolSpec) -> fluxdb_core::Result<Option<McpToolView>> {
    let Some(name) = mcp_tool_name(&spec.id) else {
        // 像 db.describe_object 之外的内部工具（artifact.*/knowledge.* 等）不对外暴露。
        return Ok(None);
    };
    Ok(Some(McpToolView {
        name: name.to_string(),
        internal_id: Some(spec.id.clone()),
        version: spec.version,
        description: spec.description.clone(),
        input_schema: spec.input_schema.clone(),
        risk: spec.risk,
    }))
}

fn tool_view_from_internal(
    internal_id: &str,
    spec: &crate::ToolSpec,
) -> fluxdb_core::Result<Option<McpToolView>> {
    let Some(name) = mcp_tool_name(internal_id) else {
        return Ok(None);
    };
    Ok(Some(McpToolView {
        name: name.to_string(),
        internal_id: Some(internal_id.to_string()),
        version: spec.version,
        description: spec.description.clone(),
        input_schema: spec.input_schema.clone(),
        risk: spec.risk,
    }))
}

/// `fluxdb_connections_list` 的输入 schema：无参数。
pub fn connections_list_view() -> fluxdb_core::Result<McpToolView> {
    Ok(McpToolView {
        name: "fluxdb_connections_list".into(),
        internal_id: None,
        version: 1,
        description: "列出当前 Profile 已授权的连接资源与作用域（不返回 endpoint 或凭据）".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }),
        risk: fluxdb_core::ToolRisk::Read,
    })
}

/// `fluxdb_operation_status` 的输入 schema。
pub fn operation_status_view() -> fluxdb_core::Result<McpToolView> {
    Ok(McpToolView {
        name: "fluxdb_operation_status".into(),
        internal_id: None,
        version: 1,
        description: "查询本 caller 有权查看的 request 状态与已知结果引用".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["request_id"],
            "properties": {
                "request_id": { "type": "string", "minLength": 1, "maxLength": 128 }
            }
        }),
        risk: fluxdb_core::ToolRisk::Read,
    })
}

/// `fluxdb_operation_cancel` 的输入 schema。
pub fn operation_cancel_view() -> fluxdb_core::Result<McpToolView> {
    Ok(McpToolView {
        name: "fluxdb_operation_cancel".into(),
        internal_id: None,
        version: 1,
        description: "取消 caller 自己的进行中请求（不能取消他人请求）".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["request_id"],
            "properties": {
                "request_id": { "type": "string", "minLength": 1, "maxLength": 128 }
            }
        }),
        risk: fluxdb_core::ToolRisk::Read,
    })
}

/// 一次外部调用的授权范围：函数/资源只作用于已授权句柄。
///
/// 模型提交的 `target` 必须命中某条已授权资源；未命中返回 `STALE_RESOURCE` 或
/// `RESOURCE_UNAVAILABLE`，绝不静默换绑到别的库/连接。
pub struct McpGrantResolver {
    /// (target 字符串, 句柄)；只有已授权资源才在列表里。
    handles: Vec<McpResourceHandle>,
}

impl McpGrantResolver {
    pub fn new(handles: Vec<McpResourceHandle>) -> Self {
        Self { handles }
    }

    pub fn handles(&self) -> &[McpResourceHandle] {
        &self.handles
    }

    /// 解析模型提交的 target：只接受已授权句柄的原字符串。
    ///
    /// 不做"模糊匹配"或"默认取第一个"——那样等于替用户扩大范围。
    pub fn resolve(&self, target: &str) -> std::result::Result<&McpResourceHandle, McpCallError> {
        self.handles
            .iter()
            .find(|handle| handle.target == target)
            .ok_or_else(|| {
                McpCallError::new(
                    "STALE_RESOURCE",
                    format!("未授权或已失效的资源句柄: {target}"),
                    false,
                )
            })
    }
}

/// 受控调用错误（映射到 MCP `isError=true` 的 tool result）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCallError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl McpCallError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
}

/// 一次调用产生的强制审计事实（供宿主落 `ai_tool_audits`，绑定 mcp_request_id）。
///
/// 服务本身不持有存储连接，因此把审计事实作为结果的一部分返回；宿主据此持久化。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAuditFact {
    pub tool_id: String,
    pub tool_version: u32,
    pub plan_hash: String,
    /// `allow` / `deny` / `require_approval`。
    pub decision: &'static str,
    pub risk: String,
    /// 命中的规则/原因（当前内核规则集为固定标签）。
    pub reason_codes: Vec<String>,
}

/// 一次外部调用的结果投影。
#[derive(Clone, Debug)]
pub struct McpCallOutcome {
    pub ok: bool,
    /// 结构化结果（object 顶层）；失败时为错误结构。
    pub structured: serde_json::Value,
    /// 可读 text JSON 投影（客户端不支持结构字段时的等价投影）。
    pub text: String,
    pub error: Option<McpCallError>,
    /// 本次调用的强制审计事实；服务内工具（无内核审计）为 `None`。
    pub audit: Option<McpAuditFact>,
}

impl McpCallOutcome {
    fn success(structured: serde_json::Value) -> Self {
        let text = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".into());
        Self {
            ok: true,
            structured,
            text,
            error: None,
            audit: None,
        }
    }

    /// 公开的失败构造：供协议层在工具线程异常时给出受控错误。
    pub fn failure_of(error: McpCallError) -> Self {
        Self::failure(error)
    }

    /// 附加强制审计事实（宿主持久化用）。
    fn with_audit(mut self, audit: McpAuditFact) -> Self {
        self.audit = Some(audit);
        self
    }

    fn failure(error: McpCallError) -> Self {
        let structured = serde_json::json!({
            "ok": false,
            "code": error.code,
            "message": error.message,
            "retryable": error.retryable,
        });
        let text = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".into());
        Self {
            ok: false,
            structured,
            text,
            error: Some(error),
            audit: None,
        }
    }
}

/// 宿主提供的 request 仓储：实现 `fluxdb_operation_status` / `fluxdb_operation_cancel`。
///
/// 执行服务本身不持有存储连接（保持可测试、无 IO），由宿主（stdio 二进制）注入。
/// 未注入时这两个工具返回 `operation_unsupported`，不静默假装成功。
pub trait McpRequestHost: Send + Sync {
    fn request_status(
        &self,
        identity: &McpCallIdentity,
        request_id: &str,
    ) -> std::result::Result<serde_json::Value, McpCallError>;
    fn cancel_request(
        &self,
        identity: &McpCallIdentity,
        request_id: &str,
    ) -> std::result::Result<serde_json::Value, McpCallError>;
}

/// 共享执行服务的输入：已授权的资源句柄、Profile 运行视图与结果仓库。
pub struct McpExecutionService {
    resolver: Arc<MapSqlResolver>,
    /// 已授权资源句柄（按 target 精确解析）。
    pub grants: McpGrantResolver,
    pub profile: fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
    /// 本 session 的结果仓库：ResultRef 只在本 session 内可读（跨 session 取不到）。
    pub results: Arc<Mutex<ResultStore>>,
    audit: Arc<dyn crate::SecurityAuditHook>,
    host: Option<Arc<dyn McpRequestHost>>,
    /// 原始授权视图（受限写需要 write_policy_json）。
    grant_views: Vec<fluxdb_storage::ai_platform::mcp_server::ResourceGrantView>,
    /// 本次调用的 SQL 文本（由 `call` 在执行前写入；受限写核对用）。
    ///
    /// 单次调用内使用；服务实例可被并发调用时应改为按调用传递，当前由协议层
    /// 一次只处理一个 request（stdio 顺序），故用互斥保护。
    call_sql: std::sync::Mutex<Option<String>>,
}

impl McpExecutionService {
    /// 装配服务：只需非敏感连接配置与已授权资源；凭据解密发生在连接器层。
    ///
    /// `redis_scopes` 把每条 Redis 授权的 key 范围注入 resolver，保证工具链
    /// 按 prefix 收窄（未配置 prefix 的授权不会退化成"整库可读"）。
    pub fn new(
        connections: Vec<fluxdb_core::ConnectionConfig>,
        profile: fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
        grants: Vec<fluxdb_storage::ai_platform::mcp_server::ResourceGrantView>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        let mut resolver = MapSqlResolver::new(connections.iter().cloned());
        for grant in grants.iter().filter(|g| g.enabled && g.allow_read) {
            let config = connections
                .iter()
                .find(|c| c.id.0 == grant.connection_id as u64);
            if resource_kind_for_config(config) == McpResourceKind::Redis {
                match redis_scope_from_grant(grant) {
                    Ok(scope) => {
                        resolver =
                            resolver.with_redis_key_scope(crate::ConnectionId(grant.connection_id as u64), scope);
                    }
                    Err(error) => {
                        tracing::warn!(grant_id = %grant.id, %error, "Redis 授权范围解析失败，按最小权限处理");
                    }
                }
            }
        }
        let handles = grants
            .iter()
            .filter(|g| g.enabled)
            .map(|g| {
                let config = connections
                    .iter()
                    .find(|c| c.id.0 == g.connection_id as u64);
                resource_handle(g, config)
            })
            .collect();
        let grant_views = grants;
        Ok(Self {
            resolver: Arc::new(resolver),
            grants: McpGrantResolver::new(handles),
            profile,
            results,
            audit: Arc::new(KernelAuditHook),
            host: None,
            grant_views,
            call_sql: std::sync::Mutex::new(None),
        })
    }

    /// 注入宿主 request 仓储（`operation.status`/`operation.cancel` 用）。
    pub fn with_request_host(mut self, host: Arc<dyn McpRequestHost>) -> Self {
        self.host = Some(host);
        self
    }

    /// 列出已授权资源（`fluxdb_connections_list` 的服务实现）。
    pub fn list_resources(&self) -> serde_json::Value {
        let resources = self
            .grants
            .handles()
            .iter()
            .map(|handle| {
                serde_json::json!({
                    "target": handle.target,
                    "label": handle.label,
                    "kind": match handle.kind {
                        McpResourceKind::Relational => "relational",
                        McpResourceKind::Redis => "redis",
                    },
                    "database": handle.database,
                    "schema": handle.schema,
                    "read_only": handle.read_only,
                    "revision": handle.grant_revision,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "ok": true, "resources": resources })
    }
}

/// 一次调用的身份：session/request/seq（用于审计归属与结果绑定）。
#[derive(Clone, Debug)]
pub struct McpCallIdentity {
    pub session_id: String,
    pub request_id: String,
}

impl McpExecutionService {
    /// 执行一次外部工具调用：schema 校验 → 强制审计 → 授权 → 执行 → 投影。
    ///
    /// 与内部 `TurnRunner` 使用同一内核：强制审计 fail closed、写操作按 Profile
    /// write_mode 判定、`tools/list` 之外再检查一次授权（隐藏的写工具直接调用也被拒）。
    pub fn call(
        &self,
        identity: &McpCallIdentity,
        mcp_tool_name: &str,
        arguments_json: &str,
        cancel: &CancelFlag,
    ) -> McpCallOutcome {
        // 1) 服务内实现的工具优先处理（connections.list / operation.*）。
        match mcp_tool_name {
            "fluxdb_connections_list" => {
                return match self.prepare_no_args(arguments_json) {
                    Ok(()) => McpCallOutcome::success(self.list_resources()),
                    Err(error) => McpCallOutcome::failure(error),
                };
            }
            "fluxdb_operation_status" | "fluxdb_operation_cancel" => {
                return self.handle_operation(identity, mcp_tool_name, arguments_json);
            }
            _ => {}
        }

        // 2) 映射到内部工具。
        let Some(internal_id) = mcp_tool_internal_id(mcp_tool_name) else {
            return McpCallOutcome::failure(McpCallError::new(
                "unknown_tool",
                format!("未知工具: {mcp_tool_name}"),
                false,
            ));
        };
        if !self.profile.enabled {
            return McpCallOutcome::failure(McpCallError::new(
                "profile_disabled",
                "该 Server Profile 已禁用",
                false,
            ));
        }
        // 3) 可见性不是授权：即使 tools/list 隐藏了写工具，直接调用也要拒绝。
        if !self
            .profile
            .allowed_tool_ids
            .iter()
            .any(|id| id == mcp_tool_name)
        {
            return McpCallOutcome::failure(McpCallError::new(
                "tool_not_allowed",
                format!("Profile 未允许工具 {mcp_tool_name}"),
                false,
            ));
        }

        // 4) 装配对应能力的工具目录，并把 target 精确绑定到已授权句柄。
        let catalog = match self.build_catalog(internal_id) {
            Ok(catalog) => catalog,
            Err(error) => return McpCallOutcome::failure(error),
        };
        let Some(spec) = catalog.describe(internal_id).cloned() else {
            return McpCallOutcome::failure(McpCallError::new(
                "tool_unavailable",
                format!("工具 {mcp_tool_name} 在当前授权下不可用"),
                false,
            ));
        };
        // 写工具必须非 disabled（scoped 允许；desktop_approval 未实现则拒绝）。
        if spec.risk == fluxdb_core::ToolRisk::Write {
            match self.profile.write_mode {
                fluxdb_storage::ai_platform::mcp_server::WriteMode::Disabled => {
                    return McpCallOutcome::failure(McpCallError::new(
                        "write_disabled",
                        "该 Profile 未开启写权限",
                        false,
                    ));
                }
                fluxdb_storage::ai_platform::mcp_server::WriteMode::DesktopApproval => {
                    // 首版未实现桌面审批桥：明确 Unsupported，不假装等待 UI。
                    return McpCallOutcome::failure(McpCallError::new(
                        "approval_unsupported",
                        "desktop_approval 写模式需要桌面审批桥，当前构建不支持",
                        false,
                    ));
                }
                fluxdb_storage::ai_platform::mcp_server::WriteMode::Scoped => {}
            }
        }

        // 5) schema 校验（不做类型强转/猜参数）。
        let prepared = match catalog.prepare(internal_id, arguments_json) {
            Ok(prepared) => prepared,
            Err(error) => return McpCallOutcome::failure(tool_error_to_call(error)),
        };
        // 外部 MCP 调用不绑定内部插件 Hook：安全约束走强制审计（下方），
        // 不做参数补丁，避免插件借外部通道改写执行目标。
        // 记录本次 SQL 文本，供受限写核对（读工具不涉及）。
        if let Ok(mut slot) = self.call_sql.lock() {
            *slot = prepared
                .as_value()
                .get("sql")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        // 5c) 受限写：scoped 模式下按结构化写规则逐条核对，命中才允许执行；
        //     desktop_approval/disabled 在上方已拒绝，这里只处理预授权路径。
        let scoped_write_ok = if spec.risk == fluxdb_core::ToolRisk::Write {
            match self.scoped_write_check(internal_id, target_of(&prepared)) {
                Ok(()) => true,
                Err(error) => return McpCallOutcome::failure(error),
            }
        } else {
            false
        };
        // 6) 校验 target 是已授权资源（读/写工具都带 target）。
        let target_value = extract_target(&prepared);
        let target_str = target_value.as_str().unwrap_or_default();
        if !target_str.is_empty() {
            if let Err(error) = self.grants.resolve(target_str) {
                return McpCallOutcome::failure(error);
            }
        }
        let target_json = target_str.to_string();

        // 7) 强制审计（fail closed）+ 内核授权。
        let hash = crate::plan_hash(
            &spec.id,
            spec.version,
            &target_json,
            &prepared.to_json_string(),
            None,
        );
        let audit = audit_or_fail_closed(
            self.audit.as_ref(),
            &crate::AuditInput {
                tool_id: spec.id.clone(),
                tool_version: spec.version,
                plan_hash: hash.clone(),
                target_json: target_json.clone(),
                normalized_arguments_json: prepared.to_json_string(),
                ruleset_version: "kernel-v1".to_string(),
            },
        );
        let policy = self.kernel_policy(&target_json, scoped_write_ok);
        // 受限写（scoped）已把"用户对具体资源的预授权"落实到 ScopedWritePolicy：命中即视为
        // 已获批准（设计 §28.7 scoped 语义），不经内核的逐次审批（当前无审批桥）。强制审计
        // 仍然生效——审计 Deny 一律拒绝，不得被预授权覆盖。
        let decision = if spec.risk == fluxdb_core::ToolRisk::Write
            && scoped_write_ok
            && audit != AuditOutcome::Deny
            && policy.write_allowed
        {
            PermissionDecision::Allow { plan_hash: hash.clone() }
        } else {
            crate::authorize(&AuthorizeInput {
                tool_id: spec.id.clone(),
                tool_version: spec.version,
                risk: spec.risk,
                target_json: target_json.clone(),
                normalized_arguments_json: prepared.to_json_string(),
                plugin_digest: None,
                globally_disabled_tools: BTreeSet::new(),
                agent_allowed_tools: Vec::new(),
                policy,
                audit: Some(audit),
            })
        };
        let audit_fact = |decision: &'static str, reasons: Vec<String>| McpAuditFact {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            plan_hash: hash.clone(),
            decision,
            risk: match spec.risk {
                fluxdb_core::ToolRisk::Read => "read",
                fluxdb_core::ToolRisk::Write => "write",
                fluxdb_core::ToolRisk::Admin => "admin",
            }
            .to_string(),
            reason_codes: reasons,
        };
        match decision {
            PermissionDecision::Deny { code, reason, .. } => {
                return McpCallOutcome::failure(McpCallError::new(code, reason.clone(), false))
                    .with_audit(audit_fact("deny", vec![code.to_string()]));
            }
            PermissionDecision::RequireApproval { .. } => {
                // 没有桌面审批桥时，写操作明确需要审批；此处不静默放行。
                return McpCallOutcome::failure(McpCallError::new(
                    "approval_required",
                    "该调用需要用户审批；当前无可用审批桥",
                    false,
                ))
                .with_audit(audit_fact("require_approval", vec!["no_desktop_approval_bridge".into()]));
            }
            PermissionDecision::Allow { .. } => {}
        }

        // 8) 执行并投影（与内部同链）。
        let Some(tool) = catalog.executable(internal_id) else {
            return McpCallOutcome::failure(McpCallError::new(
                "tool_not_executable",
                format!("工具 {mcp_tool_name} 没有可执行实现"),
                false,
            ));
        };
        let ctx = fluxdb_core::ToolExecutionContext {
            session_id: identity.session_id.clone(),
            turn_id: identity.request_id.clone(),
            iteration: 1,
            call_id: identity.request_id.clone(),
            cancel: cancel.clone(),
            progress: None,
        };
        let receipt = match crate::block_on_ready(tool.execute(ctx, prepared.to_json_string())) {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(error)) => {
                return McpCallOutcome::failure(McpCallError::new(
                    "execution_error",
                    error.message,
                    false,
                ));
            }
            Err(error) => {
                return McpCallOutcome::failure(McpCallError::new(
                    "execution_error",
                    error.message,
                    false,
                ));
            }
        };
        // 9) 归属校验：回执必须对应当前调用，否则按未知处理，不自动重放。
        if receipt.call_id != identity.request_id || receipt.tool_id != internal_id {
            return McpCallOutcome::failure(McpCallError::new(
                "receipt_identity_mismatch",
                "工具回执归属与调用不符，结果不可确认",
                false,
            ));
        }
        let receipt_status = receipt.status;
        let receipt_json = receipt.result_json.clone();
        let projection = catalog
            .project(internal_id, &receipt, None)
            .map(|projection| projection.text)
            .unwrap_or_else(|error| {
                serde_json::json!({"error": error.code, "message": error.message}).to_string()
            });
        if receipt_status == fluxdb_core::ToolReceiptStatus::Succeeded {
            let structured = serde_json::from_str::<serde_json::Value>(&projection)
                .unwrap_or_else(|_| serde_json::json!({ "result": projection.clone() }));
            let envelope = serde_json::json!({
                "ok": true,
                "request_id": identity.request_id,
                "outcome": "succeeded",
                "data": structured,
            });
            McpCallOutcome::success(envelope).with_audit(audit_fact("allow", Vec::new()))
        } else {
            let parsed: serde_json::Value = serde_json::from_str(&receipt_json)
                .unwrap_or_else(|_| serde_json::json!({ "raw": receipt_json }));
            let code = receipt
                .error_code
                .clone()
                .or_else(|| parsed.get("code").and_then(|c| c.as_str()).map(str::to_string))
                .unwrap_or_else(|| "execution_failed".into());
            McpCallOutcome::failure(McpCallError::new(
                code,
                parsed
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("工具执行失败")
                    .to_string(),
                false,
            ))
        }
    }

    /// 该调用所属 request 的终态（供宿主写入 `ai_mcp_server_requests`）。
    pub fn terminal_status(outcome: &McpCallOutcome) -> fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus {
        if outcome.ok {
            fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Succeeded
        } else {
            match outcome.error.as_ref().map(|error| error.code.as_str()) {
                Some("write_disabled") | Some("tool_not_allowed") | Some("unknown_tool") => {
                    fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Denied
                }
                Some("cancelled") => fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Cancelled,
                _ => fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Failed,
            }
        }
    }

    fn prepare_no_args(&self, arguments_json: &str) -> std::result::Result<(), McpCallError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json).map_err(|_| {
            McpCallError::new("invalid_arguments", "工具入参不是合法 JSON", true)
        })?;
        if !value.is_object() {
            return Err(McpCallError::new(
                "invalid_arguments",
                "工具入参必须是对象",
                true,
            ));
        }
        Ok(())
    }

    fn kernel_policy(&self, target_json: &str, scoped_write_ok: bool) -> KernelPolicy {
        // 写权限只在授权允许写、连接非只读、Profile 非 disabled，且本轮 scoped 写规则通过时开启。
        let handle = self.grants.handles().iter().find(|h| h.target == target_json);
        let writable = handle.is_some_and(|handle| {
            !handle.read_only
                && self.profile.write_mode
                    != fluxdb_storage::ai_platform::mcp_server::WriteMode::Disabled
        });
        KernelPolicy {
            read_allowed: true,
            write_allowed: writable && scoped_write_ok,
            admin_allowed: false,
        }
    }

    /// scoped 写：从对应资源的写规则构造策略并核对 SQL。
    fn scoped_write_check(
        &self,
        internal_id: &str,
        target: Option<&str>,
    ) -> std::result::Result<(), McpCallError> {
        if internal_id != "db.execute" {
            // 首版仅放行关系型受限写；Redis 写仍走内核审批（当前无桥 → 拒绝）。
            return Err(McpCallError::new(
                "write_not_preauthorized",
                "该写工具暂无预授权路径，需桌面审批（当前不可用）",
                false,
            ));
        }
        let target = target.ok_or_else(|| {
            McpCallError::new("invalid_arguments", "写工具缺少 target", true)
        })?;
        let handle = self.grants.resolve(target)?;
        let grant = self
            .grant_views
            .iter()
            .find(|grant| grant.id == handle.grant_id)
            .ok_or_else(|| McpCallError::new("STALE_RESOURCE", "授权已失效", false))?;
        let policy = ScopedWritePolicy::parse(&grant.write_policy_json)?;
        let sql = self
            .call_sql
            .lock()
            .ok()
            .and_then(|sql| sql.clone())
            .unwrap_or_default();
        let dialect = self
            .resolver
            .connection_config(handle.connection_id)
            .and_then(|config| dialect_for_kind(config.kind))
            .ok_or_else(|| {
                McpCallError::new("unsupported_database", "该连接类型不支持 SQL 写工具", false)
            })?;
        policy.authorize_sql(&sql, dialect)
    }

    /// `operation.status`/`operation.cancel`：需要宿主 request 仓储。
    fn handle_operation(
        &self,
        identity: &McpCallIdentity,
        mcp_tool_name: &str,
        arguments_json: &str,
    ) -> McpCallOutcome {
        let value: serde_json::Value = match serde_json::from_str(arguments_json) {
            Ok(value) => value,
            Err(_) => {
                return McpCallOutcome::failure(McpCallError::new(
                    "invalid_arguments",
                    "工具入参不是合法 JSON",
                    true,
                ));
            }
        };
        let Some(request_id) = value.get("request_id").and_then(|v| v.as_str()) else {
            return McpCallOutcome::failure(McpCallError::new(
                "invalid_arguments",
                "缺少 request_id",
                true,
            ));
        };
        let Some(host) = self.host.as_ref() else {
            return McpCallOutcome::failure(McpCallError::new(
                "operation_unsupported",
                "宿主未提供 request 仓储，操作状态/取消不可用",
                false,
            ));
        };
        let result = if mcp_tool_name == "fluxdb_operation_status" {
            host.request_status(identity, request_id)
        } else {
            host.cancel_request(identity, request_id)
        };
        match result {
            Ok(structured) => McpCallOutcome::success(structured),
            Err(error) => McpCallOutcome::failure(error),
        }
    }

    fn build_catalog(
        &self,
        internal_id: &str,
    ) -> std::result::Result<ToolCatalog, McpCallError> {
        // 复用标准目录装配（读写工具都注册，授权由下方 authorize/write_mode 逐次判定），
        // 再收窄到本次调用的单个工具，避免把未授权工具的实现带进来。
        let full = if internal_id.starts_with("redis.") {
            crate::standard_redis_catalog(Arc::clone(&self.resolver), true)
                .map_err(|error| McpCallError::new("catalog_error", error.message, false))?
        } else {
            crate::standard_sql_catalog_with_results(
                Arc::clone(&self.resolver),
                Arc::clone(&self.results),
                true,
            )
            .map_err(|error| McpCallError::new("catalog_error", error.message, false))?
        };
        let disabled: BTreeSet<String> = full
            .specs()
            .iter()
            .map(|spec| spec.id.clone())
            .filter(|id| id != internal_id)
            .collect();
        let mut catalog = full;
        catalog.without_tools(&disabled);
        Ok(catalog)
    }
}

/// 从已校验参数中取 `target`（读/写工具都有；服务内工具无）。
fn extract_target(prepared: &crate::ValidatedArguments) -> serde_json::Value {
    prepared
        .as_value()
        .get("target")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// 从已校验参数取 `target` 字符串。
fn target_of(prepared: &crate::ValidatedArguments) -> Option<&str> {
    prepared.as_value().get("target").and_then(|v| v.as_str())
}

/// 连接类型 → SQL 方言。
fn dialect_for_kind(kind: fluxdb_core::DatabaseKind) -> Option<crate::SqlDialectKind> {
    match kind {
        fluxdb_core::DatabaseKind::MySql | fluxdb_core::DatabaseKind::TiDb => {
            Some(crate::SqlDialectKind::MySql)
        }
        fluxdb_core::DatabaseKind::Postgres => Some(crate::SqlDialectKind::Postgres),
        fluxdb_core::DatabaseKind::Sqlite => Some(crate::SqlDialectKind::Sqlite),
        _ => None,
    }
}

fn tool_error_to_call(error: ToolError) -> McpCallError {
    McpCallError::new(error.code, error.message, error.retryable)
}

/// 受限写规则（Profile write_mode=scoped 时启用）：把"允许写多少"变成可判定的结构约束。
///
/// 只从**结构化授权**（`write_policy_json`）派生；`intent` 文字不构成权限（设计 §28.6.1）。
/// 无法证明范围时一律拒绝，不做"大概安全"的放行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedWritePolicy {
    /// 允许的操作：仅 `update`（首版限定，避免 delete/insert 的范围难证）。
    pub allowed_operations: Vec<String>,
    /// 允许改写的表（小写比对）；为空表示不放行任何表。
    pub allowed_objects: Vec<String>,
    /// 是否强制 WHERE 子句（默认 true）。
    pub require_where: bool,
    /// 允许的受影响行数上限。
    pub max_affected_rows: i64,
    /// 是否允许 DDL（默认 false，始终拒绝）。
    pub allow_ddl: bool,
}

impl ScopedWritePolicy {
    /// 从授权 JSON 解析；缺字段按最保守默认（必须 WHERE、0 行、禁 DDL）。
    pub fn parse(raw: &str) -> std::result::Result<Self, McpCallError> {
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
            McpCallError::new("invalid_write_policy", "写规则不是合法 JSON", false)
        })?;
        let object = value.as_object().ok_or_else(|| {
            McpCallError::new("invalid_write_policy", "写规则必须是对象", false)
        })?;
        let list = |key: &str| -> Vec<String> {
            object
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(|s| s.to_ascii_lowercase())
                        .collect()
                })
                .unwrap_or_default()
        };
        let flag = |key: &str, default: bool| {
            object
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default)
        };
        Ok(Self {
            allowed_operations: list("allowed_operations"),
            allowed_objects: list("allowed_objects"),
            require_where: flag("require_where", true),
            max_affected_rows: object
                .get("max_affected_rows")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            allow_ddl: flag("allow_ddl", false),
        })
    }

    /// 判定一条 SQL 是否落在预授权范围内。
    ///
    /// 只做**结构判定**（语句种类、对象、是否有 WHERE、谓词结构）；真正的行数复核
    /// 由连接器读到真实 affected rows 后由调用方核对，不能只靠这里断言。
    pub fn authorize_sql(
        &self,
        sql: &str,
        dialect: crate::SqlDialectKind,
    ) -> std::result::Result<(), McpCallError> {
        use sqlparser::ast::{Statement, TableFactor};

        if self.allow_ddl {
            return Err(McpCallError::new(
                "ddl_not_allowed",
                "MCP 首版不允许 DDL，即使 write_policy 声明 allow_ddl",
                false,
            ));
        }
        if self.allowed_objects.is_empty() || self.max_affected_rows <= 0 {
            return Err(McpCallError::new(
                "write_not_scoped",
                "写规则未限定对象或行数上限，拒绝执行",
                false,
            ));
        }
        if !self.allowed_operations.iter().any(|op| op == "update") {
            return Err(McpCallError::new(
                "write_operation_not_allowed",
                "写规则未允许 update",
                false,
            ));
        }
        let statements =
            sqlparser::parser::Parser::parse_sql(dialect.parser_dialect().as_ref(), sql).map_err(
                |_| McpCallError::new("write_unverifiable", "SQL 无法解析，无法核对写范围", false),
            )?;
        if statements.len() != 1 {
            return Err(McpCallError::new(
                "write_unverifiable",
                "写工具只接受单条语句",
                false,
            ));
        }
        let Statement::Update(update) = &statements[0] else {
            return Err(McpCallError::new(
                "write_operation_not_allowed",
                "受限写首版仅允许单表 UPDATE",
                false,
            ));
        };
        let table = &update.table;
        let selection = update.selection.as_ref();
        // 只允许普通表；派生表/join 等无法静态核对，直接拒绝。
        let TableFactor::Table { name, .. } = &table.relation else {
            return Err(McpCallError::new(
                "write_unverifiable",
                "只允许对普通表执行受限写",
                false,
            ));
        };
        if !table.joins.is_empty() {
            return Err(McpCallError::new(
                "write_unverifiable",
                "受限写不允许 JOIN",
                false,
            ));
        }
        let table_name = name
            .0
            .last()
            .and_then(|part| match part {
                sqlparser::ast::ObjectNamePart::Identifier(ident) => Some(ident.value.clone()),
                _ => None,
            })
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !self.allowed_objects.iter().any(|obj| obj == &table_name) {
            return Err(McpCallError::new(
                "write_object_not_allowed",
                format!("写规则未允许表 {table_name}"),
                false,
            ));
        }
        match selection {
            None if self.require_where => Err(McpCallError::new(
                "write_requires_where",
                "写规则要求 WHERE 子句",
                false,
            )),
            None => Ok(()),
            Some(expr) => {
                if scoped_predicate_supported(expr) {
                    Ok(())
                } else {
                    Err(McpCallError::new(
                        "write_unverifiable",
                        "写谓词包含子查询或不支持的结构，无法核对范围",
                        false,
                    ))
                }
            }
        }
    }
}

/// 只接受不含子查询的简单谓词（AND/OR/比较/IN 字面量/BETWEEN/IS NULL）。
fn scoped_predicate_supported(expr: &sqlparser::ast::Expr) -> bool {
    use sqlparser::ast::Expr;
    match expr {
        Expr::BinaryOp { left, right, .. } => {
            scoped_predicate_supported(left) && scoped_predicate_supported(right)
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::IsNull(expr) | Expr::IsNotNull(expr) => {
            scoped_predicate_supported(expr)
        }
        Expr::Between { expr, low, high, .. } => {
            scoped_predicate_supported(expr)
                && scoped_predicate_supported(low)
                && scoped_predicate_supported(high)
        }
        Expr::InList { expr, list, .. } => {
            scoped_predicate_supported(expr) && list.iter().all(scoped_predicate_supported)
        }
        Expr::Value(_) | Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        // 其它一切（子查询、函数调用、CASE 等）保守拒绝。
        _ => false,
    }
}

// 对外 MCP Server 的 Claude 客户端配置导出（设计 §39.1，T060）。
//
// 导出**不含任何秘密**：只有绝对命令路径、`--server-profile` 与（可选）`--config-root`。
// 客户端 JSON 里不出现数据库密码、API Key、连接 URI 或任意 SQL。

/// 生成 Claude Code 的 `.mcp.json` 片段（不含秘密）。
///
/// `binary_path` 必须是三端绝对路径；`config_root` 非空时写入 `--config-root`，
/// 让客户端启动的 Server 与桌面端看到同一份配置（测试隔离时尤其重要）。
pub fn claude_mcp_config_json(
    server_name: &str,
    binary_path: &str,
    server_profile_id: &str,
    config_root: Option<&str>,
) -> fluxdb_core::Result<String> {
    if server_name.trim().is_empty() || binary_path.trim().is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "server 名称与二进制路径不能为空",
        ));
    }
    if !std::path::Path::new(binary_path).is_absolute() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "MCP 客户端配置必须使用绝对二进制路径",
        ));
    }
    if server_profile_id.trim().is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "Server Profile ID 不能为空",
        ));
    }
    let mut args = vec![
        "--server-profile".to_string(),
        server_profile_id.to_string(),
    ];
    if let Some(root) = config_root.filter(|root| !root.trim().is_empty()) {
        if !std::path::Path::new(root).is_absolute() {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "config-root 必须是绝对路径",
            ));
        }
        args.push("--config-root".to_string());
        args.push(root.to_string());
    }
    let config = serde_json::json!({
        "mcpServers": {
            server_name: {
                "command": binary_path,
                "args": args,
                // 默认不注入任何环境变量；凭据由 Server 从既有 AES 连接凭据临时读取。
                "env": {}
            }
        }
    });
    serde_json::to_string_pretty(&config)
        .map_err(|e| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, format!("导出配置失败: {e}")))
}
