//! SQL Server 脚本边界：T-SQL `BEGIN … END` 块与 `GO` 批分隔符。
//!
//! 与 Oracle 的差别只有一处：`BEGIN` 并不总是开启块。
//! `BEGIN TRANSACTION` / `BEGIN TRAN` / `BEGIN DISTRIBUTED TRANSACTION`
//! 开启的是事务，没有配对的 `END`，当成块会让后面所有语句被吞。
//! `BEGIN TRY` / `BEGIN CATCH` 反而配对（`END TRY` / `END CATCH`），保留。

use super::{ScriptSyntax, StatementForm};

pub(super) const SQLSERVER_SYNTAX: SqlServerSyntax = SqlServerSyntax;

pub(super) struct SqlServerSyntax;

/// `BEGIN` 后面跟这些词是事务控制，不是块起点。
const TRANSACTION_AFTER_BEGIN: &[&str] = &["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG"];

impl ScriptSyntax for SqlServerSyntax {
    fn statement_form(&self, head: &[String]) -> StatementForm {
        let words: Vec<&str> = head.iter().map(String::as_str).collect();
        match strip_create_prefix(&words).as_slice() {
            // 只列「块开启前有声明段」的语句：`IS`/`AS` 之后、`BEGIN` 之前的变量声明
            // 以 `;` 结尾，这些分号不能切。匿名 `BEGIN … END` 没有声明段，走 Sql 即可——
            // 块深度跟踪与语句形态无关，`BEGIN`/`END` 在任何形态下都成对计数。
            ["PROCEDURE", ..] | ["PROC", ..] | ["FUNCTION", ..] | ["TRIGGER", ..] => {
                StatementForm::Block {
                    header_opens: false,
                }
            }
            _ => StatementForm::Sql,
        }
    }

    fn opens_block(
        &self,
        _form: StatementForm,
        word: &str,
        _prev: Option<&str>,
        next: Option<&str>,
    ) -> bool {
        match word {
            "BEGIN" => !TRANSACTION_AFTER_BEGIN.contains(&next.unwrap_or_default()),
            "CASE" => true,
            _ => false,
        }
    }

    fn closes_block(&self, word: &str) -> bool {
        word == "END"
    }

    fn batch_separator(&self) -> Option<&'static str> {
        Some("GO")
    }
}

/// 剥掉 `CREATE [OR ALTER] [OR REPLACE]` 前缀。
fn strip_create_prefix<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut rest = words;
    if rest.first() == Some(&"CREATE") {
        rest = &rest[1..];
        if rest.first() == Some(&"OR") && matches!(rest.get(1), Some(&"ALTER") | Some(&"REPLACE")) {
            rest = &rest[2..];
        }
    }
    rest.to_vec()
}
