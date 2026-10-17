//! Snippet 解析：从 `insert_text` 中提取纯文本与 tabstop 占位符范围。
//!
//! 支持的语法（对齐 LSP / VS Code snippet 语法子集）：
//! - `$1`、`$2`：匿名 tabstop。
//! - `${1:placeholder}`：带默认值的 tabstop。
//! - `$$1`：转义字面量 `$1`（不产生 tabstop）。
//!
//! 解析结果用于 `accept_completion`：先得到纯文本（去除控制标记），再记录每个
//! tabstop 在纯文本中的相对 byte range，供 snippet 会话选中首个占位、Tab 切换。
//!
//! 未知语法（如 `${1:foo` 未闭合）返回受控错误，不把控制标记插入用户文本。

use crate::model::Range;

/// 解析后的 snippet：纯文本 + 有序 tabstop 列表。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// 去除控制标记后的纯文本（实际插入 buffer 的内容）。
    pub text: String,
    /// 每个 tabstop 在 `text` 中的相对 byte range，按出现顺序排列。
    pub tabstops: Vec<Range>,
}

/// 解析错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetError {
    /// 占位符未闭合（如 `${1:foo`）。
    UnclosedPlaceholder,
    /// tabstop 编号非法（非数字）。
    InvalidTabstop,
}

/// 解析 snippet 文本。
///
/// 扫描输入，识别 `$1`、`${1:placeholder}` 与转义 `$$`。未闭合的 `${` 返回错误。
pub fn parse_snippet(input: &str) -> Result<Snippet, SnippetError> {
    let mut text = String::with_capacity(input.len());
    let mut tabstops: Vec<Range> = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            // 普通字符：按 UTF-8 边界拷贝整个 char，避免切分多字节。
            let ch = input[i..].chars().next().expect("非 $ 起点必为合法 char");
            text.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // 以 $ 起点：看后续字符决定语义。
        match bytes.get(i + 1) {
            Some(b'$') => {
                // 转义：$$ → 字面量 $。
                text.push('$');
                i += 2;
            }
            Some(b'{') => {
                // ${num:placeholder} 形式。
                let close = input[i + 1..]
                    .find('}')
                    .ok_or(SnippetError::UnclosedPlaceholder)?;
                let inner = &input[i + 2..i + 1 + close];
                let colon = inner.find(':');
                let (num_str, default) = match colon {
                    Some(idx) => (&inner[..idx], &inner[idx + 1..]),
                    None => (inner, ""),
                };
                num_str
                    .parse::<u32>()
                    .map_err(|_| SnippetError::InvalidTabstop)?;
                let start = text.len();
                text.push_str(default);
                let end = text.len();
                tabstops.push(Range::new(start, end));
                i = i + 1 + close + 1;
            }
            Some(c) if c.is_ascii_digit() => {
                // $num 形式：匿名 tabstop，不插入默认文本。
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let num_str = &input[i + 1..j];
                num_str
                    .parse::<u32>()
                    .map_err(|_| SnippetError::InvalidTabstop)?;
                // 匿名 tabstop 在纯文本中对应零宽位置；记录为 start==end 的 range，
                // 会话层据此把光标定位到该位置。
                let pos = text.len();
                tabstops.push(Range::new(pos, pos));
                i = j;
            }
            _ => {
                // 孤立的 $（后接非数字非 { 非 $）：保留字面量。
                text.push('$');
                i += 1;
            }
        }
    }
    Ok(Snippet { text, tabstops })
}
