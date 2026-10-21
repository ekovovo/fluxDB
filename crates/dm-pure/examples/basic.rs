//! 达梦 DM8 驱动基本用法示例（对照 README 快速上手）。
//!
//! 运行：`cargo run --example basic`（需可达的达梦服务器，可改下面的连接参数）

#![allow(unused_imports)] // OutType 用于下方注释的存储过程示例
use dm_pure::{DmClient, ExecuteResult, OutType, Param};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = std::env::var("DM_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("DM_PORT")
        .unwrap_or_else(|_| "5236".into())
        .parse()?;
    let user = std::env::var("DM_USER").unwrap_or_else(|_| "SYSDBA".into());
    let pwd = std::env::var("DM_PWD").unwrap_or_else(|_| "password".into());

    let mut c = DmClient::new(&host, port, &user, &pwd);
    c.connect()?;
    println!(
        "✅ 连接成功，会话密钥前缀: {:02x?}...",
        &c.session_key().unwrap()[..4]
    );

    // 1. 简单查询（非参数化）
    let r = c.execute("select 1", &[])?;
    if let ExecuteResult::Query { columns, rows } = r {
        println!(
            "select 1 => cols={:?} rows={:?}",
            columns.iter().map(|c| &c.name).collect::<Vec<_>>(),
            rows
        );
    }

    // 2. 参数化查询（先建表）
    c.execute(
        "CREATE TABLE IF NOT EXISTS rust_demo (id INT, name VARCHAR(100), d DATE, f DOUBLE)",
        &[],
    )?;
    let r = c.execute(
        "INSERT INTO rust_demo VALUES (?, ?, ?, ?)",
        &[
            Param::Int(1),
            Param::Str("测试".into()),
            Param::Date(dm_pure::DmDate::new(2026, 8, 12)),
            Param::Float(std::f64::consts::PI),
        ],
    )?;
    println!("INSERT affected: {:?}", r.affected());

    let r = c.execute(
        "SELECT id, name, d, f FROM rust_demo WHERE id >= ?",
        &[Param::Int(1)],
    )?;
    if let ExecuteResult::Query { columns, rows } = r {
        println!("查询 {} 列, {} 行:", columns.len(), rows.len());
        for row in &rows {
            println!("  {:?}", row);
        }
    }

    // 3. 存储过程 OUT 参数
    // c.execute("CALL sp_test(?, ?, ?, ?)", &[Param::Int(5), Param::Str("hello".into()),
    //     Param::out(Some(OutType::Int)), Param::out(Some(OutType::Str))])?;

    // 4. 事务
    c.begin()?;
    c.execute(
        "UPDATE rust_demo SET name = ? WHERE id = ?",
        &[Param::Str("改".into()), Param::Int(1)],
    )?;
    c.rollback()?; // 回滚
    println!("事务回滚完成");

    c.execute("DROP TABLE rust_demo", &[])?;
    c.close();
    println!("✅ 示例结束");
    Ok(())
}
