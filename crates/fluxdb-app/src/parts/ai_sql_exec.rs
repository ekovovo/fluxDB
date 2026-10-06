// AI SQL 执行服务（设计 §20–21，T023/T024）。
//
// 目标：把「元数据探索 + 受控 SQL 执行」做成**独立服务**，供 Agent 工具（将来也给 MCP）
// 复用同一执行链；不依赖 GPUI，也不复用编辑器标签的会话——Agent 用隔离连接执行，
// 避免与用户编辑器的临时表 / 事务互相串（设计 §21.6）。
//
// 硬规则（每条都有对应测试）：
// 1. **单条语句**：`split_statements=false`、`continue_on_error=false`，多语句由策略层拒绝；
// 2. **只读入口只放只读**：`db.query`/`db.explain` 只执行分类为 `ReadOnly` 的语句，
//    其余一律拒绝并给出可见原因，不"降级执行"；
// 3. **结果有界**：`max_rows` 行 + 字节上限，超限只标记 `truncated`，不静默丢；
// 4. **可取消**：在连接器检查点轮询 `ToolExecutionContext.cancel`，取消返回真实状态；
// 5. **参数化**：当前 Connector 尚无 bind 能力，因此**拒绝**带参数的请求（返回受控错误），
//    而不是退化成字符串拼接（设计 §20.3）。

/// 连接解析：把 AI 的 target 句柄映射到连接配置。
///
/// 只暴露非敏感配置（kind/endpoint/options）；凭据解密发生在 connector 层，
/// 不经过这里，也不进任何事件/日志。
pub trait SqlExecutionResolver: Send + Sync {
    fn connection_config(
        &self,
        id: fluxdb_core::ConnectionId,
    ) -> Option<fluxdb_core::ConnectionConfig>;
}

/// 内存实现：给测试与未来运行时装"当前可见连接"用。
pub struct MapSqlResolver {
    configs: HashMap<fluxdb_core::ConnectionId, fluxdb_core::ConnectionConfig>,
}

impl MapSqlResolver {
    pub fn new(configs: impl IntoIterator<Item = fluxdb_core::ConnectionConfig>) -> Self {
        Self {
            configs: configs
                .into_iter()
                .map(|config| (config.id, config))
                .collect(),
        }
    }
}

impl SqlExecutionResolver for MapSqlResolver {
    fn connection_config(
        &self,
        id: fluxdb_core::ConnectionId,
    ) -> Option<fluxdb_core::ConnectionConfig> {
        self.configs.get(&id).cloned()
    }
}

/// 结果上限（工具层硬上限，模型不能放大）。
pub const SQL_MAX_ROWS_HARD: u64 = 1000;
pub const SQL_MAX_ROWS_DEFAULT: u64 = 200;
pub const SQL_RESULT_MAX_BYTES: usize = 128 * 1024;

/// 受控执行错误（作为 ToolReceipt 的 `error_code`，不是 panic/Err）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlExecError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl SqlExecError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }
}

/// AI 执行目标（不含凭据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiSqlTarget {
    pub connection_id: fluxdb_core::ConnectionId,
    pub database: Option<String>,
    pub schema: Option<String>,
}

impl AiSqlTarget {
    /// 解析内核提供的资源句柄：`conn:<id>[/db:<database>][/schema:<schema>]`。
    ///
    /// 句柄是**定位信息**，不是连接字符串：解析结果只用于选连接与默认作用域，
    /// 不允许模型借它注入任意 endpoint。
    pub fn parse(handle: &str) -> Result<Self, SqlExecError> {
        let mut parts = handle.split('/');
        let head = parts.next().unwrap_or_default();
        let id_text = head
            .strip_prefix("conn:")
            .or_else(|| head.strip_prefix("connection:"))
            .ok_or_else(|| {
                SqlExecError::new(
                    "invalid_target",
                    "target 必须是内核句柄（conn:<id>[/db:<database>][/schema:<schema>]）",
                )
            })?;
        let connection_id = id_text.parse::<u64>().map_err(|_| {
            SqlExecError::new("invalid_target", format!("target 连接 ID 非法: {id_text}"))
        })?;
        let mut database = None;
        let mut schema = None;
        for part in parts {
            if let Some(value) = part.strip_prefix("db:") {
                database = Some(value.to_string());
            } else if let Some(value) = part.strip_prefix("schema:") {
                schema = Some(value.to_string());
            } else if !part.is_empty() {
                return Err(SqlExecError::new(
                    "invalid_target",
                    format!("target 片段无法识别: {part}"),
                ));
            }
        }
        Ok(Self {
            connection_id: fluxdb_core::ConnectionId(connection_id),
            database,
            schema,
        })
    }
}

/// 解析后的 SQL 工具入参。
#[derive(Clone, Debug)]
struct ParsedSqlArgs {
    target: AiSqlTarget,
    sql: String,
    max_rows: u64,
    /// `db.explain` 的模式（`plan` 默认；`execute` 当前不开放）。
    mode: Option<String>,
    /// 精确 tagged 参数（驱动真实 bind；不支持绑定的连接器会明确拒绝）。
    parameters: Vec<fluxdb_core::SqlParameter>,
}

fn parse_sql_args(arguments_json: &str) -> Result<ParsedSqlArgs, SqlExecError> {
    let value: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|_| SqlExecError::new("invalid_arguments", "工具入参不是合法 JSON"))?;
    let handle = value
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SqlExecError::new("invalid_arguments", "缺少 target"))?;
    let sql = value
        .get("sql")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SqlExecError::new("invalid_arguments", "缺少 sql"))?
        .to_string();
    if sql.trim().is_empty() {
        return Err(SqlExecError::new("invalid_arguments", "sql 不能为空"));
    }
    let parameters = parse_sql_parameters(value.get("parameters"))?;
    let max_rows = value
        .get("max_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(SQL_MAX_ROWS_DEFAULT)
        .clamp(1, SQL_MAX_ROWS_HARD);
    let mode = value
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    Ok(ParsedSqlArgs {
        target: AiSqlTarget::parse(handle)?,
        sql,
        max_rows,
        mode,
        parameters,
    })
}

/// 参数上限：schema 允许 1024 项，但执行链给更小的硬上限（避免一次调用把驱动压垮）。
const SQL_PARAM_MAX_ITEMS: usize = 64;
const SQL_PARAM_MAX_TEXT_BYTES: usize = 16 * 1024;
const SQL_PARAM_MAX_BYTES: usize = 64 * 1024;
const SQL_PARAM_MAX_DECIMAL_CHARS: usize = 64;

/// 解析设计 §20.2 的 tagged wire 值：`{type:"text",value:"..."}` / `{type:"null"}` / …
///
/// 只做**类型与范围校验**，值原样交给驱动 bind；这里绝不把值格式化成 SQL 文本。
fn parse_sql_parameters(
    value: Option<&serde_json::Value>,
) -> Result<Vec<fluxdb_core::SqlParameter>, SqlExecError> {
    use fluxdb_core::SqlParameter;
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        SqlExecError::new("invalid_parameters", "parameters 必须是 tagged 值数组")
    })?;
    if items.len() > SQL_PARAM_MAX_ITEMS {
        return Err(SqlExecError::new(
            "invalid_parameters",
            format!("参数个数超过上限 {SQL_PARAM_MAX_ITEMS}"),
        ));
    }
    let mut parameters = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let tag = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                SqlExecError::new("invalid_parameters", format!("parameters[{index}] 缺少 type"))
            })?;
        let text_value = || -> Result<&str, SqlExecError> {
            item.get("value")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 缺少字符串 value"),
                    )
                })
        };
        let parameter = match tag {
            "null" => SqlParameter::Null,
            "bool" => SqlParameter::Bool(
                item.get("value")
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| {
                        SqlExecError::new(
                            "invalid_parameters",
                            format!("parameters[{index}] 不是布尔值"),
                        )
                    })?,
            ),
            "text" => {
                let value = text_value()?;
                if value.len() > SQL_PARAM_MAX_TEXT_BYTES {
                    return Err(SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 文本超过 {SQL_PARAM_MAX_TEXT_BYTES} 字节"),
                    ));
                }
                SqlParameter::Text(value.to_string())
            }
            "i64" => {
                let value = text_value()?;
                value.parse::<i64>().map_err(|_| {
                    SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 不是合法 i64 文本"),
                    )
                })?;
                SqlParameter::I64(value.to_string())
            }
            "u64" => {
                let value = text_value()?;
                value.parse::<u64>().map_err(|_| {
                    SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 不是合法 u64 文本"),
                    )
                })?;
                SqlParameter::U64(value.to_string())
            }
            "f64" => {
                let value = item
                    .get("value")
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| {
                        SqlExecError::new(
                            "invalid_parameters",
                            format!("parameters[{index}] 不是数值"),
                        )
                    })?;
                if !value.is_finite() {
                    return Err(SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 必须是有限数值"),
                    ));
                }
                SqlParameter::F64(value)
            }
            "decimal" => {
                let value = text_value()?;
                if value.len() > SQL_PARAM_MAX_DECIMAL_CHARS
                    || !looks_like_decimal(value)
                {
                    return Err(SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 不是合法十进制文本"),
                    ));
                }
                SqlParameter::Decimal(value.to_string())
            }
            "bytes" => {
                let value = text_value()?;
                use base64::Engine as _;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .map_err(|_| {
                        SqlExecError::new(
                            "invalid_parameters",
                            format!("parameters[{index}] 不是合法 base64"),
                        )
                    })?;
                if decoded.len() > SQL_PARAM_MAX_BYTES {
                    return Err(SqlExecError::new(
                        "invalid_parameters",
                        format!("parameters[{index}] 字节数超过 {SQL_PARAM_MAX_BYTES}"),
                    ));
                }
                SqlParameter::Bytes(decoded)
            }
            other => {
                return Err(SqlExecError::new(
                    "invalid_parameters",
                    format!("parameters[{index}] 未知类型标记: {other}"),
                ))
            }
        };
        parameters.push(parameter);
    }
    Ok(parameters)
}

/// 只接受 `[+-]?digits[.digits]`（不做浮点转换，避免丢精度）。
fn looks_like_decimal(value: &str) -> bool {
    let body = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mut parts = body.split('.');
    let (Some(int_part), frac, None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if int_part.is_empty() || !int_part.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match frac {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

fn dialect_for(kind: fluxdb_core::DatabaseKind) -> Option<SqlDialectKind> {
    match kind {
        fluxdb_core::DatabaseKind::MySql | fluxdb_core::DatabaseKind::TiDb => {
            Some(SqlDialectKind::MySql)
        }
        fluxdb_core::DatabaseKind::Postgres => Some(SqlDialectKind::Postgres),
        fluxdb_core::DatabaseKind::Sqlite => Some(SqlDialectKind::Sqlite),
        _ => None,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次受控 SQL 执行的结果（已按字节/行数有界）。
#[derive(Clone, Debug, PartialEq)]
pub struct SqlExecutionOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: u64,
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// 受影响行数（写语句）；只读为 None。
    pub affected_rows: Option<i64>,
    pub statement_kind: &'static str,
}

impl SqlExecutionOutcome {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "columns": self.columns,
            "rows": self.rows,
            "row_count": self.row_count,
            "truncated": self.truncated,
            "elapsed_ms": self.elapsed_ms,
            "affected_rows": self.affected_rows,
            "statement_kind": self.statement_kind,
        })
    }
}

fn cell_to_json(cell: &fluxdb_core::CellValue) -> serde_json::Value {
    match cell {
        fluxdb_core::CellValue::Null => serde_json::Value::Null,
        fluxdb_core::CellValue::Bool(v) => serde_json::Value::Bool(*v),
        fluxdb_core::CellValue::I64(v) => serde_json::json!(*v),
        fluxdb_core::CellValue::F64(v) => serde_json::json!(*v),
        fluxdb_core::CellValue::Text(v) => serde_json::Value::String(v.clone()),
        fluxdb_core::CellValue::Json(v) => serde_json::Value::String(v.clone()),
        fluxdb_core::CellValue::Bytes(bytes) => {
            serde_json::json!({ "bytes": bytes.len() })
        }
        fluxdb_core::CellValue::BinarySummary(summary) => serde_json::json!({
            "type": summary.type_name,
            "is_null": summary.is_null,
            "byte_length": summary.byte_length,
        }),
    }
}

/// 复用既有 scope 执行链：单条语句、隔离会话、有界页、可取消。
///
/// `read_only` 为 true 时先过 §21 分类器；分类不是 `ReadOnly` 直接拒绝。
pub fn execute_sql(
    resolver: &dyn SqlExecutionResolver,
    target: &AiSqlTarget,
    sql: &str,
    max_rows: u64,
    read_only: bool,
    should_cancel: &dyn Fn() -> bool,
) -> Result<SqlExecutionOutcome, SqlExecError> {
    execute_sql_with_parameters(
        resolver,
        target,
        sql,
        max_rows,
        &[],
        read_only,
        should_cancel,
    )
}

/// 带精确参数（驱动真实 bind）的受控执行；无参数时等价于 [`execute_sql`]。
pub fn execute_sql_with_parameters(
    resolver: &dyn SqlExecutionResolver,
    target: &AiSqlTarget,
    sql: &str,
    max_rows: u64,
    parameters: &[fluxdb_core::SqlParameter],
    read_only: bool,
    should_cancel: &dyn Fn() -> bool,
) -> Result<SqlExecutionOutcome, SqlExecError> {
    let config = resolver
        .connection_config(target.connection_id)
        .ok_or_else(|| {
            SqlExecError::new(
                "connection_not_found",
                format!("连接不存在: {}", target.connection_id.0),
            )
        })?;
    let dialect = dialect_for(config.kind)
        .ok_or_else(|| SqlExecError::new("unsupported_database", "该连接类型不支持 SQL 工具"))?;

    let risk = analyze_sql(sql, dialect);
    // 结构性拒绝（多语句、事务控制）优先给出更具体的原因。
    if let SqlRisk::Rejected { reasons } = &risk {
        return Err(SqlExecError::new(
            "rejected_sql",
            format!("语句被策略拒绝（{}）", reasons.join(",")),
        ));
    }
    if read_only && !risk.is_read_only() {
        return Err(SqlExecError::new(
            "not_read_only",
            format!("只读入口拒绝该语句（{}）", risk.reasons().join(",")),
        ));
    }

    let connector = fluxdb_connectors::connector_for(&config).map_err(|error| {
        SqlExecError::new("connector_error", format!("无法创建连接器: {error}"))
    })?;

    let bounded = sql_text_for_execution(sql, max_rows, config.kind);
    let request = fluxdb_core::QueryRequest {
        connection_id: target.connection_id,
        database: target.database.clone(),
        schema: target.schema.clone(),
        text: bounded,
        mode: fluxdb_core::QueryMode::Selection,
        options: fluxdb_core::QueryExecutionOptions {
            // AI 路径不做多语句 split，也不继续执行后续语句。
            continue_on_error: false,
            split_statements: false,
            page_offset: 0,
            page_size: max_rows,
        },
        // 隔离会话：不与用户编辑器的临时表/事务串用。
        session_id: None,
    };

    let started = Instant::now();
    let mut first_error: Option<String> = None;
    let execution = if parameters.is_empty() {
        connector
            .execute_with_progress(
                &request,
                &mut |summary| {
                    if !summary.success && first_error.is_none() {
                        first_error = Some(summary.message.clone());
                    }
                },
                should_cancel,
            )
            .map_err(|error| SqlExecError::new("execution_error", format!("执行失败: {error}")))?
    } else {
        // 有参数就必须真 bind；不支持绑定的连接器由默认实现明确拒绝（绝不插值）。
        connector
            .execute_parameterized(&fluxdb_core::ParameterizedQueryRequest {
                request: request.clone(),
                parameters: parameters.to_vec(),
            })
            .map_err(|error| {
                SqlExecError::new(
                    if error.kind == fluxdb_core::ErrorKind::Unsupported {
                        "parameters_unsupported"
                    } else {
                        "execution_error"
                    },
                    format!("参数化执行失败: {}", error.message),
                )
            })?
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if should_cancel() {
        // 读操作取消 = 没有副作用，直接报告已取消；
        // 写操作在"取消信号晚到"的竞态下无法确定是否已提交，必须报告 unknown，
        // 交给 §T029 的恢复流程核实，既不能当成功，也不能当已取消。
        return Err(if read_only {
            SqlExecError::new("cancelled", "执行已取消")
        } else {
            SqlExecError::new(
                "cancel_race_unknown",
                "取消信号在写操作返回后才到达，是否已提交无法确定",
            )
        });
    }
    if let Some(message) = first_error {
        return Err(SqlExecError::new("execution_failed", message));
    }

    Ok(project_execution(&execution, max_rows, elapsed_ms))
}

/// 把连接器结果投影成有界 JSON：行数/字节双上限，超限只标记 truncated。
fn project_execution(
    execution: &fluxdb_core::QueryExecutionResult,
    max_rows: u64,
    elapsed_ms: u64,
) -> SqlExecutionOutcome {
    let mut columns = Vec::new();
    let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut truncated = false;
    let mut bytes = 0usize;
    let mut row_count = 0u64;
    let mut statement_kind = "command";
    let mut affected_rows = None;

    if let Some(summary) = execution.summaries.last() {
        statement_kind = match summary.kind {
            fluxdb_core::QueryStatementKind::ResultSet => "result_set",
            fluxdb_core::QueryStatementKind::Command => "command",
        };
        if summary.kind == fluxdb_core::QueryStatementKind::Command {
            affected_rows = Some(summary.affected_rows as i64);
        }
    }

    for page in &execution.results {
        if columns.is_empty() {
            columns = page.columns.iter().map(|c| c.name.clone()).collect();
        }
        if page.has_more {
            truncated = true;
        }
        for row in &page.rows {
            if row_count >= max_rows {
                truncated = true;
                break;
            }
            let projected: Vec<serde_json::Value> = row.values.iter().map(cell_to_json).collect();
            let row_bytes = serde_json::to_string(&projected)
                .map(|s| s.len())
                .unwrap_or(0);
            if bytes + row_bytes > SQL_RESULT_MAX_BYTES {
                truncated = true;
                break;
            }
            bytes += row_bytes;
            rows.push(projected);
            row_count += 1;
        }
        if truncated {
            break;
        }
    }

    SqlExecutionOutcome {
        columns,
        rows,
        row_count,
        truncated,
        elapsed_ms,
        affected_rows,
        statement_kind,
    }
}

/// 组装一个真实回执（时间戳 + 载荷/错误码）。
fn receipt_from(
    descriptor: &fluxdb_core::ToolDescriptor,
    call_id: &str,
    started_at_ms: i64,
    outcome: Result<SqlExecutionOutcome, SqlExecError>,
) -> fluxdb_core::ToolReceipt {
    let finished_at_ms = now_ms();
    match outcome {
        Ok(outcome) => fluxdb_core::ToolReceipt {
            call_id: call_id.to_string(),
            tool_id: descriptor.id.clone(),
            status: fluxdb_core::ToolReceiptStatus::Succeeded,
            result_json: outcome.to_json().to_string(),
            error_code: None,
            affected_rows: outcome.affected_rows,
            started_at_ms,
            finished_at_ms,
        },
        Err(error) => fluxdb_core::ToolReceipt {
            call_id: call_id.to_string(),
            tool_id: descriptor.id.clone(),
            status: match error.code {
                "cancelled" => fluxdb_core::ToolReceiptStatus::Cancelled,
                "cancel_race_unknown" => fluxdb_core::ToolReceiptStatus::Unknown,
                _ => fluxdb_core::ToolReceiptStatus::Failed,
            },
            result_json: serde_json::json!({
                "ok": false,
                "code": error.code,
                "message": error.message,
                "retryable": error.retryable,
            })
            .to_string(),
            error_code: Some(error.code.to_string()),
            affected_rows: None,
            started_at_ms,
            finished_at_ms,
        },
    }
}

/// 元数据探索：按名字模糊搜索表/视图（只读、有界、可取消）。
///
/// 复用连接器补全索引（各方言自己实现），不 dump 全库 DDL。
pub fn search_objects(
    resolver: &dyn SqlExecutionResolver,
    target: &AiSqlTarget,
    query: &str,
    limit: u64,
    should_cancel: &dyn Fn() -> bool,
) -> Result<serde_json::Value, SqlExecError> {
    let config = resolver
        .connection_config(target.connection_id)
        .ok_or_else(|| {
            SqlExecError::new(
                "connection_not_found",
                format!("连接不存在: {}", target.connection_id.0),
            )
        })?;
    let bounded = limit.clamp(1, 100);
    let connector = fluxdb_connectors::connector_for(&config).map_err(|error| {
        SqlExecError::new("connector_error", format!("无法创建连接器: {error}"))
    })?;
    let tables = connector
        .list_completion_tables_with_cancel(
            target.database.as_deref(),
            target.schema.as_deref(),
            query,
            bounded,
            should_cancel,
        )
        .map_err(|error| SqlExecError::new("metadata_error", format!("元数据检索失败: {error}")))?;
    if should_cancel() {
        return Err(SqlExecError::new("cancelled", "检索已取消"));
    }
    let items: Vec<serde_json::Value> = tables
        .iter()
        .map(|table| {
            serde_json::json!({
                "name": table.name,
                "database": table.database,
                "schema": table.schema,
                "kind": format!("{:?}", table.kind).to_lowercase(),
                "comment": table.comment,
            })
        })
        .collect();
    Ok(serde_json::json!({ "objects": items, "count": items.len() }))
}

/// 只读工具：`db.query`（SELECT 类）与 `db.explain`（普通 EXPLAIN）。
///
/// 两者共用同一只读执行链；差别只在描述与默认 schema 提示。
pub struct AiSqlReadTool<R: SqlExecutionResolver> {
    descriptor: fluxdb_core::ToolDescriptor,
    resolver: Arc<R>,
    /// `db.explain` 需要把入参 SQL 包成 plan（不执行）；`db.query` 原样执行。
    wrap_explain: bool,
}

impl<R: SqlExecutionResolver> AiSqlReadTool<R> {
    pub fn query(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Self::from_spec(resolver, "db.query", DB_QUERY_INPUT_SCHEMA, false)
    }

    pub fn explain(resolver: Arc<R>) -> fluxdb_core::Result<Self> {
        Self::from_spec(resolver, "db.explain", DB_EXPLAIN_INPUT_SCHEMA, true)
    }

    fn from_spec(
        resolver: Arc<R>,
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
        let wrap_explain = self.wrap_explain;
        Box::pin(async move {
            let outcome = match parse_sql_args(&arguments_json) {
                Ok(mut args) => {
                    let prepared = if wrap_explain {
                        apply_explain_mode(&mut args)
                    } else {
                        Ok(())
                    };
                    match prepared {
                        Ok(()) => execute_sql_with_parameters(
                            resolver.as_ref(),
                            &args.target,
                            &args.sql,
                            args.max_rows,
                            &args.parameters,
                            true,
                            &|| cancel.is_cancelled(),
                        ),
                        Err(error) => Err(error),
                    }
                }
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

/// 修改工具：`db.execute`（Write）。
///
/// 只由**审批通过之后**的运行时调用：本工具自身不判断审批，内核（`TurnRunner` 的
/// `authorize` + 审批行 plan_hash 复核）是唯一放行点；未接审批时不得注册给模型。
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
            let outcome = match parse_sql_args(&arguments_json) {
                Ok(args) => execute_sql_with_parameters(
                    resolver.as_ref(),
                    &args.target,
                    &args.sql,
                    args.max_rows,
                    &args.parameters,
                    // 修改入口：不做只读分类拦截，但仍受"单条语句/事务控制拒绝"约束。
                    false,
                    &|| cancel.is_cancelled(),
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
    let mut specs = standard_sql_tool_specs()?;
    if !include_write {
        specs.retain(|spec| spec.risk != fluxdb_core::ToolRisk::Write);
    }
    let mut catalog = ToolCatalog::new(specs);
    catalog.register(Box::new(AiSqlReadTool::query(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiSqlReadTool::explain(Arc::clone(&resolver))?))?;
    catalog.register(Box::new(AiSqlSearchTool::new(Arc::clone(&resolver))?))?;
    if include_write {
        catalog.register(Box::new(AiSqlExecuteTool::new(resolver)?))?;
    }
    Ok(catalog)
}
