/// `db.run` 内部的专用只读执行器（普通查询与 EXPLAIN）。
///
/// 两者共用同一只读执行链；差别只在描述与默认 schema 提示。
pub struct AiSqlReadTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
    /// EXPLAIN 模式把入参 SQL 包成 plan（不执行）；普通查询原样执行。
    wrap_explain: bool,
    /// 结果句柄仓库：查询与 EXPLAIN 输出可分页读取（T027）。
    results: Arc<Mutex<ResultStore>>,
}

impl<R: SqlExecutionResolver> AiSqlReadTool<R> {
    /// 注入本次会话的结果仓库，使 SQL 回执带可续读的 ResultRef。
    pub fn query_with_results(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        Self::from_spec(resolver, results, "db.run", DB_RUN_INPUT_SCHEMA, false)
    }

    /// EXPLAIN 与普通查询共用同一结果仓库。
    pub fn explain_with_results(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        Self::from_spec(
            resolver,
            results,
            "db.run",
            DB_RUN_INPUT_SCHEMA,
            true,
        )
    }

    fn from_spec(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
        tool_id: &str,
        schema: &str,
        wrap_explain: bool,
    ) -> fluxdb_core::Result<Self> {
        let specs = standard_sql_tool_specs()?;
        let spec = specs
            .into_iter()
            .find(|spec| spec.id == tool_id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("缺少标准 SQL 工具定义: {tool_id}"),
                )
            })?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: schema.to_string(),
            },
            resolver,
            results,
            wrap_explain,
        })
    }
}

/// EXPLAIN 的入参规整：默认只做 plan，不执行 ANALYZE（设计 §21.5）。
fn apply_explain_mode(args: &mut ParsedSqlArgs) -> Result<(), SqlExecError> {
    if args.mode.as_deref() == Some("execute") {
        return Err(SqlExecError::new(
            "explain_execute_unsupported",
            "执行式查询计划（EXPLAIN ANALYZE）默认不开放，需走修改入口另行审批",
        ));
    }
    let trimmed = args.sql.trim_start();
    if !trimmed.to_ascii_lowercase().starts_with("explain") {
        // 包装成 plan：模型给纯 SELECT 时也必须返回计划而不是真实执行。
        args.sql = format!("EXPLAIN {}", args.sql.trim());
    }
    Ok(())
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiSqlReadTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        let cancel = ctx.cancel.clone();
        let progress_events = ctx.progress.clone();
        let wrap_explain = self.wrap_explain;
        let results = Arc::clone(&self.results);
        Box::pin(async move {
            let mut report_progress = |event: fluxdb_core::ToolProgressEvent| {
                if let Some(sink) = &progress_events {
                    sink.report(event);
                }
            };
            let outcome = match parse_sql_args(&arguments_json) {
                Ok(mut args) => {
                    let prepared = if wrap_explain {
                        apply_explain_mode(&mut args)
                    } else {
                        Ok(())
                    };
                    match prepared {
                        Ok(()) => execute_sql_with_parameters_and_progress(
                            resolver.as_ref(),
                            &args.target,
                            &args.sql,
                            args.max_rows,
                            &args.parameters,
                            true,
                            &|| cancel.is_cancelled(),
                            &mut report_progress,
                            false,
                        ),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            match outcome {
                Ok(outcome) => {
                    let result_ref = {
                        let mut store = results.lock().map_err(|_| {
                            fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "结果仓库锁已损坏",
                            )
                        })?;
                        let (result, _) = store
                            .put(
                                &descriptor.id,
                                &ctx.call_id,
                                &outcome.columns,
                                started_at_ms,
                                outcome
                                    .rows
                                    .clone()
                                    .into_iter()
                                    .map(serde_json::Value::from),
                            )
                            .map_err(|error| {
                                fluxdb_core::Error::new(
                                    fluxdb_core::ErrorKind::Internal,
                                    format!("结果保存失败: {}", error.message),
                                )
                            })?;
                        result
                    };
                    Ok(receipt_from_with_ref(
                        &descriptor,
                        &ctx.call_id,
                        started_at_ms,
                        Ok(outcome),
                        Some(&result_ref),
                    ))
                }
                Err(error) => Ok(receipt_from_with_ref(
                    &descriptor,
                    &ctx.call_id,
                    started_at_ms,
                    Err(error),
                    None,
                )),
            }
        })
    }
}

/// 元数据探索工具：`db.search_objects`（只读）。
pub struct AiSqlSearchTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiSqlSearchTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        let spec = standard_sql_tool_specs()?
            .into_iter()
            .find(|spec| spec.id == "db.search_objects")
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "缺少标准 SQL 工具定义: db.search_objects",
                )
            })?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: spec.input_schema.to_string(),
            },
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiSqlSearchTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            let outcome: Result<serde_json::Value, SqlExecError> = (|| {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let handle = value
                    .get("target")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SqlExecError::new("invalid_arguments", "缺少 target"))?;
                let query = value.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let limit = value.get("limit").and_then(|v| v.as_u64()).unwrap_or(30);
                let target = AiSqlTarget::parse(handle)?;
                search_objects(resolver.as_ref(), &target, query, limit, &|| {
                    cancel.is_cancelled()
                })
            })();
            let receipt = match outcome {
                Ok(value) => fluxdb_core::ToolReceipt {
                    call_id: ctx.call_id.clone(),
                    tool_id: descriptor.id.clone(),
                    status: fluxdb_core::ToolReceiptStatus::Succeeded,
                    result_json: value.to_string(),
                    error_code: None,
                    affected_rows: None,
                    started_at_ms,
                    finished_at_ms: now_ms(),
                },
                Err(error) => fluxdb_core::ToolReceipt {
                    call_id: ctx.call_id.clone(),
                    tool_id: descriptor.id.clone(),
                    status: if error.code == "cancelled" {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    result_json: serde_json::json!({
                        "ok": false,
                        "code": error.code,
                        "message": error.message,
                    })
                    .to_string(),
                    error_code: Some(error.code.to_string()),
                    affected_rows: None,
                    started_at_ms,
                    finished_at_ms: now_ms(),
                },
            };
            Ok(receipt)
        })
    }
}

/// 元数据工具：`db.describe_object`（只读）。
pub struct AiSqlDescribeTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiSqlDescribeTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        let spec = standard_sql_tool_specs()?
            .into_iter()
            .find(|spec| spec.id == "db.describe_object")
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "缺少标准 SQL 工具定义: db.describe_object",
                )
            })?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: spec.input_schema.to_string(),
            },
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiSqlDescribeTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        let cancel = ctx.cancel.clone();
        Box::pin(async move {
            let outcome: Result<serde_json::Value, SqlExecError> = (|| {
                let value: serde_json::Value = serde_json::from_str(&arguments_json)
                    .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
                let target = value
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| SqlExecError::new("invalid_arguments", "缺少 target"))?;
                let target = AiSqlTarget::parse(target)?;
                let object_ref = value
                    .get("object_ref")
                    .ok_or_else(|| SqlExecError::new("invalid_object_ref", "缺少 object_ref"))?;
                let include = value
                    .get("include")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                describe_object(resolver.as_ref(), &target, object_ref, &include, &|| {
                    cancel.is_cancelled()
                })
            })();
            let receipt = match outcome {
                Ok(value) => fluxdb_core::ToolReceipt {
                    call_id: ctx.call_id.clone(),
                    tool_id: descriptor.id.clone(),
                    status: fluxdb_core::ToolReceiptStatus::Succeeded,
                    result_json: value.to_string(),
                    error_code: None,
                    affected_rows: None,
                    started_at_ms,
                    finished_at_ms: now_ms(),
                },
                Err(error) => fluxdb_core::ToolReceipt {
                    call_id: ctx.call_id.clone(),
                    tool_id: descriptor.id.clone(),
                    status: if error.code == "cancelled" {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    result_json: serde_json::json!({
                        "ok": false,
                        "code": error.code,
                        "message": error.message,
                    })
                    .to_string(),
                    error_code: Some(error.code.to_string()),
                    affected_rows: None,
                    started_at_ms,
                    finished_at_ms: now_ms(),
                },
            };
            Ok(receipt)
        })
    }
}

/// `db.run` 内部的修改执行器（Write）。
///
/// 只由授权链放行后的运行时调用；本工具不授予权限。AI 自动批准必须使用有界提交，
/// 人工批准由审批行 plan_hash 复核，自动执行也不能绕过目标与语句硬校验。
pub struct AiSqlExecuteTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
}

impl<R: SqlExecutionResolver> AiSqlExecuteTool<R> {
    pub fn new(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        let spec = standard_sql_tool_specs()?
            .into_iter()
            .find(|spec| spec.id == "db.run")
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "缺少标准 SQL 工具定义: db.run",
                )
            })?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: spec.input_schema.to_string(),
            },
            resolver,
        })
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiSqlExecuteTool<R> {
    fn write_risk_facts(
        &self,
        arguments_json: &str,
    ) -> fluxdb_core::Result<fluxdb_core::WriteRiskFacts> {
        let args = parse_sql_args(arguments_json).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Unsupported, "计划参数无效")
        })?;
        let config = self
            .resolver
            .connection_config(args.target.connection_id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Permission, "连接未授权")
            })?;
        let dialect = dialect_for(config.kind).ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Unsupported, "无法检查数据库类型")
        })?;
        // 作用域越界不是"高风险"而是确定性拒绝：直接返回受控错误，
        // 授权链据此停在人工，不把越界当成可以靠评审模型背书的形状问题。
        validate_sql_scope(&args.sql, dialect, &args.target).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Permission, "SQL 超出作用域")
        })?;
        let bounded = fluxdb_connectors::connector_for(&config)?.supports_bounded_write();
        Ok(crate::sql_write_risk_facts(&args.sql, dialect, bounded))
    }
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn approval_details(&self, arguments_json: &str) -> fluxdb_core::ApprovalDetails {
        // 展示投影：工具自己按入参里的连接与 SQL 判定风险，不依赖外层会话传入方言。
        match parse_sql_args(arguments_json) {
            Ok(args) => {
                let dialect = self
                    .resolver
                    .connection_config(args.target.connection_id)
                    .and_then(|config| dialect_for(config.kind));
                match dialect {
                    Some(dialect) => crate::sql_approval_details(&args.sql, dialect),
                    None => fluxdb_core::ApprovalDetails {
                        risk: fluxdb_core::ApprovalRisk::High,
                        guardrails: vec!["计划风险尚未核实".to_string()],
                        impact_basis: "未核实".to_string(),
                        ..Default::default()
                    },
                }
            }
            Err(_) => fluxdb_core::ApprovalDetails {
                risk: fluxdb_core::ApprovalRisk::High,
                guardrails: vec!["计划风险尚未核实".to_string()],
                impact_basis: "未核实".to_string(),
                ..Default::default()
            },
        }
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let started_at_ms = now_ms();
        let descriptor = self.descriptor.clone();
        let resolver = Arc::clone(&self.resolver);
        let cancel = ctx.cancel.clone();
        let progress_events = ctx.progress.clone();
        Box::pin(async move {
            let mut report_progress = |event: fluxdb_core::ToolProgressEvent| {
                if let Some(sink) = &progress_events {
                    sink.report(event);
                }
            };
            let outcome = match parse_sql_args(&arguments_json) {
                Ok(args) => execute_sql_with_parameters_and_progress(
                    resolver.as_ref(),
                    &args.target,
                    &args.sql,
                    args.max_rows,
                    &args.parameters,
                    // 修改入口：不做只读分类拦截，但仍受"单条语句/事务控制拒绝"约束。
                    false,
                    &|| cancel.is_cancelled(),
                    &mut report_progress,
                    ctx.bounded_write,
                ),
                Err(error) => Err(error),
            };
            Ok(receipt_from(
                &descriptor,
                &ctx.call_id,
                started_at_ms,
                outcome,
            ))
        })
    }
}

/// 统一 SQL 入口：`db.run`。
///
/// 模型不再需要在多个 SQL 入口之间选择；工具在授权前
/// 用内核分类决定有效风险：只读语句直接执行，写语句进入审批，`mode=explain` 只看计划。
pub struct AiSqlRunTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
    query: AiSqlReadTool<R>,
    explain: AiSqlReadTool<R>,
    execute: AiSqlExecuteTool<R>,
}

impl<R: SqlExecutionResolver> AiSqlRunTool<R> {
    pub fn new(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        let spec = standard_sql_tool_specs()?
            .into_iter()
            .find(|spec| spec.id == "db.run")
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "缺少标准 SQL 工具定义: db.run",
                )
            })?;
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: spec.id,
                version: spec.version,
                description: spec.description,
                risk: spec.risk,
                input_schema_json: spec.input_schema.to_string(),
            },
            query: AiSqlReadTool::query_with_results(
                Arc::clone(&resolver),
                Arc::clone(&results),
            )?,
            explain: AiSqlReadTool::explain_with_results(
                Arc::clone(&resolver),
                Arc::clone(&results),
            )?,
            execute: AiSqlExecuteTool::new(resolver.clone())?,
            resolver,
        })
    }

    fn effective_risk_for(&self, arguments_json: &str) -> fluxdb_core::ToolRisk {
        let Ok(args) = parse_sql_args(arguments_json) else {
            // 参数无效时不放行；后续 schema/解析会给出具体错误。
            return fluxdb_core::ToolRisk::Write;
        };
        if args.mode.as_deref() == Some("explain") {
            return fluxdb_core::ToolRisk::Read;
        }
        let Some(config) = self.resolver.connection_config(args.target.connection_id) else {
            return fluxdb_core::ToolRisk::Write;
        };
        let Some(dialect) = dialect_for(config.kind) else {
            return fluxdb_core::ToolRisk::Write;
        };
        match analyze_sql(&args.sql, dialect) {
            // Rejected 会在执行入口被确定性拒绝，不产生副作用，不需要人工审批。
            SqlRisk::ReadOnly | SqlRisk::Rejected { .. } => fluxdb_core::ToolRisk::Read,
            SqlRisk::RequiresAuthorization { .. } => fluxdb_core::ToolRisk::Write,
        }
    }
}

impl<R: SqlExecutionResolver + 'static> fluxdb_core::AiTool for AiSqlRunTool<R> {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn effective_risk(&self, arguments_json: &str) -> fluxdb_core::ToolRisk {
        self.effective_risk_for(arguments_json)
    }

    fn write_risk_facts(
        &self,
        arguments_json: &str,
    ) -> fluxdb_core::Result<fluxdb_core::WriteRiskFacts> {
        self.execute.write_risk_facts(arguments_json)
    }

    fn approval_details(&self, arguments_json: &str) -> fluxdb_core::ApprovalDetails {
        self.execute.approval_details(arguments_json)
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        let descriptor = self.descriptor.clone();
        Box::pin(async move {
            let mode = parse_sql_args(&arguments_json)
                .ok()
                .and_then(|args| args.mode);
            let mut receipt = if mode.as_deref() == Some("explain") {
                self.explain.execute(ctx, arguments_json).await?
            } else if self.effective_risk_for(&arguments_json) == fluxdb_core::ToolRisk::Read {
                self.query.execute(ctx, arguments_json).await?
            } else {
                self.execute.execute(ctx, arguments_json).await?
            };
            receipt.tool_id = descriptor.id;
            Ok(receipt)
        })
    }
}

/// 用标准 SQL 工具集装配 `ToolCatalog`。
///
/// SQL 只有一个执行入口 `db.run`；静态风险为 Write，单次调用由 SQL 分类器
/// 计算有效风险。是否允许写入仍由统一授权与审批链决定。
pub fn standard_sql_catalog<R: SqlExecutionResolver + 'static>(
    resolver: Arc<R>,
    include_write: bool,
) -> fluxdb_core::Result<ToolCatalog> {
    // 默认装配使用无归属的内存仓库：调用方未提供会话上下文，结果不可跨会话读取。
    let results = Arc::new(Mutex::new(ResultStore::new(SQL_RESULT_MAX_BYTES)));
    standard_sql_catalog_with_results(resolver, results, include_write)
}

/// 生产装配入口：显式注入本次会话的结果仓库，使只读工具回执带可续读的 ResultRef。
pub fn standard_sql_catalog_with_results<R: SqlExecutionResolver + 'static>(
    resolver: Arc<R>,
    results: Arc<Mutex<ResultStore>>,
    include_write: bool,
) -> fluxdb_core::Result<ToolCatalog> {
    standard_sql_catalog_impl(resolver, results, include_write)
}

fn standard_sql_catalog_impl<R: SqlExecutionResolver + 'static>(
    resolver: Arc<R>,
    results: Arc<Mutex<ResultStore>>,
    include_write: bool,
) -> fluxdb_core::Result<ToolCatalog> {
    let mut specs = standard_sql_tool_specs()?;
    // 结果分页必须与实现同时进入目录：只注册实现会让模型看得到 spec 却调不动，
    // 反之只给 spec 则 describe 得到、execute 失败。
    specs.push(result_read_page_spec()?);
    let mut catalog = ToolCatalog::new(specs);
    catalog.register(Box::new(AiSqlSearchTool::new(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiSqlDescribeTool::new(Arc::clone(&resolver))?))?;
    // 结果分页与数据库类型无关，但必须共用同一仓库，句柄才读得到。
    catalog.register(Box::new(AiResultReadPageTool::new(Arc::clone(&results))?))?;
    if include_write {
        catalog.register(Box::new(AiSqlRunTool::new(resolver, results.clone())?))?;
    } else {
        catalog.register(Box::new(AiSqlReadTool::query_with_results(
            Arc::clone(&resolver),
            Arc::clone(&results),
        )?))?;
    }
    // 仓库随目录一起交出：回执登记溢写事实按目录查，不再由调用方另传一遍。
    Ok(catalog.with_results(results))
}

#[cfg(test)]
mod ai_sql_run_catalog_tests {
    use super::*;
    use fluxdb_core::AiTool;

    fn run_tool() -> AiSqlRunTool<MapSqlResolver> {
        let resolver = Arc::new(MapSqlResolver::new([er_test_connection()]));
        let results = Arc::new(Mutex::new(ResultStore::new(1024 * 1024)));
        AiSqlRunTool::new(resolver, results).expect("db.run")
    }

    #[test]
    fn writable_catalog_exposes_one_sql_entry() {
        let resolver = Arc::new(MapSqlResolver::new([er_test_connection()]));
        let results = Arc::new(Mutex::new(ResultStore::new(1024 * 1024)));
        let catalog =
            standard_sql_catalog_with_results(resolver, results, true).expect("catalog");
        assert!(catalog.describe("db.run").is_some());
        assert!(catalog.describe("db.query").is_none());
        assert!(catalog.describe("db.execute").is_none());
        assert!(catalog.describe("db.explain").is_none());
    }

    #[test]
    fn read_only_catalog_keeps_the_sql_entry_only() {
        let resolver = Arc::new(MapSqlResolver::new([er_test_connection()]));
        let results = Arc::new(Mutex::new(ResultStore::new(1024 * 1024)));
        let catalog =
            standard_sql_catalog_with_results(resolver, results, false).expect("catalog");
        assert!(catalog.describe("db.run").is_some());
        assert!(catalog.describe("db.query").is_none());
        assert!(catalog.describe("db.execute").is_none());
        assert!(catalog.describe("db.explain").is_none());
    }

    #[test]
    fn run_risk_follows_sql_classification() {
        let tool = run_tool();
        assert_eq!(
            tool.effective_risk(r#"{"target":"conn:1","sql":"SELECT 1"}"#),
            fluxdb_core::ToolRisk::Read
        );
        assert_eq!(
            tool.effective_risk(r#"{"target":"conn:1","sql":"DELETE FROM orders"}"#),
            fluxdb_core::ToolRisk::Write
        );
        assert_eq!(
            tool.effective_risk(
                r#"{"target":"conn:1","sql":"DELETE FROM orders","mode":"explain"}"#
            ),
            fluxdb_core::ToolRisk::Read
        );
    }
}
