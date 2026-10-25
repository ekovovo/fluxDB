// MongoDB 连接与错误映射。
//
// 所有跨层错误在这里收敛成 `fluxdb_core::Error`：驱动错误文本对使用者没有意义，
// 而认证失败 / 超时 / 网络不可达需要被 UI 区分展示与重试判断。

use mongodb::error::{ErrorKind as MongoErrorKind, Error as MongoError};
use mongodb::options::ClientOptions;

/// 进程级共享 runtime：驱动是异步的，Connector trait 是同步的。
///
/// 与 PostgreSQL / SQL Server 一致，全进程只建一个 multi-thread runtime，
/// 避免每次请求都新建线程池。
fn mongo_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 MongoDB tokio runtime 失败")
    })
}

/// 建连：优先使用结构化档案现场拼 URI（密码不落盘），缺失时退回 `endpoint`。
///
/// 超时由驱动自身的 `connectTimeoutMS` / `serverSelectionTimeoutMS` 控制。
/// `ClientOptions::parse` 的 SRV/TXT 解析会落到 runtime 的阻塞线程池，单纯的
/// `tokio::time::timeout` 只能取消 future、无法中断已经卡住的 DNS 调用（实测可
/// 无限挂住），所以整段解析放到独立线程，调用方按墙钟等待并在超时后放弃该线程。
fn mongo_connect(config: &ConnectionConfig) -> fluxdb_core::Result<MongoClient> {
    let uri = match config.mongo_profile.as_ref() {
        Some(profile) => {
            if let Some(message) = profile.validate() {
                return Err(Error::new(ErrorKind::Connection, message));
            }
            profile.to_uri()
        }
        None => match &config.endpoint {
            Endpoint::Uri { uri } => uri.clone(),
            Endpoint::Tcp { host, port, .. } => format!("mongodb://{host}:{port}"),
            Endpoint::SqliteFile { .. } => {
                return Err(Error::new(ErrorKind::Connection, "MongoDB 不支持文件型连接"))
            }
        },
    };

    // 解析线程：SRV/TXT 查询可能卡在系统解析器里，调用方不能跟着一起等。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fluxdb-mongo-parse".to_string())
        .spawn(move || {
            let parsed = mongo_runtime().block_on(async move {
                ClientOptions::parse(&uri).await.map_err(mongo_error)
            });
            let _ = tx.send(parsed);
        })
        .map_err(|error| {
            Error::new(ErrorKind::Internal, format!("启动 MongoDB 解析线程失败：{error}"))
        })?;

    let options = rx
        .recv_timeout(Duration::from_secs(MONGO_URI_PARSE_TIMEOUT_SECS))
        .map_err(|_| Error::new(ErrorKind::Timeout, "解析 MongoDB 连接串超时（DNS 或 SRV 查询无响应）"))??;

    // 只建客户端不会真正握手（无 IO），但驱动要在 runtime 上下文里登记连接池任务，
    // 因此仍回到共享 runtime 上构造；认证/网络问题交给首次命令暴露。
    mongo_runtime().block_on(async move { MongoClient::with_options(options).map_err(mongo_error) })
}

/// 解析连接串（含 SRV 查询）的墙钟上限。
const MONGO_URI_PARSE_TIMEOUT_SECS: u64 = 20;

/// 在共享 runtime 上执行一次驱动操作，并加墙钟上限。
///
/// 驱动 3.x 的操作是 action builder（只实现 `IntoFuture`，不是 `Future`），调用方
/// 必须先包成 `async {}` 再传进来。超时统一收敛为 `ErrorKind::Timeout`，避免驱动
/// 内部长时间等待把同步的 Connector 调用挂住。
fn mongo_block_on<T>(
    seconds: u64,
    label: &str,
    future: impl Future<Output = mongodb::error::Result<T>>,
) -> fluxdb_core::Result<T> {
    mongo_runtime().block_on(async move {
        tokio::time::timeout(Duration::from_secs(seconds), future)
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, format!("{label}超时")))?
            .map_err(mongo_error)
    })
}

/// 把驱动错误映射成领域错误。
///
/// 分区口径：认证 → `Authentication`，选主/网络/DNS → `Connection`，命令与写入失败
/// → `Query`，服务端能力不足 → `Unsupported`。默认分支保留原始文本，不吞错误。
fn mongo_error(error: MongoError) -> Error {
    let message = mongo_error_message(&error);
    let kind = match &*error.kind {
        // 认证失败单独归类：UI 要提示检查用户名/密码/authSource，且不应自动重试。
        MongoErrorKind::Authentication { .. } => ErrorKind::Authentication,
        // DNS 失败对 Atlas SRV 常意味着网络/防火墙问题，归 Connection 更利于重试判断。
        MongoErrorKind::DnsResolve { .. }
        | MongoErrorKind::ServerSelection { .. }
        | MongoErrorKind::Io(_)
        | MongoErrorKind::ConnectionPoolCleared { .. }
        | MongoErrorKind::InvalidTlsConfig { .. }
        | MongoErrorKind::Shutdown => ErrorKind::Connection,
        MongoErrorKind::Command(_)
        | MongoErrorKind::Write(_)
        | MongoErrorKind::BulkWrite(_)
        | MongoErrorKind::InsertMany(_)
        | MongoErrorKind::InvalidArgument { .. }
        | MongoErrorKind::InvalidResponse { .. }
        | MongoErrorKind::Transaction { .. } => ErrorKind::Query,
        MongoErrorKind::IncompatibleServer { .. }
        | MongoErrorKind::SessionsNotSupported
        | MongoErrorKind::MissingResumeToken
        | MongoErrorKind::GridFs(_) => ErrorKind::Unsupported,
        _ => ErrorKind::Internal,
    };
    Error::new(kind, message)
}

/// 提取驱动错误里对用户有意义的部分：服务端消息优先，其次错误码，最后整体文本。
fn mongo_error_message(error: &MongoError) -> String {
    match &*error.kind {
        MongoErrorKind::Command(command) => {
            let mut text = command.message.clone();
            if !command.code_name.is_empty() {
                text.push_str(&format!("（{} {}）", command.code_name, command.code));
            }
            text
        }
        MongoErrorKind::Authentication { .. } => {
            "MongoDB 认证失败：请检查用户名、密码与 authSource".to_string()
        }
        MongoErrorKind::DnsResolve { message, .. } => format!("解析 MongoDB 主机失败：{message}"),
        MongoErrorKind::ServerSelection { message, .. } => format!("无法选择 MongoDB 节点：{message}"),
        MongoErrorKind::Io(io) => format!("MongoDB 网络错误：{io}"),
        _ => {
            let text = error.to_string();
            // 驱动会把连接串回显到错误里；密码已由 `to_uri` 编码，但错误文本仍可能含 userinfo，
            // 这里统一抹掉 `user:pass@` 形式的口令再上抛。
            redact_mongo_uri_password(&text)
        }
    }
}

/// 抹掉错误文本里 `scheme://user:pass@host` 的口令片段。
fn redact_mongo_uri_password(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("://") {
        let (head, tail) = rest.split_at(start + 3);
        out.push_str(head);
        // authority 段到第一个分隔符为止。
        let end = tail
            .find(['/', '?', ' ', '"', '\'', ','])
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rsplit_once('@') {
            Some((userinfo, host)) => {
                let user = userinfo.split(':').next().unwrap_or("");
                out.push_str(user);
                out.push_str(":***@");
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// 解析 Extended JSON 对象；非对象输入直接报错（`find` 的 filter/projection/sort 都是对象）。
fn mongo_parse_object(label: &str, text: &str) -> fluxdb_core::Result<Document> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Document::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|error| Error::new(ErrorKind::Query, format!("{label} 不是合法 JSON：{error}")))?;
    if !value.is_object() {
        return Err(Error::new(
            ErrorKind::Query,
            format!("{label} 必须是 JSON 对象"),
        ));
    }
    // Extended JSON（Canonical / Relaxed 都接受）。
    match Bson::try_from(value) {
        Ok(Bson::Document(document)) => Ok(document),
        Ok(_) => Err(Error::new(
            ErrorKind::Query,
            format!("{label} 必须是 JSON 对象"),
        )),
        Err(error) => Err(Error::new(
            ErrorKind::Query,
            format!("{label} 不是合法的 Extended JSON：{error}"),
        )),
    }
}

/// BSON 值转 Relaxed Extended JSON 文本（保留 ObjectId / Date / Decimal128 等类型标注）。
fn mongo_json_text(value: Bson) -> String {
    serde_json::to_string_pretty(&value.into_relaxed_extjson())
        .unwrap_or_else(|_| "{}".to_string())
}

/// 文档 `_id` 的可读标识：ObjectId 取 24 位十六进制，其它类型用其 Extended JSON 文本。
fn mongo_document_id(document: &Document) -> String {
    match document.get("_id") {
        Some(Bson::ObjectId(oid)) => oid.to_hex(),
        Some(Bson::String(value)) => value.clone(),
        Some(other) => mongo_json_text(other.clone()),
        None => String::new(),
    }
}

/// 驱动命令结果转统一文本；`ok: 0` 由驱动转成 `Command` 错误，这里只处理成功结果。
fn mongo_command_text(document: Document) -> String {
    mongo_json_text(Bson::Document(document))
}

#[cfg(test)]
mod mongo_wire_tests {
    use super::*;

    #[test]
    fn redacts_password_in_uri_like_text() {
        let text = "connection string contained mongodb://alice:s3cret@db.example.com:27017/?x=1 failed";
        let redacted = redact_mongo_uri_password(text);
        assert!(!redacted.contains("s3cret"), "{redacted}");
        assert!(redacted.contains("alice:***@db.example.com:27017"), "{redacted}");
    }

    #[test]
    fn parses_extended_json_object_and_rejects_non_object() {
        let document = mongo_parse_object("filter", r#"{"a": {"$oid": "671700000000000000000001"}}"#)
            .expect("应解析 Extended JSON");
        assert!(matches!(document.get("a"), Some(Bson::ObjectId(_))));

        assert!(mongo_parse_object("filter", "[1,2]").is_err());
        assert!(mongo_parse_object("filter", "{bad").is_err());
        // 空文本等价于 `{}`。
        assert!(mongo_parse_object("filter", "  ").unwrap().is_empty());
    }

    #[test]
    fn document_id_prefers_readable_forms() {
        let oid = mongodb::bson::oid::ObjectId::parse_str("671700000000000000000001").unwrap();
        assert_eq!(
            mongo_document_id(&doc! {"_id": oid}),
            "671700000000000000000001"
        );
        assert_eq!(mongo_document_id(&doc! {"_id": "abc"}), "abc");
        assert_eq!(mongo_document_id(&doc! {"x": 1}), "");
    }

    #[test]
    fn json_text_keeps_bson_types() {
        let oid = mongodb::bson::oid::ObjectId::parse_str("671700000000000000000001").unwrap();
        let text = mongo_json_text(Bson::Document(doc! {"_id": oid, "n": 1i32}));
        assert!(text.contains("$oid"), "{text}");
        assert!(text.contains("671700000000000000000001"), "{text}");
    }
}
