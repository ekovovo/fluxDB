const FILE_PLUGIN_DISABLED: &str = "文件与附件已停用，请启用该插件或移除附件后重试";

fn ai_files_enabled(conn: &rusqlite::Connection) -> fluxdb_core::Result<bool> {
    Ok(
        fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
            .iter()
            .any(|instance| {
                instance.id == "builtin.files"
                    && instance.package_key == "fluxdb.context.files"
                    && instance.runtime_kind == "builtin"
                    && instance.enabled
            }),
    )
}

fn ensure_ai_files_enabled(conn: &rusqlite::Connection) -> fluxdb_core::Result<()> {
    if ai_files_enabled(conn)? {
        Ok(())
    } else {
        Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            FILE_PLUGIN_DISABLED,
        ))
    }
}

struct AiFileTool {
    descriptor: fluxdb_core::ToolDescriptor,
    references: Vec<AiContextReference>,
    storage: fluxdb_storage::FileStorage,
}
impl fluxdb_core::AiTool for AiFileTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }
    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let now = ai_chat_now();
            let result = (|| -> fluxdb_core::Result<serde_json::Value> {
                let args: serde_json::Value =
                    serde_json::from_str(&arguments_json).map_err(|_| {
                        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "文件参数无效")
                    })?;
                if ctx.cancel.is_cancelled() {
                    return Err(fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "文件读取已取消",
                    ));
                }
                // 目录可能早于插件停用创建，每次调用仍需在打开文件前核对当前状态。
                with_ai_storage(&self.storage, ensure_ai_files_enabled)?;
                if self.descriptor.id == "file.search_content" {
                    let id = args["file_ref"].as_str().unwrap_or("");
                    let reference = self
                        .references
                        .iter()
                        .find(|r| r.id == id && r.kind == "file")
                        .ok_or_else(|| {
                            fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "附件未授权或已移除",
                            )
                        })?;
                    let grant: serde_json::Value = serde_json::from_str(&reference.source)
                        .map_err(|_| {
                            fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "附件授权损坏",
                            )
                        })?;
                    if grant["binary"].as_bool().unwrap_or(false) {
                        return Err(fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "二进制附件不提供正文搜索",
                        ));
                    }
                    let path = std::path::Path::new(grant["path"].as_str().unwrap_or(""));
                    let mut file = open_ai_authorized_file(path)?;
                    let meta = file.metadata().map_err(|_| {
                        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "附件状态不可读")
                    })?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos().to_string());
                    if !meta.is_file()
                        || grant["size"].as_u64() != Some(meta.len())
                        || modified.as_deref() != grant["modified"].as_str()
                    {
                        return Err(fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "附件已变更，请移除后重新授权",
                        ));
                    }
                    let (query, options) = search_options_from_arguments(&arguments_json)?;
                    let delimited = ai_file_delimited_format(path);
                    let result = if let Some(delimiter) = delimited {
                        search_delimited_content(file, &query, delimiter, options, &ctx.cancel)?
                    } else {
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        file.read_to_end(&mut bytes).map_err(|_| {
                            fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "附件搜索读取失败",
                            )
                        })?;
                        let text = String::from_utf8(bytes).map_err(|_| {
                            fluxdb_core::Error::new(
                                fluxdb_core::ErrorKind::Internal,
                                "附件不是 UTF-8 文本，无法按行搜索",
                            )
                        })?;
                        search_text_content(&text, &query, options, &ctx.cancel)?
                    };
                    let mut payload =
                        with_file_read_hints(search_match_json(&result), delimited.is_some());
                    payload["file_ref"] = serde_json::json!(id);
                    payload["content_type"] = serde_json::json!(if delimited.is_some() {
                        "delimited"
                    } else {
                        "text"
                    });
                    payload["scope"] = serde_json::json!("仅本轮已授权附件");
                    return Ok(payload);
                }
                if self.descriptor.id == "file.search" {
                    let query = args["query"].as_str().unwrap_or("").to_lowercase();
                    return Ok(
                        serde_json::json!({"files":self.references.iter().filter(|r|r.kind=="file"&&r.label.to_lowercase().contains(&query)).take(16).map(|r|serde_json::json!({"file_ref":r.id,"label":r.label,"summary":r.summary})).collect::<Vec<_>>(),"scope":"仅已授权附件"}),
                    );
                }
                let id = args["file_ref"].as_str().unwrap_or("");
                let reference = self
                    .references
                    .iter()
                    .find(|r| r.id == id && r.kind == "file")
                    .ok_or_else(|| {
                        fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "附件未授权或已移除",
                        )
                    })?;
                let grant: serde_json::Value =
                    serde_json::from_str(&reference.source).map_err(|_| {
                        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "附件授权损坏")
                    })?;
                let path = std::path::Path::new(grant["path"].as_str().unwrap_or(""));
                let mut file = open_ai_authorized_file(path)?;
                let meta = file.metadata().map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "附件状态不可读")
                })?;
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos().to_string());
                if !meta.is_file()
                    || grant["size"].as_u64() != Some(meta.len())
                    || modified.as_deref() != grant["modified"].as_str()
                {
                    return Err(fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "附件已变更，请移除后重新授权",
                    ));
                }
                let offset = args["offset"].as_u64().unwrap_or(0);
                let limit = args["max_bytes"].as_u64().unwrap_or(16384).min(65536);
                // 二进制判定读当前段窗口即可；CSV 则整段交给记录解析，不按字节切。
                use std::io::{Read, Seek};
                file.seek(std::io::SeekFrom::Start(offset)).map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "附件偏移无效")
                })?;
                let mut bytes = Vec::new();
                file.take(limit).read_to_end(&mut bytes).map_err(|_| {
                    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "附件读取失败")
                })?;
                if ctx.cancel.is_cancelled() {
                    return Err(fluxdb_core::Error::new(
                        fluxdb_core::ErrorKind::Internal,
                        "文件读取已取消",
                    ));
                }
                // 是否二进制以授权时的头部取样为准（grant.binary），不能用当前段判定：
                // offset 落在一个多字节字符中间时，本段必然不是合法 UTF-8，会被误判成二进制。
                // 旧快照没有该字段时才回退到头部取样，同样不看当前段。
                let binary = match grant["binary"].as_bool() {
                    Some(value) => value,
                    None => {
                        let head_file = open_ai_authorized_file(path)?;
                        let mut head = Vec::new();
                        head_file.take(8192).read_to_end(&mut head).ok();
                        ai_bytes_are_binary(&head)
                    }
                };
                // 二进制不回传原始字节：只给类型、大小与可打印预览，避免把任意载荷塞进模型上下文。
                if binary {
                    return Ok(
                        serde_json::json!({"file_ref":id,"content_type":"binary","size":meta.len(),"offset":offset,"preview":ai_binary_preview(&bytes),"note":"二进制附件不返回正文；如需内容请由用户提供文本版本"}),
                    );
                }
                // CSV/TSV 走记录分段，避免按字节续读切坏引号与嵌入换行结构。
                if ai_file_delimited_format(path).is_some() {
                    let max_records = args["max_records"].as_u64().unwrap_or(64).clamp(1, 256);
                    let segment = ai_read_csv_segment(
                        path,
                        open_ai_authorized_file(path)?,
                        offset,
                        max_records as usize,
                        meta.len(),
                    )?;
                    if ctx.cancel.is_cancelled() {
                        return Err(fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "文件读取已取消",
                        ));
                    }
                    return Ok(
                        serde_json::json!({"file_ref":id,"content_type":"csv","offset":offset,"records":segment.value["records"],"record_count":segment.value["record_count"],"next_offset":segment.next_offset,"truncated":segment.value["truncated"]}),
                    );
                }
                let segment = ai_read_text_segment(
                    open_ai_authorized_file(path)?,
                    offset,
                    limit,
                    meta.len(),
                )?;
                let mut value = segment.value;
                value["file_ref"] = serde_json::json!(id);
                Ok(value)
            })();
            let (status, value, error) = match result {
                Ok(v) => (fluxdb_core::ToolReceiptStatus::Succeeded, v, None),
                Err(e) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error":e.message}),
                    Some("file_unavailable".into()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: value.to_string(),
                error_code: error,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: ai_chat_now(),
            })
        })
    }
}
pub fn add_ai_file_tools(
    catalog: &mut ToolCatalog,
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::Result<()> {
    if !with_ai_storage(storage, ai_files_enabled)? {
        return Ok(());
    }
    let raw = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)
    })?;
    let snapshot: serde_json::Value =
        serde_json::from_str(&raw.runtime_snapshot_json).map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "任务附件快照损坏")
        })?;
    let references = snapshot["file_grants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(AiContextReference::parse)
        .collect::<Vec<_>>();
    if references.is_empty() {
        return Ok(());
    }
    for (id, desc, schema) in [
        (
            "file.read",
            "按 opaque FileRef 分段读取已授权附件：文本按字节、CSV/TSV 按记录、二进制只返回类型与预览",
            serde_json::json!({"type":"object","properties":{"file_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":65536},"max_records":{"type":"integer","minimum":1,"maximum":256,"description":"CSV/TSV 每次返回的记录数上限"}},"required":["file_ref"],"additionalProperties":false}),
        ),
        (
            "file.search",
            "搜索本会话已授权附件，不扫描未授权目录",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"additionalProperties":false}),
        ),
        (
            "file.search_content",
            "在本轮已授权附件正文中搜索字面量，返回字节偏移、行号、上下文片段与续读位置；不扫描未授权路径",
            ai_search_tool_schema(Some((
                "file_ref",
                "已授权附件的 FileRef，来自 file.search 返回结果",
                true,
            ))),
        ),
    ] {
        let descriptor = fluxdb_core::ToolDescriptor {
            id: id.into(),
            description: desc.into(),
            version: 1,
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: schema.to_string(),
        };
        // 分段内容与原续读位置必须一起交付，使用独立读取预算。
        catalog.add_tool_with_shape(
            Box::new(AiFileTool {
                descriptor,
                references: references.clone(),
                storage: storage.clone(),
            }),
            ToolOutputShape::Read,
        )?;
    }
    Ok(())
}
