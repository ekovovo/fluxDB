// MongoDB 集合导出为 CSV：列集合必须由全部文档求并集后再写行。
//
// 与 Compass 的 export-csv 一致：先把整份结果下载到临时文件并记录列并集，再按固定列序
// 逐行写 CSV。没有这一步就无法确定表头，混字段集合会静默丢列或错位。
// 文档在本仓库里是 Relaxed Extended JSON 文本，列收集与取值都基于解析后的 JSON 值。

use std::collections::HashSet;
use std::io::{BufRead, BufReader};

/// 列路径的一段：对象字段或数组下标（与 Compass 的 `PathPart` 同构）。
#[derive(Clone, Debug, Eq, PartialEq)]
enum MongoCsvPathPart {
    Field(String),
    Index(usize),
}

impl MongoCsvPathPart {
    fn header_text(&self, first: bool) -> String {
        match self {
            Self::Field(name) if first => name.clone(),
            Self::Field(name) => format!(".{name}"),
            Self::Index(index) => format!("[{index}]"),
        }
    }
}

/// 全量列并集：按首次出现顺序保序，重复路径只记一次。
#[derive(Default)]
struct MongoCsvColumns {
    seen: HashSet<String>,
    columns: Vec<Vec<MongoCsvPathPart>>,
}

impl MongoCsvColumns {
    /// 收录一篇文档的全部叶子路径；空对象/空数组本身不产生列。
    fn add_document(&mut self, document: &serde_json::Value) {
        let mut path = Vec::new();
        self.add_value(document, &mut path);
    }

    fn add_value(&mut self, value: &serde_json::Value, path: &mut Vec<MongoCsvPathPart>) {
        match value {
            serde_json::Value::Object(fields) if !is_extended_json_wrapper(value) => {
                for (name, child) in fields {
                    path.push(MongoCsvPathPart::Field(name.clone()));
                    self.add_value(child, path);
                    path.pop();
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    path.push(MongoCsvPathPart::Index(index));
                    self.add_value(child, path);
                    path.pop();
                }
            }
            _ => {
                if path.is_empty() {
                    return;
                }
                let header = mongo_csv_header_name(path);
                // 同名列只保留首次出现的位置：后出现的文档在此列取值，不重排表头。
                if self.seen.insert(header) {
                    self.columns.push(path.clone());
                }
            }
        }
    }

    fn header_names(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|path| mongo_csv_header_name(path))
            .collect()
    }

    /// 按列并集取一行，返回值已按 CSV 规则转义（未拼分隔符）。
    fn row(&self, document: &serde_json::Value) -> Vec<String> {
        self.columns
            .iter()
            .map(|path| {
                let text = lookup_path(document, path)
                    .map(mongo_csv_value_text)
                    .unwrap_or_default();
                mongo_csv_escape_cell(&text)
            })
            .collect()
    }
}

fn mongo_csv_header_name(path: &[MongoCsvPathPart]) -> String {
    path.iter()
        .enumerate()
        .map(|(index, part)| part.header_text(index == 0))
        .collect()
}

fn lookup_path<'a>(
    document: &'a serde_json::Value,
    path: &[MongoCsvPathPart],
) -> Option<&'a serde_json::Value> {
    let mut value = document;
    for part in path {
        value = match part {
            MongoCsvPathPart::Field(name) => value.get(name)?,
            MongoCsvPathPart::Index(index) => value.get(*index)?,
        };
    }
    Some(value)
}

/// Extended JSON 的类型包装（`{"$oid": …}`、`{"$date": …}` 等）：整体是单个叶子值，
/// 不能再按对象展开，否则表头会变成 `$oid` 这种毫无意义的列。
fn is_extended_json_wrapper(value: &serde_json::Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    fields.len() == 1
        && fields
            .keys()
            .next()
            .is_some_and(|key| key.starts_with('$'))
}

/// 单元格文本口径对齐 Compass：标量原样，BSON 包装还原成可读值，其余保留紧凑 EJSON。
fn mongo_csv_value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(fields) => {
            let wrapper = fields.iter().next().map(|(key, inner)| (key.as_str(), inner));
            match wrapper {
                Some(("$oid", inner)) => inner.as_str().unwrap_or_default().to_string(),
                Some(("$numberLong" | "$numberInt" | "$numberDouble" | "$numberDecimal", inner)) => {
                    inner.as_str().unwrap_or_default().to_string()
                }
                Some(("$date", inner)) => mongo_csv_date_text(inner),
                _ => serde_json::Value::Object(fields.clone()).to_string(),
            }
        }
        serde_json::Value::Array(_) => value.to_string(),
    }
}

/// 日期取 ISO 文本；Relaxed EJSON 既可能给字符串也可能给 `{"$numberLong": "<ms>"}`。
fn mongo_csv_date_text(inner: &serde_json::Value) -> String {
    if let Some(text) = inner.as_str() {
        return text.to_string();
    }
    let millis = inner
        .get("$numberLong")
        .and_then(|value| value.as_str())
        .and_then(|text| text.parse::<i64>().ok());
    match millis.and_then(chrono::DateTime::from_timestamp_millis) {
        Some(date) => date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => inner.to_string(),
    }
}

/// 转义并加引号；同时按 Compass 的默认口径给公式前缀加 `'`，避免表格软件把
/// 用户数据当公式执行（CSV 注入）。
fn mongo_csv_escape_cell(text: &str) -> String {
    let needs_formula_guard = text
        .chars()
        .next()
        .is_some_and(|first| matches!(first, '=' | '+' | '-' | '@'));
    let text = if needs_formula_guard {
        format!("'{text}")
    } else {
        text.to_string()
    };
    csv_cell(&text)
}

/// 读出临时文件里的每一行紧凑 EJSON 文档。
fn mongo_csv_read_rows(
    path: &Path,
    mut on_row: impl FnMut(serde_json::Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let reader = BufReader::new(fs::File::open(path)?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        on_row(serde_json::from_str(&line)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod mongo_export_csv_tests {
    use super::*;

    fn parse(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("测试输入应为合法 JSON")
    }

    /// 列并集必须跨文档求并集并保持首次出现顺序，否则后出现的字段会丢列。
    #[test]
    fn columns_union_keeps_first_seen_order() {
        let mut columns = MongoCsvColumns::default();
        columns.add_document(&parse(r#"{"a":1,"b":{"c":2}}"#));
        columns.add_document(&parse(r#"{"b":{"d":3},"e":"x"}"#));
        assert_eq!(columns.header_names(), vec!["a", "b.c", "b.d", "e"]);
        let row = columns.row(&parse(r#"{"b":{"d":3},"e":"x"}"#));
        assert_eq!(row, vec!["", "", "3", "x"]);
    }

    /// 数组按下标展开；Extended JSON 包装是叶子，不能展开成 `$oid` 列。
    #[test]
    fn arrays_expand_and_extjson_wrappers_stay_leaf() {
        let mut columns = MongoCsvColumns::default();
        columns.add_document(&parse(
            r#"{"_id":{"$oid":"6571c1b0a1b2c3d4e5f60001"},"tags":["x","y"]}"#,
        ));
        assert_eq!(columns.header_names(), vec!["_id", "tags[0]", "tags[1]"]);
        let row = columns.row(&parse(
            r#"{"_id":{"$oid":"6571c1b0a1b2c3d4e5f60001"},"tags":["x"]}"#,
        ));
        assert_eq!(row, vec!["6571c1b0a1b2c3d4e5f60001", "x", ""]);
    }

    /// 数字原样、null 空串、含逗号/引号的值加引号、公式前缀加 `'`。
    #[test]
    fn cells_match_compass_escaping() {
        assert_eq!(mongo_csv_value_text(&parse("9000000000")), "9000000000");
        assert_eq!(mongo_csv_value_text(&parse("null")), "");
        assert_eq!(mongo_csv_escape_cell("a,b"), "\"a,b\"");
        assert_eq!(mongo_csv_escape_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(mongo_csv_escape_cell("=1+1"), "'=1+1");
    }

    /// 日期包装转 ISO 文本，避免把 `$date` 对象塞进单元格。
    #[test]
    fn dates_render_as_iso_text() {
        let value = parse(r#"{"$date":{"$numberLong":"1704164645000"}}"#);
        assert_eq!(mongo_csv_value_text(&value), "2024-01-02T03:04:05.000Z");
        let value = parse(r#"{"$date":"2024-01-02T03:04:05Z"}"#);
        assert_eq!(mongo_csv_value_text(&value), "2024-01-02T03:04:05Z");
    }
}
