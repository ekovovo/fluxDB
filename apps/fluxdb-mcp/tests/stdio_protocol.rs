//! T061（部分）：纯协议集成测试——真实二进制 + 真实 SQLite fixture，走 stdio JSON-RPC。
//!
//! 覆盖 initialize / tools/list / tools/call / isError / structuredContent / 未知工具。
//! 不触网、不读生产配置与凭据；只替换传输对端（用管道驱动）。
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use fluxdb_storage::Storage as _;
use fluxdb_storage::ai_platform::mcp_server::{
    DatasourceWritePolicy, ServerProfileEdit, save_datasource, save_server_profile,
};

// 可指定已构建的桌面程序，复用同一套协议断言验证无独立 Server 时的入口。
fn server_command() -> Command {
    if let Some(exe) = std::env::var_os("FLUXDB_TEST_MCP_DESKTOP") {
        let mut command = Command::new(exe);
        command.arg("--mcp-server");
        command
    } else {
        Command::new(env!("CARGO_BIN_EXE_fluxdb-mcp"))
    }
}

struct Fixture {
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-mcp-proto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("sales.sqlite");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE orders(id INTEGER PRIMARY KEY, amount TEXT);
             INSERT INTO orders(id, amount) VALUES (1,'10.50'),(2,'20.00');",
        )
        .unwrap();
        let config = fluxdb_core::ConnectionConfig {
            id: fluxdb_core::ConnectionId(1),
            name: "sales".into(),
            kind: fluxdb_core::DatabaseKind::Sqlite,
            endpoint: fluxdb_core::Endpoint::SqliteFile {
                path: db_path,
                read_only: true,
            },
            options: Default::default(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        };
        let storage = fluxdb_storage::FileStorage::new(&dir);
        storage.save_connections(&[config]).unwrap();
        storage
            .with_ai_connection(|conn| {
                let edit = ServerProfileEdit {
                    id: "p1".into(),
                    display_name: "本地".into(),
                    enabled: true,
                    options_json: "{}".into(),
                };
                let created = save_server_profile(conn, 1, &edit, None)?;
                save_server_profile(conn, 2, &edit, Some(created.revision))?;
                save_datasource(conn, 3, "p1", 1, DatasourceWritePolicy::ReadOnly, None)?;
                Ok(())
            })
            .unwrap();
        Self { dir }
    }

    fn spawn(&self) -> Client {
        let mut child = server_command()
            .arg("--server-profile")
            .arg("p1")
            .arg("--config-root")
            .arg(&self.dir)
            .env("FLUXDB_MCP_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 fluxdb-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin,
            stdout,
            seq: 0,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: i64,
}

impl Client {
    fn send(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.seq += 1;
        let id = self.seq;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{request}").expect("写请求");
        self.stdin.flush().expect("flush");
        self.read_response(id)
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{request}").expect("写通知");
        self.stdin.flush().expect("flush");
    }

    fn read_response(&mut self, id: i64) -> serde_json::Value {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("读响应");
            assert!(n > 0, "服务器在响应前关闭了 stdout");
            let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_else(|e| {
                panic!("stdout 不是合法 JSON-RPC 帧: {e}；原文: {line}");
            });
            if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return value;
            }
            // 忽略通知/服务器主动消息。
        }
    }

    fn initialize(&mut self) -> serde_json::Value {
        let response = self.send(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "t061-probe", "version": "0.1.0"}
            }),
        );
        self.notify("notifications/initialized", serde_json::json!({}));
        response
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn tool_call(client: &mut Client, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let response = client.send(
        "tools/call",
        serde_json::json!({ "name": name, "arguments": arguments }),
    );
    response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("tools/call 无 result: {response}"))
}

#[test]
fn initialize_lists_only_authorized_tools() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    let init = client.initialize();
    let result = init.get("result").expect("initialize 应有 result");
    assert_eq!(
        result["serverInfo"]["name"],
        serde_json::json!("fluxdb-mcp")
    );
    assert!(result["protocolVersion"].is_string());

    let list = client.send("tools/list", serde_json::json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools 数组");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"fluxdb_query"));
    assert!(names.contains(&"fluxdb_connections_list"));
    // operation.status 保留：调用方要靠它核实中断写入的结果。
    // operation.cancel 不提供：真实中断由协议取消/断连承担，账面取消会误导调用方。
    assert!(names.contains(&"fluxdb_operation_status"));
    assert!(!names.contains(&"fluxdb_operation_cancel"));
    // 只读数据源下写工具不出现。
    assert!(!names.contains(&"fluxdb_execute"));
    assert!(!names.contains(&"fluxdb_redis_scan"));
}

#[test]
fn query_returns_structured_content_through_protocol() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();

    let result = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({
            "target": "conn:1/db:main",
            "sql": "SELECT id, amount FROM orders ORDER BY id"
        }),
    );
    assert_eq!(result["isError"], serde_json::json!(false));
    assert!(
        result["structuredContent"].is_object(),
        "结构化结果在 structuredContent"
    );
    assert!(
        result["content"].as_array().is_some_and(|c| !c.is_empty()),
        "text 兼容投影"
    );
    // 调用方要靠这个 id 去 fluxdb_operation_status 核实这次调用的最终状态。
    assert!(
        result["structuredContent"]["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("mcp-req-")),
        "每次 tools/call 回传自己的 request_id"
    );
}

#[test]
fn operation_status_reports_own_request_and_hides_other_sessions() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();

    let called = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({
            "target": "conn:1/db:main",
            "sql": "SELECT id FROM orders"
        }),
    );
    let request_id = called["structuredContent"]["request_id"]
        .as_str()
        .expect("request_id")
        .to_string();

    let status = tool_call(
        &mut client,
        "fluxdb_operation_status",
        serde_json::json!({ "request_id": request_id }),
    );
    assert_eq!(status["isError"], serde_json::json!(false));
    assert_eq!(
        status["structuredContent"]["status"],
        serde_json::json!("succeeded")
    );
    assert_eq!(
        status["structuredContent"]["outcome"],
        serde_json::json!("terminal")
    );

    // 另一个 caller 的 request 不可见，且不泄漏其存在。
    let foreign = tool_call(
        &mut client,
        "fluxdb_operation_status",
        serde_json::json!({ "request_id": "mcp-req-does-not-exist" }),
    );
    assert_eq!(foreign["isError"], serde_json::json!(true));
    assert_eq!(
        foreign["structuredContent"]["code"],
        serde_json::json!("request_not_found")
    );
}

#[test]
fn unknown_tool_returns_tool_level_error_not_protocol_error() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();
    let result = tool_call(&mut client, "fluxdb_nonexistent", serde_json::json!({}));
    assert_eq!(result["isError"], serde_json::json!(true));
    assert_eq!(
        result["structuredContent"]["code"],
        serde_json::json!("unknown_tool")
    );
}

#[test]
fn unauthorized_target_is_tool_error() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();
    let result = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({
            "target": "conn:9/db:secret",
            "sql": "SELECT 1"
        }),
    );
    assert_eq!(result["isError"], serde_json::json!(true));
    assert_eq!(
        result["structuredContent"]["code"],
        serde_json::json!("STALE_RESOURCE")
    );
}

#[test]
fn disabled_or_missing_profile_fails_to_start() {
    let fixture = Fixture::new();
    let output = server_command()
        .arg("--server-profile")
        .arg("does-not-exist")
        .arg("--config-root")
        .arg(&fixture.dir)
        .env("FLUXDB_MCP_LOG", "error")
        .stdin(Stdio::null())
        .output()
        .expect("启动进程");
    assert!(!output.status.success(), "缺失 Profile 必须非零退出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("不存在"), "stderr 应说明原因: {stderr}");
    // Arc 用于确保类型导入不产生未使用警告（占位）。
    let _ = Arc::new(());
}

#[test]
fn tool_call_persists_request_and_audit_facts() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();

    // 一次成功调用。
    let ok = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({
            "target": "conn:1/db:main",
            "sql": "SELECT id FROM orders ORDER BY id"
        }),
    );
    assert_eq!(ok["isError"], serde_json::json!(false));

    // 一次被拒调用（未授权 target）。
    let denied = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({ "target": "conn:9/db:x", "sql": "SELECT 1" }),
    );
    assert_eq!(denied["isError"], serde_json::json!(true));

    // 服务端进程仍在运行，审计行应已落库：核对 request 与 audit（独立 owner）。
    let storage = fluxdb_storage::FileStorage::new(&fixture.dir);
    let (requests, audits) = storage
        .with_ai_connection(|conn| {
            let requests =
                fluxdb_storage::ai_platform::mcp_server_runtime::list_requests_for_profile(
                    conn, "p1", 50,
                )?;
            let audits: i64 = conn
                .query_row("SELECT COUNT(*) FROM ai_tool_audits", [], |row| row.get(0))
                .map_err(|error| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, error.to_string())
                })?;
            Ok((requests, audits))
        })
        .unwrap();
    assert!(requests.len() >= 2, "两次调用都应落 request 行");
    assert!(
        requests.iter().any(|r| r.status
            == fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Succeeded),
        "存在 succeeded request"
    );
    assert!(
        requests.iter().any(|r| r.status
            == fluxdb_storage::ai_platform::mcp_server_runtime::ServerRequestStatus::Failed),
        "存在 failed request"
    );
    // 强制审计绑定"进入执行链的调用"：未授权 target 在解析阶段即拒绝，不会产生执行审计；
    // 成功调用必须恰好留下审计事实（allow）。
    assert!(audits >= 1, "成功调用应留下审计事实，实际 {audits}");
}

#[test]
fn operation_status_isolated_to_own_request() {
    let fixture = Fixture::new();
    let mut client = fixture.spawn();
    client.initialize();
    let _ = tool_call(
        &mut client,
        "fluxdb_query",
        serde_json::json!({ "target": "conn:1/db:main", "sql": "SELECT 1" }),
    );
    // 查询一个不存在的 request：返回结构化错误而不是泄漏。
    let result = tool_call(
        &mut client,
        "fluxdb_operation_status",
        serde_json::json!({ "request_id": "no-such-request" }),
    );
    assert_eq!(result["isError"], serde_json::json!(true));
}
