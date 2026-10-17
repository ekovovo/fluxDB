fn reference_json_error(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::Internal, format!("引用载荷无效：{error}"))
}

// 草稿单项上限 8 KiB：给 JSON 转义、身份及摘要留出空间，不全量搬入结果。
fn reference_excerpt(text: &str) -> BoundedExcerpt {
    bound_excerpt(
        text,
        ContextLimits {
            max_excerpt_bytes: 1024,
            max_excerpt_lines: 40,
            head_ratio: 0.6,
        },
    )
}

fn reference_page_excerpt(page: &fluxdb_core::DataPage) -> (String, bool) {
    let mut truncated = page.has_more || page.rows.len() > 10 || page.columns.len() > 32;
    let columns = page
        .columns
        .iter()
        .take(32)
        .map(|column| {
            truncated |= column.name.len() > 128;
            column.name.chars().take(128).collect::<String>()
        })
        .collect::<Vec<_>>();
    let rows = page
        .rows
        .iter()
        .take(10)
        .map(|row| {
            truncated |= row.values.len() > 32;
            row.values
                .iter()
                .take(32)
                .map(|cell| match cell {
                    fluxdb_core::CellValue::Bytes(bytes) => {
                        truncated = true;
                        serde_json::json!({"binary_bytes": bytes.len(), "body_omitted": true})
                    }
                    fluxdb_core::CellValue::Text(text) | fluxdb_core::CellValue::Json(text) => {
                        truncated |= text.chars().count() > 128;
                        serde_json::json!(text.chars().take(128).collect::<String>())
                    }
                    other => serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({"columns": columns, "rows": rows, "loaded_rows": page.rows.len(),
        "offset": page.offset, "limit": page.limit, "truncated": truncated})
        .to_string(),
        truncated,
    )
}

impl AppController {
    /// 后台检查草稿引用；返回逐项说明，不能把检查失败当作有效或无匹配。
    pub fn ai_reference_errors(&self) -> BTreeMap<String, String> {
        let references = &self.state.ai_conversation.context_references;
        let mut errors: BTreeMap<String, String> = references
            .iter()
            .filter_map(|reference| {
                self.validate_ai_reference(reference)
                    .err()
                    .map(|error| (reference.id.clone(), error.message))
            })
            .collect();
        if let Err(error) = self.referenced_execution_target(references) {
            for reference in references
                .iter()
                .filter(|reference| ai_reference_target(reference).ok().flatten().is_some())
            {
                errors
                    .entry(reference.id.clone())
                    .or_insert_with(|| error.message.clone());
            }
        }
        errors
    }

    fn validate_ai_reference(&self, reference: &AiContextReference) -> fluxdb_core::Result<()> {
        if !self.ai_reference_is_valid(reference) {
            return Err(Error::new(
                ErrorKind::Internal,
                "引用定位或连接已失效，请移除后重新选择",
            ));
        }
        match reference.kind.as_str() {
            "table" | "view" => {
                let path: fluxdb_core::ObjectPath =
                    serde_json::from_str(&reference.source).map_err(reference_json_error)?;
                let config = &self
                    .state
                    .connections
                    .iter()
                    .find(|connection| connection.config.id == path.connection_id)
                    .ok_or_else(|| Error::new(ErrorKind::Internal, "引用连接已移除"))?
                    .config;
                if dialect_for(config.kind).is_some() {
                    self.validate_ai_database_resource(config)?;
                    let connector = fluxdb_connectors::connector_for(config)?;
                    let tables = connector
                        .list_completion_tables(
                            path.database.as_deref(),
                            path.schema.as_deref(),
                            &path.name,
                            100,
                        )
                        .map_err(|_| {
                            Error::new(ErrorKind::Internal, "引用对象校验失败，请确认连接并刷新")
                        })?;
                    if !tables.iter().any(|table| {
                        table.name == path.name
                            && table.kind == path.kind
                            && path
                                .schema
                                .as_deref()
                                .is_none_or(|schema| table.schema.as_deref() == Some(schema))
                    }) {
                        return Err(Error::new(
                            ErrorKind::Internal,
                            if tables.len() >= 100 {
                                "引用对象未能在有界检索内确认，请刷新后重新选择"
                            } else {
                                "引用对象已删除或改名，请重新选择"
                            },
                        ));
                    }
                }
            }
            "saved_query" => {
                self.saved_reference_text(reference)?;
            }
            "file" => {
                self.ai_storage()?.with_ai_connection(ensure_ai_files_enabled)?;
                let grant: serde_json::Value =
                    serde_json::from_str(&reference.source).map_err(reference_json_error)?;
                let path = grant["path"]
                    .as_str()
                    .ok_or_else(|| Error::new(ErrorKind::Internal, "附件授权损坏"))?;
                let authorized =
                    authorize_ai_file(std::path::Path::new(path), self.ai_storage()?.root_dir())?;
                if authorized.source != reference.source {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "附件已变更，请移除后重新授权",
                    ));
                }
            }
            "selection" | "result_set" => {
                let source: serde_json::Value =
                    serde_json::from_str(&reference.source).map_err(reference_json_error)?;
                if source["text"].as_str().is_none() {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "引用快照不可用，请重新选择",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn saved_reference_text(&self, reference: &AiContextReference) -> fluxdb_core::Result<String> {
        let source: serde_json::Value =
            serde_json::from_str(&reference.source).map_err(reference_json_error)?;
        let saved = self
            .ai_storage()?
            .load_saved_queries()?
            .into_iter()
            .find(|saved| {
                Some(saved.id) == source["query_id"].as_u64()
                    && Some(saved.connection_id.0) == source["connection_id"].as_u64()
                    && saved.database.as_deref() == source["database"].as_str()
                    && saved.schema.as_deref() == source["schema"].as_str()
                    && Some(source_hash(&[&saved.text]).as_str()) == source["fingerprint"].as_str()
            })
            .ok_or_else(|| Error::new(ErrorKind::Internal, "保存查询已删除或修改，请重新选择"))?;
        Ok(saved.text)
    }

    /// 详情只留在宿主授权快照；公开上下文仅发 ID、摘要和定位。
    fn ai_reference_detail_grants(
        &self,
        references: &[AiContextReference],
    ) -> fluxdb_core::Result<Vec<serde_json::Value>> {
        let mut grants = Vec::new();
        for reference in references {
            self.validate_ai_reference(reference)?;
            let text = match reference.kind.as_str() {
                "saved_query" => self.saved_reference_text(reference)?,
                "selection" | "result_set" => {
                    let source: serde_json::Value =
                        serde_json::from_str(&reference.source).map_err(reference_json_error)?;
                    source["text"].as_str().unwrap_or_default().to_string()
                }
                _ => continue,
            };
            let excerpt = reference_excerpt(&text);
            let source: serde_json::Value =
                serde_json::from_str(&reference.source).map_err(reference_json_error)?;
            grants.push(serde_json::json!({"id": reference.id, "kind": reference.kind,
                "text": excerpt.text, "truncated": excerpt.truncated || source["truncated"].as_bool().unwrap_or(false)}));
        }
        Ok(grants)
    }
}

struct AiReferenceReadTool {
    descriptor: fluxdb_core::ToolDescriptor,
    grants: Vec<serde_json::Value>,
    session_id: String,
    turn_id: String,
}

impl fluxdb_core::AiTool for AiReferenceReadTool {
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
            let result = (|| {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::new(ErrorKind::Internal, "引用读取已取消"));
                }
                if ctx.session_id != self.session_id || ctx.turn_id != self.turn_id {
                    return Err(Error::new(ErrorKind::Internal, "引用不属于本轮任务"));
                }
                let args: serde_json::Value =
                    serde_json::from_str(&arguments_json).map_err(reference_json_error)?;
                let id = args["reference_id"].as_str().unwrap_or_default();
                let grant = self
                    .grants
                    .iter()
                    .find(|grant| grant["id"].as_str() == Some(id))
                    .ok_or_else(|| Error::new(ErrorKind::Internal, "引用未授权或不属于本轮"))?;
                let text = grant["text"].as_str().unwrap_or_default();
                let offset =
                    usize::try_from(args["offset"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX);
                if offset > text.len() || !text.is_char_boundary(offset) {
                    return Err(Error::new(ErrorKind::Internal, "引用偏移不在 UTF-8 边界"));
                }
                let limit = args["max_bytes"].as_u64().unwrap_or(1024).clamp(1, 4096) as usize;
                let mut end = text.len().min(offset.saturating_add(limit));
                while end > offset && !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end == offset && offset < text.len() {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "读取长度不足一个 UTF-8 字符",
                    ));
                }
                Ok(
                    serde_json::json!({"reference_id": id, "kind": grant["kind"], "text": &text[offset..end],
                    "next_offset": end, "has_more": end < text.len(), "truncated": grant["truncated"],
                    "untrusted": true, "scope": "用户已选择的有界快照，不是实时全量结果"}),
                )
            })();
            let (status, result_json, error_code) = match result {
                Ok(value) => (
                    fluxdb_core::ToolReceiptStatus::Succeeded,
                    value.to_string(),
                    None,
                ),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({"error": error.message}).to_string(),
                    Some("reference_unavailable".into()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json,
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: ai_chat_now(),
            })
        })
    }
}

pub fn add_ai_reference_tools(
    catalog: &mut ToolCatalog,
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
) -> fluxdb_core::Result<()> {
    let turn = storage.with_ai_connection(|conn| {
        fluxdb_storage::ai_platform::sessions::load_turn(conn, &job.turn_id)
    })?;
    let snapshot: serde_json::Value =
        serde_json::from_str(&turn.runtime_snapshot_json).map_err(reference_json_error)?;
    let grants = snapshot["reference_grants"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if grants.is_empty() {
        return Ok(());
    }
    // 读取的是用户选中的原文片段，内容本身就是产物：声明 Opaque 让兜底裁剪
    // 不去猜结构、按"缩小读取范围"提示续读，而不是丢字段。
    catalog.add_tool_with_shape(Box::new(AiReferenceReadTool {
        descriptor: fluxdb_core::ToolDescriptor {
            id: "context.read".into(),
            version: 1,
            risk: fluxdb_core::ToolRisk::Read,
            description: "按引用 ID 分段读取用户显式选择的保存查询、选区或结果页快照；不执行 SQL"
                .into(),
            input_schema_json: serde_json::json!({"type":"object","properties":{
                "reference_id":{"type":"string"}, "offset":{"type":"integer","minimum":0},
                "max_bytes":{"type":"integer","minimum":1,"maximum":4096}
            },"required":["reference_id"],"additionalProperties":false})
            .to_string(),
        },
        grants,
        session_id: job.session_id.clone(),
        turn_id: job.turn_id.clone(),
    }), ToolOutputShape::Read)
}
