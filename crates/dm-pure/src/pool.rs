//! 连接池：复用连接避免重复握手（Web 服务场景必需）。
//!
//! ```no_run
//! use dm_pure::{ConnectionPool, ClientConfig, Param};
//!
//! let mut pool = ConnectionPool::new(ClientConfig {
//!     user: "SYSDBA".into(),
//!     password: "password".into(),
//!     ..Default::default()
//! }, 2, 10)?;
//! {
//!     let mut conn = pool.acquire()?; // 借用/归还（Drop 自动归还）
//!     let mut cur = conn.cursor();
//!     cur.execute("SELECT 1", &[])?;
//! }
//! pool.close();
//! # Ok::<(), dm_pure::DmError>(())
//! ```

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::ClientConfig;
use crate::{DmClient, Result};

/// 已连接会话的封装。
pub struct Connection {
    pub(crate) client: DmClient,
}

impl Connection {
    pub fn connect(cfg: ClientConfig) -> Result<Self> {
        let mut client = DmClient::with_config(cfg);
        client.connect()?;
        Ok(Connection { client })
    }

    pub fn cursor(&mut self) -> crate::Cursor<'_> {
        crate::Cursor::new(&mut self.client)
    }

    pub fn execute(&mut self, sql: &str, params: &[crate::Param]) -> Result<crate::ExecuteResult> {
        self.client.execute(sql, params)
    }

    pub fn begin(&mut self) -> Result<()> {
        self.client.begin()
    }

    pub fn commit(&mut self) -> Result<()> {
        self.client.commit()
    }

    pub fn rollback(&mut self) -> Result<()> {
        self.client.rollback()
    }

    pub fn close(&mut self) {
        self.client.close();
    }
}

/// 连接池。
pub struct ConnectionPool {
    cfg: ClientConfig,
    idle: Mutex<Vec<(Connection, Instant)>>,
    total: AtomicUsize,
    max: usize,
    idle_timeout: Duration,
    closed: AtomicBool,
    /// acquire 时跳过 SELECT 1 保活：坏连接延迟到首次使用时由 auto_reconnect 重连
    lazy_check: bool,
}

impl ConnectionPool {
    /// `min_connections`: 最小空闲连接数；`max_connections`: 最大连接数。
    /// 预热时创建的连接失败会被忽略（服务器未启动时仍可创建池）。
    pub fn new(cfg: ClientConfig, min_connections: usize, max_connections: usize) -> Result<Self> {
        let pool = ConnectionPool {
            cfg,
            idle: Mutex::new(Vec::new()),
            total: AtomicUsize::new(0),
            max: max_connections.max(1),
            idle_timeout: Duration::from_secs(300),
            closed: AtomicBool::new(false),
            lazy_check: false,
        };
        for _ in 0..min_connections.min(max_connections.max(1)) {
            if let Ok(c) = pool.new_connection() {
                pool.total.fetch_add(1, Ordering::SeqCst);
                pool.idle.lock().unwrap().push((c, Instant::now()));
            }
        }
        Ok(pool)
    }

    /// 开启/关闭惰性保活检查：`acquire` 跳过 SELECT 1，坏连接延迟到首次使用时
    /// 由 auto_reconnect 自动重连（省一次往返；需 `ClientConfig.auto_reconnect = true`）。
    /// 用法：`ConnectionPool::new(cfg, 1, 10)?.with_lazy_check(true)`
    pub fn with_lazy_check(mut self, v: bool) -> Self {
        self.lazy_check = v;
        self
    }

    fn new_connection(&self) -> Result<Connection> {
        Connection::connect(self.cfg.clone())
    }

    fn check_alive(&self, conn: &mut Connection) -> bool {
        let mut cur = conn.cursor();
        cur.execute("SELECT 1", &[]).map(|_| true).unwrap_or(false)
    }

    /// 借用一个连接（必要时新建；坏连接自动剔除重连）。
    pub fn acquire(&self) -> Result<PooledConnection<'_>> {
        let mut retry = 0;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(crate::DmError::arg("连接池已关闭"));
            }
            // 1. 优先取空闲连接
            {
                let mut idle = self.idle.lock().unwrap();
                while let Some((mut conn, _)) = idle.pop() {
                    if !self.lazy_check && !self.check_alive(&mut conn) {
                        // 坏连接：关闭并重连
                        conn.close();
                        self.total.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    return Ok(PooledConnection { pool: self, conn: Some(conn) });
                }
            }
            // 2. 未达上限则新建
            let total = self.total.load(Ordering::SeqCst);
            if total < self.max {
                if self.total.compare_exchange(total, total + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                    match self.new_connection() {
                        Ok(conn) => return Ok(PooledConnection { pool: self, conn: Some(conn) }),
                        Err(e) => {
                            self.total.fetch_sub(1, Ordering::SeqCst);
                            return Err(e);
                        }
                    }
                }
                continue;
            }
            // 3. 达到上限：等待空闲（简化实现：轮询等待）
            if retry >= 600 {
                // ~30s
                return Err(crate::DmError::arg("连接池已满，等待超时"));
            }
            retry += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 归还连接；连接无效则丢弃。
    pub fn release(&self, mut conn: Connection) {
        if self.closed.load(Ordering::SeqCst) {
            conn.close();
            return;
        }
        let alive = self.check_alive(&mut conn);
        if alive {
            self.idle.lock().unwrap().push((conn, Instant::now()));
        } else {
            conn.close();
            self.total.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// 回收超时空闲连接。
    pub fn reap_idle(&self) {
        let now = Instant::now();
        let mut keep = Vec::new();
        {
            let mut idle = self.idle.lock().unwrap();
            for (mut c, last) in idle.drain(..) {
                if now.duration_since(last) > self.idle_timeout {
                    c.close();
                    self.total.fetch_sub(1, Ordering::SeqCst);
                } else {
                    keep.push((c, last));
                }
            }
        }
        *self.idle.lock().unwrap() = keep;
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut idle = self.idle.lock().unwrap();
        for (mut c, _) in idle.drain(..) {
            c.close();
            self.total.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn total(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }

    pub fn idle_count(&self) -> usize {
        self.idle.lock().unwrap().len()
    }
}

/// 借出的连接：Drop 时自动归还连接池。
pub struct PooledConnection<'a> {
    pool: &'a ConnectionPool,
    conn: Option<Connection>,
}

impl<'a> Deref for PooledConnection<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl<'a> DerefMut for PooledConnection<'a> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().unwrap()
    }
}

impl<'a> Drop for PooledConnection<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}

/// 便于跨线程共享的池句柄。
pub type SharedPool = Arc<ConnectionPool>;

impl ConnectionPool {
    /// 包装为 Arc 以便跨线程共享。
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}
