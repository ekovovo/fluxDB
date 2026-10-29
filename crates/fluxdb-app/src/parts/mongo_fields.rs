// MongoDB 补全用的字段目录：把文档摊平成点分路径 + 取值词表。
//
// 与 Compass 的 compass-field-store 同一思路，但只做补全需要的那部分：路径、
// 每个路径出现过的 BSON 类型、字符串取值。深度与条数都有上限——补全候选不是
// 模式分析结果，一屏能翻完才算有用。

use serde_json::Value;

/// 补全候选里的一条字段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MongoField {
    /// 点分路径，如 `customer.address.city`。
    pub path: String,
    /// 出现过的类型名（`String` / `ObjectId` / `Int32` …），显示在候选右侧。
    pub types: Vec<String>,
    /// **这个字段**上出现过的字符串取值，用于 `{"status": "pai` 这类值补全。
    ///
    /// 必须按字段分开存：早期版本把全部字符串塞进一张全局词表，写 `status` 时会冒出
    /// 文档里任何地方出现过的手机号、订单号，等于给了一屏错误候选。
    pub values: Vec<String>,
}

/// 一次采集的结果：字段路径 → 类型与取值。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MongoFieldCatalog {
    pub fields: Vec<MongoField>,
}

/// 采集字段时抽样的文档条数：与 Compass 的 schema 采样量级一致，
/// 既能覆盖稀有字段，又不会因为一次打开集合页就扫全表。
pub const MONGO_FIELD_SAMPLE_SIZE: u32 = 1000;

/// 字段路径深度上限（按段计，`a.b.c.d` = 4 段）：更深的多半是数组里嵌数组，
/// 补全列表里几乎不会选到。
const MONGO_FIELD_MAX_PATH_DEPTH: usize = 4;
/// 递归深度兜底：数组元素不增加路径段数，只靠上面的段数上限挡不住
/// `[[[[[…]]]]]`，留一个比段数上限宽松的硬上限。
const MONGO_FIELD_MAX_RECURSION: usize = 16;
/// 字段条数上限：与补全弹层一次能翻完的量级对齐。
const MONGO_FIELD_MAX_COUNT: usize = 300;
/// 取值词条上限（按字段算）。
const MONGO_FIELD_MAX_VALUES: usize = 50;
/// 取值词长度上限：长文本（描述、HTML 片段）不是补全素材。
const MONGO_WORD_MAX_LEN: usize = 64;

impl MongoFieldCatalog {
    /// 把一批文档（Relaxed Extended JSON 文本）并入目录；解析失败的文档跳过。
    ///
    /// 跳过而不是报错：字段目录是补全的辅助信息，一条脏文档不该让整次采集失败。
    pub fn extend_from_documents(&mut self, documents: &[String]) {
        let mut index: std::collections::HashMap<String, usize> = self
            .fields
            .iter()
            .enumerate()
            .map(|(position, field)| (field.path.clone(), position))
            .collect();
        for document in documents {
            let Ok(value) = serde_json::from_str::<Value>(document) else {
                continue;
            };
            self.collect_value(&value, "", 0, &mut index);
        }
    }

    /// 递归摊平一个值：对象按键展开，数组按同路径展开元素。
    fn collect_value(
        &mut self,
        value: &Value,
        path: &str,
        depth: usize,
        index: &mut std::collections::HashMap<String, usize>,
    ) {
        // 深度只用来兜住递归：真正的候选上限按路径段数算（见 `add_field`），
        // 数组元素递归不会增加段数，用同一把尺子会误伤嵌套数组。
        if depth > MONGO_FIELD_MAX_RECURSION {
            return;
        }
        match value {
            Value::Object(map) => {
                // Extended JSON 包装（`{"$oid": …}`）是叶子：往里展开只会得到 `$oid`。
                if let Some(name) = mongo_extended_json_type(map) {
                    if !path.is_empty() {
                        self.add_field(path, name, index);
                    }
                    return;
                }
                // 子文档本身也是可查询字段（`{"address": ...}`），先记一笔再往下展开。
                if !path.is_empty() {
                    self.add_field(path, "Object", index);
                }
                for (key, child) in map {
                    // `$` 开头的键是操作符，不是字段名。
                    if key.starts_with('$') {
                        continue;
                    }
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    self.collect_value(child, &child_path, depth + 1, index);
                }
            }
            Value::Array(items) => {
                if path.is_empty() {
                    return;
                }
                // 空数组也要记一笔：用户得知道这个字段存在。
                if !self.add_field(path, "Array", index) {
                    return;
                }
                for item in items {
                    match item {
                        // 只展开子文档/子数组：`["a","b"]` 里的标量元素不是 `tags` 的另一种类型，
                        // 往里递归会把 `tags` 记成 `Array | String`，那是错的。
                        Value::Object(_) | Value::Array(_) => {
                            self.collect_value(item, path, depth + 1, index)
                        }
                        // 元素取值记在数组字段自己名下：`{"tags": ["a"]}` 补 `tags` 的值时给 `a`。
                        Value::String(text) => add_value(&mut self.fields, path, index, text),
                        _ => {}
                    }
                }
            }
            other => {
                if path.is_empty() {
                    return;
                }
                if !self.add_field(path, mongo_scalar_type(other), index) {
                    return;
                }
                if let Value::String(text) = other {
                    add_value(&mut self.fields, path, index, text);
                }
            }
        }
    }

    /// 记录一个字段及其类型；已存在时并入类型。
    ///
    /// 返回是否已收录：返回 false 说明这条路径被上限挡掉了，调用方不该再往里塞取值，
    /// 否则会写进一个不在 `fields` 里的下标。
    fn add_field(
        &mut self,
        path: &str,
        type_name: &str,
        index: &mut std::collections::HashMap<String, usize>,
    ) -> bool {
        // 段数上限在这里判：数组元素递归不改路径，深度计数对它没有意义。
        if path.split('.').count() > MONGO_FIELD_MAX_PATH_DEPTH {
            return false;
        }
        if let Some(position) = index.get(path) {
            let types = &mut self.fields[*position].types;
            if !types.iter().any(|existing| existing == type_name) {
                types.push(type_name.to_string());
            }
            return true;
        }
        if self.fields.len() >= MONGO_FIELD_MAX_COUNT {
            return false;
        }
        index.insert(path.to_string(), self.fields.len());
        self.fields.push(MongoField {
            path: path.to_string(),
            types: vec![type_name.to_string()],
            values: Vec::new(),
        });
        true
    }
}

/// 把字符串取值记到**它所属字段**名下。
fn add_value(
    fields: &mut [MongoField],
    path: &str,
    index: &std::collections::HashMap<String, usize>,
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() || text.len() > MONGO_WORD_MAX_LEN {
        return;
    }
    let Some(position) = index.get(path) else {
        return;
    };
    let values = &mut fields[*position].values;
    if values.len() >= MONGO_FIELD_MAX_VALUES {
        return;
    }
    if !values.iter().any(|value| value == text) {
        values.push(text.to_string());
    }
}

/// 标量类型名；`Number` 不再细分整数/浮点（JSON 文本里已经看不出 BSON 宽度）。
fn mongo_scalar_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Number(_) => "Number",
        Value::String(_) => "String",
        Value::Array(_) => "Array",
        Value::Object(_) => "Object",
    }
}

/// Extended JSON 单键包装 → 对应的 BSON 类型名。
fn mongo_extended_json_type(map: &serde_json::Map<String, Value>) -> Option<&'static str> {
    if map.len() != 1 {
        return None;
    }
    Some(match map.keys().next()?.as_str() {
        "$oid" => "ObjectId",
        "$date" => "Date",
        "$numberInt" => "Int32",
        "$numberLong" => "Int64",
        "$numberDouble" => "Double",
        "$numberDecimal" => "Decimal128",
        "$binary" => "Binary",
        "$timestamp" => "Timestamp",
        "$regularExpression" => "Regex",
        "$minKey" => "MinKey",
        "$maxKey" => "MaxKey",
        "$code" => "Code",
        "$symbol" => "Symbol",
        "$undefined" => "Undefined",
        "$dbPointer" => "DBPointer",
        _ => return None,
    })
}

#[cfg(test)]
mod mongo_field_catalog_tests {
    use super::*;

    #[test]
    fn nested_paths_flatten_and_merge_types() {
        let documents = vec![
            r#"{"_id": {"$oid": "651111111111111111111111"}, "name": "张三", "tags": ["a"],
                "address": {"city": "杭州", "geo": {"lat": 1.5}}}"#
                .to_string(),
            r#"{"_id": {"$oid": "652222222222222222222222"}, "name": 42, "tags": [],
                "address": {"city": "上海"}}"#
                .to_string(),
        ];
        let mut catalog = MongoFieldCatalog::default();
        catalog.extend_from_documents(&documents);
        let field = |path: &str| {
            catalog
                .fields
                .iter()
                .find(|field| field.path == path)
                .unwrap_or_else(|| panic!("缺少字段 {path}"))
        };
        assert_eq!(field("_id").types, vec!["ObjectId"]);
        // 同一路径在不同文档里类型不同：都保留，候选右侧显示 `String | Number`。
        assert_eq!(field("name").types, vec!["String", "Number"]);
        assert_eq!(field("tags").types, vec!["Array"]);
        assert_eq!(field("address.city").types, vec!["String"]);
        assert_eq!(field("address.geo.lat").types, vec!["Number"]);
        // 取值挂在**自己字段**下，不是一张全局词表：`status` 不该拿到手机号那种串。
        assert_eq!(field("address.city").values, vec!["杭州", "上海"]);
        assert_eq!(field("name").values, vec!["张三"]);
        // 数组的标量元素记在数组字段名下。
        assert_eq!(field("tags").values, vec!["a"]);
    }

    #[test]
    fn values_stay_scoped_to_their_field() {
        let documents = vec![
            r#"{"status": "paid", "phone": "13900000001"}"#.to_string(),
            r#"{"status": "active", "phone": "13900000002"}"#.to_string(),
        ];
        let mut catalog = MongoFieldCatalog::default();
        catalog.extend_from_documents(&documents);
        let values = |path: &str| {
            catalog
                .fields
                .iter()
                .find(|field| field.path == path)
                .map(|field| field.values.clone())
                .unwrap_or_default()
        };
        assert_eq!(values("status"), vec!["paid", "active"]);
        assert_eq!(values("phone"), vec!["13900000001", "13900000002"]);
    }

    #[test]
    fn operators_and_depth_are_bounded() {
        let document = r#"{"$comment": "x", "a": {"b": {"c": {"d": {"e": {"f": 1}}}}}}"#;
        let mut catalog = MongoFieldCatalog::default();
        catalog.extend_from_documents(&[document.to_string()]);
        // `$` 开头是操作符，不进字段表。
        assert!(catalog.fields.iter().all(|field| !field.path.starts_with('$')));
        // 路径段数到上限后截断，不再产出更深的路径。
        assert!(catalog.fields.iter().any(|field| field.path == "a.b.c.d"));
        assert!(catalog.fields.iter().all(|field| field.path != "a.b.c.d.e"));
    }

    #[test]
    fn nested_arrays_do_not_eat_the_depth_budget() {
        // 数组递归不改路径段数：`tags` 里嵌对象时仍应展开出 `tags.name`，
        // 而不是被「递归深度」误判成超限。
        let document = r#"{"tags": [{"name": "a", "children": [{"name": "b"}]}]}"#;
        let mut catalog = MongoFieldCatalog::default();
        catalog.extend_from_documents(&[document.to_string()]);
        let paths: Vec<&str> = catalog.fields.iter().map(|field| field.path.as_str()).collect();
        assert!(paths.contains(&"tags"), "{paths:?}");
        assert!(paths.contains(&"tags.name"), "{paths:?}");
        assert!(paths.contains(&"tags.children"), "{paths:?}");
        assert!(paths.contains(&"tags.children.name"), "{paths:?}");
    }

    #[test]
    fn malformed_documents_are_skipped_not_fatal() {
        let mut catalog = MongoFieldCatalog::default();
        catalog.extend_from_documents(&["not json".to_string(), r#"{"ok": 1}"#.to_string()]);
        assert_eq!(catalog.fields.len(), 1);
        assert_eq!(catalog.fields[0].path, "ok");
    }
}
