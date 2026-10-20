//! 词法层：光标所在词的边界、限定符、前序关键字，以及语句切分。
//!
//! 这一层不依赖 AST，残句也永远可用；AST 通道失败时它就是兜底。

use sqlparser::dialect::Dialect;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer};

use crate::model::WordContext;
use crate::parse::LineColIndex;

/// 带字节偏移的 token。
#[derive(Debug, Clone)]
pub(crate) struct TokenSpan {
    pub(crate) token: Token,
    /// 相对窗口文本的起始字节偏移。
    pub(crate) start: usize,
    /// 相对窗口文本的结束字节偏移（不含）。
    pub(crate) end: usize,
}

impl TokenSpan {
    /// 该 token 是否为普通（非引号）标识符或关键字。
    pub(crate) fn word_value(&self) -> Option<&str> {
        match &self.token {
            Token::Word(word) => Some(word.value.as_str()),
            _ => None,
        }
    }

    /// 该 token 是否为关键字（非引号且被方言识别）。
    pub(crate) fn keyword(&self) -> Option<Keyword> {
        match &self.token {
            Token::Word(word)
                if word.quote_style.is_none() && word.keyword != Keyword::NoKeyword =>
            {
                Some(word.keyword)
            }
            _ => None,
        }
    }
}

/// 词法切词；tokenize 失败时保留已产出的部分（残句常见，不应整体丢弃）。
pub(crate) fn tokenize_spans(text: &str, dialect: &dyn Dialect) -> Vec<TokenSpan> {
    let index = LineColIndex::new(text);
    let mut tokenizer = Tokenizer::new(dialect, text);
    // 失败时拿到部分 token 流；拿不到就退化为空（调用方走纯字符扫描）。
    let tokens: Vec<TokenWithSpan> = tokenizer.tokenize_with_location().unwrap_or_default();
    tokens
        .into_iter()
        .filter(|token| !matches!(token.token, Token::EOF))
        .map(|token| TokenSpan {
            start: index.offset(token.span.start),
            end: index
                .offset(token.span.end)
                .saturating_add(1)
                .min(text.len()),
            token: token.token,
        })
        .collect()
}

/// 按分号切分语句，返回每段的 `[start, end)` 字节区间（已去除首尾空白）。
pub(crate) fn statement_ranges(text: &str, dialect: &dyn Dialect) -> Vec<(usize, usize)> {
    let tokens = tokenize_spans(text, dialect);
    let mut ranges = Vec::new();
    let mut segment_start = 0usize;
    for token in &tokens {
        if matches!(token.token, Token::SemiColon) {
            push_trimmed_range(text, segment_start, token.start, &mut ranges);
            segment_start = token.end;
        }
    }
    push_trimmed_range(text, segment_start, text.len(), &mut ranges);
    ranges
}

fn push_trimmed_range(text: &str, start: usize, end: usize, ranges: &mut Vec<(usize, usize)>) {
    let slice = &text[start..end];
    let trimmed_start = start + (slice.len() - slice.trim_start().len());
    let trimmed_end = end - (slice.len() - slice.trim_end().len());
    if trimmed_start < trimmed_end {
        ranges.push((trimmed_start, trimmed_end));
    }
}

/// 光标所在语句区间；窗口内无语句时返回整个窗口的去空白区间。
pub(crate) fn statement_range_at(
    text: &str,
    cursor: usize,
    dialect: &dyn Dialect,
) -> (usize, usize) {
    let ranges = statement_ranges(text, dialect);
    for &(start, end) in &ranges {
        if cursor >= start && cursor <= end {
            return (start, end);
        }
    }
    // 光标落在语句之间（分号后空白处）：归到其后最近的一条，否则归到前一条。
    for &(start, end) in &ranges {
        if cursor < start {
            return (start, end);
        }
    }
    ranges.last().copied().unwrap_or((0, text.len()))
}

/// 光标所在词的字符判定：字母数字、下划线、美元符（含非 ASCII 标识符）。
fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

fn is_quote_char(ch: char) -> bool {
    ch == '"' || ch == '`' || ch == '['
}

/// 从 `end` 往前扫一个标识符（可带引号），返回其起点。
fn scan_ident_start(text: &str, end: usize) -> usize {
    let mut start = end;
    for (offset, ch) in text[..end].char_indices().rev() {
        if is_ident_char(ch) {
            start = offset;
        } else {
            break;
        }
    }
    // 引号包裹的标识符：把开引号也纳入替换区间。
    if start > 0 {
        let before = text[..start].chars().next_back();
        if before.is_some_and(is_quote_char) {
            return start - before.unwrap().len_utf8();
        }
    }
    start
}

/// 光标是否位于一个未闭合的引号标识符内；是则返回开引号的位置与字符。
///
/// 引号标识符可以含空格（`"my tab`），因此不能用标识符字符反扫。必须从语句头正向
/// 配对扫描：反向只看「最近一个引号」会把已闭合标识符的闭引号误判成开引号，
/// 导致 `from \`3d_user\` u left` 的词起点被拉到闭引号之后。
fn open_quote_before(text: &str, cursor: usize) -> Option<(usize, char)> {
    let mut open: Option<(usize, char)> = None;
    for (offset, ch) in text[..cursor].char_indices() {
        if !is_quote_char(ch) {
            continue;
        }
        let closer = if ch == '[' { ']' } else { ch };
        match open {
            // 同类引号成对：遇到闭引号即出栈。
            Some((_, opened)) if (if opened == '[' { ']' } else { opened }) == closer => {
                open = None;
            }
            // 不同类型引号各自独立，不互相闭合。
            Some(_) => {}
            None => open = Some((offset, ch)),
        }
    }
    open
}

/// 光标是否落在字符串字面量或注释里。
///
/// 这两种位置不是 SQL 语法位置，任何候选都是噪声（`-- 待办：补 JOIN |` 补出 `JOIN` 尤其
/// 荒诞）。单引号是字符串；双引号 / 反引号 / 方括号是标识符（见 [`is_quote_char`]），
/// 仍要补全。不支持 PostgreSQL 的 `$tag$` 美元引用。
pub(crate) fn in_literal_or_comment(text: &str, cursor: usize) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < cursor {
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < cursor {
                    match bytes[i] {
                        // 转义：两个连续单引号算一个字符，MySQL 的 `\'` 也认。
                        b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 2,
                        b'\\' => i += 2,
                        b'\'' => break,
                        _ => i += 1,
                    }
                }
                if i >= cursor {
                    return true; // 未闭合的字符串，光标还在里面
                }
                i += 1; // 跳过闭引号
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < cursor && bytes[i] != b'\n' {
                    i += 1;
                }
                if i >= cursor {
                    return true;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < cursor {
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                if i >= cursor {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// 计算光标处的词法上下文。
pub(crate) fn word_context(text: &str, cursor: usize, dialect: &dyn Dialect) -> WordContext {
    let cursor = cursor.min(text.len());

    // 引号标识符：`"my tab` 这类未闭合输入要按大小写敏感处理，
    // 且替换区间从开引号开始，插入时由宿主补全整个带引号标识符。
    let open_quote = open_quote_before(text, cursor);
    let quoted = open_quote.is_some();
    let word_start = match open_quote {
        Some((quote_pos, quote)) => quote_pos + quote.len_utf8(),
        None => scan_ident_start(text, cursor),
    };
    let word_part = text[word_start..cursor].to_string();

    let mut replace_start = match open_quote {
        Some((quote_pos, _)) => quote_pos,
        None => word_start,
    };
    let mut qualifier_segments: Vec<String> = Vec::new();

    // 向前吃掉 `a.b.` 形式的限定符链；`schema.tbl.col` 整体作为 qualifier。
    let mut seg_end = replace_start;
    while let Some((qual_end, '.')) = text[..seg_end].char_indices().next_back() {
        let qual_start = scan_ident_start(text, qual_end);
        if qual_start == qual_end {
            break;
        }
        qualifier_segments.push(text[qual_start..qual_end].to_string());
        replace_start = qual_start;
        // 继续向前：可能是 `schema.table.` 的上一级。
        if !text[..qual_start].ends_with('.') {
            break;
        }
        // 下一轮从这段标识符的起点继续往前找：那里的字符就是上一级的点号。
        seg_end = qual_start;
    }
    let qualifier = (!qualifier_segments.is_empty()).then(|| {
        qualifier_segments.reverse();
        qualifier_segments.join(".")
    });

    // 词尾：光标后已存在的同词字符一并纳入替换区间；引号词吃到闭引号。
    let mut replace_end = cursor;
    if let Some((_, quote)) = open_quote {
        let closer = if quote == '[' { ']' } else { quote };
        if let Some(offset) = text[cursor..].find(closer) {
            replace_end = cursor + offset + closer.len_utf8();
        }
    } else {
        for (offset, ch) in text[cursor..].char_indices() {
            if is_ident_char(ch) {
                replace_end = cursor + offset + ch.len_utf8();
            } else {
                break;
            }
        }
    }

    let tokens = tokenize_spans(&text[..replace_start], dialect);
    let mut prev_words: Vec<String> = tokens
        .iter()
        .filter_map(|token| token.word_value().map(|value| value.to_ascii_lowercase()))
        .collect();
    if prev_words.len() > 8 {
        prev_words.drain(..prev_words.len() - 8);
    }
    let prev_keyword = tokens.iter().rev().find_map(|token| {
        token
            .keyword()
            .map(|keyword| keyword.to_string().to_ascii_uppercase())
    });

    WordContext {
        word_part,
        replace_start,
        prefix_start: if open_quote.is_some() {
            replace_start
        } else {
            word_start
        },
        replace_end,
        quoted,
        qualifier,
        prev_words,
        prev_keyword,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::GenericDialect;

    fn ctx(text: &str) -> WordContext {
        let cursor = text.find('|').expect("用例必须用 | 标记光标");
        let stripped = text.replace('|', "");
        word_context(&stripped, cursor, &GenericDialect {})
    }

    #[test]
    fn empty_prefix_at_statement_start() {
        let c = ctx("|SELECT 1");
        assert_eq!(c.word_part, "");
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.prev_keyword, None);
    }

    #[test]
    fn captures_partial_word() {
        let c = ctx("SELECT us|");
        assert_eq!(c.word_part, "us");
        assert_eq!(c.replace_start, 7);
        assert_eq!(c.prev_keyword.as_deref(), Some("SELECT"));
    }

    #[test]
    fn captures_qualifier() {
        let c = ctx("SELECT u.| FROM users u");
        assert_eq!(c.word_part, "");
        assert_eq!(c.qualifier.as_deref(), Some("u"));
        assert_eq!(c.replace_start, 7);
    }

    #[test]
    fn captures_multi_level_qualifier() {
        let c = ctx("SELECT public.users.| FROM public.users");
        assert_eq!(c.qualifier.as_deref(), Some("public.users"));
        assert_eq!(c.word_part, "");
    }

    #[test]
    fn captures_qualifier_with_partial_column() {
        let c = ctx("SELECT u.na| FROM users u");
        assert_eq!(c.qualifier.as_deref(), Some("u"));
        assert_eq!(c.word_part, "na");
    }

    /// 已闭合的引号标识符不能把词起点拉到闭引号之后（否则 `u left` 会被当成一个词）。
    #[test]
    fn closed_quoted_identifier_does_not_leak_into_word() {
        let c = ctx("SELECT * from `3d_user` u left|");
        assert_eq!(c.word_part, "left");
        assert!(!c.quoted);
        let c = ctx("SELECT * from `3d_user` u |");
        assert_eq!(c.word_part, "");
        assert!(!c.quoted);
    }

    /// 未闭合引号仍按大小写敏感处理，替换区间含开引号。
    #[test]
    fn unterminated_quote_keeps_quote_in_replacement() {
        let c = ctx("SELECT * from `3d_us|");
        assert_eq!(c.word_part, "3d_us");
        assert!(c.quoted);
        assert_eq!(c.replace_start, "SELECT * from ".len());
    }

    #[test]
    fn marks_quoted_identifier() {
        let c = ctx("SELECT \"my tab|");
        assert!(c.quoted);
        assert_eq!(c.word_part, "my tab");
    }

    #[test]
    fn replaces_suffix_word() {
        let c = ctx("SELECT us|ers");
        assert_eq!(c.replace_end, 12);
    }

    #[test]
    fn prev_keyword_skips_string_literals() {
        let c = ctx("SELECT 'from x' |");
        assert_eq!(c.prev_keyword.as_deref(), Some("SELECT"));
    }

    #[test]
    fn detects_cursor_inside_literals_and_comments() {
        let inside = |text: &str| {
            let cursor = text.find('|').expect("用例必须用 | 标记光标");
            in_literal_or_comment(&text.replace('|', ""), cursor)
        };
        assert!(inside("SELECT 'abc|"));
        assert!(inside("SELECT 'a''b|"));
        assert!(inside("SELECT 'it\\'s|"));
        assert!(inside("SELECT * FROM t -- 待办：补 JOIN |"));
        assert!(inside("SELECT /* 未闭合 |"));
        assert!(!inside("SELECT 'abc' |"));
        assert!(!inside("SELECT 'a''b' |"));
        assert!(!inside("SELECT * FROM t -- 注释\n|"));
        assert!(!inside("SELECT /* 注释 */ |"));
        // 双引号 / 反引号是标识符，不是字面量。
        assert!(!inside("SELECT \"my tab|"));
        assert!(!inside("SELECT * from `3d_us|"));
    }

    #[test]
    fn statement_range_picks_cursor_statement() {
        let text = "SELECT 1; SELECT 2; SELECT 3";
        let ranges = statement_ranges(text, &GenericDialect {});
        assert_eq!(ranges.len(), 3);
        let (start, end) = statement_range_at(text, 15, &GenericDialect {});
        assert_eq!(&text[start..end], "SELECT 2");
    }
}
