//! 引擎编排：词法 → 容错解析 → 符号表 → 槽位 → 候选 → 过滤排序。

use crate::dialect::CompletionDialect;
use crate::locate::{lexical_slot, locate_slot};
use crate::metadata::MetadataSource;
use crate::model::{Candidate, CompletionOutput, CompletionRequest, WordContext};
use crate::parse::{LineColIndex, parse_tolerant};
use crate::strategy::{SlotContext, slot_candidates};
use crate::symbols::{Symbols, collect};
use crate::word::{statement_range_at, word_context};

/// 补全主入口：输入窗口文本与光标，输出替换区间与候选。
pub fn complete(
    request: &CompletionRequest<'_>,
    dialect: &dyn CompletionDialect,
    meta: &dyn MetadataSource,
) -> CompletionOutput {
    let text = request.text;
    let cursor = clamp_to_boundary(text, request.cursor);
    let parser_dialect = dialect.parser_dialect();

    let word = word_context(text, cursor, parser_dialect);
    let (statement_start, statement_end) = statement_range_at(text, cursor, parser_dialect);
    // 语句区间按去空白后的边界给出；光标可能落在尾部空白里，必须把窗口扩到光标。
    let statement_end = statement_end.max(cursor);
    let statement = &text[statement_start..statement_end];
    let cursor_in_statement = cursor.saturating_sub(statement_start);

    // 掩码区间：把光标处的残词换成等长占位标识符，保住 FROM 等后文子句。
    let mask =
        (word.replace_start >= statement_start && word.replace_end <= statement_end).then(|| {
            (
                word.replace_start - statement_start,
                word.replace_end - statement_start,
            )
        });

    let index = LineColIndex::new(statement);
    let parsed = parse_tolerant(statement, mask, cursor_in_statement, parser_dialect);
    let prefix_is_blank = statement[..cursor_in_statement.min(statement.len())]
        .trim()
        .is_empty();

    // 作用域链按光标投影：内层同名别名遮蔽外层。span 与光标同属解析后的文本坐标
    // （插入式兜底就发生在光标处，光标之前不受偏移影响）。
    let symbols = match &parsed {
        Some(parsed) => collect(&parsed.statements, &index).visible_at(cursor_in_statement),
        None => Symbols::default(),
    };

    let prefix = &statement[..cursor_in_statement.min(statement.len())];
    let slot = match &parsed {
        Some(parsed) => locate_slot(
            parsed,
            &index,
            cursor_in_statement,
            &word,
            prefix_is_blank,
            prefix,
        ),
        None => lexical_slot(&word, prefix_is_blank, prefix),
    };

    let ctx = SlotContext {
        dialect,
        meta,
        symbols: &symbols,
        word: &word,
        database: request.database,
        schema: request.schema,
    };

    let mut items = slot_candidates(&slot, &ctx);
    retain_matching(&mut items, &word);
    let filter = word.word_part.trim().to_ascii_lowercase();
    items.sort_by_key(|left| sort_key(left, &filter));
    dedupe(&mut items);

    CompletionOutput {
        replace_start: word.prefix_start,
        replace_end: word.replace_end,
        items,
    }
}

/// 光标对齐到字符边界，避免切片 panic。
fn clamp_to_boundary(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

/// 按已输入内容过滤。引号标识符大小写敏感且按前缀匹配；否则走子序列匹配。
fn retain_matching(items: &mut Vec<Candidate>, word: &WordContext) {
    let filter = word.word_part.trim();
    if filter.is_empty() {
        return;
    }
    items.retain(|item| {
        let haystack = item.filter_text.as_deref().unwrap_or(&item.label);
        if word.quoted {
            haystack.starts_with(filter)
        } else {
            matches_fuzzy(haystack, filter)
        }
    });
}

/// ASCII 大小写不敏感的子序列匹配（与宿主 `matches_completion_fuzzy` 同语义）。
fn matches_fuzzy(value: &str, filter: &str) -> bool {
    let value = value.to_ascii_lowercase();
    let mut value_chars = value.chars();
    filter
        .to_ascii_lowercase()
        .chars()
        .all(|filter_char| value_chars.any(|value_char| value_char == filter_char))
}

/// 排序键：语义分优先，其次匹配度（前缀 > 子序列），最后按标签。
fn sort_key(item: &Candidate, filter: &str) -> (i32, i32, String) {
    let (score, label) = item
        .sort_text
        .as_deref()
        .and_then(|sort_text| sort_text.split_once(':'))
        .and_then(|(score, label)| score.parse::<i32>().ok().map(|score| (score, label)))
        .unwrap_or((0, item.label.as_str()));
    // 前缀命中优先于子序列命中。
    let match_rank =
        i32::from(!filter.is_empty() && !label.to_ascii_lowercase().starts_with(filter));
    (-score, match_rank, label.to_string())
}

/// 同 label + kind 去重，保留先出现者（高分在前）。
fn dedupe(items: &mut Vec<Candidate>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| {
        seen.insert((
            item.label.to_ascii_lowercase(),
            item.kind as u8,
            item.schema.clone(),
        ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect;
    use crate::metadata::{ColumnMeta, ForeignKeyMeta, TableKind, TableMeta};
    use std::collections::HashMap;

    /// 两张表：users(id, name) / orders(id, user_id)，orders.user_id → users.id。
    struct MockMeta {
        columns: HashMap<String, Vec<ColumnMeta>>,
        with_fk: bool,
    }

    impl MockMeta {
        fn new() -> Self {
            Self::build(true)
        }

        fn without_fk() -> Self {
            Self::build(false)
        }

        fn build(with_fk: bool) -> Self {
            let mut columns = HashMap::new();
            columns.insert(
                "users".to_string(),
                vec![column("users", "id", true), column("users", "name", false)],
            );
            columns.insert(
                "orders".to_string(),
                vec![
                    column("orders", "id", true),
                    column("orders", "user_id", false),
                ],
            );
            Self { columns, with_fk }
        }
    }

    fn column(table: &str, name: &str, primary_key: bool) -> ColumnMeta {
        ColumnMeta {
            table: table.to_string(),
            name: name.to_string(),
            type_name: Some("int".to_string()),
            nullable: true,
            primary_key,
            comment: None,
        }
    }

    impl MetadataSource for MockMeta {
        fn tables(&self, _schema: Option<&str>) -> Vec<TableMeta> {
            vec![
                TableMeta {
                    schema: Some("public".to_string()),
                    name: "users".to_string(),
                    kind: TableKind::Table,
                    comment: None,
                },
                TableMeta {
                    schema: Some("public".to_string()),
                    name: "orders".to_string(),
                    kind: TableKind::Table,
                    comment: None,
                },
            ]
        }

        fn schemas(&self) -> Vec<String> {
            vec!["public".to_string()]
        }

        fn columns(&self, key: &crate::metadata::TableKey) -> Vec<ColumnMeta> {
            self.columns
                .get(&key.table.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default()
        }

        fn foreign_keys(&self, key: &crate::metadata::TableKey) -> Vec<ForeignKeyMeta> {
            if !self.with_fk || !key.table.eq_ignore_ascii_case("orders") {
                return Vec::new();
            }
            vec![ForeignKeyMeta {
                columns: vec!["user_id".to_string()],
                ref_schema: Some("public".to_string()),
                ref_table: "users".to_string(),
                ref_columns: vec!["id".to_string()],
            }]
        }
    }

    /// 用 `|` 标记光标位置跑一次补全。
    fn labels(text: &str) -> Vec<String> {
        labels_with(&MockMeta::new(), text)
    }

    fn labels_with(meta: &MockMeta, text: &str) -> Vec<String> {
        let cursor = text.find('|').expect("用例必须用 | 标记光标");
        let sql = text.replace('|', "");
        let request = CompletionRequest::new(&sql, cursor);
        let output = complete(&request, &dialect::POSTGRES, meta);
        output.items.into_iter().map(|item| item.label).collect()
    }

    fn replace_range(text: &str) -> (usize, usize) {
        let cursor = text.find('|').expect("用例必须用 | 标记光标");
        let sql = text.replace('|', "");
        let request = CompletionRequest::new(&sql, cursor);
        let output = complete(&request, &dialect::POSTGRES, &MockMeta::new());
        (output.replace_start, output.replace_end)
    }

    fn candidates_with(
        dialect: &dyn crate::dialect::CompletionDialect,
        text: &str,
    ) -> Vec<Candidate> {
        let cursor = text.find('|').expect("用例必须用 | 标记光标");
        let sql = text.replace('|', "");
        complete(
            &CompletionRequest::new(&sql, cursor),
            dialect,
            &MockMeta::new(),
        )
        .items
    }

    #[test]
    fn dameng_bare_table_inserts_schema_qualified_name() {
        let items = candidates_with(&dialect::DAMENG, "SELECT * FROM |");
        let users = items
            .iter()
            .find(|item| item.label == "users")
            .expect("应补出 users 表");
        assert_eq!(users.insert_text, "public.users");
    }

    #[test]
    fn schema_candidate_inserts_trailing_dot() {
        let items = candidates_with(&dialect::POSTGRES, "SELECT * FROM |");
        let schema = items
            .iter()
            .find(|item| item.label == "public")
            .expect("应补出 public schema");
        assert_eq!(schema.insert_text, "public.");
    }

    /// 空前缀下不能把 DDL 关键字灌进表达式候选（IDE 手感的核心）。
    #[test]
    fn empty_prefix_keeps_expression_keywords_contextual() {
        for text in [
            "SELECT * FROM users u WHERE u.id = |",
            "SELECT * FROM users u ORDER BY |",
        ] {
            let items = labels(text);
            for noise in ["CREATE", "DROP", "ALTER", "TRUNCATE", "DATABASE"] {
                assert!(
                    !items.contains(&noise.to_string()),
                    "{text} 候选：{items:?}"
                );
            }
        }
    }

    /// 各方言的上下文关键字表都要过滤出实际存在的项，不能整片落空。
    #[test]
    fn contextual_keywords_exist_in_every_dialect() {
        for dialect in [
            &dialect::MYSQL as &dyn crate::dialect::CompletionDialect,
            &dialect::DAMENG,
            &dialect::SQLITE,
        ] {
            let items = candidates_with(dialect, "SELECT * FROM users u WHERE u.id = |");
            for keyword in ["AND", "OR", "LIKE", "IS NULL", "GROUP BY"] {
                assert!(
                    items.iter().any(|item| item.label == keyword),
                    "缺少 {keyword}，候选：{items:?}"
                );
            }
        }
    }

    /// 打字中仍放开全量关键字：`SELECT * fro|` → FROM。
    #[test]
    fn typed_prefix_still_offers_full_keyword_set() {
        let items = labels("SELECT * FROM users u WHERE u.id > 1 AND|");
        assert!(items.contains(&"AND".to_string()), "候选：{items:?}");
    }

    /// FROM 列表里的逗号后是新表名位置，不是别名位置。
    #[test]
    fn comma_in_from_starts_new_table_slot() {
        let items = labels("SELECT * FROM users u, |");
        assert!(items.contains(&"orders".to_string()), "候选：{items:?}");
        assert!(items.contains(&"users".to_string()), "候选：{items:?}");
        // 别名位置（无逗号）仍然只给续接关键字。
        let items = labels("SELECT * FROM users u |");
        assert!(!items.contains(&"orders".to_string()), "候选：{items:?}");
        assert!(items.contains(&"WHERE".to_string()), "候选：{items:?}");
    }

    /// 赋值右侧也是表达式，函数要能补出来。
    #[test]
    fn update_set_value_suggests_functions() {
        let items = labels("UPDATE users SET name = COU|");
        assert!(items.contains(&"COUNT".to_string()), "候选：{items:?}");
    }

    #[test]
    fn suggests_columns_for_bare_projection() {
        let items = labels("SELECT | FROM users");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn suggests_columns_for_qualified_projection() {
        let items = labels("SELECT u.| FROM users u");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn unknown_qualifier_yields_no_columns() {
        let items = labels("SELECT nope.| FROM users u");
        assert!(items.is_empty(), "候选应为空，实际：{items:?}");
    }

    #[test]
    fn suggests_tables_after_from() {
        let items = labels("SELECT * FROM |");
        assert!(items.contains(&"users".to_string()), "候选：{items:?}");
        assert!(items.contains(&"orders".to_string()), "候选：{items:?}");
    }

    #[test]
    fn suggests_statement_starters_on_empty_input() {
        let items = labels("|");
        assert!(items.contains(&"SELECT".to_string()), "候选：{items:?}");
    }

    /// 表达式位置也要能补子句关键字：`SELECT * fro|` → FROM。
    #[test]
    fn suggests_clause_keywords_in_expression_scope() {
        let items = labels("SELECT * fro|");
        assert!(items.contains(&"FROM".to_string()), "候选：{items:?}");
        let items = labels("SELECT * FROM t WHERE x = 1 AND|");
        assert!(items.contains(&"AND".to_string()), "候选：{items:?}");
    }

    /// 已闭合的引号标识符之后，别名位置的关键字仍可补全。
    #[test]
    fn suggests_join_keyword_after_quoted_identifier() {
        let items = labels("SELECT * from `3d_user` u left|");
        assert!(items.contains(&"LEFT JOIN".to_string()), "候选：{items:?}");
    }

    #[test]
    fn filters_by_partial_word() {
        let items = labels("SELECT na| FROM users");
        // 子序列匹配允许 name/name 之外的函数名命中，但列必须优先且 id 被滤掉。
        assert_eq!(
            items.first().map(String::as_str),
            Some("name"),
            "候选：{items:?}"
        );
        assert!(!items.contains(&"id".to_string()), "候选：{items:?}");
    }

    #[test]
    fn qualified_insert_text_excludes_qualifier() {
        let cursor = "SELECT u.na| FROM users u".find('|').unwrap();
        let sql = "SELECT u.na FROM users u".to_string();
        let output = complete(
            &CompletionRequest::new(&sql, cursor),
            &dialect::POSTGRES,
            &MockMeta::new(),
        );
        let name = output
            .items
            .iter()
            .find(|item| item.label == "name")
            .expect("应补出 name 列");
        assert_eq!(name.insert_text, "name");
        // 只替换词本身，保留 `u.` 限定符。
        assert_eq!(&sql[output.replace_start..output.replace_end], "na");
    }

    #[test]
    fn replaces_partial_word_range() {
        assert_eq!(replace_range("SELECT na| FROM users"), (7, 9));
    }

    #[test]
    fn suggests_join_table_after_join_keyword() {
        let items = labels("SELECT * FROM users u JOIN |");
        assert!(items.contains(&"orders".to_string()), "候选：{items:?}");
    }

    #[test]
    fn join_on_left_limits_to_foreign_key_columns() {
        // users 只有 id 参与外键（name 被排除）。
        let items = labels("SELECT * FROM users u JOIN orders o ON u.|");
        assert_eq!(items, vec!["id".to_string()], "候选：{items:?}");
    }

    #[test]
    fn join_on_left_without_qualifier_offers_both_sides() {
        let items = labels("SELECT * FROM users u JOIN orders o ON |");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"user_id".to_string()), "候选：{items:?}");
        assert!(!items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn join_on_right_limits_to_paired_column() {
        let items = labels("SELECT * FROM users u JOIN orders o ON u.id = o.|");
        assert_eq!(items, vec!["user_id".to_string()], "候选：{items:?}");
    }

    #[test]
    fn join_on_right_supports_reversed_declaration() {
        let items = labels("SELECT * FROM users u JOIN orders o ON o.user_id = u.|");
        assert_eq!(items, vec!["id".to_string()], "候选：{items:?}");
    }

    #[test]
    fn join_on_falls_back_to_all_columns_without_fk() {
        let items = labels_with(
            &MockMeta::without_fk(),
            "SELECT * FROM users u JOIN orders o ON u.|",
        );
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn suggests_on_snippet_after_join_table() {
        let items = labels("SELECT * FROM users u JOIN orders o |");
        assert!(
            items.contains(&"ON u.id = o.user_id".to_string()),
            "候选：{items:?}"
        );
    }

    #[test]
    fn order_by_suggests_output_alias() {
        let items = labels("SELECT u.name AS n FROM users u ORDER BY |");
        assert!(items.contains(&"n".to_string()), "候选：{items:?}");
    }

    #[test]
    fn insert_column_list_suggests_table_columns() {
        let items = labels("INSERT INTO users (|");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn insert_column_list_excludes_written_columns() {
        let items = labels("INSERT INTO users (id, |");
        assert!(!items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn update_assignments_suggest_unassigned_columns() {
        let items = labels("UPDATE users SET name = 'a', |");
        assert!(!items.contains(&"name".to_string()), "候选：{items:?}");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
    }

    #[test]
    fn cte_columns_are_visible_through_alias() {
        let items = labels("WITH c AS (SELECT id FROM users) SELECT c.|");
        assert_eq!(items, vec!["id".to_string()], "候选：{items:?}");
    }

    #[test]
    fn derived_table_columns_are_visible_through_alias() {
        let items = labels("SELECT * FROM (SELECT id, name FROM users) t WHERE t.|");
        assert!(items.contains(&"id".to_string()), "候选：{items:?}");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
    }

    #[test]
    fn nested_subquery_sees_outer_relation() {
        let items = labels("SELECT * FROM users u WHERE u.id IN (SELECT | FROM orders)");
        assert!(items.contains(&"name".to_string()), "候选：{items:?}");
        assert!(items.contains(&"user_id".to_string()), "候选：{items:?}");
    }

    #[test]
    fn where_does_not_suggest_output_alias() {
        let items = labels("SELECT u.name AS n FROM users u WHERE |");
        assert!(!items.contains(&"n".to_string()), "候选：{items:?}");
    }
}
