//! 容错解析：把光标所在语句解析成 AST，残句按「最长可解析前缀」截断。
//!
//! sqlparser 的 span 是 1 基 line/column 且 end 为闭区间，这里统一转成
//! 相对窗口文本的字节偏移，供槽位定位使用。

use sqlparser::ast::Statement;
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Location;

/// 文本行首偏移表，用于把 `Location`（1 基 line/column）转成字节偏移。
#[derive(Debug, Clone)]
pub(crate) struct LineColIndex {
    line_starts: Vec<usize>,
    len: usize,
}

impl LineColIndex {
    pub(crate) fn new(text: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            line_starts,
            len: text.len(),
        }
    }

    /// `Location` → 字节偏移。空 span（line=0）返回 0；越界裁剪到文本长度。
    pub(crate) fn offset(&self, location: Location) -> usize {
        if location.line == 0 {
            return 0;
        }
        let line_index = (location.line - 1) as usize;
        let Some(&line_start) = self.line_starts.get(line_index) else {
            return self.len;
        };
        // column 从 1 开始；column=0 表示空 span，按行首处理。
        let column = location.column.saturating_sub(1) as usize;
        (line_start + column).min(self.len)
    }
}

/// 一次成功（可能截断）的解析结果。
#[derive(Debug)]
pub(crate) struct TolerantParse {
    pub(crate) statements: Vec<Statement>,
    /// 实际参与解析的文本长度（原语句的前缀）。掩码修复时等于原语句长度。
    pub(crate) parsed_len: usize,
    /// 插入占位标识符的位置：该点之后的偏移比原文多 1，需要回映射。
    pub(crate) inserted_at: Option<usize>,
}

impl TolerantParse {
    /// 把解析文本里的偏移映射回原语句偏移。
    pub(crate) fn map_offset(&self, offset: usize) -> usize {
        match self.inserted_at {
            Some(at) if offset > at => offset - 1,
            _ => offset,
        }
    }
}

/// 截断重试上限：够覆盖 `SELECT a, b, c ... WHERE x =` 这类残句，又不会失控。
const MAX_TRUNCATE_ATTEMPTS: usize = 12;

/// 解析当前语句，按四级修复：
///
/// 1. 先把光标处的残词掩码成**等长**占位标识符再解析——长度不变、偏移可原样使用，
///    且不会丢掉后文子句。**必须优先于原文解析**：sqlparser 会把
///    `SELECT u. FROM users` 贪婪地吞成 `u.FROM AS users`（FROM 子句变空），
///    原文解析成功反而得到错误 AST。
/// 2. 原文直接解析（掩码无法解析时兜底）。
/// 3. 在光标处**插入**一个占位标识符再解析（`SELECT  FROM ...` 这类光标在空隙的
///    残句，掩码无从下手）；插入点之后的偏移需要回映射。仅在光标前不是标识符字符时
///    启用，避免把 `WHERE` 粘成 `WHEREx`。
/// 4. 从尾部按 token 边界截断，取最长可解析前缀。
///
/// 返回 `None` 表示四级都失败，调用方应降级到词法通道。
pub(crate) fn parse_tolerant(
    statement: &str,
    mask: Option<(usize, usize)>,
    cursor: usize,
    dialect: &dyn Dialect,
) -> Option<TolerantParse> {
    if statement.trim().is_empty() {
        return None;
    }

    if let Some(masked) = mask_word(statement, mask)
        && let Ok(statements) = Parser::parse_sql(dialect, &masked)
        && !statements.is_empty()
    {
        return Some(TolerantParse {
            statements,
            // 掩码保持长度，偏移与原文一一对应。
            parsed_len: statement.len(),
            inserted_at: None,
        });
    }

    if let Ok(statements) = Parser::parse_sql(dialect, statement) {
        return Some(TolerantParse {
            statements,
            parsed_len: statement.len(),
            inserted_at: None,
        });
    }

    if cursor <= statement.len()
        && statement.is_char_boundary(cursor)
        && can_insert_placeholder(statement, cursor)
    {
        let inserted = insert_placeholder(statement, cursor);
        if let Ok(statements) = Parser::parse_sql(dialect, &inserted)
            && !statements.is_empty()
        {
            return Some(TolerantParse {
                statements,
                parsed_len: statement.len(),
                inserted_at: Some(cursor),
            });
        }
    }

    // 从尾部往前找 token 起点作为截断点：第一个成功的就是最长可解析前缀。
    let offsets = token_start_offsets(statement, dialect);
    let mut attempts = 0usize;
    for &cut in offsets.iter().rev() {
        if attempts >= MAX_TRUNCATE_ATTEMPTS {
            break;
        }
        if cut == 0 || cut >= statement.len() {
            continue;
        }
        let candidate = &statement[..cut];
        if candidate.trim().is_empty() {
            continue;
        }
        attempts += 1;
        if let Ok(statements) = Parser::parse_sql(dialect, candidate)
            && !statements.is_empty()
        {
            return Some(TolerantParse {
                statements,
                parsed_len: cut,
                inserted_at: None,
            });
        }
    }

    None
}

/// 把 `range` 区间内的字节替换为等长占位标识符；区间非法或为空时返回 `None`。
fn mask_word(statement: &str, mask: Option<(usize, usize)>) -> Option<String> {
    let (start, end) = mask?;
    if start >= end
        || end > statement.len()
        || !statement.is_char_boundary(start)
        || !statement.is_char_boundary(end)
    {
        return None;
    }
    let mut masked = String::with_capacity(statement.len());
    masked.push_str(&statement[..start]);
    masked.push_str(&"x".repeat(end - start));
    masked.push_str(&statement[end..]);
    Some(masked)
}

/// 仅在「token 间隙」插入占位符：光标前一个字符不是标识符/引号，
/// 否则会把前一个词粘成一个新词（`WHERE` + `x` → `WHEREx`）。
fn can_insert_placeholder(statement: &str, at: usize) -> bool {
    match statement[..at].chars().next_back() {
        None => true,
        Some(ch) => {
            !(ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '"' || ch == '`' || ch == '[')
        }
    }
}

/// 在 `at` 处插入一个占位标识符，长度 +1。
fn insert_placeholder(statement: &str, at: usize) -> String {
    let mut inserted = String::with_capacity(statement.len() + 1);
    inserted.push_str(&statement[..at]);
    inserted.push('x');
    inserted.push_str(&statement[at..]);
    inserted
}

/// token 起点的字节偏移列表（升序）。
fn token_start_offsets(statement: &str, dialect: &dyn Dialect) -> Vec<usize> {
    let index = LineColIndex::new(statement);
    let mut tokenizer = sqlparser::tokenizer::Tokenizer::new(dialect, statement);
    let Ok(tokens) = tokenizer.tokenize_with_location() else {
        return Vec::new();
    };
    tokens
        .iter()
        .map(|token| index.offset(token.span.start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::GenericDialect;

    fn parse(sql: &str) -> Option<TolerantParse> {
        parse_tolerant(sql, None, sql.len(), &GenericDialect {})
    }

    /// 掩码区间取「残词」本身：调用方（引擎）从 word_context 算出。
    fn parse_masked(sql: &str, mask: (usize, usize)) -> Option<TolerantParse> {
        parse_tolerant(sql, Some(mask), mask.0, &GenericDialect {})
    }

    #[test]
    fn parses_complete_statement() {
        let parsed = parse("SELECT id FROM users").expect("完整语句应解析成功");
        assert_eq!(parsed.parsed_len, "SELECT id FROM users".len());
    }

    #[test]
    fn masks_incomplete_word_keeping_length() {
        // `SELECT u. FROM users`：掩码 `u.` 后整句可解析，且 FROM 子句仍在。
        let sql = "SELECT u. FROM users";
        let parsed = parse_masked(sql, (7, 9)).expect("掩码后应解析成功");
        assert_eq!(parsed.parsed_len, sql.len());
        assert!(matches!(
            parsed.statements.first(),
            Some(Statement::Query(_))
        ));
    }

    #[test]
    fn mask_is_preferred_over_greedy_parse() {
        // 原文解析会贪婪吞掉 FROM；掩码解析必须优先，保住 FROM 子句。
        let sql = "SELECT u. FROM users";
        let parsed = parse_masked(sql, (7, 9)).expect("掩码后应解析成功");
        match parsed.statements.first() {
            Some(Statement::Query(query)) => match query.body.as_ref() {
                sqlparser::ast::SetExpr::Select(select) => {
                    assert_eq!(select.from.len(), 1, "FROM 子句必须保留：{select:?}");
                }
                other => panic!("期望 Select，实际 {other:?}"),
            },
            other => panic!("期望 Query，实际 {other:?}"),
        }
    }

    #[test]
    fn truncates_incomplete_where() {
        // 光标在 `WHERE` 词尾：不能插入占位符（否则粘成 WHEREx），退回截断。
        let sql = "SELECT id FROM users WHERE";
        let parsed =
            parse_tolerant(sql, None, sql.len(), &GenericDialect {}).expect("应截断到 FROM 子句");
        assert!(parsed.parsed_len < sql.len());
    }

    #[test]
    fn inserts_placeholder_in_gap() {
        // 光标在 `WHERE` 之后的空隙：原文/掩码都失败，插入占位符后可解析。
        let sql = "SELECT * FROM users WHERE  ORDER BY id";
        let cursor = sql.find("WHERE ").unwrap() + "WHERE ".len();
        let parsed =
            parse_tolerant(sql, None, cursor, &GenericDialect {}).expect("应插入占位符后解析成功");
        assert_eq!(parsed.inserted_at, Some(cursor));
        assert_eq!(parsed.map_offset(cursor + 1), cursor);
        assert_eq!(parsed.map_offset(cursor), cursor);
    }

    #[test]
    fn does_not_glue_placeholder_to_word() {
        // `WHEREx` 会伪造出合法语句，必须在词尾禁用插入级。
        let sql = "SELECT id FROM users WHERE";
        assert!(!can_insert_placeholder(sql, sql.len()));
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(parse("@@@ ###").is_none());
    }

    #[test]
    fn returns_none_for_empty() {
        assert!(parse("   ").is_none());
    }

    #[test]
    fn line_col_index_maps_multiline() {
        let text = "SELECT 1\nFROM users\nWHERE id = 1";
        let index = LineColIndex::new(text);
        assert_eq!(index.offset(Location { line: 1, column: 1 }), 0);
        assert_eq!(index.offset(Location { line: 2, column: 1 }), 9);
        assert_eq!(index.offset(Location { line: 3, column: 1 }), 20);
    }
}
