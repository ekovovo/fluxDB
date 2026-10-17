/// 会话标题生成：首轮完成后由同一生成模型产出短语标题。
///
/// 设计约束：
/// - 只覆盖"第一轮尚未被用户重命名"的默认标题（`revision=1`）；用户改名/归档/恢复后不再覆盖。
/// - 标题请求计量走 `purpose='title'`，与主模型、摘要共用计量宿主，失败或缺 usage 也会记录。
/// - 失败保持原截断标题，不影响会话或回复。
use fluxdb_core::ModelRequest;

const MAX_TITLE_CHARS: usize = 40;

pub fn build_title_prompt(prompt: &str, reply: &str) -> String {
    format!(
        "请为以下数据库助手对话生成一个不超过 {MAX_TITLE_CHARS} 字的简洁标题。\
         直接输出标题正文，不要引号、编号或多余说明。\n\n\
         用户消息：\n{prompt}\n\n助手回复摘要：\n{}",
        &reply.chars().take(2048).collect::<String>()
    )
}

/// 校验并归一化模型输出为可入库标题；空串或超长截断后仍可作为默认标题。
pub fn normalize_session_title(raw: &str) -> Option<String> {
    let title: String = raw
        .trim()
        .trim_start_matches(['"', '\'', '「', '《'])
        .trim_end_matches(['"', '\'', '」', '》', '。', '，', ','])
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    let title = title.trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// 首轮完成后生成标题；尽力而为，任何失败静默保留原截断标题。
pub fn generate_session_title(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    provider: &dyn fluxdb_core::GenerationProvider,
    reply: &str,
) {
    let prompt = match with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::first_user_message_text(conn, &job.session_id)
    }) {
        Ok(Some(text)) => text,
        _ => return,
    };
    let request = ModelRequest {
        model: job.model_profile_id.clone(),
        messages: vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            build_title_prompt(&prompt, reply),
        )],
        tools: Vec::new(),
        max_output_tokens: Some(64),
    };
    let cancel = fluxdb_core::CancelFlag::new();
    let metered = MeteredProvider {
        provider,
        storage,
        job,
        model_profile_id: &job.model_profile_id,
        purpose: "title",
    };
    // 标题是尽力而为的附加请求，仍然复用统一重试：瞬时网络抖动不该白丢一次标题。
    let events = match retry_model_call(&cancel, || {
        crate::ai_executor::block_on_cancellable(
            fluxdb_core::GenerationProvider::stream(
            &metered,
            request.clone(),
            cancel.clone(),
        ),
            &cancel,
        )
        .and_then(|result| result)
    }) {
        Ok(events) => events,
        Err(_) => return,
    };
    let mut raw = String::new();
    let mut finished = false;
    for event in events {
        match event {
            fluxdb_core::ModelEvent::TextDelta(delta) => raw.push_str(&delta),
            fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop) => {
                finished = true;
            }
            _ => {}
        }
    }
    if !finished {
        return;
    }
    let Some(title) = normalize_session_title(&raw) else {
        return;
    };
    let result = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::sessions::rename_session_if_default(
            conn,
            ai_chat_now(),
            &job.session_id,
            &title,
        )
    });
    if let Err(error) = result {
        tracing::debug!(session_id=%job.session_id, error=%error.message, "会话标题生成失败");
    }
}

