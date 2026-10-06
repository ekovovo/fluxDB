//! B 验收：真实 MySQL 上的**参数绑定**（不做字符串插值）。
//!
//! 用法：`FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_ROOT=<隔离配置库副本> FLUXDB_AI_LIVE_MYSQL_PORT=38831 \
//!        cargo run -p fluxdb-app --example ai_live_mysql_params`
//! 只写专用库 `fluxdb_ai_smoke`，结束即 DROP DATABASE；凭据只走宿主解密路径，不打印。
use fluxdb_core::{
    ConnectionId, DatabaseKind, ParameterizedQueryRequest, QueryExecutionOptions, QueryMode,
    QueryRequest, SqlParameter,
};
use fluxdb_storage::Storage as _;

const DB: &str = "fluxdb_ai_smoke";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要 FLUXDB_AI_LIVE=1（会真实写入测试库）".into());
    }
    let root = std::env::var("FLUXDB_AI_LIVE_ROOT")
        .map_err(|_| "需要 FLUXDB_AI_LIVE_ROOT 指向用户配置库的隔离副本")?;
    let port: u16 = std::env::var("FLUXDB_AI_LIVE_MYSQL_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(36531);
    let connection_id: u64 = std::env::var("FLUXDB_AI_LIVE_MYSQL_CONNECTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let storage = fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));
    let mut config = storage
        .load_connections()?
        .into_iter()
        .find(|config| config.id == ConnectionId(connection_id))
        .ok_or("找不到 MySQL 连接")?;
    if config.kind != DatabaseKind::MySql {
        return Err("该连接不是 MySQL".into());
    }
    if let fluxdb_core::Endpoint::Tcp { port: ref mut p, .. } = config.endpoint {
        *p = port;
    }
    let connector = fluxdb_connectors::connector_for(&config)?;
    connector.test_connection(&config)?;
    println!("① 连通 MySQL 127.0.0.1:{port}（凭据未外显）");

    let raw = |text: &str| -> fluxdb_core::Result<()> {
        connector.execute(&QueryRequest {
            connection_id: ConnectionId(connection_id),
            database: None,
            schema: None,
            text: text.to_string(),
            mode: QueryMode::All,
            options: QueryExecutionOptions {
                continue_on_error: false,
                split_statements: true,
                page_offset: 0,
                page_size: 100,
            },
            session_id: None,
        })?;
        Ok(())
    };
    let parameterized = |text: &str,
                         parameters: Vec<SqlParameter>|
     -> fluxdb_core::Result<fluxdb_core::QueryExecutionResult> {
        connector.execute_parameterized(&ParameterizedQueryRequest {
            request: QueryRequest {
                connection_id: ConnectionId(connection_id),
                database: Some(DB.to_string()),
                schema: None,
                text: text.to_string(),
                mode: QueryMode::All,
                options: QueryExecutionOptions {
                    continue_on_error: false,
                    split_statements: false,
                    page_offset: 0,
                    page_size: 100,
                },
                session_id: None,
            },
            parameters,
        })
    };

    println!("② 建立 fixture 库/表（无参数路径）");
    raw(&format!("CREATE DATABASE IF NOT EXISTS {DB}"))?;
    raw(&format!(
        "CREATE TABLE IF NOT EXISTS {DB}.orders (id BIGINT PRIMARY KEY, status VARCHAR(32) NOT NULL, amount DECIMAL(10,2) NOT NULL)"
    ))?;
    raw(&format!("DELETE FROM {DB}.orders"))?;

    println!("③ 参数化 INSERT（i64/text/decimal 全部走 bind）");
    let inserted = parameterized(
        "INSERT INTO orders (id, status, amount) VALUES (?, ?, ?)",
        vec![
            SqlParameter::I64("1".into()),
            SqlParameter::Text("pending".into()),
            SqlParameter::Decimal("100.50".into()),
        ],
    )?;
    let affected = inserted.summaries[0].affected_rows;
    println!("   affected_rows={affected}");
    if affected != 1 {
        return Err("参数化 INSERT 未影响 1 行".into());
    }

    println!("④ 注入串必须被当作字面量（证明是真 bind 而不是拼接）");
    let injection = "x' OR '1'='1";
    parameterized(
        "INSERT INTO orders (id, status, amount) VALUES (?, ?, ?)",
        vec![
            SqlParameter::I64("2".into()),
            SqlParameter::Text(injection.into()),
            SqlParameter::Decimal("1.00".into()),
        ],
    )?;
    let matched = parameterized(
        "SELECT id, status FROM orders WHERE status = ? ORDER BY id",
        vec![SqlParameter::Text(injection.into())],
    )?;
    let rows: Vec<String> = matched
        .results
        .iter()
        .flat_map(|page| page.rows.iter())
        .map(|row| {
            row.values
                .iter()
                .map(fluxdb_core::CellValue::display_label)
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    println!("   注入串查询命中: {rows:?}");
    if rows != vec![format!("2|{injection}")] {
        return Err("注入串没有被当作字面量处理".into());
    }

    println!("⑤ 参数化 UPDATE + 真实 affected_rows");
    let updated = parameterized(
        "UPDATE orders SET status = ? WHERE id = ?",
        vec![
            SqlParameter::Text("paid".into()),
            SqlParameter::I64("1".into()),
        ],
    )?;
    let update_affected = updated.summaries[0].affected_rows;
    println!("   affected_rows={update_affected}");
    if update_affected != 1 {
        return Err("参数化 UPDATE 未影响 1 行".into());
    }
    let after = parameterized(
        "SELECT id, status FROM orders WHERE id = ?",
        vec![SqlParameter::I64("1".into())],
    )?;
    let values: Vec<String> = after
        .results
        .iter()
        .flat_map(|page| page.rows.iter())
        .map(|row| {
            row.values
                .iter()
                .map(fluxdb_core::CellValue::display_label)
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    println!("   更新后: {values:?}");
    if values != vec!["1|paid".to_string()] {
        return Err("参数化 UPDATE 未生效".into());
    }

    println!("⑥ 清理 fixture");
    raw(&format!("DROP DATABASE IF EXISTS {DB}"))?;
    println!("✅ 真实 MySQL 参数绑定：INSERT/UPDATE/SELECT + 注入串字面量处理 全部通过");
    Ok(())
}
