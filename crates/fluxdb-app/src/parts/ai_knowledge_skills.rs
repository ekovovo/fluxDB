// 业务知识与按需 Skills 的运行期装配（设计 §19 / T038）。
//
// `fluxdb.knowledge.skills` 开启时，Turn 开始先按固定目标筛选 confirmed 知识，
// 提示词只放有界摘录，完整正文用 knowledge.read 按需读取；关闭或无条目时不注入。
// confirm/retire 只能走用户显式入口，模型提议（proposed）不自动成为事实。

pub const KNOWLEDGE_INSTANCE_ID: &str = "builtin.knowledge";
/// 系统提示词里的知识摘录上限；更长的正文交给知识工具读取。
const EXCERPT_LIMIT: usize = 1024;
/// 单轮注入的条目上限：搜索无关的字段比正文占预算少，但也要防止无界标题。
const HINT_LIMIT: usize = 20;

pub struct KnowledgeHint {
    pub id: String,
    pub title: String,
    pub excerpt: String,
    pub truncated: bool,
}

/// 有界摘录：按字符边界截断，不把多字节字符切一半。
fn excerpt_bounded(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

impl KnowledgeHint {
    pub fn to_prompt(&self) -> String {
        let truncated_note = if self.truncated {
            "（按需 knowledge.read 续读）"
        } else {
            ""
        };
        format!("- {}（{}）：{}{}\n", self.title, self.id, self.excerpt, truncated_note)
    }
}

/// 把本轮可用的 confirmed 知识加工成 hint；每条只读一次摘录。
pub fn ai_knowledge_hints(
    conn: &rusqlite::Connection,
    target: Option<(i64, Option<&str>)>,
) -> fluxdb_core::Result<Vec<KnowledgeHint>> {
    let entries =
        fluxdb_storage::ai_platform::knowledge::list_confirmed_scoped(conn, target, "", HINT_LIMIT)?;
    let mut hints = Vec::with_capacity(entries.len());
    for entry in entries {
        let (excerpt, truncated) = excerpt_bounded(&entry.content_json, EXCERPT_LIMIT);
        hints.push(KnowledgeHint {
            id: entry.id,
            title: entry.title,
            excerpt,
            truncated,
        });
    }
    Ok(hints)
}

/// `fluxdb.knowledge.skills` 关闭时返回 None：本轮无知识提示，也不读表。
fn knowledge_enabled(conn: &rusqlite::Connection) -> fluxdb_core::Result<bool> {
    fluxdb_core::Result::Ok(
        fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
            .iter()
            .any(|instance| instance.id == KNOWLEDGE_INSTANCE_ID && instance.enabled),
    )
}

/// 把本轮可用的 confirmed 知识作为系统消息追加；无条目或插件禁用时不追加。
pub struct KnowledgeInjection {
    pub system_messages: Vec<fluxdb_core::ModelMessage>,
    pub knowledge_ids: Vec<String>,
}

/// 把本轮可用的 confirmed 知识作为系统消息追加；无条目或插件禁用时不追加。
pub fn ai_knowledge_message(
    storage: &fluxdb_storage::FileStorage,
    turn_target_json: &str,
    system_messages: Vec<fluxdb_core::ModelMessage>,
) -> KnowledgeInjection {
    let Ok(target_value) = serde_json::from_str::<serde_json::Value>(turn_target_json) else {
        return KnowledgeInjection { system_messages, knowledge_ids: Vec::new() };
    };
    let connection_id = target_value.get("connection_id").and_then(serde_json::Value::as_i64);
    let database = target_value.get("database").and_then(serde_json::Value::as_str);
    let hints = {
        let result = with_ai_storage(storage, |conn| {
            if !knowledge_enabled(conn)? {
                return Ok(Vec::new());
            }
            let target = connection_id.map(|id| (id, database));
            ai_knowledge_hints(conn, target)
        });
        result.unwrap_or_default()
    };
    if hints.is_empty() {
        return KnowledgeInjection { system_messages, knowledge_ids: Vec::new() };
    }
    let mut message = String::from("已确认业务知识（须遵循；不是可执行 SQL，信息不足时用 knowledge.read 按需续读）：\n");
    for hint in &hints {
        message.push_str(&hint.to_prompt());
    }
    let knowledge_ids = hints.iter().map(|hint| hint.id.clone()).collect();
    let mut system_messages = system_messages;
    system_messages.push(fluxdb_core::ModelMessage::new(
        fluxdb_core::ModelRole::User,
        message,
    ));
    KnowledgeInjection { system_messages, knowledge_ids }
}

/// Turn 快照只存放知识 ID 列表；正文在 runtime 装配时再读，退休/删除立即生效。
pub fn knowledge_ids_for_snapshot(
    storage: &fluxdb_storage::FileStorage,
    target_json: &str,
) -> fluxdb_core::Result<Vec<String>> {
    let target_value: serde_json::Value = serde_json::from_str(target_json).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "固定目标损坏")
    })?;
    with_ai_storage(storage, |conn| {
        if !knowledge_enabled(conn)? {
            return Ok(Vec::new());
        }
        let target = target_value
            .get("connection_id")
            .and_then(serde_json::Value::as_i64)
            .map(|id| {
                (
                    id,
                    target_value
                        .get("database")
                        .and_then(serde_json::Value::as_str),
                )
            });
        ai_knowledge_hints(conn, target).map(|hints| {
            hints
                .iter()
                .map(|hint| hint.id.clone())
                .collect::<Vec<_>>()
        })
    })
}
