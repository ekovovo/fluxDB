// sql_editor_adapter/execution.rs —— SQL 语句切分与执行单元。
//
// 提供纯函数语句切分器 `split_statements` 与 `statement_around`，
// 均可被 fluxdb-editor-core 的 `ExecutionAdapter` 复用；不执行任何 SQL。

/// 把 SQL 文本按 `;` 切分成语句区间（字节偏移，半开区间 `[start, end)`）。
///
/// 切分时正确跳过字符串（`'`、`"`、反引号）与注释（`--`、`#`、`/* */`）。
/// 返回的区间已去除首尾空白，但不包含结尾分号。
pub fn split_statements(text: &str) -> Vec<Range> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut statements = Vec::new();
    let mut i = 0;
    // 当前语句首个非空白字符偏移；None 表示尚未开始。
    let mut start_ptr: Option<usize> = None;
    // 当前字符串引号（None 表示不在字符串内）。
    let mut quote: Option<u8> = None;
    let mut in_block = false;
    let mut in_line = false;

    while i < n {
        let b = bytes[i];
        // 字符串优先于注释识别：`'--'`、`'/*'` 和 `';'` 都只是字符串内容。
        if let Some(q) = quote {
            if b == b'\\' && i + 1 < n {
                i += 2;
                continue;
            }
            if b == q {
                if i + 1 < n && bytes[i + 1] == q {
                    i += 2;
                } else {
                    quote = None;
                    i += 1;
                }
                continue;
            }
            i += 1;
            continue;
        }
        if in_line {
            if b == b'\n' {
                in_line = false;
            }
            i += 1;
            continue;
        }
        if in_block {
            if b == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        // 注释起始
        if b == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            in_line = true;
            i += 2;
            continue;
        }
        if b == b'#' {
            in_line = true;
            i += 1;
            continue;
        }
        // 行注释 `//`（兼容代码编辑器输入）。
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            in_line = true;
            i += 2;
            continue;
        }
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            in_block = true;
            i += 2;
            continue;
        }
        // PG dollar-quoting `$tag$...$tag$` / `$$...$$`：其内分号/引号皆属函数体文本，不得切分。
        // 开启符为 `$` + 可选标识符标签 + `$`；`$1` 参数、SQLite `$name`（无尾 `$`）均不构成开启符。
        if b == b'$' {
            // 解析标签：空（`$$`）或 `[A-Za-z_][A-Za-z0-9_]*`。
            let mut j = i + 1;
            if j < n && bytes[j] != b'$' {
                if !(bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                    i += 1; // 非标签起始，按普通字符处理
                    continue;
                }
                j += 1;
                while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j >= n || bytes[j] != b'$' {
                    i += 1; // 未闭合为 `$tag$`，非 dollar-quote
                    continue;
                }
            } else if j >= n {
                i += 1;
                continue;
            }
            // 此处 `$...$` 是合法开启符（tag 空或非空），寻找匹配闭合 `$tag$`。
            let tag_len = j - (i + 1);
            let tag_start = i + 1;
            let mut k = j + 1;
            let mut closed = false;
            while k + tag_len + 1 <= n {
                if bytes[k] == b'$'
                    && bytes[k + tag_len] == b'$'
                    && &bytes[k + 1..k + 1 + tag_len] == &bytes[tag_start..tag_start + tag_len]
                {
                    i = k + tag_len + 1; // 跳过整个 dollar-quote
                    closed = true;
                    break;
                }
                k += 1;
            }
            if !closed {
                i = n; // 未闭合：保守读到结尾，其内分号不切分
            }
            continue;
        }
        // 进入字符串
        if b == b'\'' || b == b'"' || b == b'`' {
            quote = Some(b);
            i += 1;
            continue;
        }
        // 语句分隔符
        if b == b';' {
            if let Some(s) = start_ptr {
                let mut end = i;
                while end > s && bytes[end - 1].is_ascii_whitespace() {
                    end -= 1;
                }
                statements.push(Range::new(s, end));
                start_ptr = None;
            }
            i += 1;
            continue;
        }
        // 记录语句起点（忽略空白）
        if start_ptr.is_none() && !b.is_ascii_whitespace() {
            start_ptr = Some(i);
        }
        i += 1;
    }
    // 收尾：最后一段语句
    if let Some(s) = start_ptr {
        let mut end = n;
        while end > s && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end > s {
            statements.push(Range::new(s, end));
        }
    }
    statements
}

/// 在不可变编辑器快照上切分语句，不物化全文字符串。
///
/// 该扫描器与 `split_statements` 保持同一语义，但每次只读取一个 Rope 字节，
/// 适合折叠、诊断索引和执行定位等只需要范围的路径。
pub fn split_statement_ranges_snapshot(
    snapshot: &fluxdb_editor_core::BufferSnapshot,
) -> Vec<Range> {
    let n = snapshot.len();
    let mut statements = Vec::new();
    let mut bytes = snapshot
        .text_chunks_in_range(Range::new(0, n))
        .flat_map(|chunk| chunk.iter().copied())
        .enumerate()
        .peekable();
    let mut start_ptr = None;
    let mut quote = None;
    let mut in_block = false;
    let mut in_line = false;

    while let Some((i, b)) = bytes.next() {
        if let Some(q) = quote {
            if b == b'\\' && bytes.peek().is_some() {
                bytes.next();
                continue;
            }
            if b == q {
                if bytes.peek().is_some_and(|(_, next)| *next == q) {
                    bytes.next();
                } else {
                    quote = None;
                }
                continue;
            }
            continue;
        }
        if in_line {
            if b == b'\n' {
                in_line = false;
            }
            continue;
        }
        if in_block {
            if b == b'*' && bytes.peek().is_some_and(|(_, next)| *next == b'/') {
                in_block = false;
                bytes.next();
            }
            continue;
        }
        // PG dollar-quoting：与 split_statements 同语义，跳过 `$tag$...$tag$`（含未闭合）。
        if b == b'$' {
            let at = |pos: usize| snapshot.byte_at(pos);
            let mut j = i + 1;
            let mut tag_start = 0usize;
            let mut tag_len = 0usize;
            let is_opener = match at(j) {
                Some(b'$') => true, // `$$`：空标签
                Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                    tag_start = j;
                    j += 1;
                    while at(j).is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                        j += 1;
                    }
                    if at(j) == Some(b'$') {
                        tag_len = j - tag_start;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if is_opener {
                let opener_end = j + 1; // `$` 之后
                let mut k = opener_end;
                let mut closed = false;
                while k + tag_len + 1 <= n {
                    if at(k) == Some(b'$')
                        && at(k + tag_len) == Some(b'$')
                        && snapshot.text_in_range(Range::new(k + 1, k + 1 + tag_len))
                            == snapshot.text_in_range(Range::new(tag_start, tag_start + tag_len))
                    {
                        // 跳到结束 `$` 之后：少消费 1 个（当前位已由外层 next 取过）。
                        for _ in 0..(k + tag_len + 1 - i) {
                            bytes.next();
                        }
                        closed = true;
                        break;
                    }
                    k += 1;
                }
                if !closed {
                    // 未闭合：消费到结尾。
                    while bytes.next().is_some() {}
                }
                continue;
            }
            continue; // 非开启符，按普通字符（后续若为分号自然切分）
        }
        if b == b'\'' || b == b'"' || b == b'`' {
            quote = Some(b);
            continue;
        }
        if (b == b'-' && bytes.peek().is_some_and(|(_, next)| *next == b'-'))
            || b == b'#'
            || (b == b'/' && bytes.peek().is_some_and(|(_, next)| *next == b'/'))
        {
            in_line = true;
            if b != b'#' {
                bytes.next();
            }
            continue;
        }
        if b == b'/' && bytes.peek().is_some_and(|(_, next)| *next == b'*') {
            in_block = true;
            bytes.next();
            continue;
        }
        if b == b';' {
            if let Some(s) = start_ptr {
                let mut end = i;
                while end > s
                    && snapshot
                        .byte_at(end.saturating_sub(1))
                        .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    end -= 1;
                }
                if end > s {
                    statements.push(Range::new(s, end));
                }
                start_ptr = None;
            }
            continue;
        }
        if start_ptr.is_none() && !b.is_ascii_whitespace() {
            start_ptr = Some(i);
        }
    }
    if let Some(s) = start_ptr {
        let mut end = n;
        while end > s
            && snapshot
                .byte_at(end.saturating_sub(1))
                .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            end -= 1;
        }
        if end > s {
            statements.push(Range::new(s, end));
        }
    }
    statements
}

/// 在快照上定位光标所在语句，避免为执行单条 SQL 复制全文。
pub fn statement_around_snapshot(
    snapshot: &fluxdb_editor_core::BufferSnapshot,
    offset: usize,
) -> Option<(usize, usize)> {
    let offset = snapshot.clamp_to_char_boundary(offset.min(snapshot.len()));
    split_statement_ranges_snapshot(snapshot)
        .into_iter()
        .find(|range| offset >= range.start && offset <= range.end)
        .map(|range| (range.start, range.end))
}

/// 返回包含给定字节偏移的语句区间 `(start, end)`。
///
/// 若 offset 位于语句边界处（等于某结束端），归入该语句；否则返回
/// `None` 供调用方决定回退策略。
pub fn statement_around(text: &str, offset: usize) -> Option<(usize, usize)> {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let statements = split_statements(text);
    for stmt in &statements {
        if offset >= stmt.start && offset <= stmt.end {
            return Some((stmt.start, stmt.end));
        }
    }
    None
}

