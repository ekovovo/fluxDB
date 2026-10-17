// 内外工具共用的写入风险评审与授权；模型只影响风险分，不提供资源授权。
/// 评审不可用时统一走这条路径：完全按代码规则判定，不把"没有模型"当成低风险。
fn rule_only_decision(
    facts: &WriteRiskFacts,
    settings: &ExecutionPolicySettings,
    reason: &str,
) -> (WriteRiskDecision, String) {
    let decision = fluxdb_core::combine_write_risk(
        facts,
        None,
        settings.write_risk_threshold,
        settings.write_review_weight,
    );
    (
        decision,
        format!("评审模型不可用（{reason}），按代码规则判定"),
    )
}

/// 把评审结论合成为最终决定；`Scored` 走加权混合，`Unavailable` 退回规则分。
fn score_write(
    facts: &WriteRiskFacts,
    outcome: WriteReviewOutcome,
    settings: &ExecutionPolicySettings,
) -> (WriteRiskDecision, String) {
    match outcome {
        WriteReviewOutcome::Scored(model) => {
            let decision = fluxdb_core::combine_write_risk(
                facts,
                Some(&model),
                settings.write_risk_threshold,
                settings.write_review_weight,
            );
            let note = format!("评审来源：{}", model.source);
            (decision, note)
        }
        WriteReviewOutcome::Unavailable { reason } => {
            rule_only_decision(facts, settings, &reason)
        }
    }
}

/// 内部会话和 MCP 都在最终参数校验之后调用；配置按工具边界重新读取。
pub struct ExecutionSecurity {
    storage: fluxdb_storage::FileStorage,
    reviewer: Arc<dyn WriteRiskReviewer>,
    /// 进入本轮时的连接快照：执行期间目标连接被改写或删除都必须拒绝。
    connections: Vec<ConnectionConfig>,
    /// 快照是否来自存储。来自存储时（对外 MCP）连接必须与存储保持一致；
    /// 调用方直接给出的连接（内部会话按固定目标装配）没有可重新读取的登记来源，
    /// 只能与快照比对，不能按“存储里查不到”判定为已删除。
    revalidate_from_storage: bool,
}

pub struct PolicyAuthorization {
    pub decision: PermissionDecision,
    /// 宿主已按写入风险评审自动批准，不再逐次人工确认。
    pub automatic_approval: bool,
    /// 自动批准时的写入提交路径；连接器给不出有界证明时为 [`WriteCommit::Ordinary`]。
    pub write_commit: WriteCommit,
    /// 可读的判定摘要：各风险信号、模型分与阈值，写进审计事件与审批提示。
    pub reason: String,
    /// 本次评审的合成结果；非写入调用为 `None`。
    pub risk: Option<WriteRiskDecision>,
    pub settings_revision: i64,
}

impl ExecutionSecurity {
    /// 对外 MCP：数据源引用存储中已登记的连接，执行期间按存储复核。
    ///
    /// MCP 写入由数据源写入策略直接放行，不走「帮我审批」，因此没有本轮 Agent 模型。
    pub fn new(storage: fluxdb_storage::FileStorage) -> Self {
        let connections = storage.load_connections().unwrap_or_default();
        Self::assemble(storage, connections, true, None)
    }

    /// 内部会话：连接由调用方按本轮固定目标给出，未必已登记到存储。
    ///
    /// `turn_model_profile_id` 是本轮 Agent 实际路由到的生成模型，作为 Jev 不可用时的
    /// 评审模型；调用方必须传本轮真实模型，不能传别的模型或留空。
    pub fn with_connections(
        storage: fluxdb_storage::FileStorage,
        connections: Vec<ConnectionConfig>,
        turn_model_profile_id: String,
    ) -> Self {
        Self::assemble(storage, connections, false, Some(turn_model_profile_id))
    }

    fn assemble(
        storage: fluxdb_storage::FileStorage,
        connections: Vec<ConnectionConfig>,
        revalidate_from_storage: bool,
        turn_model_profile_id: Option<String>,
    ) -> Self {
        Self {
            reviewer: Arc::new(StorageWriteRiskReviewer::new(
                storage.clone(),
                turn_model_profile_id,
            )),
            storage,
            connections,
            revalidate_from_storage,
        }
    }

    fn verify_connection(&self, input: &AuthorizeInput) -> fluxdb_core::Result<()> {
        let arguments =
            serde_json::from_str::<serde_json::Value>(&input.normalized_arguments_json).ok();
        let id = arguments
            .as_ref()
            .and_then(|v| v.get("target"))
            .and_then(|v| v.as_str())
            .and_then(|v| AiSqlTarget::parse(v).ok())
            .map(|t| t.connection_id)
            .or_else(|| {
                AiSqlTarget::parse(&input.target_json)
                    .ok()
                    .map(|t| t.connection_id)
            })
            .or_else(|| {
                serde_json::from_str::<serde_json::Value>(&input.target_json)
                    .ok()
                    .and_then(|v| v.get("connection_id").and_then(|v| v.as_u64()))
                    .map(ConnectionId)
            });
        let Some(id) = id else {
            return Ok(());
        };
        let Some(expected) = self.connections.iter().find(|config| config.id == id) else {
            return Err(Error::new(
                ErrorKind::Permission,
                "连接不在本轮授权集合内，未执行",
            ));
        };
        // 来自存储的快照必须与存储保持一致：连接被改写或删除都不能继续用旧配置执行。
        if self.revalidate_from_storage {
            let current = self
                .storage
                .load_connections()?
                .into_iter()
                .find(|config| config.id == id);
            if current.as_ref() != Some(expected) {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "连接配置已变化或连接已删除，未执行",
                ));
            }
        }
        if input.risk != fluxdb_core::ToolRisk::Read && connection_is_read_only(expected) {
            return Err(Error::new(ErrorKind::Permission, "连接已设为只读，未执行"));
        }
        if input.risk != fluxdb_core::ToolRisk::Read {
            let conflict = with_ai_storage(&self.storage, |conn| {
                conn.query_row("SELECT EXISTS(
                    SELECT 1 FROM ai_tool_calls WHERE status='unknown'
                      AND (json_extract(target_json,'$.connection_id')=?1 OR json_extract(target_json,'$.target') LIKE ?2)
                    UNION ALL SELECT 1 FROM ai_mcp_server_requests WHERE status='unknown'
                      AND (json_extract(target_json,'$.connection_id')=?1 OR json_extract(target_json,'$.target') LIKE ?2)
                )",rusqlite::params![id.0,format!("conn:{}/%",id.0)],|r|r.get::<_,bool>(0))
                    .map_err(|_|Error::new(ErrorKind::Internal,"无法核实未知写入状态"))
            })?;
            if conflict {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "该连接有尚未核实的未知写入，禁止继续修改",
                ));
            }
        }
        Ok(())
    }

    pub fn settings(&self) -> fluxdb_core::Result<(ExecutionPolicySettings, i64)> {
        with_ai_storage(
            &self.storage,
            fluxdb_storage::ai_platform::execution_policy::load,
        )
    }

    pub fn authorize(
        &self,
        input: &AuthorizeInput,
        mode: ExecutionPolicy,
        facts: impl FnOnce() -> fluxdb_core::Result<WriteRiskFacts>,
        cancel: &CancelFlag,
    ) -> fluxdb_core::Result<PolicyAuthorization> {
        let (settings, revision) = self.settings()?;
        self.verify_connection(input)?;
        let mut current = input.clone();
        current.globally_disabled_tools.extend(with_ai_storage(
            &self.storage,
            tool_settings::disabled_tool_ids,
        )?);
        let initial = authorize(&current);
        let mut result = PolicyAuthorization {
            decision: initial,
            automatic_approval: false,
            write_commit: WriteCommit::Ordinary,
            reason: format!("策略：{}", mode.label()),
            risk: None,
            settings_revision: revision,
        };
        // 硬拒绝（工具关停、白名单、连接只读、审计 Deny）不进入评分，模型无权翻案。
        if matches!(result.decision, PermissionDecision::Deny { .. }) {
            return Ok(result);
        }
        if mode == ExecutionPolicy::ReadOnly && input.risk != fluxdb_core::ToolRisk::Read {
            result.decision = PermissionDecision::Deny {
                code: "policy_read_only",
                reason: "当前执行策略为只读".into(),
                retryable: false,
            };
        } else if input.risk == fluxdb_core::ToolRisk::Write
            && input.audit == Some(AuditOutcome::Allow)
        {
            match mode {
                // 自动执行：不评审、不逐次确认；连接/工具/数据范围与硬校验仍然生效。
                ExecutionPolicy::Automatic => {
                    result.write_commit = WriteCommit::Ordinary;
                    result.decision = PermissionDecision::Allow {
                        plan_hash: result.decision.plan_hash().unwrap().into(),
                    };
                }
                ExecutionPolicy::AiApproval => {
                    let facts = match facts() {
                        Ok(facts) => facts,
                        Err(error) => {
                            // 工具报不出可评分的事实：不猜、不给分，停在人工。
                            result.reason.push_str(&format!(
                                "；无法生成写入风险证据（{}），需要人工审核",
                                error.message
                            ));
                            return self.finish(result, revision, input, cancel);
                        }
                    };
                    let (decision, note) =
                        score_write(&facts, self.reviewer.review(&facts, cancel), &settings);
                    result.reason.push_str(&format!("；{note}；{}", decision.summary()));
                    if decision.allows_automatic() {
                        result.write_commit = self.write_commit_for(&facts);
                        result.decision = PermissionDecision::Allow {
                            plan_hash: result.decision.plan_hash().unwrap().into(),
                        };
                        result.automatic_approval = true;
                    }
                    result.risk = Some(decision);
                }
                _ => {}
            }
        }
        self.finish(result, revision, input, cancel)
    }

    /// 收尾复核：审批期间策略变化、取消、连接变化都必须阻止执行。
    fn finish(
        &self,
        result: PolicyAuthorization,
        revision: i64,
        input: &AuthorizeInput,
        cancel: &CancelFlag,
    ) -> fluxdb_core::Result<PolicyAuthorization> {
        if cancel.is_cancelled() || self.settings()?.1 != revision {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Permission,
                "审批期间策略变化或调用已取消，未执行",
            ));
        }
        self.verify_connection(input)?;
        Ok(result)
    }

    /// 自动放行的写入走哪条提交路径：连接器提供有界提交时走有界路径，
    /// 否则按普通写入执行（影响行数由语句形状界定，见 §29.2）。
    fn write_commit_for(&self, facts: &WriteRiskFacts) -> WriteCommit {
        if facts.bounded_capable {
            WriteCommit::Bounded
        } else {
            WriteCommit::Ordinary
        }
    }
}

