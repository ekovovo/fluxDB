//! 事务 API：`Transaction` 借用 `DmClient`，Drop 时自动回滚（sqlx/dameng 风格）。
//!
//! ```no_run
//! use dm_pure::{DmClient, Param};
//!
//! let mut c = DmClient::new("127.0.0.1", 5236, "SYSDBA", "password");
//! c.connect()?;
//! {
//!     let mut tx = c.transaction()?;          // begin
//!     tx.execute("INSERT INTO t VALUES (?, ?)", &[Param::Int(1), Param::Str("a".into())])?;
//!     tx.commit()?;                        // 显式提交
//! }
//! // 未 commit 的 Transaction 在 Drop 时自动 rollback
//! # Ok::<(), dm_pure::DmError>(())
//! ```

use crate::{DmClient, ExecuteResult, Param, Result};

/// 借用客户端的事务句柄。Drop 时若未 commit/rollback 则自动回滚。
pub struct Transaction<'a> {
    pub(crate) client: &'a mut DmClient,
    finished: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) fn new(client: &'a mut DmClient) -> Self {
        Transaction { client, finished: false }
    }

    /// 在事务内执行 SQL。
    pub fn execute(&mut self, sql: &str, params: &[Param]) -> Result<ExecuteResult> {
        self.client.execute(sql, params)
    }

    /// 提交事务，消费自身并释放客户端借用。
    pub fn commit(mut self) -> Result<()> {
        self.client.commit()?;
        self.finished = true;
        Ok(())
    }

    /// 回滚事务，消费自身并释放客户端借用。
    pub fn rollback(mut self) -> Result<()> {
        self.client.rollback()?;
        self.finished = true;
        Ok(())
    }

    /// 是否已结束（commit/rollback 后为 true）。
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

impl<'a> Drop for Transaction<'a> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.client.rollback();
        }
    }
}

/// DmClient 的事务入口。
impl DmClient {
    /// 开始事务并返回 `Transaction` 句柄（Drop 自动回滚）。
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.begin()?;
        Ok(Transaction::new(self))
    }
}
