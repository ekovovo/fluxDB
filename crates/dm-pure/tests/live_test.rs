//! 对照 Python `test_all.py` 的完整回归测试（需可达的达梦服务器）。
//!
//! 运行：`cargo test --test live_test -- --ignored --nocapture`
//! 连接参数通过环境变量设置：DM_HOST / DM_PORT / DM_USER / DM_PWD（默认 127.0.0.1:5236 SYSDBA/password）。
//!
//! 前置：服务器需存在 proto_test / type_test 表（可用以下 SQL 准备）：
//! ```sql
//! CREATE TABLE proto_test (id INT, name VARCHAR(100), d DATE, f DOUBLE);
//! CREATE TABLE type_test (id INT, tiny SMALLINT, big BIGINT, dec38 DECIMAL(38,10),
//!                         tm TIME(6), dt DATETIME(6), ivy INTERVAL YEAR TO MONTH, ivd INTERVAL DAY TO SECOND);
//! ```

use dm_pure::result::{DmDate, DmDateTime, DmTime};
use dm_pure::{DmClient, ExecuteResult, Param, Value};

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn setup() -> DmClient {
    let host = env_or("DM_HOST", "127.0.0.1");
    let port: u16 = env_or("DM_PORT", "5236").parse().unwrap();
    let user = env_or("DM_USER", "SYSDBA");
    let pwd = env_or("DM_PWD", "password");
    let mut c = DmClient::new(&host, port, &user, &pwd);
    c.connect().expect("连接失败（需可达的达梦服务器）");
    c
}

fn row_int(row: &[Value], i: usize) -> i64 {
    row[i]
        .as_i64()
        .unwrap_or_else(|| panic!("row[{}] 不是整数: {:?}", i, row[i]))
}

#[test]
#[ignore = "需要可达的达梦服务器"]
fn full_regression() {
    let mut c = setup();
    let mut passed = 0usize;
    let mut total = 0usize;
    macro_rules! check {
        ($name:expr, $cond:expr $(, $extra:expr)?) => {{
            total += 1;
            if $cond {
                passed += 1;
                println!("  ✅ {} {}", $name, stringify!($($extra)?));
            } else {
                println!("  ❌ {} {}", $name, stringify!($($extra)?));
            }
            assert!($cond, "{}", $name);
        }};
    }

    println!("== 1. 基础连接 ==");
    match c.execute("select 1", &[]) {
        Ok(ExecuteResult::Query { rows, .. }) => {
            check!("select 1", rows == vec![vec![Value::Int(1)]])
        }
        _ => check!("select 1", false),
    }

    println!("== 2. 普通查询（非参数化）==");
    match c.execute("SELECT id, name FROM proto_test ORDER BY id LIMIT 3", &[]) {
        Ok(ExecuteResult::Query { columns, rows }) => {
            check!(
                "多行查询",
                rows.len() == 3 && columns[0].name == "ID",
                format!("{:?}", &rows[..1])
            );
        }
        _ => check!("多行查询", false),
    }
    match c.execute("SELECT COUNT(*) FROM proto_test", &[]) {
        Ok(ExecuteResult::Query { rows, .. }) => {
            check!(
                "聚合查询",
                matches!(rows[0][0], Value::BigInt(_) | Value::Int(_))
            );
        }
        _ => check!("聚合查询", false),
    }

    println!("== 3. 参数化 SELECT ==");
    let r = c
        .execute(
            "SELECT id, name FROM proto_test WHERE id >= ?",
            &[Param::Int(1)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!("1参数多行", !rows.is_empty() && row_int(&rows[0], 0) == 1);
    }
    let r = c
        .execute(
            "SELECT id, name FROM proto_test WHERE id >= ? AND id <= ?",
            &[Param::Int(1), Param::Int(3)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!(
            "2参数",
            rows.len() == 2 && row_int(&rows[0], 0) == 1 && row_int(&rows[1], 0) == 2
        );
    }
    let r = c
        .execute(
            "SELECT name FROM proto_test WHERE id = ?",
            &[Param::Int(1005)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!("单列查询", rows.is_empty());
    }
    let r = c
        .execute(
            "SELECT id FROM proto_test WHERE id >= ?",
            &[Param::Int(100000)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!(
            "大结果集分批fetch",
            rows.len() > 100 && rows.iter().all(|x| x.len() == 1),
            format!("{}行", rows.len())
        );
    }
    // 非参数化大查询（0xFFFF + cmd=7 循环，此前漏处理会丢行）
    let r = c
        .execute("SELECT id FROM proto_test WHERE id >= 100000", &[])
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!(
            "非参数化大结果集",
            rows.len() > 100,
            format!("{}行", rows.len())
        );
    }

    println!("== 4. 参数化 DML ==");
    let r = c
        .execute(
            "INSERT INTO proto_test VALUES (?, ?, ?, ?)",
            &[
                Param::Int(9000),
                Param::Str("回归测试".into()),
                Param::Date(DmDate::new(2026, 8, 12)),
                Param::Float(std::f64::consts::PI),
            ],
        )
        .unwrap();
    check!("INSERT 影响行数", r.affected() == Some(1));
    let r = c
        .execute(
            "UPDATE proto_test SET name = ? WHERE id = ?",
            &[Param::Str("改".into()), Param::Int(9000)],
        )
        .unwrap();
    check!("UPDATE 影响行数", r.affected() == Some(1));
    let r = c
        .execute(
            "SELECT name FROM proto_test WHERE id = ?",
            &[Param::Int(9000)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!("UPDATE 生效", rows == vec![vec![Value::Str("改".into())]]);
    }
    let r = c
        .execute("DELETE FROM proto_test WHERE id = ?", &[Param::Int(9000)])
        .unwrap();
    check!("DELETE 影响行数", r.affected() == Some(1));
    let r = c
        .execute(
            "SELECT id FROM proto_test WHERE id = ?",
            &[Param::Int(9000)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!("DELETE 生效", rows.is_empty());
    }

    println!("== 5. 类型覆盖 ==");
    let r = c
        .execute("SELECT d, f FROM proto_test WHERE id = ?", &[Param::Int(1)])
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!("DATE/FLOAT 列", !rows.is_empty() && rows[0].len() == 2);
    }
    let r = c
        .execute(
            "SELECT * FROM proto_test WHERE name = ?",
            &[Param::Str("测试".into())],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!(
            "中文参数",
            !rows.is_empty() && rows[0].get(1) == Some(&Value::Str("测试".into()))
        );
    }

    println!("== 6. 边界类型 ==");
    let r = c
        .execute(
            "SELECT id, tiny, big, dec38, tm, dt, ivy, ivd FROM type_test WHERE id = ?",
            &[Param::Int(1)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        let row = &rows[0];
        check!("SMALLINT 边界", matches!(row[1], Value::SmallInt(-32768)));
        check!(
            "BIGINT 边界",
            matches!(row[2], Value::BigInt(-9_223_372_036_854_775_808))
        );
        if let Value::Decimal(s) = &row[3] {
            check!(
                "DECIMAL(38) 精度",
                s == "12345678901234567890.1234567890",
                s
            );
        } else {
            check!("DECIMAL(38) 精度", false);
        }
        check!(
            "TIME(6) 微秒",
            row[4] == Value::Time(DmTime::new(23, 59, 59, 999999))
        );
        check!(
            "DATETIME(6)",
            row[5]
                == Value::DateTime(DmDateTime::new(
                    DmDate::new(9999, 12, 31),
                    DmTime::new(23, 59, 59, 999999)
                ))
        );
        check!(
            "INTERVAL YM",
            row[6] == Value::Interval("1-2".into()),
            format!("{:?}", row[6])
        );
        check!(
            "INTERVAL DT",
            row[7] == Value::Interval("1 02:03:04.567890".into()),
            format!("{:?}", row[7])
        );
    }

    // time/datetime 参数化插入往返
    let r = c
        .execute(
            "INSERT INTO type_test (id, tm, dt) VALUES (?, ?, ?)",
            &[
                Param::Int(4),
                Param::Time(DmTime::new(12, 34, 56, 789000)),
                Param::DateTime(DmDateTime::new(
                    DmDate::new(2030, 6, 15),
                    DmTime::new(8, 9, 10, 111222),
                )),
            ],
        )
        .unwrap();
    check!("time 参数化插入", r.affected() == Some(1));
    let r = c
        .execute(
            "SELECT tm, dt FROM type_test WHERE id = ?",
            &[Param::Int(4)],
        )
        .unwrap();
    if let ExecuteResult::Query { rows, .. } = r {
        check!(
            "time/datetime 参数往返",
            rows[0][0] == Value::Time(DmTime::new(12, 34, 56, 789000))
                && rows[0][1]
                    == Value::DateTime(DmDateTime::new(
                        DmDate::new(2030, 6, 15),
                        DmTime::new(8, 9, 10, 111222)
                    ))
        );
    }
    let _ = c.execute("DELETE FROM type_test WHERE id = ?", &[Param::Int(4)]);

    println!("== 7. TZ 类型往返（TIME_TZ/DATETIME_TZ）==");
    let _ = c.execute("DROP TABLE IF EXISTS tz_test", &[]);
    let _ = c.execute(
        "CREATE TABLE tz_test (id INT, tt TIME(6) WITH TIME ZONE, dt TIMESTAMP(6) WITH TIME ZONE)",
        &[],
    );
    let mut tt = DmTime::new(12, 34, 56, 789000);
    tt.timezone_minutes = Some(480); // +08:00
    let mut dtz = DmDateTime::new(DmDate::new(2026, 8, 14), DmTime::new(12, 34, 56, 789000));
    dtz.time.timezone_minutes = Some(-330); // -05:30
    let r = c
        .execute(
            "INSERT INTO tz_test VALUES (?, ?, ?)",
            &[Param::Int(1), Param::Time(tt), Param::DateTime(dtz)],
        )
        .unwrap();
    check!("TZ 参数插入", r.affected() == Some(1));
    let r = c
        .execute(
            "SELECT id, tt, dt FROM tz_test WHERE id = ?",
            &[Param::Int(1)],
        )
        .unwrap();
    if let ExecuteResult::Query { columns, rows } = r {
        check!(
            "TZ 列类型码",
            columns[1].col_type == 22 && columns[2].col_type == 23,
            format!("{}/{}", columns[1].col_type, columns[2].col_type)
        );
        check!(
            "TIME_TZ 往返",
            rows[0][1] == Value::Time(tt),
            format!("{:?}", rows[0][1])
        );
        check!(
            "DATETIME_TZ 往返",
            rows[0][2] == Value::DateTime(dtz),
            format!("{:?}", rows[0][2])
        );
    }
    let _ = c.execute("DROP TABLE tz_test", &[]);

    println!("== 7.5 列描述符定位回归（DDL 后首条参数化查询）==");
    let _ = c.execute("DROP TABLE IF EXISTS cols_repro", &[]);
    let _ = c.execute("CREATE TABLE cols_repro (id INT, name VARCHAR(50))", &[]);
    let r = c
        .execute(
            "SELECT id, name FROM cols_repro WHERE rowid = ?",
            &[Param::Int(1)],
        )
        .unwrap();
    if let ExecuteResult::Query { columns, .. } = r {
        check!(
            "DDL后首条参数化列描述符",
            columns.len() == 2
                && columns[0].name == "ID"
                && columns[0].col_type == 7
                && columns[1].name == "NAME",
            format!(
                "{:?}",
                columns
                    .iter()
                    .map(|c| (&c.name, c.col_type))
                    .collect::<Vec<_>>()
            )
        );
    }
    let _ = c.execute("DROP TABLE cols_repro", &[]);

    println!("== 8. 连接池 lazy_check（acquire 跳过 SELECT 1）==");
    use dm_pure::{ClientConfig, ConnectionPool};
    let host = env_or("DM_HOST", "127.0.0.1");
    let port: u16 = env_or("DM_PORT", "5236").parse().unwrap();
    let user = env_or("DM_USER", "SYSDBA");
    let pwd = env_or("DM_PWD", "password");
    let cfg = ClientConfig {
        host: host.clone(),
        port,
        user: user.clone(),
        password: pwd.clone(),
        ..Default::default()
    };
    let pool = ConnectionPool::new(cfg, 1, 3)
        .unwrap()
        .with_lazy_check(true);
    {
        let mut conn = pool.acquire().unwrap();
        let mut cur = conn.cursor();
        cur.execute("SELECT 1", &[]).unwrap();
        check!("lazy_check 借出+查询", true);
    } // Drop 自动归还
    {
        let mut conn = pool.acquire().unwrap();
        let mut cur = conn.cursor();
        cur.execute("SELECT id FROM proto_test WHERE id = ?", &[Param::Int(1)])
            .unwrap();
        let rows = cur.fetchall().unwrap();
        check!(
            "lazy_check 复用+参数化查询",
            !rows.is_empty() && rows[0][0].as_i64() == Some(1)
        );
    }
    check!(
        "pool 空闲连接数",
        pool.idle_count() >= 1,
        format!("{}", pool.idle_count())
    );
    pool.close();

    c.close();
    println!("\n结果: {passed}/{total} 通过");
    assert_eq!(passed, total);
}
