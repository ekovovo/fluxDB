// 内置 BI 产物编排：来源复核、草稿编辑与快照提交。
/// 内置 BI 产物插件：把声明式 spec 编译成受校验的产物，并提交给宿主 renderer。
pub struct BuiltinBiProducer {
    descriptor: ArtifactProducerDescriptor,
    storage: fluxdb_storage::FileStorage,
    session_id: String,
    turn_id: String,
    message_id: String,
    results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
    renderers: std::collections::BTreeMap<String, std::sync::Arc<dyn ArtifactRenderer>>,
    /// 渲染快照的受控根目录（`ai-artifacts`）。
    artifact_root: std::path::PathBuf,
}

impl BuiltinBiProducer {
    pub fn new(
        storage: fluxdb_storage::FileStorage,
        session_id: String,
        turn_id: String,
        message_id: String,
        results: std::sync::Arc<std::sync::Mutex<ResultStore>>,
    ) -> Self {
        let artifact_root = storage.root_dir().join("ai-artifacts");
        let mut renderers: std::collections::BTreeMap<
            String,
            std::sync::Arc<dyn ArtifactRenderer>,
        > = std::collections::BTreeMap::new();
        for id in [
            BI_REPORT_RENDERER_ID,
            BI_CHART_RENDERER_ID,
            BI_TABLE_RENDERER_ID,
        ] {
            renderers.insert(
                id.to_string(),
                std::sync::Arc::new(BuiltinMarkdownRenderer { id }),
            );
        }
        Self {
            descriptor: ArtifactProducerDescriptor {
                instance_id: BI_INSTANCE_ID,
                renderers: renderers.keys().cloned().collect(),
                kinds: vec![
                    fluxdb_core::ArtifactKind::Report,
                    fluxdb_core::ArtifactKind::Chart,
                    fluxdb_core::ArtifactKind::Table,
                ],
                max_points: fluxdb_core::ARTIFACT_MAX_POINTS,
            },
            storage,
            session_id,
            turn_id,
            message_id,
            results,
            renderers,
            artifact_root,
        }
    }

    /// 替换/追加 renderer（第三方可信实现或测试注入）。
    pub fn with_renderer(mut self, renderer: std::sync::Arc<dyn ArtifactRenderer>) -> Self {
        self.renderers.insert(renderer.id().to_string(), renderer);
        self.descriptor.renderers = self.renderers.keys().cloned().collect();
        self
    }

    /// 卸载全部 renderer：模拟"宿主没有可用 renderer"的可见不支持。
    pub fn without_renderers(mut self) -> Self {
        self.renderers.clear();
        self.descriptor.renderers.clear();
        self
    }

    /// 从本轮结果仓库解析句柄；解析不到就是"来源不可用"，不能靠猜。
    fn facts_for_handle(&self, declared: &str) -> Result<ResultFacts, ArtifactError> {
        let store = self
            .results
            .lock()
            .map_err(|_| ArtifactError::new("result_store_unavailable", "结果仓库不可用"))?;
        store.facts(declared).map_err(|error| {
            ArtifactError::new(
                "source_unavailable",
                format!("来源句柄不可解析（{}）：{}", error.code, error.message),
            )
        })
    }

    fn source_data(&self, declared: &str) -> Result<ArtifactSourceData, ArtifactError> {
        let facts = self.facts_for_handle(declared)?;
        if facts.result.session_id != self.session_id || facts.result.turn_id != self.turn_id {
            return Err(ArtifactError::new("source_unavailable", "来源不属于本任务"));
        }
        let result_id = ai_result_row_id(&self.turn_id, &facts.result.call_id);
        let (call, snapshot_at_ms) = with_ai_storage(&self.storage, |conn| {
            let (session, turn, source_call, status, created_at_ms): (String, String, String, String, i64) = conn
                .query_row(
                    "SELECT session_id,turn_id,source_call_id,status,created_at_ms FROM ai_results WHERE id=?1",
                    [&result_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .map_err(|_| Error::new(ErrorKind::Internal, "来源结果不可用"))?;
            let call = fluxdb_storage::ai_platform::sessions::load_tool_call(conn, &source_call)?;
            if session != self.session_id || turn != self.turn_id || status != "available"
                || call.session_id != session || call.turn_id != turn
                || call.status != "succeeded" || call.provider_call_id != facts.result.call_id
                || call.tool_id != facts.result.tool_id
            {
                return Err(Error::new(ErrorKind::Internal, "来源归属或执行状态不符"));
            }
            Ok((call, created_at_ms))
        }).map_err(|error| ArtifactError::new("source_unavailable", error.message))?;
        let receipt: serde_json::Value =
            serde_json::from_str(call.receipt_json.as_deref().unwrap_or(""))
                .map_err(|_| ArtifactError::new("source_unavailable", "来源回执损坏"))?;
        let columns: Vec<String> = serde_json::from_value(receipt["columns"].clone())
            .map_err(|_| ArtifactError::new("source_unavailable", "来源缺少列结构"))?;
        let mut unique = BTreeSet::new();
        if columns.iter().any(|column| !unique.insert(column)) {
            return Err(ArtifactError::new(
                "ambiguous_field",
                "来源列名重复，请通过 SQL 别名消除歧义",
            ));
        }
        // 有界读取真实快照（含 spill），不会重新执行 SQL；损坏必须显式失败。
        let page = self
            .results
            .lock()
            .map_err(|_| ArtifactError::new("result_store_unavailable", "结果仓库不可用"))?
            .read_page(
                declared,
                None,
                fluxdb_core::ARTIFACT_MAX_POINTS,
                BI_SNAPSHOT_MAX_BYTES,
            )
            .map_err(|error| ArtifactError::new("source_unavailable", error.message))?;
        let sample = page
            .items
            .into_iter()
            .map(|row| match row.as_array() {
                Some(cells) => serde_json::Value::Object(
                    columns.iter().cloned().zip(cells.iter().cloned()).collect(),
                ),
                None => row,
            })
            .collect();
        let truncated = page.truncated || receipt["truncated"].as_bool().unwrap_or(true);
        Ok(ArtifactSourceData {
            summary: ArtifactSourceSummary {
                declared_ref: Some(declared.to_string()),
                result_id,
                tool_call_id: Some(call.id),
                snapshot_at_ms,
                rows: facts.rows,
                truncated,
            },
            columns,
            sample,
        })
    }

    /// 块校验：来源必须已声明且可解析，字段必须真实存在、图表 `y` 必须是数值列。
    fn validate_block(
        &self,
        block: &fluxdb_core::ArtifactBlock,
        sources: &[ArtifactSourceData],
    ) -> Result<(), ArtifactError> {
        match block {
            fluxdb_core::ArtifactBlock::Chart {
                result_ref, x, y, ..
            } => {
                let source = source_by_ref(sources, result_ref)?;
                let (_, numeric_fields) = result_field_facts(&source.sample);
                for field in [x, y] {
                    if !source.columns.contains(field) {
                        return Err(ArtifactError::new(
                            "unknown_field",
                            format!("来源 {result_ref} 的结果里没有字段 {field}"),
                        ));
                    }
                }
                if !numeric_fields.contains(y) {
                    return Err(ArtifactError::new(
                        "non_numeric_field",
                        format!("图表 y 字段 {y} 不是数值列，不能作为指标"),
                    ));
                }
                Ok(())
            }
            fluxdb_core::ArtifactBlock::Table {
                result_ref,
                columns,
            } => {
                let source = source_by_ref(sources, result_ref)?;
                for column in columns {
                    if !source.columns.contains(column) {
                        return Err(ArtifactError::new(
                            "unknown_field",
                            format!("来源 {result_ref} 的结果里没有字段 {column}"),
                        ));
                    }
                }
                Ok(())
            }
            fluxdb_core::ArtifactBlock::Note { .. } => Ok(()),
        }
    }

    /// 渲染失败/无法渲染时的可见解释：产物置 `failed`，来源结果一字不改。
    fn fail_artifact(
        &self,
        artifact_id: &str,
        spec: &fluxdb_core::ArtifactSpec,
        truncated: bool,
        source_count: usize,
        error: &ArtifactError,
    ) -> Result<(), ArtifactError> {
        let view = with_ai_storage(&self.storage, |conn| {
            let current = fluxdb_storage::ai_platform::artifacts::load_artifact(conn, artifact_id)?;
            fluxdb_storage::ai_platform::artifacts::set_artifact_status(
                conn,
                ai_chat_now(),
                artifact_id,
                "failed",
                current.revision,
            )
        })?;
        // 失败也要在会话里留可见卡片（T040 据此显示"renderer 不可用"的解释）。
        self.append_card(&view, spec, truncated, source_count, Some(&error.code))
    }

    /// 写产物卡片：同一 artifact 复用同一片段 ID，finalize 更新而不是再插一张。
    fn append_card(
        &self,
        view: &fluxdb_storage::ai_platform::artifacts::ArtifactView,
        spec: &fluxdb_core::ArtifactSpec,
        truncated: bool,
        source_count: usize,
        failure_code: Option<&str>,
    ) -> Result<(), ArtifactError> {
        let payload = artifact_card_payload(view, spec, truncated, source_count, failure_code);
        let part_id = format!("{}-artifact-{}", self.message_id, view.id);
        let status = match view.status.as_str() {
            "ready" => fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
            "draft" | "staged" => fluxdb_storage::ai_platform::sessions::PartStatus::Pending,
            _ => fluxdb_storage::ai_platform::sessions::PartStatus::Error,
        };
        with_ai_storage(&self.storage, |conn| {
            fluxdb_storage::ai_platform::conversation::upsert_artifact_part(
                conn,
                ai_chat_now(),
                &self.session_id,
                &self.turn_id,
                &self.message_id,
                &part_id,
                status,
                &payload,
                &view.id,
            )
        })
        .map_err(|error| {
            tracing::warn!(artifact_id = %view.id, "产物卡片写入失败");
            ArtifactError::new("artifact_card_failed", error.message)
        })
    }

    fn outcome_from_view(
        &self,
        view: &fluxdb_storage::ai_platform::artifacts::ArtifactView,
        spec: &fluxdb_core::ArtifactSpec,
        sources: Vec<ArtifactSourceSummary>,
        truncated: bool,
        points: Vec<serde_json::Value>,
        file: Option<ArtifactFileFact>,
    ) -> ArtifactOutcome {
        ArtifactOutcome {
            artifact_id: view.id.clone(),
            status: view.status.clone(),
            revision: view.revision,
            kind: spec.kind.as_str().to_string(),
            title: spec.title.clone(),
            renderer: spec.renderer.clone(),
            metric: spec.metric.clone(),
            truncated,
            points,
            blocks: spec.blocks.clone(),
            sources,
            file,
        }
    }

    fn parse_spec(
        &self,
        view: &fluxdb_storage::ai_platform::artifacts::ArtifactView,
    ) -> Result<fluxdb_core::ArtifactSpec, ArtifactError> {
        serde_json::from_str::<fluxdb_core::ArtifactSpec>(&view.spec_json).map_err(|error| {
            ArtifactError::new("artifact_spec_damaged", format!("产物声明损坏: {error}"))
        })
    }

    fn load_view(
        &self,
        artifact_id: &str,
    ) -> Result<fluxdb_storage::ai_platform::artifacts::ArtifactView, ArtifactError> {
        let view = with_ai_storage(&self.storage, |conn| {
            fluxdb_storage::ai_platform::artifacts::load_artifact(conn, artifact_id)
        })
        .map_err(|error| ArtifactError::new("unknown_artifact", error.message))?;
        if view.session_id != self.session_id || view.turn_id != self.turn_id {
            return Err(ArtifactError::new("unknown_artifact", "产物不属于本任务"));
        }
        Ok(view)
    }
}

impl ArtifactProducer for BuiltinBiProducer {
    fn descriptor(&self) -> &ArtifactProducerDescriptor {
        &self.descriptor
    }

    fn create(
        &self,
        mut spec: fluxdb_core::ArtifactSpec,
        cancel: &fluxdb_core::CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError> {
        artifact_check_cancel(cancel)?;
        // 归一化可归一化写法（如 `UTC+08:00` → `+08:00`），落库 spec 与回执显示保持一致。
        spec.normalize()
            .map_err(|error| ArtifactError::new("invalid_spec", error.message))?;
        if spec.accessibility_description.trim().is_empty() {
            spec.accessibility_description = spec.title.clone();
        }
        if let Some(metric) = &mut spec.metric {
            if metric.assumption.is_none() {
                metric.assumption =
                    Some("未确认的口径假设；仅按来源原始字段展示，不视为业务事实".into());
            }
        }
        spec.validate_shape()
            .map_err(|error| ArtifactError::new("invalid_spec", error.message))?;
        if !self.descriptor.kinds.contains(&spec.kind) {
            return Err(ArtifactError::new(
                "unsupported_kind",
                format!("不支持产物类型 {}", spec.kind.as_str()),
            ));
        }
        let renderer = self.renderers.get(&spec.renderer).ok_or_else(|| {
            ArtifactError::new(
                "renderer_unavailable",
                format!(
                    "没有可用的 renderer {}（当前可用：{}）",
                    spec.renderer,
                    self.descriptor.renderers.join(", ")
                ),
            )
        })?;
        // T039 只接受结果句柄来源：工具调用来源没有可解析的数据行，不能作为图表/表格依据。
        let mut sources = Vec::with_capacity(spec.sources.len());
        let mut resolved = Vec::with_capacity(spec.sources.len());
        for source in &spec.sources {
            artifact_check_cancel(cancel)?;
            let declared = source.result_ref.as_deref().ok_or_else(|| {
                ArtifactError::new(
                    "source_unavailable",
                    "产物来源必须给出 result_ref（结果句柄）",
                )
            })?;
            let data = self.source_data(declared)?;
            if source
                .tool_call_ref
                .as_ref()
                .is_some_and(|reference| data.summary.tool_call_id.as_ref() != Some(reference))
            {
                return Err(ArtifactError::new(
                    "source_mismatch",
                    "ToolCallRef 与实际产出调用不一致",
                ));
            }
            resolved.push(
                fluxdb_storage::ai_platform::artifacts::ArtifactSourceInput {
                    declared_ref: Some(declared.to_string()),
                    result_id: Some(data.summary.result_id.clone()),
                    tool_call_id: data.summary.tool_call_id.clone(),
                },
            );
            sources.push(data);
        }
        let mut declared_blocks: Vec<fluxdb_core::ArtifactBlock> =
            Vec::with_capacity(spec.blocks.len());
        for block in &spec.blocks {
            self.validate_block(block, &sources)?;
            if declared_blocks.contains(block) {
                return Err(duplicate_block_error());
            }
            declared_blocks.push(block.clone());
        }
        let artifact_id = format!("{}-artifact-{}", self.turn_id, uuid::Uuid::new_v4());
        artifact_check_cancel(cancel)?;
        let source_count = resolved.len();
        let view = with_ai_storage(&self.storage, |conn| {
            fluxdb_storage::ai_platform::artifacts::create_artifact(
                conn,
                ai_chat_now(),
                &artifact_id,
                &self.session_id,
                &self.turn_id,
                renderer.version(),
                &spec,
                &resolved,
            )
        })?;
        let mut truncated = sources.iter().any(|source| source.summary.truncated);
        let summaries = sources
            .iter()
            .map(|source| source.summary.clone())
            .collect();
        let points = if spec.blocks.is_empty() {
            Vec::new()
        } else {
            // 创建时即回执真实点，让模型确认图表数据来自结果而不是自己编的数字。
            let mut points = Vec::new();
            for block in &spec.blocks {
                if let fluxdb_core::ArtifactBlock::Chart {
                    result_ref, x, y, ..
                } = block
                {
                    let source = source_by_ref(&sources, result_ref)?;
                    let (block_points, cut) =
                        chart_points(source, x, y, self.descriptor.max_points);
                    truncated |= cut;
                    points.extend(block_points);
                    break;
                }
            }
            points
        };
        self.append_card(&view, &spec, truncated, source_count, None)?;
        Ok(self.outcome_from_view(&view, &spec, summaries, truncated, points, None))
    }

    fn add_block(
        &self,
        artifact_id: &str,
        block: fluxdb_core::ArtifactBlock,
        cancel: &fluxdb_core::CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError> {
        artifact_check_cancel(cancel)?;
        let view = self.load_view(artifact_id)?;
        if view.status != "draft" {
            return Err(ArtifactError::new(
                "artifact_not_draft",
                format!("产物已完成（{}），不能再增加块", view.status),
            ));
        }
        let mut spec = self.parse_spec(&view)?;
        if spec.blocks.len() >= fluxdb_core::ARTIFACT_MAX_BLOCKS {
            return Err(ArtifactError::new(
                "too_many_blocks",
                format!("产物块数已达上限 {}", fluxdb_core::ARTIFACT_MAX_BLOCKS),
            ));
        }
        let sources = self.sources_for(&view)?;
        self.validate_block(&block, &sources)?;
        // 已声明过的块不再重复追加：返回受控错误而不是静默成功又渲染一份。
        if spec.blocks.contains(&block) {
            return Err(duplicate_block_error());
        }
        spec.blocks.push(block);
        spec.validate_shape()
            .map_err(|error| ArtifactError::new("invalid_spec", error.message))?;
        // 来源仍是创建时解析过的行 ID：这里只按序回填，不重新解析句柄。
        let stored = with_ai_storage(&self.storage, |conn| {
            fluxdb_storage::ai_platform::artifacts::list_artifact_sources(conn, artifact_id)
        })?;
        let resolved = stored
            .iter()
            .map(
                |source| fluxdb_storage::ai_platform::artifacts::ArtifactSourceInput {
                    declared_ref: declared_handle(&source.metadata_json),
                    result_id: source.result_id.clone(),
                    tool_call_id: source.tool_call_id.clone(),
                },
            )
            .collect::<Vec<_>>();
        let updated = with_ai_storage(&self.storage, |conn| {
            if cancel.is_cancelled() {
                return Err(Error::new(ErrorKind::Cancelled, "产物操作已取消"));
            }
            fluxdb_storage::ai_platform::artifacts::replace_artifact_spec(
                conn,
                ai_chat_now(),
                artifact_id,
                &spec,
                &resolved,
                view.revision,
            )
        })?;
        let truncated = sources.iter().any(|source| source.summary.truncated);
        let summaries = sources
            .iter()
            .map(|source| source.summary.clone())
            .collect();
        self.append_card(&updated, &spec, truncated, sources.len(), None)?;
        Ok(self.outcome_from_view(&updated, &spec, summaries, truncated, Vec::new(), None))
    }

    fn finalize(
        &self,
        artifact_id: &str,
        cancel: &fluxdb_core::CancelFlag,
    ) -> Result<ArtifactOutcome, ArtifactError> {
        artifact_check_cancel(cancel)?;
        let view = self.load_view(artifact_id)?;
        if view.status != "draft" && view.status != "staged" {
            return Err(ArtifactError::new(
                "artifact_not_open",
                format!("产物状态 {} 不能再完成", view.status),
            ));
        }
        let spec = self.parse_spec(&view)?;
        spec.validate_shape()
            .map_err(|error| ArtifactError::new("invalid_spec", error.message))?;
        // 来源仍可用：句柄必须还能从本轮结果仓库解析，字段也仍然存在。
        let sources = match self.sources_for(&view) {
            Ok(sources) => sources,
            Err(error) => {
                self.fail_artifact(artifact_id, &spec, true, spec.sources.len(), &error)?;
                return Err(error);
            }
        };
        if spec.blocks.is_empty() {
            return Err(ArtifactError::new(
                "empty_artifact",
                "产物没有内容块，不能完成",
            ));
        }
        for block in &spec.blocks {
            if let Err(error) = self.validate_block(block, &sources) {
                // 可修正的声明问题不改产物状态，让模型补块/换来源后重试。
                return Err(error);
            }
        }
        let source_count = sources.len();
        let truncated = sources.iter().any(|source| source.summary.truncated);
        let summaries: Vec<ArtifactSourceSummary> = sources
            .iter()
            .map(|source| source.summary.clone())
            .collect();
        let Some(renderer) = self.renderers.get(&spec.renderer).cloned() else {
            let error = ArtifactError::new(
                "renderer_unavailable",
                format!("没有可用的 renderer {}", spec.renderer),
            );
            self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
            return Err(error);
        };
        let rendered = match renderer.render(&spec, &sources) {
            Ok(rendered) => rendered,
            Err(error) => {
                self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
                return Err(error);
            }
        };
        artifact_check_cancel(cancel)?;
        if rendered.content.len() > BI_SNAPSHOT_MAX_BYTES {
            let error = ArtifactError::new("snapshot_too_large", "renderer 输出超过宿主上限");
            self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
            return Err(error);
        }
        // 保存机器可读的有界来源快照，历史报告无需重跑 SQL 或依赖进程内结果仓库。
        let snapshot = serde_json::json!({
            "schema_version": fluxdb_core::ARTIFACT_SCHEMA_VERSION,
            "spec": spec,
            "renderer_version": renderer.version(),
            "markdown": rendered.content,
            "truncated": truncated || rendered.truncated,
            "sources": sources.iter().map(|source| serde_json::json!({
                "result_ref": source.summary.declared_ref,
                "result_id": source.summary.result_id,
                "tool_call_id": source.summary.tool_call_id,
                "snapshot_at_ms": source.summary.snapshot_at_ms,
                "row_count": source.summary.rows,
                "truncated": source.summary.truncated,
                "columns": source.columns,
                "items": source.sample,
            })).collect::<Vec<_>>(),
        })
        .to_string();
        if snapshot.len() > BI_SNAPSHOT_MAX_BYTES {
            let error =
                ArtifactError::new("snapshot_too_large", "产物快照超过宿主上限，请缩小来源范围");
            self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
            return Err(error);
        }
        let file = match self.write_snapshot(&snapshot) {
            Ok(file) => file,
            Err(error) => {
                self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
                return Err(error);
            }
        };
        // 提交失败时清理本次快照；ready 后才解除删除保护。
        struct PendingSnapshot(Option<std::path::PathBuf>);
        impl Drop for PendingSnapshot {
            fn drop(&mut self) {
                if let Some(path) = &self.0 {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let mut pending = PendingSnapshot(Some(self.artifact_root.join(&file.relative_path)));
        artifact_check_cancel(cancel)?;
        // staged 表示"文件已写、尚未核对"；核对内容指纹后才置 ready。
        let staged = if view.status == "staged" {
            view
        } else {
            with_ai_storage(&self.storage, |conn| {
                fluxdb_storage::ai_platform::artifacts::set_artifact_status(
                    conn,
                    ai_chat_now(),
                    artifact_id,
                    "staged",
                    view.revision,
                )
            })?
        };
        let verified = match std::fs::read_to_string(self.artifact_root.join(&file.relative_path)) {
            Ok(content) => content,
            Err(error) => {
                let error = ArtifactError::new(
                    "artifact_write_failed",
                    format!("产物快照无法回读: {error}"),
                );
                self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
                return Err(error);
            }
        };
        if crate::source_hash(&[verified.as_str()]) != file.content_hash
            || verified.len() as i64 != file.byte_count
        {
            let error = ArtifactError::new("artifact_verify_failed", "产物快照内容核对不一致");
            self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
            return Err(error);
        }
        let ready = with_ai_storage(&self.storage, |conn| {
            if cancel.is_cancelled() {
                return Err(Error::new(ErrorKind::Cancelled, "产物操作已取消"));
            }
            let tx = conn
                .unchecked_transaction()
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            let ready = fluxdb_storage::ai_platform::artifacts::mark_artifact_ready_in_transaction(
                &tx,
                ai_chat_now(),
                artifact_id,
                &file.relative_path,
                file.byte_count,
                &file.content_hash,
                staged.revision,
            )?;
            let payload = artifact_card_payload(
                &ready,
                &spec,
                truncated || rendered.truncated,
                source_count,
                None,
            );
            fluxdb_storage::ai_platform::conversation::upsert_artifact_part_in_transaction(
                &tx,
                ai_chat_now(),
                &self.session_id,
                &self.turn_id,
                &self.message_id,
                &format!("{}-artifact-{}", self.message_id, ready.id),
                fluxdb_storage::ai_platform::sessions::PartStatus::Complete,
                &payload,
                &ready.id,
            )?;
            tx.commit()
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            Ok(ready)
        })
        .map_err(ArtifactError::from);
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                self.fail_artifact(artifact_id, &spec, truncated, source_count, &error)?;
                return Err(error);
            }
        };
        pending.0 = None;
        let truncated = truncated || rendered.truncated;
        Ok(self.outcome_from_view(
            &ready,
            &spec,
            summaries,
            truncated,
            rendered.points,
            Some(file),
        ))
    }
}

/// 每个有副作用的检查点都复核取消，不把开始时的检查当成永久许可。
fn artifact_check_cancel(cancel: &fluxdb_core::CancelFlag) -> Result<(), ArtifactError> {
    if cancel.is_cancelled() {
        Err(ArtifactError::new("cancelled", "产物操作已取消"))
    } else {
        Ok(())
    }
}

/// 完全相同的块没有信息增量（renderer 只会把同一份数据渲染两遍）：声明与追加走同一拒绝规则。
/// 提示模型以回执中的 `blocks` 为准，不要再追加同一块。
fn duplicate_block_error() -> ArtifactError {
    ArtifactError::new(
        "duplicate_block",
        "该块已在产物中存在，无需重复添加；请以回执中的 blocks 为准，如需不同维度请显式声明另一个字段或图表类型",
    )
}

impl BuiltinBiProducer {
    /// 从产物来源表重建来源快照（finalize/加块都要用真实结果复核，不能信声明）。
    fn sources_for(
        &self,
        view: &fluxdb_storage::ai_platform::artifacts::ArtifactView,
    ) -> Result<Vec<ArtifactSourceData>, ArtifactError> {
        let stored = with_ai_storage(&self.storage, |conn| {
            fluxdb_storage::ai_platform::artifacts::list_artifact_sources(conn, &view.id)
        })?;
        if stored.is_empty() {
            return Err(ArtifactError::new(
                "source_unavailable",
                "产物没有可用来源，不能完成",
            ));
        }
        let mut sources = Vec::with_capacity(stored.len());
        for row in &stored {
            let declared = declared_handle(&row.metadata_json).ok_or_else(|| {
                ArtifactError::new("source_unavailable", "产物来源缺少可解析的结果句柄")
            })?;
            let data = self.source_data(&declared)?;
            if row.result_id.as_ref() != Some(&data.summary.result_id)
                || row.tool_call_id != data.summary.tool_call_id
            {
                return Err(ArtifactError::new(
                    "source_mismatch",
                    "来源句柄与创建时版本不一致",
                ));
            }
            sources.push(data);
        }
        Ok(sources)
    }
}

/// 从来源行元数据里读出模型声明过的结果句柄。
fn declared_handle(metadata_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(metadata_json)
        .ok()?
        .get("result_ref")?
        .as_str()
        .map(str::to_owned)
}

/// 产物卡片只投影定位与状态，不复制来源业务行或文件载荷。
fn artifact_card_payload(
    view: &fluxdb_storage::ai_platform::artifacts::ArtifactView,
    spec: &fluxdb_core::ArtifactSpec,
    truncated: bool,
    source_count: usize,
    failure_code: Option<&str>,
) -> String {
    serde_json::json!({
            "text": format!("{} · {} · {} 个来源{}", spec.title, view.status, source_count,
                if truncated { " · 抽样/截断" } else { "" }),
            "artifact_id": view.id,
            "artifact_status": view.status,
            "kind": spec.kind.as_str(),
            "title": spec.title,
            "renderer": spec.renderer,
            "renderer_version": view.renderer_version,
            "revision": view.revision,
            "truncated": truncated,
            "source_count": source_count,
            "failure_code": failure_code,
    })
    .to_string()
}
