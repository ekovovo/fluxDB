/// ASCII 大小写不敏感的子序列匹配；过滤文本先去除首尾空白，空过滤匹配所有名称。
pub fn matches_completion_fuzzy(value: &str, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }

    let value = value.to_ascii_lowercase();
    let mut value_chars = value.chars();
    filter
        .to_ascii_lowercase()
        .chars()
        .all(|filter_char| value_chars.any(|value_char| value_char == filter_char))
}

/// 将补全过滤文本转为子序列 LIKE 模式；调用方须使用反斜杠作为 ESCAPE 字符。
pub fn completion_fuzzy_like_pattern(filter: &str) -> String {
    let filter = filter.trim().to_ascii_lowercase();
    if filter.is_empty() {
        return "%".to_string();
    }

    let mut pattern = String::from("%");
    for ch in filter.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
        pattern.push('%');
    }
    pattern
}

