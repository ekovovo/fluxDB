fn ai_part_text_offset(part: &fluxdb_app::AiConversationPart) -> Option<usize> {
    serde_json::from_str::<serde_json::Value>(&part.payload_json)
        .ok()?
        .get("text_offset")?
        .as_u64()
        .and_then(|offset| usize::try_from(offset).ok())
}

/// 正文仍是单点存储；展示时按真实事件的 UTF-8 字节位置拆段，不猜测旧记录的时序。
fn ai_timeline_parts(
    parts: &[fluxdb_app::AiConversationPart],
    revealed: Option<&str>,
) -> Vec<fluxdb_app::AiConversationPart> {
    let Some(body) = parts.iter().find(|part| part.kind == "text") else {
        return parts.to_vec();
    };
    let stored = ai_chat_part_text(body);
    let text = revealed.unwrap_or(&stored);
    let valid_offset =
        |offset| text.is_char_boundary(offset) || (revealed.is_some() && offset > text.len());
    if !parts
        .iter()
        .any(|part| part.kind != "text" && ai_part_text_offset(part).is_some_and(valid_offset))
    {
        return parts.to_vec();
    }
    let mut output = Vec::new();
    let mut start = 0;
    let append_text = |output: &mut Vec<_>, start: usize, end: usize| {
        if let Some(chunk) = text.get(start..end).filter(|chunk| !chunk.is_empty()) {
            let mut segment = body.clone();
            if start > 0 {
                segment.id = format!("{}:segment:{start}", body.id);
            }
            segment.payload_json = serde_json::json!({"text":chunk}).to_string();
            output.push(segment);
        }
    };
    for part in parts.iter().filter(|part| part.id != body.id) {
        if let Some(offset) = ai_part_text_offset(part) {
            // 流式揭示尚未到事件位置时先保留边界；静态记录的越界/损坏位置不参与切片。
            if offset >= start && valid_offset(offset) {
                append_text(&mut output, start, offset.min(text.len()));
                start = offset;
            }
        }
        output.push(part.clone());
    }
    append_text(&mut output, start, text.len());
    output
}

fn ai_completed_execution(part: &fluxdb_app::AiConversationPart) -> bool {
    matches!(part.kind.as_str(), "tool_call_ref" | "tool_result_ref")
        && !ai_execution_needs_attention(part)
}
