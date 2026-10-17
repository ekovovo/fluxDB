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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// `default` 不是为旧数据兜底：`ai_runtime_settings.execution_policy_json` 的列默认
// 是 `'{}'`，`settings::save_runtime_settings` 建行时不写该列，读取端必须能把 `{}`
// 解析成全套默认值。`deny_unknown_fields` 保证多出来的键仍然报错。
#[serde(default, deny_unknown_fields)]
pub struct ExecutionPolicySettings {
    pub tools_default: ExecutionPolicy,
    pub mcp_default: McpWritePolicy,
    /// 写入风险评审的自动放行阈值：综合风险分不高于它才自动执行。
    ///
    /// 阈值不是风险等级：调高等于把更多写入交给评审模型，调低等于把更多写入交给人。
    pub write_risk_threshold: f64,
    /// 评审模型在综合分里的权重（0–[`MAX_WRITE_REVIEW_WEIGHT`]）。
    ///
    /// 上限固定为 0.5：评审模型与代码规则平权，但不能单方面压倒规则分。
    pub write_review_weight: f64,
}

impl Default for ExecutionPolicySettings {
    fn default() -> Self {
        Self {
            tools_default: ExecutionPolicy::WriteApproval,
            mcp_default: McpWritePolicy::ReadOnly,
            write_risk_threshold: DEFAULT_WRITE_RISK_THRESHOLD,
            write_review_weight: DEFAULT_WRITE_REVIEW_WEIGHT,
        }
    }
}

impl ExecutionPolicySettings {
    /// 清洗越界取值：阈值与权重都必须在有效区间内，否则拒绝保存而不是悄悄裁剪。
    pub fn validate(&self) -> crate::Result<()> {
        if !(0.0..=1.0).contains(&self.write_risk_threshold) {
            return Err(crate::Error::new(
                crate::ErrorKind::Internal,
                "自动放行阈值必须在 0–1 之间",
            ));
        }
        if !(0.0..=MAX_WRITE_REVIEW_WEIGHT).contains(&self.write_review_weight) {
            return Err(crate::Error::new(
                crate::ErrorKind::Internal,
                format!("评审模型权重必须在 0–{MAX_WRITE_REVIEW_WEIGHT} 之间"),
            ));
        }
        Ok(())
    }
}
