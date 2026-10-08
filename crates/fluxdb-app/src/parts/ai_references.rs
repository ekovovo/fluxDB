// 显式 @/附件引用：授权不扩大写权限，文件只读句柄、不接受模型任意路径。
#[derive(Clone, Debug, PartialEq)]
pub struct AiContextReference {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub source: String,
    pub summary: String,
}
impl AiContextReference {
    pub(crate) fn json(&self)->serde_json::Value {serde_json::json!({"id":self.id,"label":self.label,"kind":self.kind,"source":self.source,"summary":self.summary})}
    pub(crate) fn parse(value:&serde_json::Value)->Option<Self> {
        Some(Self{id:value["id"].as_str()?.into(),label:value["label"].as_str()?.into(),kind:value["kind"].as_str()?.into(),source:value["source"].as_str()?.into(),summary:value["summary"].as_str()?.into()})
    }
}
/// 路径来自用户文件选择器；敏感目录、非文本文件与 symlink 不授权。
pub fn authorize_ai_file(path:&std::path::Path,storage_root:&std::path::Path)->fluxdb_core::Result<AiContextReference> {
    let fail=|reason:&str|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,reason);
    if std::fs::symlink_metadata(path).map(|m|m.file_type().is_symlink()).unwrap_or(false) {return Err(fail("不允许链接附件"));}
    let path=path.canonicalize().map_err(|_|fail("附件不存在或无法读取"))?;
    let root=storage_root.canonicalize().unwrap_or_else(|_|storage_root.to_path_buf());
    if path.starts_with(root) || path.components().any(|component| {
        let name=component.as_os_str().to_string_lossy().to_lowercase();
        [".ssh",".aws",".codex",".env","config.toml"].contains(&name.as_str()) || name.contains("credential") || name.contains("secret")
    }) {return Err(fail("配置、凭据和私钥不能作为附件"));}
    let ext=path.extension().and_then(|e|e.to_str()).unwrap_or("").to_lowercase();
    if !["sql","txt","csv","tsv","json","md","log","yaml","yml"].contains(&ext.as_str()) {return Err(fail("仅支持文本附件，不能读取数据库、二进制或密钥文件"));}
    let file=open_ai_authorized_file(&path)?;
    let meta=file.metadata().map_err(|_|fail("无法读取附件信息"))?;
    if !meta.is_file() || meta.len()>16*1024*1024 {return Err(fail("附件必须是文件且不超过 16 MiB"));}
    let mut reference=AiContextReference{id:uuid::Uuid::new_v4().to_string(),label:path.file_name().unwrap_or_default().to_string_lossy().into_owned(),kind:"file".into(),source:path.to_string_lossy().into_owned(),summary:format!("文本文件 · {} 字节；按需分段读取",meta.len())};
    // source 只供宿主，运行快照只写 opaque id、标签和摘要。
    reference.source=serde_json::json!({"path":reference.source,"size":meta.len(),"modified":meta.modified().ok().and_then(|v|v.duration_since(std::time::UNIX_EPOCH).ok()).map(|v|v.as_nanos().to_string())}).to_string();
    Ok(reference)
}
/// 引用定位解析与身份校验共用同一路径，避免公开上下文和实际执行目标不一致。
fn ai_reference_target(reference: &AiContextReference) -> fluxdb_core::Result<Option<TargetRef>> {
    let invalid = || fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "数据库引用已失效，请移除后重新选择");
    let target = if reference.kind == "connection" {
        let id = reference.source.parse::<u64>().map_err(|_| invalid())?;
        if reference.id != format!("connection:{id}") { return Err(invalid()); }
        TargetRef { connection_id: Some(i64::try_from(id).map_err(|_| invalid())?), ..Default::default() }
    } else if matches!(reference.kind.as_str(), "database" | "schema" | "table" | "view" | "column" | "index" | "collection" | "redis_db" | "redis_key") {
        let path: fluxdb_core::ObjectPath = serde_json::from_str(&reference.source).map_err(|_| invalid())?;
        let expected_kind = match path.kind {
            fluxdb_core::ObjectKind::Database => "database",
            fluxdb_core::ObjectKind::Schema => "schema",
            fluxdb_core::ObjectKind::Table => "table",
            fluxdb_core::ObjectKind::View => "view",
            fluxdb_core::ObjectKind::Column => "column",
            fluxdb_core::ObjectKind::Index => "index",
            fluxdb_core::ObjectKind::Collection => "collection",
            fluxdb_core::ObjectKind::RedisDb => "redis_db",
            fluxdb_core::ObjectKind::RedisKey => "redis_key",
        };
        if reference.kind != expected_kind || reference.id != format!("object:{}", source_hash(&[&reference.source])) { return Err(invalid()); }
        let database = path.database.clone().or_else(|| (path.kind == fluxdb_core::ObjectKind::Database).then(|| path.name.clone()));
        let schema = path.schema.clone().or_else(|| (path.kind == fluxdb_core::ObjectKind::Schema).then(|| path.name.clone()));
        let redis_db = if matches!(path.kind, fluxdb_core::ObjectKind::RedisDb | fluxdb_core::ObjectKind::RedisKey) {
            Some(database.as_deref().unwrap_or(&path.name).parse::<u32>().map_err(|_| invalid())?)
        } else { None };
        TargetRef {
            connection_id: Some(i64::try_from(path.connection_id.0).map_err(|_| invalid())?),
            database, schema, redis_db,
            object: (!matches!(path.kind, fluxdb_core::ObjectKind::Database | fluxdb_core::ObjectKind::Schema | fluxdb_core::ObjectKind::RedisDb)).then_some(path.name),
        }
    } else { return Ok(None) };
    Ok(Some(target))
}
impl AppController {
    /// 显式数据库引用决定本轮目标；文件/知识引用不改变目标，也不授予写权限。
    fn referenced_execution_target(&self, references: &[AiContextReference]) -> fluxdb_core::Result<Option<TargetRef>> {
        let invalid = || fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "数据库引用已失效，请移除后重新选择");
        let mut targets = Vec::new();
        for reference in references {
            let Some(target) = ai_reference_target(reference)? else { continue };
            if !self.state.connections.iter().any(|connection| Some(connection.config.id.0) == target.connection_id.and_then(|id| u64::try_from(id).ok())) {
                return Err(invalid());
            }
            targets.push(target);
        }
        if targets.is_empty() { return Ok(None); }
        // 不同非空定位值不能合并，否则会悄悄选错连接或扩大数据库范围。
        let merge = |values: Vec<String>| -> fluxdb_core::Result<Option<String>> {
            let values = values.into_iter().collect::<std::collections::BTreeSet<_>>();
            if values.len() > 1 { return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "本轮引用跨多个连接、数据库或 schema；请保留同一执行目标的引用后发送")); }
            Ok(values.into_iter().next())
        };
        let connection_id = merge(targets.iter().filter_map(|target| target.connection_id.map(|id| id.to_string())).collect())?
            .and_then(|id| id.parse().ok());
        let database = merge(targets.iter().filter_map(|target| target.database.clone()).collect())?;
        let schema = merge(targets.iter().filter_map(|target| target.schema.clone()).collect())?;
        let objects = targets.iter().filter_map(|target| target.object.clone()).collect::<std::collections::BTreeSet<_>>();
        Ok(Some(TargetRef {
            connection_id, database, schema,
            redis_db: targets.iter().find_map(|target| target.redis_db),
            object: (objects.len() == 1).then(|| objects.into_iter().next()).flatten(),
        }))
    }

}

/// `@` 候选上限：浮层只渲染有限列表，关键字越具体结果越准确。
pub const AI_REFERENCE_CANDIDATE_LIMIT: usize = 20;

fn ai_database_kind_label(kind: fluxdb_core::DatabaseKind) -> &'static str {
    match kind {
        fluxdb_core::DatabaseKind::MySql => "MySQL",
        fluxdb_core::DatabaseKind::TiDb => "TiDB",
        fluxdb_core::DatabaseKind::Sqlite => "SQLite",
        fluxdb_core::DatabaseKind::MongoDb => "MongoDB",
        fluxdb_core::DatabaseKind::Redis => "Redis",
        fluxdb_core::DatabaseKind::Postgres => "PostgreSQL",
    }
}

fn ai_object_kind_label(kind: fluxdb_core::ObjectKind) -> &'static str {
    match kind {
        fluxdb_core::ObjectKind::Database => "数据库",
        fluxdb_core::ObjectKind::Schema => "Schema",
        fluxdb_core::ObjectKind::Table => "表",
        fluxdb_core::ObjectKind::View => "视图",
        fluxdb_core::ObjectKind::Column => "列",
        fluxdb_core::ObjectKind::Index => "索引",
        fluxdb_core::ObjectKind::Collection => "集合",
        fluxdb_core::ObjectKind::RedisDb => "Redis DB",
        fluxdb_core::ObjectKind::RedisKey => "Redis Key",
    }
}

impl AppController {
    /// `@` 候选检索：连接、库/schema、表/视图、知识条目。
    ///
    /// 只返回**摘要元数据**（稳定 ID、标签、来源、简述）；对象正文/文件内容/知识正文
    /// 一律不在这里展开，详情走受控工具按需读取。查询为空时返回各类前若干条。
    pub fn ai_reference_candidates(&self, query: &str) -> Vec<AiContextReference> {
        let query = query.trim_start_matches('@').to_lowercase();
        let mut candidates = Vec::new();
        for connection in &self.state.connections {
            candidates.push(AiContextReference {
                id: format!("connection:{}", connection.config.id.0),
                label: connection.config.name.clone(),
                kind: "connection".into(),
                source: connection.config.id.0.to_string(),
                summary: format!(
                    "{} 连接 · 只读上下文，不授予写权限",
                    ai_database_kind_label(connection.config.kind)
                ),
            });
            for object in &connection.objects {
                let kind = match object.path.kind {
                    fluxdb_core::ObjectKind::Database => "database",
                    fluxdb_core::ObjectKind::Schema => "schema",
                    fluxdb_core::ObjectKind::Table => "table",
                    fluxdb_core::ObjectKind::View => "view",
                    fluxdb_core::ObjectKind::RedisDb => "redis_db",
                    fluxdb_core::ObjectKind::RedisKey => "redis_key",
                    fluxdb_core::ObjectKind::Collection => "collection",
                    fluxdb_core::ObjectKind::Column => "column",
                    fluxdb_core::ObjectKind::Index => "index",
                };
                let source = serde_json::to_string(&object.path).unwrap_or_default();
                // 同名对象靠 连接 + 库 + schema + 名称 全路径区分，不靠裸名。
                candidates.push(AiContextReference {
                    id: format!("object:{}", source_hash(&[&source])),
                    label: object.path.name.clone(),
                    kind: kind.into(),
                    source,
                    summary: {
                        let mut location = connection.config.name.clone();
                        if let Some(database) = object.path.database.as_deref() {
                            location.push_str(" / ");
                            location.push_str(database);
                        }
                        if let Some(schema) = object.path.schema.as_deref() {
                            location.push_str(" / ");
                            location.push_str(schema);
                        }
                        let mut summary = format!(
                            "{} · {} / {}",
                            ai_object_kind_label(object.path.kind),
                            location,
                            object.path.name
                        );
                        if let Some(rows) = object.rows {
                            summary.push_str(&format!(" · {rows} 行"));
                        }
                        if let Some(comment) = object.comment.as_deref().filter(|v| !v.is_empty()) {
                            summary.push_str(&format!(" · {comment}"));
                        }
                        summary
                    },
                });
            }
        }
        // 知识条目：只取 confirmed，只暴露标题/类型/作用域元数据，不展开正文。
        if let Ok(storage) = self.ai_storage()
            && let Ok(entries) = with_ai_storage(&storage, |conn| {
                fluxdb_storage::ai_platform::knowledge::list_confirmed(conn, &query, AI_REFERENCE_CANDIDATE_LIMIT)
            })
        {
            candidates.extend(entries.into_iter().map(|entry| AiContextReference {
                id: format!("knowledge:{}", entry.id),
                label: entry.title.clone(),
                kind: "knowledge".into(),
                source: entry.id.clone(),
                summary: format!(
                    "知识 · {} · {}（revision {}）",
                    entry.kind,
                    entry
                        .database_name
                        .as_deref()
                        .map(|db| format!("作用域 {db}"))
                        .unwrap_or_else(|| "全局作用域".into()),
                    entry.revision
                ),
            }));
        }
        candidates
            .into_iter()
            .filter(|reference| {
                format!("{} {} {}", reference.label, reference.kind, reference.summary)
                    .to_lowercase()
                    .contains(&query)
            })
            .take(AI_REFERENCE_CANDIDATE_LIMIT)
            .collect()
    }
}

/// Unix 从根目录逐级 openat(NO_FOLLOW)，不允许中间目录/末端被换为链接；Windows 明确不可用。
fn open_ai_authorized_file(path:&std::path::Path)->fluxdb_core::Result<std::fs::File> {
    let fail=|e:std::io::Error|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,format!("附件安全打开失败：{e}"));
    #[cfg(unix)] {
        use std::os::{fd::{AsRawFd,FromRawFd},unix::ffi::OsStrExt};
        let mut dir=std::fs::File::open("/").map_err(fail)?;
        let parts=path.components().filter_map(|part|if let std::path::Component::Normal(v)=part{Some(v)}else{None}).collect::<Vec<_>>();
        if !path.is_absolute() || parts.is_empty() {return Err(fail(std::io::Error::other("需要绝对文件路径")));}
        for (index,part) in parts.iter().enumerate() {
            let name=std::ffi::CString::new(part.as_bytes()).map_err(|_|fail(std::io::Error::other("路径含空字符")))?;
            let mut flags=libc::O_RDONLY|libc::O_NOFOLLOW|libc::O_CLOEXEC;
            if index+1<parts.len(){flags|=libc::O_DIRECTORY;}
            // 持有父目录 fd：并发改名/替换 symlink 不能把授权路径转向另一个目录。
            let fd=unsafe{libc::openat(dir.as_raw_fd(),name.as_ptr(),flags)};
            if fd<0 {return Err(fail(std::io::Error::last_os_error()));}
            dir=unsafe{std::fs::File::from_raw_fd(fd)};
        }
        Ok(dir)
    }
    #[cfg(not(unix))] {
        let _=path;
        Err(fail(std::io::Error::other("此平台暂未实现防目录链接替换的安全文件句柄，附件读取不可用")))
    }
}
struct AiFileTool { descriptor:fluxdb_core::ToolDescriptor, references:Vec<AiContextReference> }
impl fluxdb_core::AiTool for AiFileTool {
    fn descriptor(&self)->&fluxdb_core::ToolDescriptor {&self.descriptor}
    fn execute(&self,ctx:fluxdb_core::ToolExecutionContext,arguments_json:String)->fluxdb_core::AiFuture<'_,fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let now=ai_chat_now();
            let result=(||->fluxdb_core::Result<serde_json::Value>{
                let args:serde_json::Value=serde_json::from_str(&arguments_json).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"文件参数无效"))?;
                if ctx.cancel.is_cancelled(){return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"文件读取已取消"));}
                if self.descriptor.id=="file.search" {
                    let query=args["query"].as_str().unwrap_or("").to_lowercase();
                    return Ok(serde_json::json!({"files":self.references.iter().filter(|r|r.kind=="file"&&r.label.to_lowercase().contains(&query)).take(16).map(|r|serde_json::json!({"file_ref":r.id,"label":r.label,"summary":r.summary})).collect::<Vec<_>>(),"scope":"仅已授权附件"}));
                }
                let id=args["file_ref"].as_str().unwrap_or("");
                let reference=self.references.iter().find(|r|r.id==id&&r.kind=="file").ok_or_else(||fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件未授权或已移除"))?;
                let grant:serde_json::Value=serde_json::from_str(&reference.source).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件授权损坏"))?;
                let path=std::path::Path::new(grant["path"].as_str().unwrap_or(""));
                let mut file=open_ai_authorized_file(path)?;
                let meta=file.metadata().map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件状态不可读"))?;
                let modified=meta.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_nanos().to_string());
                if !meta.is_file()||grant["size"].as_u64()!=Some(meta.len())||modified.as_deref()!=grant["modified"].as_str() {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件已变更，请移除后重新授权"));}
                use std::io::{Read,Seek};
                let offset=args["offset"].as_u64().unwrap_or(0);
                let limit=args["max_bytes"].as_u64().unwrap_or(16384).min(65536);
                file.seek(std::io::SeekFrom::Start(offset)).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件偏移无效"))?;
                let mut bytes=Vec::new();file.take(limit).read_to_end(&mut bytes).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件读取失败"))?;
                if ctx.cancel.is_cancelled(){return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"文件读取已取消"));}
                let text=std::str::from_utf8(&bytes).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件不是 UTF-8 文本或偏移切分字符，请调整范围"))?;
                Ok(serde_json::json!({"file_ref":id,"text":text,"offset":offset,"next_offset":offset+bytes.len() as u64,"truncated":offset+(bytes.len() as u64)<meta.len()}))
            })();
            let (status,value,error)=match result {Ok(v)=>(fluxdb_core::ToolReceiptStatus::Succeeded,v,None),Err(e)=>(if ctx.cancel.is_cancelled(){fluxdb_core::ToolReceiptStatus::Cancelled}else{fluxdb_core::ToolReceiptStatus::Failed},serde_json::json!({"error":e.message}),Some("file_unavailable".into()))};
            Ok(fluxdb_core::ToolReceipt{call_id:ctx.call_id,tool_id:self.descriptor.id.clone(),status,result_json:value.to_string(),error_code:error,affected_rows:None,started_at_ms:now,finished_at_ms:ai_chat_now()})
        })
    }
}
pub fn add_ai_file_tools(catalog:&mut ToolCatalog,storage:&fluxdb_storage::FileStorage,job:&AiConversationJob)->fluxdb_core::Result<()> {
    let raw=with_ai_storage(storage,|conn|fluxdb_storage::ai_platform::sessions::load_turn(conn,&job.turn_id))?;
    let snapshot:serde_json::Value=serde_json::from_str(&raw.runtime_snapshot_json).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"任务附件快照损坏"))?;
    let references=snapshot["file_grants"].as_array().into_iter().flatten().filter_map(AiContextReference::parse).collect::<Vec<_>>();
    if references.is_empty(){return Ok(());}
    for (id,desc,schema) in [
        ("file.read","按 opaque FileRef 分段读取已授权文本附件",serde_json::json!({"type":"object","properties":{"file_ref":{"type":"string"},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":65536}},"required":["file_ref"],"additionalProperties":false})),
        ("file.search","搜索本会话已授权附件，不扫描未授权目录",serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"additionalProperties":false})),
    ] {
        let descriptor=fluxdb_core::ToolDescriptor{id:id.into(),description:desc.into(),version:1,risk:fluxdb_core::ToolRisk::Read,input_schema_json:schema.to_string()};
        catalog.add_tool(Box::new(AiFileTool{descriptor,references:references.clone()}))?;
    }
    Ok(())
}
