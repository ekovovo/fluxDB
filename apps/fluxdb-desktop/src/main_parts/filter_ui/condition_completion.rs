use gpui_component::input::{CompletionProvider, Lsp as CompletionLsp, Rope};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};

/// 筛选条件输入只补全条件片段，不开放整条 SQL 编辑。
///
/// 字段来自当前数据页；关键字与函数来自当前连接方言；操作符是四个关系型
/// 数据库共同支持的词法形式。候选不读取完成索引，也不触发数据库请求。
struct DataFilterConditionCompletionProvider {
    fields: std::sync::Arc<Vec<String>>,
    dialect: sql_editor_adapter::SqlDialect,
    database_kind: DatabaseKind,
}

impl CompletionProvider for DataFilterConditionCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: lsp_types::CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let items = data_filter_condition_completions(
            &text.to_string(),
            offset.min(text.len()),
            &self.fields,
            self.dialect,
            self.database_kind,
        );
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, _: &str, _: &mut App) -> bool {
        true
    }
}

fn configure_data_filter_condition_completion(
    editor: &mut EditorState,
    window: &mut Window,
    cx: &mut Context<EditorState>,
    fields: Vec<String>,
    dialect: sql_editor_adapter::SqlDialect,
    database_kind: DatabaseKind,
) {
    editor.set_folding(false, window, cx);
    let mut lsp = CompletionLsp::default();
    lsp.completion_provider = Some(std::rc::Rc::new(DataFilterConditionCompletionProvider {
        fields: std::sync::Arc::new(fields),
        dialect,
        database_kind,
    }));
    *editor.lsp_mut() = lsp;
}

fn data_filter_completion_signature(
    fields: &[String],
    dialect: sql_editor_adapter::SqlDialect,
    database_kind: DatabaseKind,
) -> (Vec<String>, sql_editor_adapter::SqlDialect, DatabaseKind) {
    (fields.to_vec(), dialect, database_kind)
}

fn data_filter_condition_completions(
    text: &str,
    cursor: usize,
    fields: &[String],
    dialect: sql_editor_adapter::SqlDialect,
    database_kind: DatabaseKind,
) -> Vec<CompletionItem> {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let (replace_start, query) = data_filter_condition_query_start(before);
    let query_lower = query.to_ascii_lowercase();
    let partial = query_lower
        .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
        .next()
        .unwrap_or_default();
    let expression_start = data_filter_condition_expression_start(before);
    let expression = &before[expression_start..];
    let expect_field = data_filter_condition_expects_field(expression);
    let expect_operator = data_filter_condition_expects_operator(expression);

    let mut items = Vec::new();
    if expect_field || partial.is_empty() {
        for field in fields {
            if data_filter_condition_matches(field, partial, query_lower.as_str()) {
                let mut item = CompletionItem {
                    label: field.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(tr("app.filter.completion.field").to_string()),
                    filter_text: Some(field.to_ascii_lowercase()),
                    ..CompletionItem::default()
                };
                item.sort_text = Some(format!("0{}", field.to_ascii_lowercase()));
                items.push(item);
            }
        }
    }
    if expect_operator || !expect_field {
        for (operator, insert) in DATA_FILTER_CONDITION_OPERATORS {
            if data_filter_condition_matches(operator, partial, query_lower.as_str()) {
                let mut item = CompletionItem {
                    label: (*operator).to_string(),
                    kind: Some(CompletionItemKind::OPERATOR),
                    detail: Some(tr("app.filter.completion.operator").to_string()),
                    filter_text: Some(operator.to_ascii_lowercase()),
                    insert_text: Some((*insert).to_string()),
                    ..CompletionItem::default()
                };
                item.sort_text = Some(format!("1{}", operator.to_ascii_lowercase()));
                items.push(item);
            }
        }
    }
    for definition in sql_editor_adapter::data_filter_keywords(database_kind) {
        if !data_filter_condition_matches(definition.word, partial, query_lower.as_str()) {
            continue;
        }
        let mut item = CompletionItem {
            label: definition.word.to_string(),
            kind: Some(match definition.kind {
                fluxdb_editor_core::CompletionKind::Function => {
                    CompletionItemKind::FUNCTION
                }
                _ => CompletionItemKind::KEYWORD,
            }),
            detail: Some(tr("app.filter.completion.dialect").replace(
                "%{dialect}",
                sql_editor_adapter::data_filter_dialect_name(database_kind),
            )),
            filter_text: Some(definition.word.to_string()),
            insert_text: Some(definition.word.to_string()),
            ..CompletionItem::default()
        };
        item.sort_text = Some(format!("2{}", definition.word));
        if !items
            .iter()
            .any(|existing| existing.label.eq_ignore_ascii_case(&item.label))
        {
            items.push(item);
        }
    }
    items.sort_by(|left, right| left.sort_text.cmp(&right.sort_text));
    items.truncate(80);
    for item in &mut items {
        item.text_edit = Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
            range: lsp_types::Range {
                start: offset_to_position(text, replace_start),
                end: offset_to_position(text, cursor),
            },
            new_text: data_filter_condition_completion_insert_text(item, dialect),
        }));
    }
    items
}

fn data_filter_condition_completion_insert_text(
    item: &CompletionItem,
    dialect: sql_editor_adapter::SqlDialect,
) -> String {
    if item.kind == Some(CompletionItemKind::FIELD) {
        sql_quote_ident(&item.label, database_kind_for_dialect(dialect))
    } else {
        item.insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone())
    }
}

fn database_kind_for_dialect(dialect: sql_editor_adapter::SqlDialect) -> DatabaseKind {
    match dialect {
        sql_editor_adapter::SqlDialect::Mysql => DatabaseKind::MySql,
        sql_editor_adapter::SqlDialect::Postgres => DatabaseKind::Postgres,
        sql_editor_adapter::SqlDialect::Sqlite => DatabaseKind::Sqlite,
        sql_editor_adapter::SqlDialect::SqlServer => DatabaseKind::SqlServer,
    }
}

const DATA_FILTER_CONDITION_OPERATORS: &[(&str, &str)] = &[
    ("=", " = "),
    ("!=", " != "),
    (">", " > "),
    (">=", " >= "),
    ("<", " < "),
    ("<=", " <= "),
    ("LIKE", " LIKE "),
    ("NOT LIKE", " NOT LIKE "),
    ("IN", " IN ()"),
    ("NOT IN", " NOT IN ()"),
    ("IS NULL", " IS NULL"),
    ("IS NOT NULL", " IS NOT NULL"),
    ("BETWEEN", " BETWEEN  AND "),
    ("AND", " AND "),
    ("OR", " OR "),
    ("NOT", " NOT "),
];

fn data_filter_condition_query_start(before: &str) -> (usize, &str) {
    let start = before
        .char_indices()
        .rev()
        .find(|(_, character)| {
            !(character.is_alphanumeric() || matches!(character, '_' | '.' | '"' | '`' | '[' | ']'))
        })
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0);
    (start, &before[start..])
}

fn data_filter_condition_expression_start(before: &str) -> usize {
    before
        .char_indices()
        .rev()
        .find(|(_, character)| matches!(character, '\n' | ';'))
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0)
}

fn data_filter_condition_expects_field(expression: &str) -> bool {
    let trimmed = expression.trim_end();
    trimmed.is_empty()
        || trimmed.ends_with('(')
        || data_filter_condition_last_word(trimmed)
            .is_some_and(|word| matches!(word.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT"))
}

fn data_filter_condition_expects_operator(expression: &str) -> bool {
    let trimmed = expression.trim_end();
    !trimmed.is_empty()
        && !trimmed.ends_with(|character: char| {
            matches!(
                character,
                '=' | '!' | '>' | '<' | '(' | ',' | '\'' | '"' | '`' | ']'
            )
        })
        && data_filter_condition_last_word(trimmed)
            .is_some_and(|word| !matches!(word.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT"))
}

fn data_filter_condition_last_word(expression: &str) -> Option<&str> {
    expression
        .split_whitespace()
        .next_back()
        .filter(|word| !word.is_empty())
}

fn data_filter_condition_matches(label: &str, partial: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let label = label.to_ascii_lowercase();
    label.starts_with(partial)
        || label.split(|character: char| !character.is_alphanumeric())
            .any(|word| word.starts_with(partial))
}

fn offset_to_position(text: &str, offset: usize) -> lsp_types::Position {
    let offset = offset.min(text.len());
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let character = prefix[line_start..].encode_utf16().count() as u32;
    lsp_types::Position::new(line, character)
}

#[cfg(test)]
mod data_filter_condition_completion_tests {
    use super::*;

    #[test]
    fn completion_offers_fields_after_boolean_and_dialect_keywords() {
        let fields = vec!["customer_no".to_string(), "status".to_string()];
        let text = "customer_no = '1' AND ";
        let items = data_filter_condition_completions(
            text,
            text.len(),
            &fields,
            sql_editor_adapter::SqlDialect::Postgres,
            DatabaseKind::Postgres,
        );
        assert!(items.iter().any(|item| item.label == "customer_no"));
        assert!(items.iter().any(|item| item.label == "status"));
        assert!(!items.iter().any(|item| item.label == "OR"));
        assert!(!items.iter().any(|item| item.label == "LIKE"));
    }

    #[test]
    fn completion_filters_operators_by_typed_prefix() {
        let fields = vec!["status".to_string()];
        let text = "status NO";
        let items = data_filter_condition_completions(
            text,
            text.len(),
            &fields,
            sql_editor_adapter::SqlDialect::SqlServer,
            DatabaseKind::SqlServer,
        );
        assert!(items.iter().any(|item| item.label == "NOT IN"));
        assert!(items.iter().any(|item| item.label == "NOT LIKE"));
        assert!(!items.iter().any(|item| item.label == "="));
    }

    #[test]
    fn completion_uses_connection_dialect_keywords() {
        let fields = vec!["name".to_string()];
        let text = "TOP";
        let sql_server = data_filter_condition_completions(
            text,
            text.len(),
            &fields,
            sql_editor_adapter::SqlDialect::SqlServer,
            DatabaseKind::SqlServer,
        );
        let postgres = data_filter_condition_completions(
            text,
            text.len(),
            &fields,
            sql_editor_adapter::SqlDialect::Postgres,
            DatabaseKind::Postgres,
        );
        assert!(sql_server
            .iter()
            .any(|item| item.label.eq_ignore_ascii_case("top")));
        assert!(!postgres
            .iter()
            .any(|item| item.label.eq_ignore_ascii_case("top")));
    }

    #[test]
    fn field_insert_uses_each_database_identifier_style() {
        let cases = [
            (
                sql_editor_adapter::SqlDialect::Mysql,
                DatabaseKind::MySql,
                "`status`",
            ),
            (
                sql_editor_adapter::SqlDialect::Mysql,
                DatabaseKind::TiDb,
                "`status`",
            ),
            (
                sql_editor_adapter::SqlDialect::Postgres,
                DatabaseKind::Postgres,
                "\"status\"",
            ),
            (
                sql_editor_adapter::SqlDialect::Sqlite,
                DatabaseKind::Sqlite,
                "\"status\"",
            ),
            (
                sql_editor_adapter::SqlDialect::SqlServer,
                DatabaseKind::SqlServer,
                "[status]",
            ),
            (
                sql_editor_adapter::SqlDialect::Postgres,
                DatabaseKind::Dameng,
                "\"status\"",
            ),
        ];
        for (dialect, database_kind, expected) in cases {
            let fields = vec!["status".to_string()];
            let items =
                data_filter_condition_completions("", 0, &fields, dialect, database_kind);
            let field = items
                .iter()
                .find(|item| item.label == "status")
                .expect("field candidate");
            let edit = match field.text_edit.as_ref().expect("field text edit") {
                lsp_types::CompletionTextEdit::Edit(edit) => edit,
                _ => panic!("expected a plain edit"),
            };
            assert_eq!(edit.new_text, expected, "{database_kind:?}");
        }
    }

    #[test]
    fn dameng_gets_dameng_specific_filter_completion() {
        let fields = vec!["created_at".to_string()];
        let items = data_filter_condition_completions(
            "to_",
            "to_".len(),
            &fields,
            sql_editor_adapter::SqlDialect::Postgres,
            DatabaseKind::Dameng,
        );
        assert!(items.iter().any(|item| item.label == "to_char"));
        assert!(items.iter().any(|item| item.label == "to_date"));
    }

    #[test]
    fn non_sql_data_pages_do_not_offer_dialect_keywords() {
        for database_kind in [DatabaseKind::MongoDb, DatabaseKind::Redis] {
            let items = data_filter_condition_completions(
                "",
                0,
                &["key".to_string()],
                sql_editor_adapter::SqlDialect::Mysql,
                database_kind,
            );
            assert!(
                items
                    .iter()
                    .all(|item| item.kind != Some(CompletionItemKind::KEYWORD)),
                "{database_kind:?}"
            );
        }
    }
}
