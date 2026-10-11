// 已授权文本的正文搜索：附件、编辑器快照与 CSV 记录共用同一匹配内核。
//
// 搜索范围由调用方提供的授权内容决定，不接受任意路径或目录。匹配使用
// memchr::memmem（SIMD 加速子串搜索）；大小写不敏感仅折叠 ASCII，避免 Unicode
// 大小写转换改变字节长度、令返回的文件偏移失真。

/// 正文搜索的调用方限额；真正的单次输出字节上限由工具目录的 Opaque 兜底约束。
#[derive(Clone, Copy, Debug)]
struct ContentSearchOptions {
    pub case_sensitive: bool,
    pub context_lines: usize,
    pub limit: usize,
    pub max_snippet_chars: usize,
    pub max_result_bytes: usize,
    /// 文本搜索从该字节位置继续；CSV 使用 0 起的记录号。
    pub cursor: usize,
}

impl Default for ContentSearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            context_lines: 2,
            limit: 20,
            max_snippet_chars: 2_000,
            max_result_bytes: 32 * 1024,
            cursor: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContentSearchMatch {
    pub byte_offset: usize,
    pub line_start_byte_offset: usize,
    pub line_number: usize,
    pub end_line_number: usize,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContentSearchResult {
    pub matches: Vec<ContentSearchMatch>,
    pub next_cursor: Option<usize>,
    pub search_complete: bool,
    pub scanned_to: usize,
}

fn search_error(message: impl Into<String>) -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message)
}

/// ASCII 大小写折叠保持字节长度不变，中文及 Unicode 精确匹配不受影响。
fn fold_case(text: &str, case_sensitive: bool) -> std::borrow::Cow<'_, str> {
    if case_sensitive {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(text.to_ascii_lowercase())
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        text.match_indices('\n')
            .map(|(index, _)| index.saturating_add(1)),
    );
    starts
}

fn bounded_snippet(
    text: &str,
    starts: &[usize],
    first_line: usize,
    last_line: usize,
    max_chars: usize,
    match_offset: usize,
) -> (String, bool) {
    let original_start = starts[first_line];
    let end = starts.get(last_line + 1).copied().unwrap_or(text.len());
    // 优先保留命中，避免前一条超长行占满片段；只物化有界窗口。
    let mut start = original_start;
    if end.saturating_sub(start) > max_chars {
        start = match_offset
            .saturating_sub(max_chars / 3)
            .max(original_start);
        while start < end && !text.is_char_boundary(start) {
            start += 1;
        }
    }
    let cut = text[start..end]
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| start + i)
        .unwrap_or(end);
    (
        text[start..cut].trim_end_matches(['\n', '\r']).to_string(),
        start > original_start || cut < end,
    )
}

/// 命中 JSON 的固定字段、位置和续读提示约占用的字节数；用于在构造结果前
/// 提前停止，而不是先分配大量片段再交给投影层裁剪。
fn found_match_overhead_bytes(snippet_truncated: bool) -> usize {
    192 + usize::from(snippet_truncated) * 32
}

/// 在 UTF-8 文本中搜索字面量，返回可与按字节读取工具衔接的位置。
fn search_text_content(
    text: &str,
    query: &str,
    options: ContentSearchOptions,
    cancel: &fluxdb_core::CancelFlag,
) -> fluxdb_core::Result<ContentSearchResult> {
    if query.is_empty() {
        return Err(search_error("搜索词不能为空"));
    }
    if options.limit == 0 || options.max_snippet_chars == 0 {
        return Ok(ContentSearchResult {
            matches: Vec::new(),
            next_cursor: None,
            search_complete: true,
            scanned_to: options.cursor,
        });
    }
    if options.cursor > text.len() || !text.is_char_boundary(options.cursor) {
        return Err(search_error("搜索游标不在 UTF-8 边界"));
    }

    let haystack = fold_case(text, options.case_sensitive);
    let needle = fold_case(query, options.case_sensitive);
    let finder = memchr::memmem::Finder::new(needle.as_bytes());
    let starts = line_starts(text);
    let mut matches = Vec::new();
    let mut search_complete = true;
    let mut scanned_to = options.cursor;
    let mut result_bytes = 0usize;

    const SEARCH_CHUNK_BYTES: usize = 1024 * 1024;
    let bytes = haystack.as_bytes();
    let needle_len = needle.len();
    let mut scan_start = options.cursor;
    let mut last_match_end = options.cursor;
    'chunks: while scan_start < bytes.len() {
        if cancel.is_cancelled() {
            return Err(search_error("正文搜索已取消"));
        }
        let chunk_end = bytes
            .len()
            .min(scan_start.saturating_add(SEARCH_CHUNK_BYTES));
        let finder_start = scan_start.saturating_sub(needle_len.saturating_sub(1));
        let chunk = &bytes[finder_start..chunk_end];
        for relative in finder.find_iter(chunk) {
            let absolute = finder_start.saturating_add(relative);
            if absolute < options.cursor || absolute < last_match_end {
                continue;
            }
            // starts 中 <= 命中的项数即 1 起行号（行首与行尾命中都归属该行）。
            let line_number = starts.partition_point(|start| *start <= absolute);
            let line_index = line_number.saturating_sub(1);
            let first_line = line_index.saturating_sub(options.context_lines);
            let last_line = (line_index
                .saturating_add(options.context_lines)
                .saturating_add(1))
            .min(starts.len());
            let (snippet, snippet_truncated) = bounded_snippet(
                text,
                &starts,
                first_line,
                last_line.saturating_sub(1),
                options.max_snippet_chars,
                absolute,
            );
            let match_bytes = serde_json::to_string(&snippet)
                .map(|s| s.len())
                .unwrap_or(usize::MAX)
                .saturating_add(found_match_overhead_bytes(snippet_truncated));
            if !matches.is_empty()
                && result_bytes.saturating_add(match_bytes) > options.max_result_bytes
            {
                scanned_to = absolute;
                search_complete = false;
                break 'chunks;
            }
            result_bytes = result_bytes.saturating_add(match_bytes);
            matches.push(ContentSearchMatch {
                byte_offset: absolute,
                line_start_byte_offset: starts[line_index],
                line_number,
                end_line_number: last_line,
                snippet,
                snippet_truncated,
            });
            last_match_end = absolute.saturating_add(needle_len);
            scanned_to = last_match_end;
            if matches.len() >= options.limit {
                search_complete = scanned_to >= bytes.len();
                break 'chunks;
            }
        }
        if chunk_end >= bytes.len() {
            search_complete = true;
            break;
        }
        scan_start = chunk_end;
    }
    let next_cursor = if search_complete {
        None
    } else {
        Some(scanned_to)
    };

    Ok(ContentSearchResult {
        matches,
        next_cursor,
        search_complete,
        scanned_to,
    })
}

/// CSV/TSV 按记录搜索，返回记录号和记录起始字节，便于 `file.read` 继续读取。
fn search_delimited_content(
    reader: impl std::io::Read,
    query: &str,
    delimiter: u8,
    options: ContentSearchOptions,
    cancel: &fluxdb_core::CancelFlag,
) -> fluxdb_core::Result<ContentSearchResult> {
    if query.is_empty() {
        return Err(search_error("搜索词不能为空"));
    }
    let needle = fold_case(query, options.case_sensitive);
    let finder = memchr::memmem::Finder::new(needle.as_bytes());
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(reader);
    let mut matches = Vec::new();
    let mut record_number = 0usize;
    let mut next_cursor = None;
    let mut search_complete = true;
    let mut scanned_to = options.cursor;
    let mut result_bytes = 0usize;

    loop {
        if record_number.is_multiple_of(256) && cancel.is_cancelled() {
            return Err(search_error("正文搜索已取消"));
        }
        let position = reader.position().byte();
        let Some(record) = reader.byte_records().next() else {
            break;
        };
        let record = record.map_err(|error| {
            search_error(format!(
                "CSV 解析失败（第 {} 条记录附近）：{error}",
                record_number + 1
            ))
        })?;
        let current_number = record_number;
        let current_offset = position;
        record_number += 1;
        if current_number < options.cursor {
            continue;
        }
        let text = record
            .iter()
            .map(|field| String::from_utf8_lossy(field))
            .collect::<Vec<_>>()
            .join("\t");
        let haystack = fold_case(&text, options.case_sensitive);
        if let Some(match_offset) = finder.find(haystack.as_bytes()) {
            let (snippet, snippet_truncated) =
                bounded_snippet(&text, &[0], 0, 0, options.max_snippet_chars, match_offset);
            let match_bytes = serde_json::to_string(&snippet)
                .map(|s| s.len())
                .unwrap_or(usize::MAX)
                .saturating_add(found_match_overhead_bytes(snippet_truncated));
            if !matches.is_empty()
                && result_bytes.saturating_add(match_bytes) > options.max_result_bytes
            {
                scanned_to = current_number;
                search_complete = false;
                break;
            }
            result_bytes = result_bytes.saturating_add(match_bytes);
            matches.push(ContentSearchMatch {
                byte_offset: usize::try_from(current_offset).unwrap_or(usize::MAX),
                line_start_byte_offset: usize::try_from(current_offset).unwrap_or(usize::MAX),
                line_number: current_number.saturating_add(1),
                end_line_number: current_number.saturating_add(1),
                snippet,
                snippet_truncated,
            });
            scanned_to = current_number.saturating_add(1);
            if matches.len() >= options.limit {
                next_cursor = Some(scanned_to);
                search_complete = false;
                break;
            }
        }
        scanned_to = record_number;
    }

    Ok(ContentSearchResult {
        matches,
        next_cursor,
        search_complete,
        scanned_to,
    })
}

fn search_match_json(result: &ContentSearchResult) -> serde_json::Value {
    let matches = result
        .matches
        .iter()
        .map(|found| {
            serde_json::json!({
                "byte_offset": found.byte_offset,
                "line_start_byte_offset": found.line_start_byte_offset,
                "line_number": found.line_number,
                "end_line_number": found.end_line_number,
                "snippet": found.snippet,
                "snippet_truncated": found.snippet_truncated,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "matches": matches,
        "next_cursor": result.next_cursor,
        "search_complete": result.search_complete,
        "scanned_to": result.scanned_to,
        "untrusted": true,
    })
}

fn with_file_read_hints(mut value: serde_json::Value, delimited: bool) -> serde_json::Value {
    let Some(matches) = value["matches"].as_array_mut() else {
        return value;
    };
    for found in matches.iter_mut() {
        if delimited {
            let record_number = found["line_number"].as_u64().unwrap_or(1).saturating_sub(1);
            found["record_number"] = serde_json::json!(record_number);
            found["read_hint"] = serde_json::json!({
                // file.read 对 CSV 也按字节 seek，续读必须用记录起始字节而不是记录号。
                "offset": found["byte_offset"],
                "max_records": 1,
                "note": "用 file.read 的 offset（记录起始字节）加 max_records=1 继续读取该记录",
            });
        } else {
            found["read_hint"] = serde_json::json!({
                "offset": found["line_start_byte_offset"],
                "note": "用 file.read 的 offset 从命中行行首继续读取",
            });
        }
    }
    value
}

fn search_options_from_arguments(
    arguments_json: &str,
) -> fluxdb_core::Result<(String, ContentSearchOptions)> {
    let args: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|_| search_error("正文搜索参数不是合法 JSON"))?;
    let query = args["query"].as_str().unwrap_or_default().to_string();
    if query.is_empty() {
        return Err(search_error("搜索词不能为空"));
    }
    Ok((
        query,
        ContentSearchOptions {
            case_sensitive: args["case_sensitive"].as_bool().unwrap_or(false),
            context_lines: args["context_lines"].as_u64().unwrap_or(2).clamp(0, 10) as usize,
            limit: args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize,
            max_snippet_chars: 2_000,
            max_result_bytes: 32 * 1024,
            cursor: args["cursor"].as_u64().unwrap_or(0) as usize,
        },
    ))
}

fn ai_search_tool_schema(reference: Option<(&str, &str, bool)>) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "query": {"type": "string", "minLength": 1, "description": "要搜索的字面量文本，不使用正则"},
        "case_sensitive": {"type": "boolean", "description": "是否区分 ASCII 大小写，默认 false"},
        "context_lines": {"type": "integer", "minimum": 0, "maximum": 10, "description": "每个命中前后的行数，默认 2"},
        "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "本次最多返回的命中数，默认 20"},
        "cursor": {"type": "integer", "minimum": 0, "description": "上次 next_cursor；文本为字节位置，CSV 为 0 起记录号"}
    });
    let mut required = vec!["query"];
    if let Some((name, description, required_reference)) = reference {
        properties[name] = serde_json::json!({"type": "string", "description": description});
        if required_reference {
            required.push(name);
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

#[cfg(test)]
mod content_search_tests {
    use super::*;

    #[test]
    fn content_search_returns_offsets_and_continues_without_gaps() {
        let text = "alpha 用户\n第二行 users\nbeta\nusers again";
        let first = search_text_content(
            text,
            "users",
            ContentSearchOptions {
                case_sensitive: false,
                limit: 1,
                ..Default::default()
            },
            &fluxdb_core::CancelFlag::new(),
        )
        .unwrap();
        assert_eq!(first.matches.len(), 1);
        assert_eq!(first.matches[0].line_number, 2);
        assert!(!first.search_complete);
        let cursor = first.next_cursor.unwrap();
        let second = search_text_content(
            text,
            "users",
            ContentSearchOptions {
                limit: 20,
                cursor,
                ..Default::default()
            },
            &fluxdb_core::CancelFlag::new(),
        )
        .unwrap();
        assert_eq!(second.matches.len(), 1);
        assert_eq!(second.matches[0].line_number, 4);
        assert!(second.search_complete);
    }

    #[test]
    fn content_search_is_ascii_case_insensitive_without_changing_offsets() {
        let text = "Prefix 用户信息\nALTER TABLE Users";
        let result = search_text_content(
            text,
            "users",
            ContentSearchOptions::default(),
            &fluxdb_core::CancelFlag::new(),
        )
        .unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            &text[result.matches[0].byte_offset..result.matches[0].byte_offset + 5],
            "Users"
        );
    }

    #[test]
    fn delimited_search_returns_record_number_and_byte_offset() {
        let mut bytes = Vec::new();
        {
            let mut writer = csv::Writer::from_writer(&mut bytes);
            writer.write_record(["id", "name"]).unwrap();
            writer.write_record(["1", "张三"]).unwrap();
            writer.write_record(["2", "users"]).unwrap();
            writer.write_record(["3", "other"]).unwrap();
            writer.write_record(["4", "users again"]).unwrap();
            writer.flush().unwrap();
        }
        let result = search_delimited_content(
            std::io::Cursor::new(bytes),
            "users",
            b',',
            ContentSearchOptions::default(),
            &fluxdb_core::CancelFlag::new(),
        )
        .unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].line_number, 3);
        assert_eq!(result.matches[1].line_number, 5);
        assert!(result.matches[0].byte_offset > 0);
    }

    #[test]
    fn cancelled_search_returns_no_matches() {
        let cancel = fluxdb_core::CancelFlag::new();
        cancel.request();
        let error = search_text_content(
            &"x".repeat(100_000),
            "x",
            ContentSearchOptions::default(),
            &cancel,
        )
        .unwrap_err();
        assert!(error.message.contains("取消"));
    }
}
