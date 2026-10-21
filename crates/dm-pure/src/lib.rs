//! # dm-pure：纯 Rust 达梦 DM8 数据库驱动
//!
//! 纯 Rust 实现的达梦 DM8 驱动，零数据库外部依赖。实现 DM 网络协议（握手 / 登录 /
//! 执行 / 参数绑定 / LOB / 事务 / 分批 fetch）。
//!
//! - crates.io: <https://crates.io/crates/dm-pure>
//! - 源码仓库: <https://github.com/zhangtao103239/dm8_pure_rs>
//!
//! ```no_run
//! use dm_pure::{DmClient, Param};
//!
//! let mut c = DmClient::new("127.0.0.1", 5236, "SYSDBA", "password");
//! c.connect()?;
//! // 查询（参数化）
//! let r = c.execute("SELECT id, name FROM t WHERE id >= ?", &[Param::Int(1)])?;
//! if let dm_pure::ExecuteResult::Query { columns, rows } = r {
//!     println!("cols: {:?}, first row: {:?}", columns, rows.first());
//! }
//! // 写入
//! c.execute("INSERT INTO t VALUES (?, ?)", &[Param::Int(1), Param::Str("x".into())])?;
//! # Ok::<(), dm_pure::DmError>(())
//! ```
//!
//! 更上层的 `Cursor`（DB-API 风格惰性游标）与连接池见 [`cursor`] / [`pool`] 模块。

pub mod client;
pub mod crypto;
pub mod cursor;
pub mod frame;
pub mod pool;
pub mod result;
pub mod transaction;

pub use client::{ClientConfig, ExecuteResult, DmClient, OUT, OutType, Param, ServerParam};
pub use cursor::Cursor;
pub use pool::{Connection, ConnectionPool, PooledConnection};
pub use result::{Column, DmDate, DmDateTime, DmTime, Value};
pub use transaction::Transaction;

use std::fmt;

/// 达梦驱动错误。`code` 为服务器返回码（0 表示非协议错误），`msg` 为描述信息。
#[derive(Debug, Clone)]
pub struct DmError {
    pub code: i32,
    pub msg: String,
}

impl DmError {
    pub fn new(code: i32, msg: impl Into<String>) -> Self {
        DmError { code, msg: msg.into() }
    }

    /// 服务器返回码错误（code != 0）
    pub fn server(code: i32, msg: impl Into<String>) -> Self {
        DmError { code, msg: msg.into() }
    }

    /// 连接层错误（网络断开 / 超时等）
    pub fn conn(msg: impl Into<String>) -> Self {
        DmError { code: -6852, msg: msg.into() }
    }

    /// 协议 / 解析错误
    pub fn protocol(msg: impl Into<String>) -> Self {
        DmError { code: -6001, msg: msg.into() }
    }

    /// 参数错误
    pub fn arg(msg: impl Into<String>) -> Self {
        DmError { code: -6119, msg: msg.into() }
    }
}

impl fmt::Display for DmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let desc = dm_error_desc(self.code);
        if desc.is_empty() {
            write!(f, "[CODE:{}] {}", self.code, self.msg)
        } else {
            write!(f, "[CODE:{}] ({}) {}", self.code, desc, self.msg)
        }
    }
}

impl std::error::Error for DmError {}

impl From<std::io::Error> for DmError {
    fn from(e: std::io::Error) -> Self {
        DmError::conn(e.to_string())
    }
}

/// 常见达梦错误码描述表（与 Python 驱动一致）
pub fn dm_error_desc(code: i32) -> &'static str {
    match code {
        -7003 => "无效的语句句柄",
        -6119 => "无效的数据类型",
        -2106 => "对象不存在",
        -2104 | -2124 => "对象已存在",
        -6804 => "需要更多的参数",
        -2111 => "语法错误",
        -2007 => "语法分析出错",
        -6852 => "网络异常",
        -6002 => "缓冲区溢出",
        -6001 => "内部错误",
        100 => "通用错误",
        _ => "",
    }
}

/// 统一 Result 别名
pub type Result<T> = std::result::Result<T, DmError>;
