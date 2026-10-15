/// 审批方式而非可比较的风险等级；连接覆盖可以比默认更严格或更宽松。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPolicy {
    ReadOnly,
    #[default]
    WriteApproval,
    AiApproval,
    Automatic,
}

impl ExecutionPolicy {
    pub const ALL: [Self; 4] = [
        Self::ReadOnly,
        Self::WriteApproval,
        Self::AiApproval,
        Self::Automatic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::WriteApproval => "写审批",
            Self::AiApproval => "帮我审批",
            Self::Automatic => "自动执行",
        }
    }
}

/// 对外 MCP 数据源的写入策略：只读或允许读写（不再逐次审批）。
///
/// 这不是风险等级，而是数据源维度的写入开关：只读时写工具不列出也拒绝；
/// 允许读写时仍受连接只读标记、内核权限与强制审计约束。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpWritePolicy {
    #[default]
    ReadOnly,
    ReadWrite,
}

impl McpWritePolicy {
    pub const ALL: [Self; 2] = [Self::ReadOnly, Self::ReadWrite];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "只读",
            Self::ReadWrite => "允许读写",
        }
    }

    /// 是否允许写入；只读策略下写工具一律拒绝。
    pub fn allows_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// 执行策略设置：内部工具默认（含审批方式）+ 对外 MCP 全局默认。
///
/// MCP 数据源自身的“默认”取值表示跟随这里的 `mcp_default`；数据源覆盖存在
/// `ai_mcp_server_datasources` 行上（见 storage），避免两处各存一份事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionPolicySettings {
    pub tools_default: ExecutionPolicy,
    pub mcp_default: McpWritePolicy,
}

impl Default for ExecutionPolicySettings {
    fn default() -> Self {
        Self {
            tools_default: ExecutionPolicy::WriteApproval,
            mcp_default: McpWritePolicy::ReadOnly,
        }
    }
}
