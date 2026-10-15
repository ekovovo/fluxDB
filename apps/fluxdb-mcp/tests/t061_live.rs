//! T061 纯协议 MCP 集成测试（真实后端：MySQL / PostgreSQL / Redis）。
//!
//! 门控环境变量（未配置的分组整体跳过，不触达任何数据库；不打印凭据）：
//!   FLUXDB_T061_MYSQL=host:port:user:password:db
//!   FLUXDB_T061_PG=host:port:user:password:db
//!   FLUXDB_T061_REDIS=host:port:user:password:db
//!
//! 覆盖：initialize / tools/list（只读 scope、默认不暴露写）/ tools/call / schema /
//! structuredContent / isError / 未知工具 / 越权拒绝 / EOF 正常收尾 / Client info 不能提权。
//! 驱动真实 `fluxdb-mcp` 二进制，stdout 只解析 JSON-RPC；每阶段核对真实后端数据与审计事实。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use fluxdb_storage::Storage as _;
use fluxdb_storage::ai_platform::mcp_server::{
    DatasourceWritePolicy, ServerProfileEdit, save_datasource, save_server_profile,
};

/// host:port:user:password:db（password 允许为空，但冒号分隔位必须保留）。
fn live_params(env: &str) -> Option<(String, u16, String, String, String)> {
    let value = std::env::var(env).ok()?;
    let parts: Vec<&str> = value.splitn(5, ':').collect();
    if parts.len() != 5 {
        eprintln!("跳过 {env}：格式应为 host:port:user:password:db");
        return None;
    }
    let port: u16 = parts[1].parse().ok()?;
    Some((
        parts[0].to_string(),
        port,
        parts[2].to_string(),
        parts[3].to_string(),
        parts[4].to_string(),
    ))
}

fn mysql_config(
    id: u64,
    p: &(String, u16, String, String, String),
) -> fluxdb_core::ConnectionConfig {
    let mut options = BTreeMap::new();
    options.insert("username".to_string(), p.2.clone());
    options.insert("password".to_string(), p.3.clone());
    fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(id),
        name: "t061-mysql".into(),
        kind: fluxdb_core::DatabaseKind::MySql,
        endpoint: fluxdb_core::Endpoint::Tcp {
            host: p.0.clone(),
            port: p.1,
            database: Some(p.4.clone()),
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

fn pg_config(id: u64, p: &(String, u16, String, String, String)) -> fluxdb_core::ConnectionConfig {
    fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(id),
        name: "t061-pg".into(),
        kind: fluxdb_core::DatabaseKind::Postgres,
        endpoint: fluxdb_core::Endpoint::Tcp {
            host: p.0.clone(),
            port: p.1,
            database: Some(p.4.clone()),
        },
        options: BTreeMap::new(),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: Some(fluxdb_core::PostgresConnectionProfile {
            basic: fluxdb_core::PostgresBasicOptions {
                host: p.0.clone(),
                port: p.1,
                maintenance_database: p.4.clone(),
                username: p.2.clone(),
                password: fluxdb_core::SecretRef::inline(p.3.clone()),
            },
            ..Default::default()
        }),
    }
}

fn redis_config(
    id: u64,
    p: &(String, u16, String, String, String),
) -> fluxdb_core::ConnectionConfig {
    fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(id),
        name: "t061-redis".into(),
        kind: fluxdb_core::DatabaseKind::Redis,
        endpoint: fluxdb_core::Endpoint::Tcp {
            host: p.0.clone(),
            port: p.1,
            // Redis 逻辑库固定 0（T061 未启用多 DB）；target/授权都用 db:0。
            database: Some("0".to_string()),
        },
        // Key 范围收窄来自连接配置（数据源模型不再配置表/Key 范围）。
        options: BTreeMap::from([("ai_key_prefixes".to_string(), "t061:".to_string())]),
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    }
}

struct Harness {
    dir: std::path::PathBuf,
}

impl Harness {
    /// 建隔离配置根：写入连接 + Profile + 授权；返回句柄。
    fn new(
        connections: Vec<fluxdb_core::ConnectionConfig>,
        datasources: Vec<DatasourceWritePolicy>,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("fluxdb-t061-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = fluxdb_storage::FileStorage::new(&dir);
        storage.save_connections(&connections).unwrap();
        storage
            .with_ai_connection(|conn| {
                let profile = ServerProfileEdit {
                    id: "t061".into(),
                    display_name: "T061 协议".into(),
                    enabled: true,
                    options_json: "{}".into(),
                };
                let created = save_server_profile(conn, 1, &profile, None)?;
                save_server_profile(conn, 2, &profile, Some(created.revision))?;
                for (connection, policy) in connections.iter().zip(datasources) {
                    save_datasource(conn, 3, "t061", connection.id.0 as i64, policy, None)?;
                }
                Ok(())
            })
            .unwrap();
        Self { dir }
    }

    fn spawn(&self) -> Client {
        let exe = env!("CARGO_BIN_EXE_fluxdb-mcp");
        let mut child = Command::new(exe)
            .arg("--server-profile")
            .arg("t061")
            .arg("--config-root")
            .arg(&self.dir)
            .env("FLUXDB_MCP_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
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

    /// SQL 数据源策略：`write` 为 true 时允许读写，否则只读。
    fn sql_policy(write: bool) -> DatasourceWritePolicy {
        if write {
            DatasourceWritePolicy::ReadWrite
        } else {
            DatasourceWritePolicy::ReadOnly
        }
    }

    /// Redis 数据源策略：只读。
    fn redis_policy() -> DatasourceWritePolicy {
        DatasourceWritePolicy::ReadOnly
    }
}

impl Drop for Harness {
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
        let request = serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(self.stdin, "{request}").expect("写请求");
        self.stdin.flush().expect("flush");
        self.read_response(id)
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let request = serde_json::json!({"jsonrpc":"2.0","method":method,"params":params});
        writeln!(self.stdin, "{request}").expect("写通知");
        self.stdin.flush().expect("flush");
    }

    fn read_response(&mut self, id: i64) -> serde_json::Value {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("读响应");
            assert!(n > 0, "服务器在响应前关闭 stdout");
            let value: serde_json::Value = serde_json::from_str(line.trim())
                .unwrap_or_else(|e| panic!("stdout 非合法 JSON-RPC: {e}; 原文: {line}"));
            if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return value;
            }
        }
    }

    fn initialize(&mut self) {
        self.send(
            "initialize",
            serde_json::json!({
                "protocolVersion":"2025-06-18",
                "capabilities":{},
                "clientInfo":{"name":"t061-probe","version":"1.0"}
            }),
        );
        self.notify("notifications/initialized", serde_json::json!({}));
    }

    fn call(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let response = self.send(
            "tools/call",
            serde_json::json!({"name":name,"arguments":arguments}),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("tools/call 无 result: {response}"))
    }

    fn tool_names(&mut self) -> Vec<String> {
        let list = self.send("tools/list", serde_json::json!({}));
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    }

    /// 关闭 stdin（EOF）并等待进程正常退出，返回是否成功退出。
    fn close_and_wait(mut self) -> bool {
        drop(self.stdin);
        let status = self.child.wait().expect("等待进程");
        status.success()
    }
}

fn stdout_of(result: &serde_json::Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn t061_mysql_query_through_protocol() {
    let Some(p) = live_params("FLUXDB_T061_MYSQL") else {
        eprintln!("跳过：未配置 FLUXDB_T061_MYSQL");
        return;
    };
    let config = mysql_config(301, &p);
    let harness = Harness::new(vec![config], vec![Harness::sql_policy(false)]);
    let mut client = harness.spawn();
    client.initialize();

    let names = client.tool_names();
    assert!(names.contains(&"fluxdb_query".to_string()));
    assert!(
        !names.contains(&"fluxdb_execute".to_string()),
        "只读数据源不列出写工具"
    );

    let result = client.call(
        "fluxdb_query",
        serde_json::json!({
            "target": format!("conn:301/db:{}", p.4),
            "sql": "SELECT id, amount, channel FROM orders ORDER BY id",
            "max_rows": 10
        }),
    );
    assert_eq!(
        result["isError"],
        serde_json::json!(false),
        "MySQL 查询应成功: {}",
        stdout_of(&result)
    );
    let text = stdout_of(&result);
    assert!(text.contains("10.50"), "结构化结果含真实数据: {text}");
    assert!(result["structuredContent"].is_object());
    assert!(client.close_and_wait(), "EOF 后正常退出");
}

#[test]
fn t061_postgres_query_and_write_rejected_through_protocol() {
    let Some(p) = live_params("FLUXDB_T061_PG") else {
        eprintln!("跳过：未配置 FLUXDB_T061_PG");
        return;
    };
    let config = pg_config(302, &p);
    let harness = Harness::new(vec![config], vec![Harness::sql_policy(false)]);
    let mut client = harness.spawn();
    client.initialize();

    // 参数真绑定：PG `$1` 占位符 + 精确 decimal 文本。
    let result = client.call(
        "fluxdb_query",
        serde_json::json!({
            "target": format!("conn:302/db:{}/schema:public", p.4),
            "sql": "SELECT amount FROM public.orders WHERE id = $1",
            "parameters": [{"type":"i64","value":"2"}],
            "max_rows": 10
        }),
    );
    assert_eq!(
        result["isError"],
        serde_json::json!(false),
        "PG 参数化查询应成功: {}",
        stdout_of(&result)
    );
    assert!(
        stdout_of(&result).contains("20.00"),
        "命中 id=2 精确值: {}",
        stdout_of(&result)
    );

    // 只读数据源：db.execute 直接调用被拒。
    let denied = client.call(
        "fluxdb_execute",
        serde_json::json!({
            "target": format!("conn:302/db:{}/schema:public", p.4),
            "sql": "UPDATE public.orders SET amount = 1 WHERE id = 1"
        }),
    );
    assert_eq!(denied["isError"], serde_json::json!(true));
    assert_eq!(
        denied["structuredContent"]["code"],
        serde_json::json!("write_disabled")
    );

    assert!(client.close_and_wait());
}

#[test]
fn t061_redis_scan_inspect_through_protocol() {
    let Some(p) = live_params("FLUXDB_T061_REDIS") else {
        eprintln!("跳过：未配置 FLUXDB_T061_REDIS");
        return;
    };
    let config = redis_config(303, &p);
    let harness = Harness::new(vec![config], vec![Harness::redis_policy()]);
    let mut client = harness.spawn();
    client.initialize();

    let names = client.tool_names();
    assert!(names.contains(&"fluxdb_redis_scan".to_string()));
    assert!(names.contains(&"fluxdb_redis_inspect".to_string()));
    // 只读数据源不列出 Redis 写工具。
    assert!(!names.contains(&"fluxdb_redis_execute".to_string()));

    let scan = client.call(
        "fluxdb_redis_scan",
        serde_json::json!({
            "target": "conn:303/db:0",
            "pattern": "t061:*",
            "count": 100,
            "max_keys": 100
        }),
    );
    assert_eq!(
        scan["isError"],
        serde_json::json!(false),
        "Redis 扫描应成功: {}",
        stdout_of(&scan)
    );
    assert!(
        stdout_of(&scan).contains("t061:"),
        "扫描命中授权前缀: {}",
        stdout_of(&scan)
    );

    let inspect = client.call(
        "fluxdb_redis_inspect",
        serde_json::json!({
            "target": "conn:303/db:0",
            "key": "t061:greeting",
            "view": "value"
        }),
    );
    assert_eq!(
        inspect["isError"],
        serde_json::json!(false),
        "Redis inspect 应成功: {}",
        stdout_of(&inspect)
    );
    assert!(
        stdout_of(&inspect).contains("hello"),
        "读到真实值: {}",
        stdout_of(&inspect)
    );

    // 越权前缀：授权 t061:，访问 other: 前缀应被 range 判定拒绝。
    let outside = client.call(
        "fluxdb_redis_inspect",
        serde_json::json!({
            "target": "conn:303/db:0",
            "key": "other:secret",
            "view": "value"
        }),
    );
    assert_eq!(
        outside["isError"],
        serde_json::json!(true),
        "越权前缀必须拒绝"
    );

    assert!(client.close_and_wait());
}

#[test]
fn t061_unknown_tool_and_client_info_does_not_grant_permission() {
    let Some(p) = live_params("FLUXDB_T061_MYSQL") else {
        eprintln!("跳过：未配置 FLUXDB_T061_MYSQL");
        return;
    };
    let config = mysql_config(304, &p);
    let harness = Harness::new(vec![config], vec![Harness::sql_policy(false)]);
    let mut client = harness.spawn();
    // 用伪造的 clientInfo 声明"我是管理员"；clientInfo 只审计，不授权。
    client.send(
        "initialize",
        serde_json::json!({
            "protocolVersion":"2025-06-18",
            "capabilities":{},
            "clientInfo":{"name":"root","version":"1.0","title":"admin"}
        }),
    );
    client.notify("notifications/initialized", serde_json::json!({}));

    let unknown = client.call("fluxdb_does_not_exist", serde_json::json!({}));
    assert_eq!(unknown["isError"], serde_json::json!(true));
    assert_eq!(
        unknown["structuredContent"]["code"],
        serde_json::json!("unknown_tool")
    );

    // 即便自称 admin，也不能调用未授权写工具。
    let write = client.call(
        "fluxdb_execute",
        serde_json::json!({
            "target": format!("conn:304/db:{}", p.4),
            "sql": "DELETE FROM orders"
        }),
    );
    assert_eq!(write["isError"], serde_json::json!(true));

    assert!(client.close_and_wait());
}
