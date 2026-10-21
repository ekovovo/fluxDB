# dm-pure：纯 Rust 达梦 DM8 数据库驱动

[![crates.io](https://img.shields.io/crates/v/dm-pure)](https://crates.io/crates/dm-pure)
[![docs.rs](https://img.shields.io/docsrs/dm-pure)](https://docs.rs/dm-pure)
[![license](https://img.shields.io/crates/l/dm-pure)](LICENSE)

100% Rust 实现的达梦 DM8 数据库驱动，零外部依赖（内置 DH 密钥交换 / DES-CFB 登录加密 / DM 协议解析）。

## 功能

参数化查询（含 NULL）/ 中文（自动检测服务器编码）/ 大结果集分批 fetch / CLOB·BLOB / 事务 / 存储过程 /
批量插入 executemany / 连接池 / DSN 连接串。

## 安装

```bash
cargo add dm-pure
```

## 快速上手

```rust
use dm_pure::{DmClient, Param, ExecuteResult};

fn main() -> Result<(), dm_pure::DmError> {
    let mut c = DmClient::new("127.0.0.1", 5236, "SYSDBA", "password");
    c.connect()?;

    let r = c.execute("SELECT id, name FROM t WHERE id >= ?", &[Param::Int(1)])?;
    if let ExecuteResult::Query { columns, rows } = r {
        println!("cols: {:?}, first: {:?}", columns, rows.first());
    }

    c.execute("INSERT INTO t VALUES (?, ?)", &[Param::Int(1), Param::Str("测试".into())])?;
    c.close();
    Ok(())
}
```

更多用法（惰性游标 / 连接池 / 存储过程）见 [examples/](examples/)。

## 测试

```bash
cargo test   # 单元 + mock 端到端（无需服务器）
```

## 开源协议

[MIT](LICENSE)。
