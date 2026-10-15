impl McpExecutionService {
    /// 执行一次外部工具调用：schema 校验 → 数据源判定 → 强制审计 → 内核授权 → 执行 → 投影。
    ///
    /// 与内部 TurnRunner 共用执行链；强制审计与全局工具开关始终生效。
    /// 写入只在数据源为「允许读写」时放行；只读数据源上的写工具不列出也拒绝。
    pub fn call(
        &self,
        identity: &McpCallIdentity,
        mcp_tool_name: &str,
        arguments_json: &str,
        cancel: &CancelFlag,
    ) -> McpCallOutcome {
        if !self.profile.enabled {
            return McpCallOutcome::failure(McpCallError::new(
                "profile_disabled",
                "MCP 服务已关闭",
                false,
            ));
        }
        // 1) 服务内实现的工具优先处理（connections.list / operation.status）。
        match mcp_tool_name {
            "fluxdb_connections_list" => {
                if !self.tool_visible(mcp_tool_name) {
                    return McpCallOutcome::failure(McpCallError::new(
                        "tool_not_allowed",
                        "工具未开放",
                        false,
                    ));
                }
                return match self.prepare_no_args(arguments_json) {
                    Ok(()) => McpCallOutcome::success(self.list_resources()),
                    Err(error) => McpCallOutcome::failure(error),
                };
            }
            "fluxdb_operation_status" => {
                if !self.tool_visible(mcp_tool_name) {
                    return McpCallOutcome::failure(McpCallError::new(
                        "tool_not_allowed",
                        "工具未开放",
                        false,
                    ));
                }
                return self.handle_operation(identity, arguments_json);
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
        // 3) 可见性不是授权：即使 tools/list 隐藏了写工具，直接调用也要拒绝。
        if !self.tool_visible(mcp_tool_name) {
            let code = if mcp_permission_tool_id(mcp_tool_name)
                .is_some_and(|id| self.globally_disabled.contains(id))
            {
                "tool_not_allowed"
            } else {
                "write_disabled"
            };
            return McpCallOutcome::failure(McpCallError::new(
                code,
                format!("工具 {mcp_tool_name} 在当前数据源策略下不可用"),
                false,
            ));
        }
        if let Err(error) = self.revalidate(mcp_tool_name) {
            return McpCallOutcome::failure(McpCallError::new(
                "aut[REDACTED]ed",
                error.message,
                false,
            ));
        }

        // 4) 装配对应能力的工具目录。
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
        // 5) schema 校验（不做类型强转/猜参数）。
        let prepared = match catalog.prepare(internal_id, arguments_json) {
            Ok(prepared) => prepared,
            Err(error) => return McpCallOutcome::failure(tool_error_to_call(error)),
        };
        // 6) 校验 target 指向已加入的数据源（读/写工具都带 target，服务内工具除外）。
        let target_str = target_of(&prepared).unwrap_or_default();
        let mut policy = KernelPolicy {
            read_allowed: true,
            write_allowed: false,
            admin_allowed: false,
        };
        if !target_str.is_empty() {
            match self.datasources.resolve(target_str) {
                Ok(handle) => {
                    policy = self.kernel_policy(target_str);
                    let _ = handle;
                }
                Err(error) => return McpCallOutcome::failure(error),
            }
        }
        if spec.risk == fluxdb_core::ToolRisk::Write && !policy.write_allowed {
            return McpCallOutcome::failure(McpCallError::new(
                "write_disabled",
                "该数据源为只读或连接已标记只读，禁止写入",
                false,
            ));
        }
        // 允许读写也不放开结构/管理类写入：DDL、GRANT、脚本/管理命令仍按内核白名单拒绝。
        if spec.risk == fluxdb_core::ToolRisk::Write {
            if let Err(error) = self.validate_data_write(internal_id, &prepared) {
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
        let input = AuthorizeInput {
            tool_id: spec.id.clone(),
            tool_version: spec.version,
            risk: spec.risk,
            target_json: target_json.clone(),
            normalized_arguments_json: prepared.to_json_string(),
            plugin_digest: None,
            globally_disabled_tools: BTreeSet::new(),
            agent_allowed_tools: vec![],
            policy,
            audit: Some(audit),
        };
        if let Some(security) = &self.security {
            if let Err(error) = security.verify_connection(&input) {
                return McpCallOutcome::failure(McpCallError::new(
                    "connection_changed",
                    error.message,
                    false,
                ));
            }
        }
        let decision = crate::authorize(&input);
        let reason = format!("{}", decision_reason(&decision));
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
                // 审批队列已移除：写工具走数据源「允许读写」直接执行；
                // 读工具出现需审批结论（如审计要求）时按拒绝处理，不静默放行。
                if spec.risk != fluxdb_core::ToolRisk::Write {
                    return McpCallOutcome::failure(McpCallError::new(
                        "approval_required",
                        "该调用需要人工确认，但 MCP 未启用审批队列",
                        false,
                    ))
                    .with_audit(audit_fact("require_approval", vec!["approval_required".into()]));
                }
            }
            PermissionDecision::Allow { .. } => {}
        }

        if let Err(error) = self.revalidate(mcp_tool_name) {
            return McpCallOutcome::failure(McpCallError::new(
                "aut[REDACTED]ed",
                error.message,
                false,
            ));
        }
        // 8) 执行并投影（与内部同链）。
        if let Some(security) = &self.security {
            let recorded = with_ai_storage(&security.storage, |conn| {
                let tx = conn
                    .unchecked_transaction()
                    .map_err(|_| Error::new(ErrorKind::Internal, "无法登记执行审计"))?;
                let connection_id = AiSqlTarget::parse(&target_json)
                    .ok()
                    .map(|t| t.connection_id.0);
                let target =
                    serde_json::json!({"target": target_json, "connection_id": connection_id})
                        .to_string();
                let changed = tx.execute(
                    "UPDATE ai_mcp_server_requests SET status='running',target_json=?2,plan_hash=?3,
                        connection_id=?4, revision=revision+1
                      WHERE id=?1 AND session_id=?5 AND profile_id=?6
                        AND status IN ('prepared','waiting_approval')",
                    rusqlite::params![
                        identity.request_id,
                        target,
                        hash,
                        connection_id,
                        identity.session_id,
                        self.profile.id
                    ],
                )
                .map_err(|_| Error::new(ErrorKind::Internal, "无法占用请求执行权"))?;
                if changed != 1 {
                    return Err(Error::new(
                        ErrorKind::Permission,
                        "请求已执行或失效，不会重复执行",
                    ));
                }
                use fluxdb_storage::ai_platform::audits::*;
                insert_tool_audit(
                    &tx,
                    ai_chat_now(),
                    &ToolAuditEdit {
                        id: format!("mcp-pre-{}", uuid::Uuid::new_v4()),
                        tool_call_id: None,
                        mcp_request_id: Some(identity.request_id.clone()),
                        stage: AuditStage::PreExecute,
                        hook_id: "execution_policy".into(),
                        hook_version: "1".into(),
                        ruleset_version: "execution-policy-v1".into(),
                        plan_hash: hash.clone(),
                        decision: AuditDecision::Allow,
                        risk: spec.risk.to_string(),
                        rule_ids: vec![],
                        reason_codes: vec![reason.clone()],
                    },
                )?;
                tx.commit()
                    .map_err(|_| Error::new(ErrorKind::Internal, "执行审计提交失败"))
            });
            if let Err(error) = recorded {
                return McpCallOutcome::failure(McpCallError::new(
                    "audit_unavailable",
                    error.message,
                    false,
                ));
            }
        }
        let Some(tool) = catalog.executable(internal_id) else {
            return McpCallOutcome::failure(McpCallError::new(
                "tool_not_executable",
                format!("工具 {mcp_tool_name} 没有可执行实现"),
                false,
            ));
        };
        let ctx = fluxdb_core::ToolExecutionContext {
            automatic_approval: false,
            session_id: identity.session_id.clone(),
            turn_id: identity.request_id.clone(),
            iteration: 1,
            call_id: identity.request_id.clone(),
            cancel: cancel.clone(),
            progress: None,
        };
        let receipt = match crate::block_on_ready(tool.execute(ctx, prepared.to_json_string())) {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(error)) | Err(error) => {
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
            McpCallOutcome::success(envelope).with_audit(audit_fact("allow", vec![reason]))
        } else {
            let parsed: serde_json::Value = serde_json::from_str(&receipt_json)
                .unwrap_or_else(|_| serde_json::json!({ "raw": receipt_json }));
            let code = receipt
                .error_code
                .clone()
                .or_else(|| {
                    parsed
                        .get("code")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
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

    /// 工具在当前数据源策略与全局开关下是否可见/可调用。
    fn tool_visible(&self, mcp_tool_name: &str) -> bool {
        self.tool_views()
            .map(|views| views.iter().any(|view| view.name == mcp_tool_name))
            .unwrap_or(false)
    }

    /// 调用前复核：Profile 未删除/未禁用、数据源集合与连接配置未变化。
    fn revalidate(&self, tool: &str) -> fluxdb_core::Result<()> {
        let Some(security) = &self.security else {
            return Ok(());
        };
        with_ai_storage(&security.storage, |conn| {
            use fluxdb_storage::ai_platform::{mcp_server, tool_settings};
            let profile = mcp_server::load_server_profile(conn, &self.profile.id)?
                .ok_or_else(|| Error::new(ErrorKind::Permission, "MCP 服务已删除"))?;
            if !profile.enabled || profile.revision != self.profile.revision {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "MCP 服务配置已变化，请重新调用",
                ));
            }
            if mcp_permission_tool_id(tool).is_some_and(|id| {
                tool_settings::disabled_tool_ids(conn)
                    .map(|set| set.contains(id))
                    .unwrap_or(true)
            }) {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "工具已被全局关闭，请重新调用",
                ));
            }
            Ok(())
        })?;
        let connections = security.storage.load_connections()?;
        for handle in self.datasources.handles() {
            let before = self.resolver.connection_config(handle.connection_id);
            let after = connections.iter().find(|c| c.id == handle.connection_id);
            if before.as_ref() != after {
                return Err(Error::new(
                    ErrorKind::Permission,
                    "连接配置已变化，请重新调用",
                ));
            }
        }
        Ok(())
    }
}

/// 授权结论的可读原因（写入执行审计的 reason_codes）。
fn decision_reason(decision: &PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Allow { .. } => "kernel_allow",
        PermissionDecision::RequireApproval { .. } => "kernel_require_approval",
        PermissionDecision::Deny { code, .. } => code,
    }
}

impl McpExecutionService {
    /// 「允许读写」的结构边界：放行 DML（INSERT/UPDATE/DELETE），拒绝 DDL / 权限 / 事务控制。
    ///
    /// 数据源写入策略只决定“能不能改数据”，不把结构与管理能力交给外部调用；
    /// Redis 侧同样只放行内核列为需授权的数据命令，管理命令仍被拒绝。
    fn validate_data_write(
        &self,
        internal_id: &str,
        prepared: &crate::ValidatedArguments,
    ) -> std::result::Result<(), McpCallError> {
        if internal_id == "redis.execute" {
            let argv = prepared
                .as_value()
                .get("argv")
                .and_then(|v| v.as_array())
                .ok_or_else(|| McpCallError::new("invalid_arguments", "缺少命令参数", false))?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            return match classify_redis_command(&argv) {
                RedisRisk::Rejected { reasons } => Err(McpCallError::new(
                    "write_admin_rejected",
                    format!("该命令在内核拒绝列表中，未执行（{}）", reasons.join("、")),
                    false,
                )),
                RedisRisk::RequiresAuthorization { .. } | RedisRisk::ReadOnly => Ok(()),
            };
        }
        let sql = prepared
            .as_value()
            .get("sql")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let target = target_of(prepared)
            .and_then(|t| AiSqlTarget::parse(t).ok())
            .ok_or_else(|| McpCallError::new("invalid_target", "缺少写入目标", false))?;
        let config = self
            .resolver
            .connection_config(target.connection_id)
            .ok_or_else(|| McpCallError::new("invalid_connection", "连接不存在", false))?;
        let dialect = dialect_for(config.kind).ok_or_else(|| {
            McpCallError::new("unsupported_database", "该连接类型不支持 SQL 写工具", false)
        })?;
        // `analyze_sql` 对白名单外的语句（DDL/GRANT/事务控制等）给不出只读结论；
        // 这里要求语句是明确的 DML，未知语法与结构变更一律拒绝。
        let statements =
            sqlparser::parser::Parser::parse_sql(dialect.parser_dialect().as_ref(), sql.trim())
                .map_err(|_| {
                    McpCallError::new("write_unverifiable", "SQL 无法解析，无法核对写范围", false)
                })?;
        if statements.len() != 1 {
            return Err(McpCallError::new(
                "write_unverifiable",
                "写工具只接受单条语句",
                false,
            ));
        }
        use sqlparser::ast::Statement;
        match &statements[0] {
            Statement::Update(_) | Statement::Delete(_) | Statement::Insert(_) => Ok(()),
            other => Err(McpCallError::new(
                "write_admin_rejected",
                format!(
                    "允许读写仅放行数据修改（INSERT/UPDATE/DELETE），当前语句被拒绝：{}",
                    other.to_string().split_whitespace().next().unwrap_or("unknown")
                ),
                false,
            )),
        }
    }
}
