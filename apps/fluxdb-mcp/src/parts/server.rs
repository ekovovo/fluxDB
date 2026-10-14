// rmcp ServerHandler：initialize / tools/list / tools/call 的协议适配（设计 §28.5、§28.8，T058）。
//
// 关键约束：
// - `tools/list` 动态投影当前 Profile 的可见能力；`tools/call` 即使名称已隐藏也再次检查权限
//   （见 `McpExecutionService::call` 内的 allowed_tool_ids 判定）；
// - 普通执行/权限失败映射为 `isError=true` 的 tool result（客户端可读）；
//   只有协议/未知方法用 JSON-RPC ErrorData；
// - 每次调用写入 `ai_mcp_server_requests`（独立 owner，不建假 Turn），并登记 session。

use std::sync::{Arc, Mutex};

use fluxdb_app::{
    McpAuditFact, McpCallError, McpCallIdentity, McpCallOutcome, McpExecutionService,
    McpRequestHost, McpToolView, mcp_tool_internal_id, visible_tools,
};
use fluxdb_storage::FileStorage;
use fluxdb_storage::Storage as _;
use fluxdb_storage::ai_platform::mcp_server_runtime::{
    finish_server_request, load_server_request, start_server_request, start_server_session,
    touch_server_session,
};
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};

/// 加载好的运行目标：已装配的执行服务 + 工具视图 + 审计所需的存储句柄。
///
/// 整体放进 `Arc`：`tools/call` 会把执行放到阻塞线程池（工具内部会建立自己的
/// tokio runtime 并 block_on，不能在 rmcp 的 async worker 上直接跑）。
pub struct ServeTarget {
    storage: FileStorage,
    service: Arc<McpExecutionService>,
    tools: Vec<McpToolView>,
    profile_id: String,
    session_id: String,
}

impl ServeTarget {
    /// 从存储加载 Profile、授权与连接并装配执行服务。
    ///
    /// Profile 不存在/禁用 → 明确错误；不默认开放所有已有连接。
    pub fn load(args: &crate::parts::cli::Args) -> Result<Self, String> {
        let storage = args.storage()?;
        let (profile, grants, disabled) = storage
            .with_ai_connection(|conn| {
                let profile = fluxdb_storage::ai_platform::mcp_server::load_server_profile(
                    conn,
                    &args.server_profile,
                )?
                .ok_or_else(|| {
                    fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        format!("Server Profile 不存在: {}", args.server_profile),
                    )
                })?;
                let grants = fluxdb_storage::ai_platform::mcp_server::list_resource_grants(
                    conn,
                    &profile.id,
                )?;
                let disabled = fluxdb_storage::ai_platform::tool_settings::disabled_tool_ids(conn)?;
                Ok((profile, grants, disabled))
            })
            .map_err(|error| error.message)?;
        if !profile.enabled {
            return Err(format!("Server Profile 已禁用: {}", profile.id));
        }
        let profile = filter_globally_disabled(profile, &disabled);
        let connections = storage.load_connections().map_err(|e| e.message)?;
        let tools = visible_tools(&profile, &grants).map_err(|e| e.message)?;

        // 结果仓库与内部一致：句柄只在本次 session 内可读。
        let session_id = format!("mcp-{}", uuid::Uuid::new_v4());
        let results = Arc::new(Mutex::new(fluxdb_app::ResultStore::for_turn(
            fluxdb_app::RESULT_STORE_MEMORY_BYTES,
            &session_id,
            &session_id,
            None,
        )));
        let service = McpExecutionService::new(connections, profile.clone(), grants, results)
            .map_err(|e| e.message)?;
        // 注入 request 仓储：operation.status/cancel 由宿主实现，服务保持无 IO。
        let host = Arc::new(StorageRequestHost {
            storage: storage.clone(),
            profile_id: profile.id.clone(),
            session_id: session_id.clone(),
        });
        let service = Arc::new(service.with_request_host(host));
        Ok(Self {
            storage,
            service,
            tools,
            profile_id: profile.id.clone(),
            session_id,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name.clone()).collect()
    }

    fn tool(&self, name: &str) -> Option<Tool> {
        self.tools
            .iter()
            .find(|view| view.name == name)
            .map(|view| {
                let schema: serde_json::Map<String, serde_json::Value> =
                    view.input_schema.as_object().cloned().unwrap_or_else(|| {
                        serde_json::json!({"type":"object"})
                            .as_object()
                            .cloned()
                            .unwrap()
                    });
                let mut tool = Tool::new(
                    view.name.clone(),
                    view.description.clone(),
                    Arc::new(schema),
                );
                tool.annotations = Some(tool_annotations(view.risk));
                tool
            })
    }
}

/// 从 Profile 的 allowed_tool_ids 中剔除全局停用工具（用内部工具 ID 比对）。
fn filter_globally_disabled(
    mut profile: fluxdb_storage::ai_platform::mcp_server::ServerProfileView,
    disabled: &std::collections::BTreeSet<String>,
) -> fluxdb_storage::ai_platform::mcp_server::ServerProfileView {
    profile
        .allowed_tool_ids
        .retain(|mcp_name| match mcp_tool_internal_id(mcp_name) {
            Some(internal) => !disabled.contains(internal),
            None => true,
        });
    profile
}

/// rmcp ServerHandler 实现：把协议请求映射到共享执行服务。
pub struct FluxDbMcpServer {
    target: Arc<ServeTarget>,
    started: Mutex<bool>,
}

impl FluxDbMcpServer {
    pub fn new(target: ServeTarget) -> Result<Self, String> {
        Ok(Self {
            target: Arc::new(target),
            started: Mutex::new(false),
        })
    }

    fn ensure_session(&self, protocol_version: &str, client_info_json: &str) {
        let mut started = self.started.lock().expect("session 状态锁");
        if *started {
            return;
        }
        let now = now_ms();
        if let Err(error) = self.target.storage.with_ai_connection(|conn| {
            start_server_session(
                conn,
                now,
                &self.target.session_id,
                &self.target.profile_id,
                protocol_version,
                client_info_json,
            )
            .map(|_| ())
        }) {
            tracing::warn!(%error, "MCP session 记录失败（不阻断服务）");
        }
        *started = true;
    }

    fn tool_result(&self, outcome: McpCallOutcome) -> CallToolResult {
        let mut result = CallToolResult::success(vec![Content::text(outcome.text.clone())]);
        result.structured_content = Some(outcome.structured);
        result.is_error = Some(!outcome.ok);
        result
    }

    fn record_request(
        &self,
        identity: &McpCallIdentity,
        mcp_name: &str,
        arguments_json: &str,
        now: i64,
    ) {
        let _ = self
            .target
            .storage
            .with_ai_connection(|conn| touch_server_session(conn, now, &self.target.session_id));
        let view = self.target.storage.with_ai_connection(|conn| {
            start_server_request(
                conn,
                now,
                &identity.request_id,
                &self.target.session_id,
                &self.target.profile_id,
                "\"\"",
                mcp_name,
                1,
                None,
                "{}",
                arguments_json,
                "pending",
            )
        });
        if let Err(error) = view {
            tracing::warn!(%error, "MCP request 记录失败（不阻断执行）");
        }
    }

    fn finish_request(&self, identity: &McpCallIdentity, outcome: &McpCallOutcome, now: i64) {
        let status = McpExecutionService::terminal_status(outcome);
        let receipt = serde_json::json!({
            "ok": outcome.ok,
            "request_id": identity.request_id,
        })
        .to_string();
        let report = self.target.storage.with_ai_connection(|conn| {
            // 先追加不可变审计事实（绑定 mcp_request_id，不建假 Turn）。
            if let Some(fact) = outcome.audit.as_ref() {
                persist_audit_fact(conn, now, &identity.request_id, fact)?;
            }
            let current = load_server_request(conn, &identity.request_id)?;
            finish_server_request(
                conn,
                now,
                &identity.request_id,
                status,
                Some(&receipt),
                current.revision,
            )
            .map(|_| ())
        });
        if let Err(error) = report {
            tracing::warn!(%error, "MCP request 终态记录失败");
        }
    }
}

impl ServerHandler for FluxDbMcpServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        ServerInfo::new(capabilities)
            .with_server_info(Implementation::new("fluxdb-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "fluxDB 本地 MCP Server：仅暴露本 Server Profile 已授权的连接/库/Key 范围。"
                    .to_string(),
            )
    }

    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ServerInfo, ErrorData> {
        let client_info =
            serde_json::to_string(&request.client_info).unwrap_or_else(|_| "{}".into());
        self.ensure_session(request.protocol_version.as_str(), &client_info);
        // 记录客户端身份用于审计；clientInfo 不是授权凭据。
        context.peer.set_peer_info(request);
        Ok(self.get_info())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .target
            .tools
            .iter()
            .filter_map(|view| self.target.tool(&view.name))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.target.tool(name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let mcp_name = request.name.to_string();
        let arguments_json = crate::parts::tools::arguments_to_json(request.arguments.as_ref());
        let identity = McpCallIdentity {
            session_id: self.target.session_id.clone(),
            request_id: format!("mcp-req-{}", uuid::Uuid::new_v4()),
        };
        let now = now_ms();
        self.record_request(&identity, &mcp_name, &arguments_json, now);
        // 工具执行会建立自己的 tokio runtime 并 block_on；必须在阻塞线程池上跑，
        // 不能占用 rmcp 的 async worker（否则会 panic / 挂死请求）。
        let target = Arc::clone(&self.target);
        let name_for_task = mcp_name.clone();
        let args_for_task = arguments_json.clone();
        let identity_for_task = identity.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let cancel = fluxdb_core::CancelFlag::new();
            target
                .service
                .call(&identity_for_task, &name_for_task, &args_for_task, &cancel)
        })
        .await
        .unwrap_or_else(|_| {
            McpCallOutcome::failure_of(fluxdb_app::McpCallError::new(
                "execution_panicked",
                "工具执行线程异常结束",
                false,
            ))
        });
        self.finish_request(&identity, &outcome, now);
        Ok(self.tool_result(outcome))
    }
}

/// 把强制审计事实落到 `ai_tool_audits`（MCP owner 分支，append-only）。
///
/// 通过 `fluxdb_storage` 再导出的连接类型接收连接，避免二进制直接依赖 rusqlite。
fn persist_audit_fact(
    conn: &fluxdb_storage::AiConnection,
    now: i64,
    request_id: &str,
    fact: &McpAuditFact,
) -> fluxdb_core::Result<()> {
    use fluxdb_storage::ai_platform::audits::{
        AuditDecision, AuditStage, ToolAuditEdit, insert_tool_audit,
    };
    let decision = match fact.decision {
        "allow" => AuditDecision::Allow,
        "deny" => AuditDecision::Deny,
        _ => AuditDecision::RequireApproval,
    };
    let edit = ToolAuditEdit {
        id: format!("mcp-audit-{request_id}"),
        tool_call_id: None,
        mcp_request_id: Some(request_id.to_string()),
        stage: AuditStage::PreExecute,
        hook_id: "kernel.security-audit".into(),
        hook_version: "1".into(),
        ruleset_version: "kernel-v1".into(),
        plan_hash: fact.plan_hash.clone(),
        decision,
        risk: fact.risk.clone(),
        rule_ids: Vec::new(),
        reason_codes: fact.reason_codes.clone(),
    };
    insert_tool_audit(conn, now, &edit)
}

/// 宿主 request 仓储：用存储读写 `ai_mcp_server_requests`，只允许本 session 的请求。
struct StorageRequestHost {
    storage: FileStorage,
    profile_id: String,
    session_id: String,
}

impl McpRequestHost for StorageRequestHost {
    fn request_status(
        &self,
        _identity: &McpCallIdentity,
        request_id: &str,
    ) -> Result<serde_json::Value, McpCallError> {
        let view = self
            .storage
            .with_ai_connection(|conn| load_server_request(conn, request_id))
            .map_err(|error| McpCallError::new("request_unavailable", error.message, false))?;
        if view.profile_id != self.profile_id || view.session_id != self.session_id {
            // 另一个 session/profile 的请求不可见，不泄漏其存在。
            return Err(McpCallError::new(
                "request_not_found",
                "请求不存在或不属于当前会话",
                false,
            ));
        }
        Ok(serde_json::json!({
            "ok": true,
            "request_id": view.id,
            "tool_id": view.tool_id,
            "status": view.status.as_str(),
            "outcome": if view.status.is_terminal() { "terminal" } else { "running" },
        }))
    }

    fn cancel_request(
        &self,
        _identity: &McpCallIdentity,
        request_id: &str,
    ) -> Result<serde_json::Value, McpCallError> {
        // 取消语义：把仍未终态的请求标为 cancelled；真正的执行中断由调用方 cancel flag 承担。
        let cancelled = self
            .storage
            .with_ai_connection(|conn| {
                let view = load_server_request(conn, request_id)?;
                if view.profile_id != self.profile_id || view.session_id != self.session_id {
                    return Ok(None);
                }
                if view.status.is_terminal() {
                    return Ok(Some(view));
                }
                finish_server_request(
                    conn,
                    now_ms(),
                    request_id,
                    fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Cancelled,
                    None,
                    view.revision,
                )
                .map(Some)
            })
            .map_err(|error| McpCallError::new("request_unavailable", error.message, false))?;
        match cancelled {
            Some(view) => Ok(serde_json::json!({
                "ok": true,
                "request_id": view.id,
                "status": view.status.as_str(),
            })),
            None => Err(McpCallError::new(
                "request_not_found",
                "请求不存在或不属于当前会话",
                false,
            )),
        }
    }
}

fn tool_annotations(risk: fluxdb_core::ToolRisk) -> rmcp::model::ToolAnnotations {
    rmcp::model::ToolAnnotations::new()
        .read_only(risk == fluxdb_core::ToolRisk::Read)
        .destructive(risk == fluxdb_core::ToolRisk::Write)
        .idempotent(false)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// stdio 单行帧硬上限（设计 §28.8：限 frame bytes，超限连接丢弃）。
///
/// MCP stdio 用换行分隔 JSON-RPC 帧；SDK 默认不设上限，恶意/异常客户端可用超长行撑爆内存。
/// 这里在读取侧包一层限制器：单行超过上限即返回 IO 错误，连接被丢弃。
const MAX_INPUT_LINE_BYTES: usize = 1024 * 1024;

/// 逐行字节计数的读包装：连续读到一个换行前超过 `max_line` 即报错。
struct BoundedLineReader<R> {
    inner: R,
    max_line: usize,
    since_newline: usize,
}

impl<R> BoundedLineReader<R> {
    fn new(inner: R, max_line: usize) -> Self {
        Self {
            inner,
            max_line,
            since_newline: 0,
        }
    }
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for BoundedLineReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let start = buf.filled().len();
        let poll = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &poll {
            let bytes = buf.filled().get(start..).unwrap_or(&[]);
            for byte in bytes {
                if *byte == b'\n' {
                    self.since_newline = 0;
                } else {
                    self.since_newline += 1;
                    if self.since_newline > self.max_line {
                        return std::task::Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "MCP 输入帧超过上限，连接已丢弃",
                        )));
                    }
                }
            }
        }
        poll
    }
}

/// 启动 stdio 服务并等待结束（EOF/客户端断开）。
pub async fn serve_stdio(server: FluxDbMcpServer) -> Result<(), String> {
    use rmcp::ServiceExt;
    let (stdin, stdout) = rmcp::transport::stdio();
    let bounded = BoundedLineReader::new(stdin, MAX_INPUT_LINE_BYTES);
    let running = server
        .serve((bounded, stdout))
        .await
        .map_err(|error| format!("MCP 初始化失败: {error}"))?;
    running
        .waiting()
        .await
        .map_err(|error| format!("MCP 服务异常结束: {error}"))?;
    Ok(())
}
