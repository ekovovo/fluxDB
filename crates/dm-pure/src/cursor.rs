//! DB-API 风格惰性游标：`fetchone` / `fetchmany` / `fetchall` 按需拉取，大结果集自动分批。

use crate::client::{LazyState, Param};
use crate::result::{Column, Value};
use crate::{DmClient, ExecuteResult, Result};

/// DB-API 风格游标。惰性查询（参数化 SELECT）只拉第一帧，fetch 时按需拉取剩余批次。
pub struct Cursor<'a> {
    client: &'a mut DmClient,
    /// 列描述（查询后填充）
    pub columns: Vec<Column>,
    /// 行缓存
    rows: Vec<Vec<Value>>,
    pos: usize,
    /// 影响行数（DML）或已拉取行数（查询）
    pub rowcount: i64,
    /// fetchmany 默认大小
    pub arraysize: usize,
    lazy: Option<LazyState>,
}

impl<'a> Cursor<'a> {
    pub fn new(client: &'a mut DmClient) -> Self {
        Cursor {
            client,
            columns: Vec::new(),
            rows: Vec::new(),
            pos: 0,
            rowcount: -1,
            arraysize: 100,
            lazy: None,
        }
    }

    /// DB-API description：`(name, type_code, display_size, internal_size, precision, scale, null_ok)`
    #[allow(clippy::type_complexity)]
    pub fn description(&self) -> Vec<(String, i32, Option<i32>, Option<i32>, i32, i32, Option<bool>)> {
        self.columns
            .iter()
            .map(|c| {
                (
                    c.name.clone(),
                    c.col_type,
                    None,
                    None,
                    c.prec,
                    c.scale,
                    if c.nullable { Some(true) } else { Some(false) },
                )
            })
            .collect()
    }

    /// 执行 SQL。大结果集（参数化查询）使用惰性分批 fetch。
    pub fn execute(&mut self, sql: &str, params: &[Param]) -> Result<&mut Self> {
        if is_query(sql) && !params.is_empty() {
            // 惰性查询（参数化）：只拉第一帧，fetchone 时按需拉取
            let (cols, state) = self.client.execute_lazy(sql, params)?;
            let stmt = state.stmt;
            let decoded = self.client.decode_rows(&cols, &state.rows_raw, stmt)?;
            self.columns = cols;
            self.lazy = Some(state);
            self.rows = decoded;
            self.pos = 0;
            self.rowcount = self.rows.len() as i64;
            return Ok(self);
        }
        let result = self.client.execute(sql, params)?;
        self.lazy = None;
        self.pos = 0;
        match result {
            ExecuteResult::Query { columns, rows } => {
                self.columns = columns;
                self.rows = rows;
                self.rowcount = self.rows.len() as i64;
            }
            ExecuteResult::Affected(n) => {
                self.columns = Vec::new();
                self.rows = Vec::new();
                self.rowcount = n as i64;
            }
            ExecuteResult::Call { affected, .. } => {
                self.columns = Vec::new();
                self.rows = Vec::new();
                self.rowcount = affected as i64;
            }
        }
        Ok(self)
    }

    /// 批量执行（协议级 batch）。返回总影响行数。
    pub fn executemany(&mut self, sql: &str, seq_of_params: &[Vec<Param>]) -> Result<u64> {
        if seq_of_params.is_empty() {
            return Ok(0);
        }
        self.client.executemany(sql, seq_of_params)
    }

    fn ensure_batch(&mut self) -> Result<()> {
        let need_more = match &self.lazy {
            Some(s) => s.more && self.pos >= self.rows.len(),
            None => false,
        };
        if need_more {
            let stmt = self.lazy.as_ref().unwrap().stmt;
            let ncols = self.lazy.as_ref().unwrap().ncols;
            let start = self.lazy.as_ref().unwrap().rows_raw.len() as i64;
            let more_raw = self.client.fetch_more(stmt, start, ncols)?;
            let decoded = self.client.decode_rows(&self.columns, &more_raw, stmt)?;
            let state = self.lazy.as_mut().unwrap();
            state.rows_raw.extend(more_raw);
            state.more = false; // cmd=7 一次拉剩余
            self.rows.extend(decoded);
        }
        Ok(())
    }

    pub fn fetchone(&mut self) -> Result<Option<Vec<Value>>> {
        self.ensure_batch()?;
        if self.pos >= self.rows.len() {
            return Ok(None);
        }
        let row = self.rows[self.pos].clone();
        self.pos += 1;
        Ok(Some(row))
    }

    pub fn fetchmany(&mut self, size: Option<usize>) -> Result<Vec<Vec<Value>>> {
        let size = size.unwrap_or(self.arraysize);
        // 缓存不足且还有更多时直接拉取
        loop {
            let more = match &self.lazy {
                Some(s) => s.more,
                None => false,
            };
            if !(more && self.pos + size > self.rows.len()) {
                break;
            }
            let stmt = self.lazy.as_ref().unwrap().stmt;
            let ncols = self.lazy.as_ref().unwrap().ncols;
            let start = self.lazy.as_ref().unwrap().rows_raw.len() as i64;
            let more_raw = self.client.fetch_more(stmt, start, ncols)?;
            if more_raw.is_empty() {
                break;
            }
            let decoded = self.client.decode_rows(&self.columns, &more_raw, stmt)?;
            let state = self.lazy.as_mut().unwrap();
            state.rows_raw.extend(more_raw);
            state.more = false;
            self.rows.extend(decoded);
        }
        let end = (self.pos + size).min(self.rows.len());
        let rows = self.rows[self.pos..end].to_vec();
        self.pos = end;
        Ok(rows)
    }

    pub fn fetchall(&mut self) -> Result<Vec<Vec<Value>>> {
        // 直接拉取所有剩余批次（fetch_more 内部循环到取完，一次调用返回全部剩余行），
        // 不依赖 pos 位置（修复：此前 while more + ensure_batch 在当前批次未消费完时死循环）
        while match &self.lazy {
            Some(s) => s.more,
            None => false,
        } {
            let stmt = self.lazy.as_ref().unwrap().stmt;
            let ncols = self.lazy.as_ref().unwrap().ncols;
            let start = self.lazy.as_ref().unwrap().rows_raw.len() as i64;
            let more_raw = self.client.fetch_more(stmt, start, ncols)?;
            let decoded = self.client.decode_rows(&self.columns, &more_raw, stmt)?;
            let state = self.lazy.as_mut().unwrap();
            state.rows_raw.extend(more_raw);
            state.more = false; // cmd=7 一次拉剩余（内部循环到取完）
            self.rows.extend(decoded);
        }
        let rows = self.rows[self.pos..].to_vec();
        self.pos = self.rows.len();
        Ok(rows)
    }

    /// 释放惰性游标的语句资源（cmd=44 + cmd=4）。
    pub fn close(&mut self) {
        if let Some(mut state) = self.lazy.take() {
            let _ = self.client.close_lazy(&mut state);
        }
        self.rows.clear();
    }
}

impl<'a> Drop for Cursor<'a> {
    fn drop(&mut self) {
        self.close();
    }
}

fn is_query(sql: &str) -> bool {
    crate::client::is_query_sql(sql)
}
