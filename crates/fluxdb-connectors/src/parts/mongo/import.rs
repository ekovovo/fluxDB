// MongoDB 文档导入：格式嗅探、CSV 字段类型推断与批量写入。
//
// 语义对齐 Compass（`compass-import-export`）：
//   - 格式按**内容**嗅探，不看扩展名：首个非空白字符是 `[` → JSON 数组，
//     `{` → JSON Lines（每行一个对象），其余按 CSV 解析。
//   - CSV 每个字段先按样本推断 BSON 类型，用户可在预览里逐字段改；写库前按最终
//     类型**直接构造 `Bson`**，不走「拼 Extended JSON 文本再反序列化」——后者会把
//     `_id` 的 24 位十六进制退化成字符串，与集合里既有的 ObjectId 对不上。
//   - 坏文档（`_id` 冲突、类型解析失败）**跳过并继续**，最后如实报告成功/失败条数。

/// 单批写入文档数：与 Compass 一致（1000）。批越大单次往返越省，但一次失败的重试
/// 代价也越大；写入本身用 `ordered: false`，单条失败不影响同批其余文档。
const MONGO_IMPORT_BATCH_DOCS: usize = 1000;

/// 预览最多展示的行数。
const MONGO_IMPORT_PREVIEW_ROWS: usize = 50;

/// 推断 CSV 字段类型时最多扫描的行数。只看首行会把「第一行恰好为空」的数值列判成
/// 字符串，扫一段样本再取多数类型更稳，同时不必读完整个文件。
const MONGO_IMPORT_TYPE_SAMPLE_ROWS: usize = 200;

/// 报告里保留的失败明细条数上限，避免整文件坏数据把内存与界面撑爆。
const MONGO_IMPORT_MAX_ERRORS: usize = 20;

/// 读取导入文件文本。只接受 UTF-8（MongoDB 文档本身就是 UTF-8），并去掉 BOM。
fn mongo_read_import_text(source: &Path) -> fluxdb_core::Result<String> {
    let bytes = std::fs::read(source).map_err(|error| {
        Error::new(
            ErrorKind::Query,
            format!("读取导入文件 {} 失败：{error}", source.display()),
        )
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::new(ErrorKind::Query, "导入文件必须是 UTF-8 文本"))?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

/// 按首个非空白字符嗅探格式。
fn mongo_sniff_import_format(text: &str) -> MongoImportFormat {
    match text.trim_start().chars().next() {
        Some('[') => MongoImportFormat::JsonArray,
        Some('{') => MongoImportFormat::JsonLines,
        // 空文件也按 CSV 处理：随后报「没有字段」比报格式错误更好懂。
        _ => MongoImportFormat::Csv,
    }
}

/// 解析预览：格式、字段与类型推断，最多读前若干行；不连数据库。
fn mongo_preview_import(
    request: &MongoImportPreviewRequest,
) -> fluxdb_core::Result<MongoImportPreview> {
    let text = mongo_read_import_text(&request.source)?;
    let format = mongo_sniff_import_format(&text);
    match format {
        MongoImportFormat::Csv => mongo_preview_csv(&text, &request.options),
        _ => mongo_preview_json(&text, format),
    }
}

fn mongo_preview_csv(
    text: &str,
    options: &MongoImportOptions,
) -> fluxdb_core::Result<MongoImportPreview> {
    let mut reader = mongo_csv_reader(text, options);
    let headers = mongo_csv_headers(&mut reader)?;
    let sample = mongo_csv_sample(&mut reader, headers.len())?;
    let fields = headers
        .iter()
        .enumerate()
        .map(|(index, name)| MongoImportField {
            name: name.clone(),
            detected: mongo_csv_column_type(name, &sample, index, options),
            sample: sample
                .iter()
                .map(|record| record.get(index).cloned().unwrap_or_default())
                .find(|value| !value.is_empty())
                .unwrap_or_default(),
        })
        .collect();
    let rows = sample
        .iter()
        .take(MONGO_IMPORT_PREVIEW_ROWS)
        .map(|record| record.to_vec())
        .collect();
    Ok(MongoImportPreview {
        format: MongoImportFormat::Csv,
        columns: headers,
        fields,
        rows,
        truncated: sample.len() > MONGO_IMPORT_PREVIEW_ROWS,
    })
}

fn mongo_preview_json(
    text: &str,
    format: MongoImportFormat,
) -> fluxdb_core::Result<MongoImportPreview> {
    let parsed = mongo_parse_json_documents(text, format, Some(MONGO_IMPORT_PREVIEW_ROWS + 1))?;
    let rows = parsed
        .iter()
        .take(MONGO_IMPORT_PREVIEW_ROWS)
        .map(|document| {
            vec![match document {
                Ok(document) => serde_json::to_string_pretty(&Bson::Document(document.clone()))
                    .unwrap_or_default(),
                Err(message) => message.clone(),
            }]
        })
        .collect();
    Ok(MongoImportPreview {
        format,
        // JSON 每篇文档自带键，没有「列」的概念，UI 用单列展示原文。
        columns: Vec::new(),
        fields: Vec::new(),
        rows,
        truncated: parsed.len() > MONGO_IMPORT_PREVIEW_ROWS,
    })
}

fn mongo_csv_reader<'a>(text: &'a str, options: &MongoImportOptions) -> csv::Reader<&'a [u8]> {
    csv::ReaderBuilder::new()
        .delimiter(options.delimiter_byte())
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes())
}

fn mongo_csv_headers(reader: &mut csv::Reader<&[u8]>) -> fluxdb_core::Result<Vec<String>> {
    let headers = reader
        .headers()
        .map_err(|error| Error::new(ErrorKind::Query, format!("CSV 首行无效：{error}")))?
        .iter()
        .map(|header| header.trim().to_string())
        .collect::<Vec<_>>();
    if headers.is_empty() || headers.iter().all(String::is_empty) {
        return Err(Error::new(ErrorKind::Query, "CSV 首行必须是字段名"));
    }
    Ok(headers)
}

/// 读取类型推断样本（跳过完全空白的行）。
fn mongo_csv_sample(
    reader: &mut csv::Reader<&[u8]>,
    width: usize,
) -> fluxdb_core::Result<Vec<Vec<String>>> {
    let mut sample = Vec::new();
    for record in reader.records() {
        let record =
            record.map_err(|error| Error::new(ErrorKind::Query, format!("CSV 行无效：{error}")))?;
        let values = (0..width)
            .map(|index| record.get(index).unwrap_or("").trim().to_string())
            .collect::<Vec<_>>();
        if values.iter().any(|value| !value.is_empty()) {
            sample.push(values);
        }
        if sample.len() >= MONGO_IMPORT_TYPE_SAMPLE_ROWS {
            break;
        }
    }
    Ok(sample)
}

/// 取该列样本里出现最多的非空类型；样本全空时按 `string`。
fn mongo_csv_column_type(
    name: &str,
    sample: &[Vec<String>],
    index: usize,
    options: &MongoImportOptions,
) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for record in sample {
        let value = record.get(index).map(String::as_str).unwrap_or("");
        if value.is_empty() {
            continue;
        }
        let detected = mongo_detect_csv_type(value, name).to_string();
        match counts.iter_mut().find(|(kind, _)| *kind == detected) {
            Some((_, count)) => *count += 1,
            None => counts.push((detected, 1)),
        }
    }
    let detected = counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(kind, _)| kind)
        .unwrap_or_else(|| "string".to_string());
    options.field_type(name, &detected)
}

/// 推断单个 CSV 值的 BSON 类型（口径取 Compass 的 `detectCSVFieldType`）。
fn mongo_detect_csv_type(value: &str, name: &str) -> &'static str {
    // `_id` 的 24 位十六进制是 ObjectId 的唯一可识别形态；不认这一条，导入后主键
    // 会变成字符串，和集合里既有数据查不到一起。
    if name.eq_ignore_ascii_case("_id") && mongo_parse_object_id(value).is_some() {
        return "objectId";
    }
    match value {
        "" => return "string",
        "null" | "NULL" | "Null" => return "null",
        "true" | "TRUE" | "True" | "t" | "false" | "FALSE" | "False" | "f" => return "boolean",
        _ => {}
    }
    if mongo_looks_like_number(value) {
        // 带小数点或指数一律 double：`1.0` 写成 int 会静默丢精度。
        if value.contains('.') || value.contains('e') || value.contains('E') {
            return "double";
        }
        return match value.parse::<i64>() {
            Ok(number) if i32::try_from(number).is_ok() => "int32",
            Ok(_) => "int64",
            // 超出 i64 的整数保持字符串，避免溢出成错误值。
            Err(_) => "string",
        };
    }
    if mongo_looks_like_date(value) {
        return "date";
    }
    "string"
}

/// 只接受 `-?digits[.digits][e±digits]` 形态，避免 `1e`、`-` 之类的半成品被当成数字。
fn mongo_looks_like_number(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    if matches!(chars.peek(), Some('-' | '+')) {
        chars.next();
    }
    let mut digits = 0usize;
    let mut dots = 0usize;
    while let Some(char) = chars.peek().copied() {
        match char {
            '0'..='9' => {
                digits += 1;
                chars.next();
            }
            '.' if dots == 0 => {
                dots += 1;
                chars.next();
            }
            _ => break,
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(chars.peek(), Some('e' | 'E')) {
        chars.next();
        if matches!(chars.peek(), Some('-' | '+')) {
            chars.next();
        }
        let mut exponent_digits = 0usize;
        for char in chars.by_ref() {
            if !char.is_ascii_digit() {
                return false;
            }
            exponent_digits += 1;
        }
        return exponent_digits > 0;
    }
    chars.peek().is_none()
}

/// `2024-01-02` 或 `2024-01-02T03:04:05…` / `2024-01-02 03:04:05…`。
fn mongo_looks_like_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    let head = &value[..10];
    if !head.as_bytes()[4].eq(&b'-') || !head.as_bytes()[7].eq(&b'-') {
        return false;
    }
    let date_ok = head[..4].chars().all(|char| char.is_ascii_digit())
        && head[5..7].chars().all(|char| char.is_ascii_digit())
        && head[8..10].chars().all(|char| char.is_ascii_digit());
    if !date_ok {
        return false;
    }
    // 只认日期，或日期后紧跟时间（T / 空格）。
    value.len() == 10 || matches!(bytes[10], b'T' | b' ')
}

/// 24 位十六进制（兼容 `ObjectId("…")` / `ObjectId(…)` 两种导出写法）。
fn mongo_parse_object_id(value: &str) -> Option<mongodb::bson::oid::ObjectId> {
    let hex = value
        .trim()
        .trim_start_matches("ObjectId(")
        .trim_end_matches(')')
        .trim_matches('"');
    if hex.len() != 24 || !hex.chars().all(|char| char.is_ascii_hexdigit()) {
        return None;
    }
    mongodb::bson::oid::ObjectId::parse_str(hex).ok()
}

/// 单个 CSV 值按指定类型转 BSON。解析失败时返回原因，由调用方计入失败文档。
fn mongo_csv_cell(
    value: &str,
    type_name: &str,
    options: &MongoImportOptions,
) -> Result<Bson, String> {
    // 空格子单独处理：按选项写 null 或空字符串，不参与类型解析——
    // 否则数值列里的空单元格会被判成「不是 32 位整数」而丢掉整条文档。
    if value.is_empty() {
        return Ok(if options.empty_as_null {
            Bson::Null
        } else {
            Bson::String(String::new())
        });
    }
    match type_name {
        "objectId" => mongo_parse_object_id(value)
            .map(Bson::ObjectId)
            .ok_or_else(|| format!("「{value}」不是合法的 ObjectId")),
        "int32" => value
            .parse::<i32>()
            .map(Bson::Int32)
            .map_err(|_| format!("「{value}」不是 32 位整数")),
        "int64" => value
            .parse::<i64>()
            .map(Bson::Int64)
            .map_err(|_| format!("「{value}」不是 64 位整数")),
        "double" => value
            .parse::<f64>()
            .map(Bson::Double)
            .map_err(|_| format!("「{value}」不是浮点数")),
        "boolean" => match value {
            "true" | "TRUE" | "True" | "t" | "T" | "1" => Ok(Bson::Boolean(true)),
            "false" | "FALSE" | "False" | "f" | "F" | "0" => Ok(Bson::Boolean(false)),
            _ => Err(format!("「{value}」不是布尔值")),
        },
        "date" => mongo_parse_csv_date(value)
            .map(Bson::DateTime)
            .ok_or_else(|| format!("「{value}」不是日期")),
        "null" => match value {
            "" | "null" | "NULL" | "Null" => Ok(Bson::Null),
            _ => Err(format!("「{value}」不是 null")),
        },
        _ => Ok(Bson::String(value.to_string())),
    }
}

/// UTC 时间转 BSON 日期。不启用 bson 的 chrono feature，只用毫秒即可。
fn mongo_date_from_utc(value: chrono::DateTime<chrono::Utc>) -> mongodb::bson::DateTime {
    mongodb::bson::DateTime::from_millis(value.timestamp_millis())
}

/// CSV 日期：纯日期按 UTC 零点，带时间的按 RFC3339 或 `YYYY-MM-DD HH:MM:SS`。
fn mongo_parse_csv_date(value: &str) -> Option<mongodb::bson::DateTime> {
    if value.len() == 10 {
        let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
        let midnight = date.and_hms_opt(0, 0, 0)?;
        return Some(mongo_date_from_utc(midnight.and_utc()));
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(mongo_date_from_utc(parsed.with_timezone(&chrono::Utc)));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(mongo_date_from_utc(parsed.and_utc()));
        }
    }
    None
}

/// 解析 JSON / JSONL 文档。每行保留独立错误：单条坏文档不影响其余文档。
fn mongo_parse_json_documents(
    text: &str,
    format: MongoImportFormat,
    limit: Option<usize>,
) -> fluxdb_core::Result<Vec<Result<Document, String>>> {
    if format == MongoImportFormat::JsonArray {
        return mongo_parse_json_array(text, limit);
    }
    let mut documents = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        documents.push(
            mongo_json_line_to_document(line)
                .map_err(|message| format!("第 {} 行：{message}", index + 1)),
        );
        if limit.is_some_and(|limit| documents.len() >= limit) {
            break;
        }
    }
    Ok(documents)
}

fn mongo_parse_json_array(
    text: &str,
    limit: Option<usize>,
) -> fluxdb_core::Result<Vec<Result<Document, String>>> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| Error::new(ErrorKind::Query, format!("JSON 文件无效：{error}")))?;
    let serde_json::Value::Array(items) = value else {
        return Err(Error::new(ErrorKind::Query, "JSON 文件顶层必须是对象数组"));
    };
    Ok(items
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .enumerate()
        .map(|(index, item)| {
            mongo_json_value_to_document(item)
                .map_err(|message| format!("第 {} 条：{message}", index + 1))
        })
        .collect())
}

fn mongo_json_line_to_document(line: &str) -> Result<Document, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| format!("不是合法 JSON：{error}"))?;
    mongo_json_value_to_document(value)
}

/// JSON 值转 BSON 文档；`$oid` / `$date` 等 Extended JSON 包装在此还原成真实类型。
fn mongo_json_value_to_document(value: serde_json::Value) -> Result<Document, String> {
    match Bson::try_from(value) {
        Ok(Bson::Document(document)) => Ok(document),
        Ok(_) => Err("不是 JSON 对象".to_string()),
        Err(error) => Err(format!("不是合法的 Extended JSON：{error}")),
    }
}

/// 把 CSV 记录转成文档。`types` 与 `headers` 等长，是用户确认后的最终类型。
fn mongo_csv_documents(
    text: &str,
    options: &MongoImportOptions,
    headers: &[String],
    types: &[String],
    limit: Option<usize>,
) -> fluxdb_core::Result<Vec<Result<Document, String>>> {
    let mut reader = mongo_csv_reader(text, options);
    // 跳过表头（`mongo_csv_headers` 已经读过，这里重新创建的 reader 需要自己消费）。
    let _ = mongo_csv_headers(&mut reader)?;
    let mut documents = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record =
            record.map_err(|error| Error::new(ErrorKind::Query, format!("CSV 行无效：{error}")))?;
        let mut document = Document::new();
        let mut failure = None;
        for (column, name) in headers.iter().enumerate() {
            let value = record.get(column).unwrap_or("").trim();
            let type_name = types.get(column).map(String::as_str).unwrap_or("string");
            match mongo_csv_cell(value, type_name, options) {
                Ok(value) => {
                    document.insert(name, value);
                }
                Err(message) => {
                    failure = Some(format!("第 {} 行字段 {}：{message}", index + 2, name));
                    break;
                }
            }
        }
        documents.push(match failure {
            Some(message) => Err(message),
            None => Ok(document),
        });
        if limit.is_some_and(|limit| documents.len() >= limit) {
            break;
        }
    }
    Ok(documents)
}

/// 导入入口：解析 → 按批 `insert_many(ordered: false)` → 汇总成功/失败。
fn mongo_import_documents(
    config: &ConnectionConfig,
    request: &MongoImportRequest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(MongoImportProgress),
) -> fluxdb_core::Result<MongoImportReport> {
    let database = request.object.database.clone().unwrap_or_default();
    let collection_name = request.object.name.clone();
    if database.trim().is_empty() || collection_name.trim().is_empty() {
        return Err(Error::new(ErrorKind::Query, "请指定数据库与集合"));
    }
    progress(MongoImportProgress {
        stage: "读取".into(),
        message: "正在解析导入文件".into(),
    });
    let text = mongo_read_import_text(&request.source)?;
    let format = mongo_sniff_import_format(&text);
    let parsed = match format {
        MongoImportFormat::Csv => {
            let mut reader = mongo_csv_reader(&text, &request.options);
            let headers = mongo_csv_headers(&mut reader)?;
            let sample = mongo_csv_sample(&mut reader, headers.len())?;
            let types = headers
                .iter()
                .enumerate()
                .map(|(index, name)| mongo_csv_column_type(name, &sample, index, &request.options))
                .collect::<Vec<_>>();
            mongo_csv_documents(&text, &request.options, &headers, &types, None)?
        }
        _ => mongo_parse_json_documents(&text, format, None)?,
    };
    if parsed.is_empty() {
        return Err(Error::new(ErrorKind::Query, "导入文件里没有可写入的文档"));
    }

    let total = parsed.len() as u64;
    let mut report = MongoImportReport::default();
    // 解析阶段就失败的文档不必发到服务端，直接计入失败。
    let mut documents = Vec::with_capacity(parsed.len());
    for item in parsed {
        match item {
            Ok(document) => documents.push(document),
            Err(message) => {
                report.failed += 1;
                if report.errors.len() < MONGO_IMPORT_MAX_ERRORS {
                    report.errors.push(message);
                }
            }
        }
    }

    let client = mongo_connect(config)?;
    let collection = client
        .database(database.trim())
        .collection::<Document>(collection_name.trim());
    let batch_total = documents.len().div_ceil(MONGO_IMPORT_BATCH_DOCS);
    for (batch_index, batch) in documents.chunks(MONGO_IMPORT_BATCH_DOCS).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(MongoImportProgress {
            stage: "写入".into(),
            message: format!(
                "第 {}/{} 批，已写入 {} 条（共 {} 条）",
                batch_index + 1,
                batch_total,
                report.inserted,
                total
            ),
        });
        let outcome = mongo_run("导入文档", {
            let collection = collection.clone();
            let batch = batch.to_vec();
            async move { collection.insert_many(batch).ordered(false).await }
        })?;
        match outcome {
            Ok(result) => report.inserted += result.inserted_ids.len() as u64,
            Err(error) => {
                let Some((failed, messages)) = mongo_insert_batch_failures(&error, batch.len())
                else {
                    // 服务端逐条写错误才是「坏文档」；其它错误（网络、认证）无法判断
                    // 哪些文档进了库，继续跑只会放大不一致，直接中止并报错。
                    return Err(mongo_error(error));
                };
                // `ordered: false` 下同批其余文档照常写入，成功数按批大小减去失败数。
                report.inserted += batch.len() as u64 - failed;
                report.failed += failed;
                for message in messages {
                    if report.errors.len() < MONGO_IMPORT_MAX_ERRORS {
                        report.errors.push(message);
                    }
                }
            }
        }
    }
    progress(MongoImportProgress {
        stage: "完成".into(),
        message: format!("成功 {} 条，失败 {} 条", report.inserted, report.failed),
    });
    Ok(report)
}

/// 从 `insert_many` 错误里抽出逐条写错误。
///
/// 返回 `None` 表示这不是「坏文档」类错误（网络、认证、超时……），调用方应中止导入：
/// 那种情况下无法知道哪些文档真的落库了，继续跑只会放大不一致。
fn mongo_insert_batch_failures(error: &MongoError, batch_len: usize) -> Option<(u64, Vec<String>)> {
    let MongoErrorKind::InsertMany(insert_many) = &*error.kind else {
        return None;
    };
    let mut messages = Vec::new();
    let mut failed = 0u64;
    if let Some(write_errors) = &insert_many.write_errors {
        failed = write_errors.len() as u64;
        for write_error in write_errors {
            messages.push(format!(
                "第 {} 条：{}",
                write_error.index + 1,
                write_error.message
            ));
        }
    }
    if let Some(write_concern_error) = &insert_many.write_concern_error {
        // 写关注失败时服务端没说清哪几条没落盘，按整批未确认报告，避免谎报成功。
        failed = failed.max(batch_len as u64);
        messages.push(format!("写关注未确认：{}", write_concern_error.message));
    }
    if failed == 0 {
        return None;
    }
    Some((failed, messages))
}

#[cfg(test)]
mod mongo_import_engine_tests {
    use super::*;
    use fluxdb_core::MongoImportFieldOverride;

    /// 格式按内容嗅探，不看扩展名——Compass 同样只看首个非空白字符。
    #[test]
    fn sniffs_format_from_content() {
        assert_eq!(
            mongo_sniff_import_format("  \n[ {\"a\":1} ]"),
            MongoImportFormat::JsonArray
        );
        assert_eq!(
            mongo_sniff_import_format("{\"a\":1}\n{\"a\":2}"),
            MongoImportFormat::JsonLines
        );
        assert_eq!(
            mongo_sniff_import_format("name,age\nalice,30"),
            MongoImportFormat::Csv
        );
    }

    /// `_id` 的 24 位十六进制必须是 ObjectId，否则导入后的主键与集合里既有数据对不上。
    #[test]
    fn detects_csv_types_like_compass() {
        let oid = "6571c1b0a1b2c3d4e5f60001";
        assert_eq!(mongo_detect_csv_type(oid, "_id"), "objectId");
        assert_eq!(mongo_detect_csv_type(oid, "other"), "string");
        assert_eq!(
            mongo_detect_csv_type("ObjectId(\"6571c1b0a1b2c3d4e5f60001\")", "_id"),
            "objectId"
        );
        assert_eq!(mongo_detect_csv_type("42", "n"), "int32");
        assert_eq!(mongo_detect_csv_type("9000000000", "n"), "int64");
        assert_eq!(mongo_detect_csv_type("1.5", "n"), "double");
        assert_eq!(mongo_detect_csv_type("1e3", "n"), "double");
        assert_eq!(mongo_detect_csv_type("99999999999999999999", "n"), "string");
        assert_eq!(mongo_detect_csv_type("TRUE", "b"), "boolean");
        assert_eq!(mongo_detect_csv_type("null", "x"), "null");
        assert_eq!(mongo_detect_csv_type("2024-01-02", "d"), "date");
        assert_eq!(mongo_detect_csv_type("2024-01-02 03:04:05", "d"), "date");
        // 半成品数字/日期不能被当成对应类型。
        assert_eq!(mongo_detect_csv_type("-", "n"), "string");
        assert_eq!(mongo_detect_csv_type("2024-01", "d"), "string");
    }

    /// 逐列取多数类型；用户覆盖优先于推断。
    #[test]
    fn column_type_uses_majority_and_honours_override() {
        let mut options = MongoImportOptions::new();
        let sample = vec![
            vec!["1".to_string(), "".to_string()],
            vec!["2".to_string(), "".to_string()],
            vec!["x".to_string(), "".to_string()],
        ];
        assert_eq!(mongo_csv_column_type("n", &sample, 0, &options), "int32");
        // 全空列回落 string，而不是 null。
        assert_eq!(mongo_csv_column_type("e", &sample, 1, &options), "string");
        options.field_overrides.push(MongoImportFieldOverride {
            name: "n".into(),
            type_name: "string".into(),
        });
        assert_eq!(mongo_csv_column_type("n", &sample, 0, &options), "string");
        assert_eq!(options.delimiter_byte(), b',');
        options.delimiter = ";extra".into();
        assert_eq!(options.delimiter_byte(), b';');
    }

    /// 空值策略与各类型转换；非法值必须报出可读原因而不是静默写成 null。
    #[test]
    fn converts_csv_cells_to_bson() {
        let options = MongoImportOptions::new();
        assert_eq!(
            mongo_csv_cell("", "int32", &options).expect("空值"),
            Bson::Null
        );
        let mut strict = MongoImportOptions::new();
        strict.empty_as_null = false;
        assert_eq!(
            mongo_csv_cell("", "int32", &strict).expect("空字符串"),
            Bson::String(String::new())
        );
        assert!(matches!(
            mongo_csv_cell("6571c1b0a1b2c3d4e5f60001", "objectId", &options),
            Ok(Bson::ObjectId(_))
        ));
        assert!(mongo_csv_cell("not-an-id", "objectId", &options).is_err());
        assert!(mongo_csv_cell("abc", "int32", &options).is_err());
        assert_eq!(
            mongo_csv_cell("0", "boolean", &options).expect("布尔"),
            Bson::Boolean(false)
        );
        assert!(mongo_csv_cell("yes", "boolean", &options).is_err());
        assert!(matches!(
            mongo_csv_cell("2024-01-02T03:04:05Z", "date", &options),
            Ok(Bson::DateTime(_))
        ));
        assert!(mongo_csv_cell("2024-13-45", "date", &options).is_err());
    }

    /// JSON 数组与 JSONL 的坏条目要带位置信息，好让用户能定位到源文件。
    #[test]
    fn reports_json_errors_with_position() {
        let documents = mongo_parse_json_documents(
            "[{\"a\":1}, 3, {\"a\":2}]",
            MongoImportFormat::JsonArray,
            None,
        )
        .expect("解析数组");
        assert_eq!(documents.len(), 3);
        assert!(documents[0].is_ok());
        assert!(documents[1].as_ref().unwrap_err().contains("第 2 条"));
        assert!(documents[2].is_ok());

        let documents = mongo_parse_json_documents(
            "{\"a\":1}\nnot json\n{\"a\":2}",
            MongoImportFormat::JsonLines,
            None,
        )
        .expect("解析 JSONL");
        assert_eq!(documents.len(), 3);
        assert!(documents[1].as_ref().unwrap_err().contains("第 2 行"));
    }

    /// 顶层不是对象数组时报错，而不是把单个对象静默当一条。
    #[test]
    fn rejects_json_array_of_non_objects() {
        let error = mongo_parse_json_documents("[1, 2]", MongoImportFormat::JsonArray, None);
        assert!(error.is_ok(), "逐条错误留给条目本身");
        let documents = error.expect("解析");
        assert_eq!(documents.len(), 2);
        assert!(documents.iter().all(|item| item.is_err()));

        let error = mongo_parse_json_documents("{\"a\":1}", MongoImportFormat::JsonArray, None)
            .expect_err("顶层对象不是数组");
        assert!(error.message.contains("数组"), "{}", error.message);
    }
}
