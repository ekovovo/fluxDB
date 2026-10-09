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

#[cfg(test)]
mod completion_filter_tests {
    use super::*;

    #[test]
    fn fuzzy_matching_preserves_order_whitespace_and_ascii_case() {
        for (value, filter, expected) in [
            ("Product", " pDt ", true),
            ("Product", "duct", true),
            ("Product", "tp", false),
            ("Product", "zz", false),
            ("", "", true),
            ("Product", " \t ", true),
            ("", "p", false),
            ("aba", "aa", true),
            ("ab", "aa", false),
            ("产品表", "产表", true),
            ("Ä", "ä", false),
            ("a_%\\b", "_%\\", true),
        ] {
            assert_eq!(
                matches_completion_fuzzy(value, filter),
                expected,
                "{value:?}/{filter:?}"
            );
        }
    }

    #[test]
    fn like_pattern_escapes_wildcards_and_backslashes() {
        for (filter, expected) in [
            ("", "%"),
            (" \t ", "%"),
            (" Pdt ", "%p%d%t%"),
            ("p_t", "%p%\\_%t%"),
            ("%\\_", "%\\%%\\\\%\\_%"),
            ("产表", "%产%表%"),
        ] {
            assert_eq!(completion_fuzzy_like_pattern(filter), expected);
        }
    }
}
