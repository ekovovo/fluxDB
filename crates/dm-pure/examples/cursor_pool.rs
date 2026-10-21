//! DB-API 风格惰性游标示例：fetchone / fetchmany / fetchall + 连接池。

use dm_pure::{ClientConfig, ConnectionPool, Cursor, Param};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = ClientConfig {
        host: std::env::var("DM_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("DM_PORT")
            .unwrap_or_else(|_| "5236".into())
            .parse()?,
        user: std::env::var("DM_USER").unwrap_or_else(|_| "SYSDBA".into()),
        password: std::env::var("DM_PWD").unwrap_or_else(|_| "password".into()),
        ..Default::default()
    };

    // 连接池（最小 1 / 最大 5）
    let pool = ConnectionPool::new(cfg, 1, 5)?;
    {
        let mut conn = pool.acquire()?;
        {
            let mut cur = conn.cursor();
            cur.execute("select 1", &[])?;
            println!("description: {:?}", cur.description());
            while let Some(row) = cur.fetchone()? {
                println!("row: {:?}", row);
            }
        }
        // 惰性游标：大结果集自动分批拉取
        {
            let mut cur: Cursor = conn.cursor();
            cur.execute("SELECT id FROM proto_test WHERE id >= ?", &[Param::Int(0)])?;
            let rows = cur.fetchall()?;
            println!("fetchall {} 行", rows.len());
        }
    } // Drop 自动归还连接

    // executemany 批量插入
    {
        let mut conn = pool.acquire()?;
        let batch: Vec<Vec<Param>> = (0..10)
            .map(|i| vec![Param::Int(i), Param::Str(format!("name-{i}"))])
            .collect();
        let mut cur = conn.cursor();
        let n = cur.executemany("INSERT INTO batch_demo VALUES (?, ?)", &batch)?;
        println!("executemany 影响行数: {n}");
    }

    pool.close();
    println!("✅ 游标/连接池示例结束");
    Ok(())
}
