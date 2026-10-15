/// 只读工具：`db.query`（SELECT 类）与 `db.explain`（普通 EXPLAIN）。
///
/// 两者共用同一只读执行链；差别只在描述与默认 schema 提示。
pub struct AiSqlReadTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
    /// `db.explain` 需要把入参 SQL 包成 plan（不执行）；`db.query` 原样执行。
    wrap_explain: bool,
    /// 结果句柄仓库：`db.query` / `db.explain` 输出可分页读取（T027）。
    results: Arc<Mutex<ResultStore>>,
}

impl<R: SqlExecutionResolver> AiSqlReadTool<R> {
    pub fn query(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        let results = Arc::new(Mutex::new(ResultStore::new(SQL_RESULT_MAX_BYTES)));
        Self::from_spec(resolver, results, "db.query", DB_QUERY_INPUT_SCHEMA, false)
    }

    pub fn explain(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        let results = Arc::new(Mutex::new(ResultStore::new(SQL_RESULT_MAX_BYTES)));
        Self::from_spec(
            resolver,
            results,
            "db.explain",
            DB_EXPLAIN_INPUT_SCHEMA,
            true,
        )
    }

    /// 生产装配入口：注入本次会话的结果仓库，使 `db.query` 回执带可续读的 ResultRef。
    pub fn query_with_results(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        Self::from_spec(resolver, results, "db.query", DB_QUERY_INPUT_SCHEMA, false)
    }

    /// 见 [`AiSqlReadTool::query_with_results`]：`db.explain` 共用同一结果仓库。
    pub fn explain_with_results(
        resolver: Arc<R>,
        results: Arc<Mutex<ResultStore>>,
    ) -> fluxdb_core::Result<Self> {
        Self::from_spec(
            resolver,
            results,
            "db.explain",
            DB_EXPLAIN_INPUT_SCHEMA,
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

/// `db.explain` 的入参规整：默认只做 plan，不执行 ANALYZE（设计 §21.5）。
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
                if let Some(events) = &progress_events {
                    if let Ok(mut events) = events.lock() {
                        events.push(event);
                    }
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

/// 修改工具：`db.execute`（Write）。
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
            .find(|spec| spec.id == "db.execute")
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "缺少标准 SQL 工具定义: db.execute",
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
    fn automatic_approval_facts(&self, arguments_json: &str) -> fluxdb_core::Result<String> {
        let args=parse_sql_args(arguments_json).map_err(|_| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Unsupported,"计划参数无效"))?;
        let config=self.resolver.connection_config(args.target.connection_id)
            .ok_or_else(|| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Permission,"连接未授权"))?;
        let dialect=dialect_for(config.kind).ok_or_else(|| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Unsupported,"无法检查数据库类型"))?;
        validate_sql_scope(&args.sql,dialect,&args.target)
            .map_err(|_| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Permission,"SQL 超出作用域"))?;
        fluxdb_connectors::connector_for(&config)?.inspect_bounded_update(&fluxdb_core::ParameterizedQueryRequest {
            request:fluxdb_core::QueryRequest {
                connection_id:args.target.connection_id, database:args.target.database,
                schema:args.target.schema,text:args.sql,mode:fluxdb_core::QueryMode::Selection,
                options:fluxdb_core::QueryExecutionOptions {
                    continue_on_error:false,split_statements:false,page_offset:0,page_size:1,read_only:false,
                },session_id:Some(self.resolver.query_session_id()),
            },parameters:args.parameters,
        })
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
                if let Some(events) = &progress_events {
                    if let Ok(mut events) = events.lock() {
                        events.push(event);
                    }
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
                    ctx.automatic_approval,
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

/// 用标准 SQL 工具集装配 `ToolCatalog`。
///
/// - `include_write=false`（默认）：只装只读工具（search/query/explain），
///   `db.execute` 的 spec 也一并去掉，避免"工具可见但不可执行"的不一致；
/// - `include_write=true`：只有审批链（T020 审批行 + plan_hash 复核）就绪后才可传，
///   否则写工具会停在 `RequireApproval`（这也是安全默认）。
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
    let mut specs = standard_sql_tool_specs()?;
    if !include_write {
        specs.retain(|spec| spec.risk != fluxdb_core::ToolRisk::Write);
    }
    // 结果分页必须与实现同时进入目录：只注册实现会让模型看得到 spec 却调不动，
    // 反之只给 spec 则 describe 得到、execute 失败。
    specs.push(result_read_page_spec()?);
    let mut catalog = ToolCatalog::new(specs);
    catalog.register(Box::new(AiSqlReadTool::query_with_results(
        Arc::clone(&resolver),
        Arc::clone(&results),
    )?))?;
    catalog.register(Box::new(AiSqlReadTool::explain_with_results(
        resolver.clone(),
        Arc::clone(&results),
    )?))?;
    catalog.register(Box::new(AiSqlSearchTool::new(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiSqlDescribeTool::new(Arc::clone(&resolver))?))?;
    // 结果分页与数据库类型无关，但必须共用同一仓库，句柄才读得到。
    catalog.register(Box::new(AiResultReadPageTool::new(Arc::clone(&results))?))?;
    if include_write {
        catalog.register(Box::new(AiSqlExecuteTool::new(resolver)?))?;
    }
    // 仓库随目录一起交出：回执登记溢写事实按目录查，不再由调用方另传一遍。
    Ok(catalog.with_results(results))
}
