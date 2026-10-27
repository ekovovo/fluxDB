// sql_editor_adapter/execution.rs —— SQL 语句切分与执行单元。
//
// 提供纯函数语句切分器 `split_statements` 与 `statement_around`，
// 均可被 fluxdb-editor-core 的 `ExecutionAdapter` 复用；不执行任何 SQL。

/// 按数据库方言切分语句（字节偏移，半开区间 `[start, end)`）。
///
/// 真正决定语句边界的是 [`fluxdb_app::sql_statement_byte_ranges`]：词法（`q'…'`、`$$…$$`、
/// `#` 注释、反斜杠转义）与块结构（`BEGIN … END`、包体、`GO`、独占一行的 `/`）
/// 都按连接类型判定，和执行侧的切分器**同一套规则**——否则编辑器切出的语句
/// 送到驱动后是另一个东西。
///
/// 块语句（`BEGIN … END;`）的区间保留结尾分号：那是 PL/SQL / T-SQL 语法的一部分；
/// 普通 SQL 不含结尾分号，与历史行为一致。
///
/// 词法器失败（例如 Oracle 条件编译 `$IF $$x`）时退回 [`legacy_split_statements`]。
pub fn split_statements(text: &str, kind: DatabaseKind) -> Vec<Range> {
    if let Some(ranges) = fluxdb_app::sql_statement_byte_ranges(text, kind) {
        return ranges
            .into_iter()
            .map(|range| Range::new(range.start, range.end))
            .collect();
    }
    legacy_split_statements(text)
}

/// 无方言概念的历史扫描器：`#` 一律当行注释、`\` 一律当转义、只认 `;`。
///
/// 仅作为词法器失败时的兜底。它的两处方言无关假设正是本轮要修的：
/// PostgreSQL 的 `#` 是标识符字符、标准 SQL 里 `\` 不是转义。
fn legacy_split_statements(text: &str) -> Vec<Range> {
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

/// 在不可变编辑器快照上切分语句。
///
/// 与 [`split_statements`] 同一套方言规则；词法器需要完整 `&str`，因此这里把快照
/// 拼成字符串。调用方（折叠、诊断、执行定位）都在编辑后的单次扫描里，复制一次
/// 远小于「切错语句再执行」的代价。
pub fn split_statement_ranges_snapshot(
    snapshot: &fluxdb_editor_core::BufferSnapshot,
    kind: DatabaseKind,
) -> Vec<Range> {
    split_statements(&snapshot.to_string(), kind)
}

/// 在快照上定位光标所在语句。
pub fn statement_around_snapshot(
    snapshot: &fluxdb_editor_core::BufferSnapshot,
    offset: usize,
    kind: DatabaseKind,
) -> Option<(usize, usize)> {
    let offset = snapshot.clamp_to_char_boundary(offset.min(snapshot.len()));
    split_statement_ranges_snapshot(snapshot, kind)
        .into_iter()
        .find(|range| offset >= range.start && offset <= range.end)
        .map(|range| (range.start, range.end))
}

/// 返回包含给定字节偏移的语句区间 `(start, end)`。
///
/// 若 offset 位于语句边界处（等于某结束端），归入该语句；否则返回
/// `None` 供调用方决定回退策略。
pub fn statement_around(text: &str, offset: usize, kind: DatabaseKind) -> Option<(usize, usize)> {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let statements = split_statements(text, kind);
    for stmt in &statements {
        if offset >= stmt.start && offset <= stmt.end {
            return Some((stmt.start, stmt.end));
        }
    }
    None
}

