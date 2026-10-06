// 目标授权与风险策略（设计 §25、§29，T020）。
//
// 内核不变量：
// - **deny 优先**：内核拒绝不能被插件 Allow 覆盖；`RequireApproval` 优先于 `Allow`。
// - **批准绑定 plan_hash**：参数/目标/插件摘要任何变化都会换 hash，旧批准自然失效。
// - **fail closed**：强制安全审计无法执行/无法落盘时，一律拒绝执行。
// - `denied` / `unknown` 不是可重试错误（不能诱导模型换工具绕过）。

/// 审批范围（写进审批行，供 UI 与执行前复核）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalScope {
    pub session_id: String,
    pub turn_id: String,
    pub tool_id: String,
    pub tool_version: u32,
    pub target_json: String,
    /// 计划影响的资源摘要（连接/库/表/Key 前缀等）。
    pub resource_json: String,
}

impl ApprovalScope {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "tool_id": self.tool_id,
            "tool_version": self.tool_version,
            "target": self.target_json,
            "resource": self.resource_json,
        })
        .to_string()
    }
}

/// 计算最终 plan hash：工具 + 版本 + 目标 + **规范化参数** + 插件摘要。
///
/// 任何一项变化都会改变 hash，从而让旧审计/审批失效（设计 §25.1）。
pub fn plan_hash(
    tool_id: &str,
    tool_version: u32,
    target_json: &str,
    normalized_arguments_json: &str,
    plugin_digest: Option<&str>,
) -> String {
    // 规范化必须在哈希内部完成：serde_json 的 `Map` 顺序会随 `preserve_order` 特性
    // 在不同编译单元/依赖图里变化，同一语义参数会算出不同 hash（跨进程复核会误判失效）。
    let canonical_target = canonical_json_str(target_json);
    let canonical_arguments = canonical_json_str(normalized_arguments_json);
    let material = format!(
        "v1\u{1f}{tool_id}\u{1f}{tool_version}\u{1f}{canonical_target}\u{1f}{canonical_arguments}\u{1f}{}",
        plugin_digest.unwrap_or("-")
    );
    // FNV-1a 64：稳定、无额外依赖；这里只用于"变化即失效"，不作为安全哈希。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in material.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 内核权限策略（连接与 Agent 授权的交集结果，不含用户偏好）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelPolicy {
    /// 目标连接/对象是否允许读取。
    pub read_allowed: bool,
    /// 目标是否允许写入（连接只读时恒为 false）。
    pub write_allowed: bool,
    /// 是否允许管理类操作（默认 false）。
    pub admin_allowed: bool,
}

impl Default for KernelPolicy {
    fn default() -> Self {
        // 保守默认：可读、不可写、不可管理。
        Self {
            read_allowed: true,
            write_allowed: false,
            admin_allowed: false,
        }
    }
}

/// 授权输入。
#[derive(Clone, Debug)]
pub struct AuthorizeInput {
    pub tool_id: String,
    pub tool_version: u32,
    pub risk: fluxdb_core::ToolRisk,
    pub target_json: String,
    pub normalized_arguments_json: String,
    pub plugin_digest: Option<String>,
    /// Agent 允许的工具白名单；空表示不限制。
    pub agent_allowed_tools: Vec<String>,
    pub policy: KernelPolicy,
    /// 强制安全审计的判定（`None` = 未审计；调用方必须先审计，见 `audit_or_fail_closed`）。
    pub audit: Option<AuditOutcome>,
}

/// 审计结果（内核最严格结果优先）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    Allow,
    RequireApproval,
    Deny,
}

/// 授权结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow {
        plan_hash: String,
    },
    RequireApproval {
        plan_hash: String,
        scope: ApprovalScope,
    },
    Deny {
        code: &'static str,
        reason: String,
        /// 权限类拒绝不是可重试错误（不能诱导换工具绕过）。
        retryable: bool,
    },
}

impl PermissionDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, PermissionDecision::Allow { .. })
    }

    pub fn plan_hash(&self) -> Option<&str> {
        match self {
            PermissionDecision::Allow { plan_hash }
            | PermissionDecision::RequireApproval { plan_hash, .. } => Some(plan_hash),
            PermissionDecision::Deny { .. } => None,
        }
    }
}

/// 强制安全审计：审计无法执行时 **fail closed**（返回 Deny）。
pub fn audit_or_fail_closed(hook: &dyn SecurityAuditHook, input: &AuditInput) -> AuditOutcome {
    match hook.audit(input) {
        Ok(outcome) => outcome,
        // 审计不可用 = 拒绝，绝不"先执行再说"。
        Err(_) => AuditOutcome::Deny,
    }
}

/// 审计输入（不含 API Key / 加密载荷 / 数据样本）。
#[derive(Clone, Debug)]
pub struct AuditInput {
    pub tool_id: String,
    pub tool_version: u32,
    pub plan_hash: String,
    pub target_json: String,
    pub normalized_arguments_json: String,
    pub ruleset_version: String,
}

/// 强制安全审计 Hook（内核注册，插件不能卸载/跳过）。
pub trait SecurityAuditHook: Send + Sync {
    fn audit(&self, input: &AuditInput) -> fluxdb_core::Result<AuditOutcome>;
}

/// 内核授权：把工具风险、Agent 白名单、连接权限与审计结果求交集。
///
/// 顺序（严格度递增）：Agent 白名单 → 连接/内核权限 → 风险 → 审计。
/// 任一 Deny 立即返回 Deny，插件 Allow 无法覆盖。
pub fn authorize(input: &AuthorizeInput) -> PermissionDecision {
    let hash = plan_hash(
        &input.tool_id,
        input.tool_version,
        &input.target_json,
        &input.normalized_arguments_json,
        input.plugin_digest.as_deref(),
    );
    let deny = |code: &'static str, reason: String| PermissionDecision::Deny {
        code,
        reason,
        retryable: false,
    };

    // 1) Agent 白名单（空 = 不限制）。
    if !input.agent_allowed_tools.is_empty()
        && !input
            .agent_allowed_tools
            .iter()
            .any(|id| id == &input.tool_id)
    {
        return deny(
            "tool_not_allowed",
            format!("Agent 未允许工具 {}", input.tool_id),
        );
    }

    // 2) 连接/内核权限：与风险取交集。
    match input.risk {
        fluxdb_core::ToolRisk::Read => {
            if !input.policy.read_allowed {
                return deny("read_not_allowed", "目标不允许读取".to_string());
            }
        }
        fluxdb_core::ToolRisk::Write => {
            if !input.policy.write_allowed {
                return deny("write_not_allowed", "目标不允许写入".to_string());
            }
        }
        fluxdb_core::ToolRisk::Admin => {
            if !input.policy.admin_allowed {
                return deny("admin_not_allowed", "管理类操作默认拒绝".to_string());
            }
        }
    }

    // 3) 强制审计（未审计视为不可执行；fail closed）。
    let audit = match input.audit {
        Some(AuditOutcome::Deny) => {
            return deny("audit_denied", "安全审计拒绝该调用".to_string());
        }
        Some(AuditOutcome::RequireApproval) => AuditOutcome::RequireApproval,
        Some(AuditOutcome::Allow) => AuditOutcome::Allow,
        // 没有任何审计结论时按"需要审批"处理，而不是直接放行。
        None => AuditOutcome::RequireApproval,
    };

    // 4) 写操作一律需要审批（内核规则），读操作按审计结论。
    let needs_approval =
        audit == AuditOutcome::RequireApproval || input.risk != fluxdb_core::ToolRisk::Read;
    if needs_approval {
        return PermissionDecision::RequireApproval {
            plan_hash: hash,
            scope: ApprovalScope {
                session_id: String::new(),
                turn_id: String::new(),
                tool_id: input.tool_id.clone(),
                tool_version: input.tool_version,
                target_json: input.target_json.clone(),
                resource_json: input.target_json.clone(),
            },
        };
    }
    PermissionDecision::Allow { plan_hash: hash }
}

/// 已批准后复核：批准必须与**当前 plan_hash** 完全一致，且状态仍是 approved 且未过期。
pub fn approval_matches(
    approval_plan_hash: &str,
    current_plan_hash: &str,
    status: fluxdb_storage::ai_platform::audits::ApprovalStatus,
    expires_at_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    use fluxdb_storage::ai_platform::audits::ApprovalStatus;
    approval_plan_hash == current_plan_hash
        && status == ApprovalStatus::Approved
        && expires_at_ms.is_none_or(|expiry| expiry > now_ms)
}

/// 内置规则审计（内核自带规则集）。
///
/// 当前规则集只有"内核自身的风险 / Agent 白名单 / 连接权限"，这些由 `authorize` 强制执行；
/// 所以这里放行，不代表跳过审计——`authorize` 仍会对写操作强制审批。
/// 新增规则（命令黑名单、路径/前缀限制、跨库限制等）在此实现，且**不能被插件禁用**。
pub struct KernelAuditHook;

impl SecurityAuditHook for KernelAuditHook {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::Allow)
    }
}

/// 连接是否被标记为只读：SQLite 走 endpoint 标记，其它类型走显式 `options["read_only"]`。
pub fn connection_is_read_only(config: &fluxdb_core::ConnectionConfig) -> bool {
    matches!(
        config.endpoint,
        fluxdb_core::Endpoint::SqliteFile {
            read_only: true,
            ..
        }
    ) || config
        .options
        .get("read_only")
        .is_some_and(|value| value == "true")
}

/// 由**本轮固定目标连接**推导内核权限（设计 §25、§29）。
///
/// - 读：允许（真正的资源校验仍在 resolver/连接层，未固定连接时工具会因目标无效失败）；
/// - 写：只有目标连接未被标记只读才允许，且**仍需逐次审批**（`authorize` 对 Write 强制 RequireApproval）；
/// - 管理：默认拒绝。
///
/// 没有固定连接时写权限为 false：不能因为"没指定目标"就落到任意连接去写。
pub fn kernel_policy_for_connection(
    config: Option<&fluxdb_core::ConnectionConfig>,
) -> KernelPolicy {
    KernelPolicy {
        read_allowed: true,
        write_allowed: config.is_some_and(|config| !connection_is_read_only(config)),
        admin_allowed: false,
    }
}

/// 规范化 JSON：对象键递归按字典序排序，数组保持顺序；解析失败时原样返回。
///
/// `plan_hash` 的输入必须经过它，否则同一语义的 JSON 只因键顺序不同就会得到不同 hash。
pub fn canonical_json_str(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => canonical_json(&value),
        Err(_) => raw.to_string(),
    }
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(key.clone()),
                        canonical_json(&map[key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        serde_json::Value::Array(items) => {
            let body = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        other => other.to_string(),
    }
}
