//! Oracle 脚本边界：PL/SQL 块与 `q'…'` 替代引号。
//!
//! 替代引号由 `OracleDialect` 词法器处理（含 `nq'…'`），本文件只管块结构。
//! 块规则对齐 Flyway `OracleParser.adjustBlockDepth` 与 DBeaver
//! `OracleSQLDialect.ORACLE_BEGIN_END_BLOCK`：`BEGIN`/`IF`/`LOOP`/`CASE` 开启一层，
//! `END` 关闭一层，`END IF`/`END LOOP`/`END CASE` 的尾巴不重复计数。

use super::{ScriptSyntax, StatementForm};

pub(super) const ORACLE_SYNTAX: OracleSyntax = OracleSyntax;

pub(super) struct OracleSyntax;

/// `CASE` 紧跟在这些词后面只能是标识符，不是 `CASE` 表达式起点
/// （Flyway `KEYWORDS_PRECEDING_CASE_AS_IDENTIFIER`）。
const CASE_AS_IDENTIFIER_AFTER: &[&str] = &["FROM", "INTO", "UPDATE", "JOIN", "TABLE", "AS"];

impl ScriptSyntax for OracleSyntax {
    fn statement_form(&self, head: &[String]) -> StatementForm {
        let words: Vec<&str> = head.iter().map(String::as_str).collect();
        match strip_create_prefix(&words).as_slice() {
            // 匿名块与 `DECLARE` 声明段：真正的第一层由 `BEGIN` 贡献，
            // 声明段的 `;` 因此在块开启前不能切。
            ["DECLARE", ..] | ["BEGIN", ..] => StatementForm::Block {
                header_opens: false,
            },
            // 包体与类型体自带一层：初始化段的 `BEGIN` 与包本身共用一个 `END`，
            // 深度按设计就回不到 0，收尾靠独占一行的 `/`。
            ["PACKAGE", "BODY", ..] | ["TYPE", "BODY", ..] => {
                StatementForm::Block { header_opens: true }
            }
            ["PACKAGE", ..]
            | ["TYPE", ..]
            | ["PROCEDURE", ..]
            | ["FUNCTION", ..]
            | ["TRIGGER", ..] => StatementForm::Block {
                header_opens: false,
            },
            _ => StatementForm::Sql,
        }
    }

    fn opens_block(
        &self,
        _form: StatementForm,
        word: &str,
        prev: Option<&str>,
        _next: Option<&str>,
    ) -> bool {
        match word {
            "BEGIN" => true,
            // 复合触发器 `COMPOUND TRIGGER … END TRIGGER` 整体是一层块。
            "TRIGGER" => prev == Some("COMPOUND"),
            // GOTO 后面的词可能是恰好叫 IF/LOOP 的标签，不能当块起点。
            "IF" | "LOOP" | "CASE" => {
                prev != Some("GOTO")
                    && !CASE_AS_IDENTIFIER_AFTER.contains(&prev.unwrap_or_default())
            }
            _ => false,
        }
    }

    fn closes_block(&self, word: &str) -> bool {
        word == "END"
    }

    fn batch_separator(&self) -> Option<&'static str> {
        Some("/")
    }
}

/// 剥掉 `CREATE [OR REPLACE] [NON]EDITIONABLE` 前缀，剩余词序列的头部即语句类型。
fn strip_create_prefix<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut rest = words;
    if rest.first() == Some(&"CREATE") {
        rest = &rest[1..];
        if rest.first() == Some(&"OR") && rest.get(1) == Some(&"REPLACE") {
            rest = &rest[2..];
        }
        if matches!(rest.first(), Some(&"EDITIONABLE") | Some(&"NONEDITIONABLE")) {
            rest = &rest[1..];
        }
    }
    rest.to_vec()
}
