// HTTP/SSE 流式传输原型（设计 §6/§17，T003）。
//
// 三条硬约束：
// 1. **可取消**：每次读取前后检查 `CancelFlag`，取消后立即停止读取（不把结果当成成功）。
// 2. **有界**：单帧（SSE event frame）与整包 body 都有字节上限，超限报错而不是吃掉内存。
// 3. **有 deadline**：connect / read 都有超时；网络逻辑只走 ureq（同步、已锁定版本），
//    由调用方放进后台执行器，不在 UI 线程跑。
//
// 解析与传输分开：`stream_sse_reader` 只依赖 `Read`，因此绝大多数测试用 `Cursor` 即可，
// 不需要开本地端口。

use std::fmt;
use std::time::Duration;

/// 传输上限与超时。
///
/// 三个超时字段的默认值都取自 [`MODEL_REQUEST_TIMEOUT_MS`]：需要单独收紧某一阶段
/// （例如测试里的短超时）时仍可显式覆盖。
#[derive(Clone, Copy, Debug)]
pub struct TransportLimits {
    /// 单个 SSE 帧（两个空行之间）的最大字节数。
    pub max_frame_bytes: usize,
    /// 整个响应体的最大字节数。
    pub max_total_bytes: usize,
    /// 建连（含 DNS/TLS 握手）超时。
    pub connect_timeout: Duration,
    /// 响应体读取的空闲预算：两次数据之间允许的最长间隔（不是整包总时长）。
    pub read_timeout: Duration,
    /// 等待模型**开始响应**（响应头）的截止时间。
    pub response_deadline: Duration,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 256 * 1024,
            max_total_bytes: 4 * 1024 * 1024,
            connect_timeout: Duration::from_millis(MODEL_REQUEST_TIMEOUT_MS),
            read_timeout: Duration::from_millis(MODEL_REQUEST_TIMEOUT_MS),
            response_deadline: Duration::from_millis(MODEL_REQUEST_TIMEOUT_MS),
        }
    }
}

/// 所有模型 HTTP 请求的统一超时（毫秒）。
///
/// 唯一来源：连接/DNS/TLS、等待响应头（首响应）、正文读取，以及决策（Jev）
/// 请求、连接测试、拉取模型列表都引用这一个值，不再各处各自定义。
///
/// 各阶段语义不同但共用同一时长：连接与首响应是硬 deadline；正文读取是
/// **空闲超时**（两次数据之间不空超过该值，见 `IdleBodyTimeoutConnector`），
/// 因此长回答只要持续产出就不会被总时长截断。
///
/// 3000ms 对真实远程/中转模型过短：排队、冷启动与网关抖动都会让"还没开始吐字"
/// 被误报成超时；30s 只在真正无响应时才失败。
pub const MODEL_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// 聊天协议（决定 SSE 载荷如何投影成 `fluxdb_core::ModelEvent`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatProtocol {
    OpenAiCompatible,
    AnthropicMessages,
    /// OpenAI Responses API（`POST /v1/responses`，SSE 事件以 `response.*` 命名）。
    OpenAiResponses,
}

/// 传输层错误（不含任何凭据/响应正文碎片）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// 单帧超过上限。
    FrameTooLarge { limit: usize },
    /// 整包超过上限。
    BodyTooLarge { limit: usize },
    /// 非 2xx。
    HttpStatus(u16),
    /// 载荷不是合法 JSON 或结构不符合协议。
    Malformed(String),
    /// 连接/DNS/传输层失败（已脱敏；瞬时故障，可重试）。
    Network(String),
    /// 超时：连接/响应头阶段或正文读取（已脱敏；瞬时故障，可重试）。
    Timeout(String),
    /// 取消（不是错误，但调用方通常需要区分）。
    Cancelled,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::FrameTooLarge { limit } => {
                write!(f, "SSE 帧超过 {limit} 字节上限")
            }
            TransportError::BodyTooLarge { limit } => {
                write!(f, "响应体超过 {limit} 字节上限")
            }
            TransportError::HttpStatus(status) => write!(f, "HTTP {status}"),
            TransportError::Malformed(reason) => write!(f, "响应格式错误: {reason}"),
            TransportError::Network(reason) => write!(f, "网络错误: {reason}"),
            TransportError::Timeout(reason) => write!(f, "网络错误: {reason}"),
            TransportError::Cancelled => f.write_str("已取消"),
        }
    }
}

impl std::error::Error for TransportError {}

impl TransportError {
    /// 转成领域错误。
    ///
    /// 必须保留错误类别：`is_retryable` 只看 `ErrorKind` 与 `http_status`，
    /// 一律降级成 `Internal` 会让超时/限流/上游故障都变成"不可重试"。
    pub fn to_core_error(self) -> fluxdb_core::Error {
        match self {
            TransportError::Timeout(reason) => fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Timeout,
                format!("网络错误: {reason}"),
            ),
            TransportError::Network(reason) => fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Connection,
                format!("网络错误: {reason}"),
            ),
            TransportError::Cancelled => {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Cancelled, "已取消")
            }
            TransportError::HttpStatus(status) => {
                let kind = if status == 401 || status == 403 {
                    fluxdb_core::ErrorKind::Authentication
                } else {
                    fluxdb_core::ErrorKind::Internal
                };
                fluxdb_core::Error::with_http_status(kind, format!("HTTP {status}"), status)
            }
            // 载荷超限/协议不合法都是确定性失败，重试只会再消费一次额度。
            TransportError::FrameTooLarge { limit } => fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("SSE 帧超过 {limit} 字节上限"),
            ),
            TransportError::BodyTooLarge { limit } => fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("响应体超过 {limit} 字节上限"),
            ),
            TransportError::Malformed(reason) => fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                format!("响应格式错误: {reason}"),
            ),
        }
    }
}

/// 增量 SSE 解析器：只认识 `data:` 载荷，忽略 event/id/retry/注释。
pub struct SseParser {
    buf: Vec<u8>,
    max_frame_bytes: usize,
    finished: bool,
}

impl SseParser {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame_bytes,
            finished: false,
        }
    }

    /// 追加一段字节，返回本次解析出的完整 `data:` 载荷（按出现顺序）。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, TransportError> {
        if self.finished {
            return Err(TransportError::Malformed("解析器已结束".into()));
        }
        self.buf.extend_from_slice(chunk);
        let mut payloads = Vec::new();
        loop {
            let Some((frame_end, sep_len)) = find_frame_end(&self.buf) else {
                // 未结束的残余帧超过上限：直接失败，避免无界增长。
                if self.buf.len() > self.max_frame_bytes {
                    return Err(TransportError::FrameTooLarge {
                        limit: self.max_frame_bytes,
                    });
                }
                break;
            };
            let frame: Vec<u8> = self.buf.drain(..frame_end + sep_len).collect();
            let text = String::from_utf8_lossy(&frame[..frame_end]);
            if let Some(payload) = frame_data_payload(text.as_ref()) {
                payloads.push(payload);
            }
        }
        Ok(payloads)
    }

    /// 结束流：残余未以空行结尾的帧按 SSE 语义丢弃。
    pub fn finish(&mut self) {
        self.finished = true;
        self.buf.clear();
    }
}

/// 找 SSE 帧分隔（`\n\n` 或 `\r\n\r\n`），返回（帧结束位置, 分隔符长度）。
fn find_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// 从一帧里取 `data:` 载荷（多行 `data:` 用 `\n` 连接）；没有 data 行时返回 None。
fn frame_data_payload(frame: &str) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    for line in frame.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// 从字节流读 SSE 并投影成 `fluxdb_core::ModelEvent`（有界 + 可取消）。
///
/// 大多数测试传 `std::io::Cursor`；生产调用方传 ureq 的响应体 reader。
pub fn stream_sse_reader<R: std::io::Read>(
    reader: &mut R, limits: TransportLimits, protocol: ChatProtocol, cancel: &fluxdb_core::CancelFlag,
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    stream_sse_reader_emit(reader,limits,protocol,cancel,&mut |_| {})
}

include!("ai_transport_stream.rs");

/// 只保留错误类别，不带可能的响应体/URL 细节。
fn sanitize_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::TimedOut => "读取超时".to_string(),
        std::io::ErrorKind::UnexpectedEof => "连接提前结束".to_string(),
        other => format!("{other:?}"),
    }
}

/// 正文读是否因超时结束。
///
/// ureq 的读取超时会以 `Error::Timeout` 包进 `io::Error`（kind 退化成 `Other`），
/// 因此不能只看 `ErrorKind::TimedOut`，还要看被包裹的原始错误。
fn is_read_timeout(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
        .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
}

/// 流式解析过程中累积的状态（工具调用参数是分片到达的）。
#[derive(Default)]
pub struct StreamState {
    /// OpenAI 工具调用：按 index 累积 id/name/arguments。
    openai_tools: Vec<OpenAiToolAccumulator>,
    /// Anthropic 工具调用：按 content block index 累积。
    anthropic_tools: Vec<AnthropicToolAccumulator>,
    /// Anthropic 输入在 message_start，最终输出在 message_delta；缓存输入另列。
    anthropic_input_tokens: Option<i64>,
    anthropic_output_tokens: Option<i64>,
    anthropic_cache_read_tokens: i64,
    anthropic_cache_creation_tokens: i64,
    /// Responses 工具调用：按 output item id 累积（`response.output_item.added` → delta → done）。
    responses_tools: Vec<(String, OpenAiToolAccumulator)>,
    /// 是否已经报过 usage（避免重复）。
    usage_seen: bool,
}

#[derive(Default, Clone)]
struct OpenAiToolAccumulator {
    provider_call_id: String,
    tool_id: String,
    arguments: String,
}

#[derive(Default)]
struct AnthropicToolAccumulator {
    provider_call_id: String,
    tool_id: String,
    arguments: String,
}

/// 把一个 SSE `data:` JSON 载荷投影成 `fluxdb_core::ModelEvent`。
pub fn parse_sse_payload(
    protocol: ChatProtocol,
    payload: &str,
    state: &mut StreamState,
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| TransportError::Malformed(format!("不是合法 JSON: {e}")))?;
    match protocol {
        ChatProtocol::OpenAiCompatible => parse_openai_chunk(&value, state),
        ChatProtocol::AnthropicMessages => parse_anthropic_chunk(&value, state),
        ChatProtocol::OpenAiResponses => parse_responses_chunk(&value, state),
    }
}

/// 只有输入/输出均已知才上报实际用量；不把缺字段伪装为零 token。
fn token_usage(
    input: Option<i64>,
    output: Option<i64>,
    cached: Option<i64>,
) -> Result<Option<fluxdb_core::Usage>, TransportError> {
    let (Some(input_tokens), Some(output_tokens)) = (input, output) else {
        return Ok(None);
    };
    let cached_input_tokens = cached.unwrap_or(0);
    if input_tokens < 0 || output_tokens < 0 || cached_input_tokens < 0
        || cached_input_tokens > input_tokens || input_tokens.checked_add(output_tokens).is_none()
    {
        return Err(TransportError::Malformed("模型 token 用量无效".into()));
    }
    Ok(Some(fluxdb_core::Usage { input_tokens, output_tokens, cached_input_tokens }))
}

fn parse_openai_chunk(
    value: &serde_json::Value,
    state: &mut StreamState,
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let mut events = Vec::new();
    if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
        if !state.usage_seen {
            if let Some(usage) = token_usage(
                usage.get("prompt_tokens").and_then(|v| v.as_i64()),
                usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_i64()),
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_i64()),
            )? {
                state.usage_seen = true;
                events.push(fluxdb_core::ModelEvent::Usage(usage));
            }
        }
    }
    let choices = match value.get("choices") {
        None => return Ok(events),
        Some(raw) if raw.is_null() => return Ok(events),
        Some(raw) => raw
            .as_array()
            .ok_or_else(|| TransportError::Malformed("choices 不是数组".into()))?,
    };
    let Some(choice) = choices.first() else {
        return Ok(events);
    };
    // 结构不符要显式失败，而不是被当成"没有事件的空响应"（A 报出的生产缺口）。
    if !choice.is_object() {
        return Err(TransportError::Malformed("choice 不是对象".into()));
    }
    let choice = choice.clone();
    if let Some(delta) = choice.get("delta").filter(|d| !d.is_null()) {
        if !delta.is_object() {
            return Err(TransportError::Malformed("delta 不是对象".into()));
        }
        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                events.push(fluxdb_core::ModelEvent::TextDelta(text.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for call in calls {
                let index = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                while state.openai_tools.len() <= index {
                    state.openai_tools.push(OpenAiToolAccumulator::default());
                }
                let slot = &mut state.openai_tools[index];
                if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                    slot.provider_call_id = id.to_string();
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                        slot.tool_id = name.to_string();
                    }
                    if let Some(args) = function.get("arguments").and_then(|v| v.as_str()) {
                        slot.arguments.push_str(args);
                    }
                }
            }
        }
    }
    if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
        match reason {
            "tool_calls" | "function_call" => {
                for slot in state.openai_tools.drain(..) {
                    if slot.tool_id.is_empty() {
                        continue;
                    }
                    events.push(fluxdb_core::ModelEvent::ToolCallRequested(fluxdb_core::ToolCallRequest {
                        provider_call_id: slot.provider_call_id,
                        tool_id: slot.tool_id,
                        arguments_json: slot.arguments,
                    }));
                }
                events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::ToolCalls));
            }
            "length" => events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Length)),
            _ => events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop)),
        }
    }
    Ok(events)
}

fn parse_anthropic_chunk(
    value: &serde_json::Value,
    state: &mut StreamState,
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let mut events = Vec::new();
    let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let usage = if kind == "message_start" {
        value.get("message").and_then(|message| message.get("usage"))
    } else if kind == "message_delta" {
        value.get("usage")
    } else {
        None
    };
    if let Some(usage) = usage {
        if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_i64()) {
            state.anthropic_input_tokens = Some(input);
        }
        if kind == "message_delta" {
            if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                state.anthropic_output_tokens = Some(output);
            }
        }
        if let Some(cache) = usage.get("cache_read_input_tokens").and_then(|v| v.as_i64()) {
            state.anthropic_cache_read_tokens = cache;
        }
        if let Some(cache) = usage.get("cache_creation_input_tokens").and_then(|v| v.as_i64()) {
            state.anthropic_cache_creation_tokens = cache;
        }
    }
    let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match kind {
        "content_block_start" => {
            let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(block) = value.get("content_block") {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    while state.anthropic_tools.len() <= index {
                        state.anthropic_tools.push(AnthropicToolAccumulator::default());
                    }
                    let slot = &mut state.anthropic_tools[index];
                    slot.provider_call_id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    slot.tool_id = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                }
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(delta) = value.get("delta") {
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                events.push(fluxdb_core::ModelEvent::TextDelta(text.to_string()));
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                            if let Some(slot) = state.anthropic_tools.get_mut(index) {
                                slot.arguments.push_str(partial);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(slot) = state.anthropic_tools.get_mut(index) {
                if !slot.tool_id.is_empty() {
                    events.push(fluxdb_core::ModelEvent::ToolCallRequested(fluxdb_core::ToolCallRequest {
                        provider_call_id: slot.provider_call_id.clone(),
                        tool_id: slot.tool_id.clone(),
                        arguments_json: slot.arguments.clone(),
                    }));
                    slot.tool_id.clear();
                }
            }
        }
        "message_stop" => {
            if !state.usage_seen {
                // 统一 Usage 的输入包含缓存读取/创建；它们不能在汇总时再次相加。
                let input = state.anthropic_input_tokens.map(|input| {
                    if input < 0 || state.anthropic_cache_creation_tokens < 0 {
                        return Err(TransportError::Malformed("模型 token 用量无效".into()));
                    }
                    input.checked_add(state.anthropic_cache_read_tokens)
                        .and_then(|total| total.checked_add(state.anthropic_cache_creation_tokens))
                        .ok_or_else(|| TransportError::Malformed("模型 token 用量溢出".into()))
                }).transpose()?;
                if let Some(usage) = token_usage(input, state.anthropic_output_tokens,
                    Some(state.anthropic_cache_read_tokens))? {
                    state.usage_seen = true;
                    events.push(fluxdb_core::ModelEvent::Usage(usage));
                }
            }
            events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop));
        }
        _ => {}
    }
    Ok(events)
}

/// 认证方式（Debug 不含明文）。
#[derive(Clone)]
pub enum TransportAuth {
    /// `Authorization: Bearer <key>`
    Bearer(String),
    /// 自定义 header（如 Anthropic 的 `x-api-key`）。
    Header { name: &'static str, value: String },
    None,
}

impl fmt::Debug for TransportAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportAuth::Bearer(_) => f.write_str("TransportAuth::Bearer([REDACTED])"),
            TransportAuth::Header { name, .. } => {
                write!(f, "TransportAuth::Header({name}, [REDACTED])")
            }
            TransportAuth::None => f.write_str("TransportAuth::None"),
        }
    }
}

/// 真实 HTTP 流式调用（同步 ureq；调用方负责放到后台线程）。
///
/// `body_json` 由调用方按协议构造；本函数只负责投递、按协议解析 SSvE、施加 deadline 与大小上限。
pub fn stream_chat(
    limits: TransportLimits, protocol: ChatProtocol, url: &str, auth: &TransportAuth, body_json: &str, extra_headers: &[(&str,&str)], cancel:&fluxdb_core::CancelFlag,
) -> Result<Vec<fluxdb_core::ModelEvent>,TransportError> {
    stream_chat_emit(limits,protocol,url,auth,body_json,extra_headers,cancel,&mut |_| {})
}

/// 流式对话用的 HTTP Agent：分阶段超时 + 正文空闲超时 + IPv4 优先解析。
///
/// 与设置页「测试连接」共用 IPv4 优先解析器：2026-10-12 真实验收中，系统 DNS
/// 返回的不可用 IPv6 地址会让默认解析器在远程模型上挂起并最终超时。
///
/// 连接与响应头仍用硬 deadline（取 [`TransportLimits`] 的对应字段，默认同源）；
/// 正文读取交给 [`IdleBodyTimeoutConnector`]，按"空闲间隔"而不是"整包总时长"计。
pub(crate) fn chat_agent(limits: TransportLimits) -> ureq::Agent {
    use ureq::unversioned::transport::Connector as _;
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        // 不跟随重定向：跨域 302 可能把请求（含认证头）引向非预期主机。
        // 3xx 直接按 HTTP 状态返回，由调用方决定如何处理。
        .max_redirects(0)
        .timeout_connect(Some(limits.connect_timeout))
        // 等模型开口的阶段预算（DNS/连接/响应头）。
        .timeout_recv_response(Some(limits.response_deadline))
        // 不设 timeout_recv_body：ureq 把该值当作"整包正文总预算"，
        // 长回答会被总时长截断；正文改由空闲超时连接器逐次读预算兜底。
        .build();
    let connector = ureq::unversioned::transport::DefaultConnector::default().chain(
        IdleBodyTimeoutConnector::new(limits.read_timeout),
    );
    ureq::Agent::with_parts(config, connector, PrefersIpv4Resolver)
}

/// 把**正文读取**阶段的每次读预算替换为固定空闲预算的连接器。
///
/// ureq 的 `timeout_recv_body` 语义是"收到响应头起算的整包总预算"，且预算耗尽后
/// 连接直接失效、不可续读；因此长回答（例如分片缓慢但持续产出）会被总时长误杀。
/// 这里把正文阶段的每次等待预算替换成固定的空闲预算，覆盖在底层传输上，使
/// "每两次数据之间不空超过该时长"成为真正的判定口径；连接、响应头仍走各自阶段预算。
#[derive(Debug)]
struct IdleBodyTimeoutConnector {
    idle: ureq::unversioned::transport::time::Duration,
}

impl IdleBodyTimeoutConnector {
    fn new(idle: Duration) -> Self {
        Self {
            idle: ureq::unversioned::transport::time::Duration::from_millis(
                u64::try_from(idle.as_millis()).unwrap_or(u64::MAX),
            ),
        }
    }
}

impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for IdleBodyTimeoutConnector
{
    type Out = IdleBodyTimeoutTransport<In>;

    fn connect(
        &self,
        _details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| IdleBodyTimeoutTransport {
            inner,
            idle: self.idle,
        }))
    }
}

#[derive(Debug)]
struct IdleBodyTimeoutTransport<In> {
    inner: In,
    idle: ureq::unversioned::transport::time::Duration,
}

impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Transport
    for IdleBodyTimeoutTransport<In>
{
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        self.inner.buffers()
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        // 连接/响应头阶段由 agent 配置了明确预算，ureq 传进来的是有限时长；
        // 正文阶段没有配置 `timeout_recv_body`（见 `chat_agent`），ureq 因此给出
        // `NotHappening`——正好用来识别"这次等待属于正文读取"，替换成空闲预算。
        if !timeout.after.is_not_happening() {
            return self.inner.await_input(timeout);
        }
        let timeout = ureq::unversioned::transport::NextTimeout {
            after: self.idle,
            reason: ureq::Timeout::RecvBody,
        };
        self.inner.await_input(timeout)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn stream_chat_emit(
    limits: TransportLimits,
    protocol: ChatProtocol,
    url: &str,
    auth: &TransportAuth,
    body_json: &str,
    extra_headers: &[(&str, &str)],
    cancel: &fluxdb_core::CancelFlag,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let mut events = Vec::new();
    stream_chat_events(limits, protocol, url, auth, body_json, extra_headers, cancel, &mut |event| {
        emit(event);
        events.push(event.clone());
    })?;
    Ok(events)
}

/// 在 SSE 解析点通知宿主；只保留当前帧，不累计整段响应的事件副本。
#[allow(clippy::too_many_arguments)]
pub fn stream_chat_events(
    limits: TransportLimits,
    protocol: ChatProtocol,
    url: &str,
    auth: &TransportAuth,
    body_json: &str,
    extra_headers: &[(&str, &str)],
    cancel: &fluxdb_core::CancelFlag,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> Result<(), TransportError> {
    if cancel.is_cancelled() {
        emit(&fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Cancelled));
        return Ok(());
    }
    let agent = chat_agent(limits);
    let mut request = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream");
    request = match auth {
        TransportAuth::Bearer(key) => request.header("Authorization", &format!("Bearer {key}")),
        TransportAuth::Header { name, value } => request.header(*name, value),
        TransportAuth::None => request,
    };
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send(body_json).map_err(|e| match e {
        ureq::Error::Timeout(_) => TransportError::Timeout(sanitize_ureq_error(&e)),
        other => TransportError::Network(sanitize_ureq_error(&other)),
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(TransportError::HttpStatus(status));
    }
    let mut reader = response.body_mut().as_reader();
    read_sse_events(&mut reader, limits, protocol, cancel, emit)
}

/// 只映射错误类别，不带底层文本（避免 URL/凭据细节外泄）。
fn sanitize_ureq_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        ureq::Error::Timeout(_) => "超时".to_string(),
        ureq::Error::ConnectionFailed => "连接失败".to_string(),
        _ => "传输失败".to_string(),
    }
}

/// Responses API 的 SSE 事件投影。
///
/// 关键事件：`response.output_text.delta`（正文）、`response.reasoning_summary_text.delta`（公开摘要）、
/// `response.output_item.added` + `response.function_call_arguments.delta` + `response.output_item.done`（工具调用）、
/// `response.completed`（结束 + usage）、`response.incomplete`（长度截断）、`response.failed` / `error`（失败）。
fn parse_responses_chunk(
    value: &serde_json::Value,
    state: &mut StreamState,
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let mut events = Vec::new();
    let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                if !delta.is_empty() {
                    events.push(fluxdb_core::ModelEvent::TextDelta(delta.to_string()));
                }
            }
        }
        "response.reasoning_summary_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                if !delta.is_empty() {
                    events.push(fluxdb_core::ModelEvent::ReasoningSummaryDelta(delta.to_string()));
                }
            }
        }
        "response.output_item.added" => {
            if let Some(item) = value.get("item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    state.responses_tools.push((
                        item_id,
                        OpenAiToolAccumulator {
                            provider_call_id: item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            tool_id: item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            arguments: item
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        },
                    ));
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let item_id = value.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                if let Some((_, tool)) = state
                    .responses_tools
                    .iter_mut()
                    .find(|(id, _)| id == item_id)
                {
                    tool.arguments.push_str(delta);
                }
            }
        }
        "response.output_item.done" => {
            if let Some(item) = value.get("item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    // 以 done 事件里的最终参数为准（delta 可能只覆盖一部分）。
                    let accumulated = state
                        .responses_tools
                        .iter()
                        .find(|(id, _)| id == item_id)
                        .map(|(_, tool)| (*tool).clone())
                        .unwrap_or_default();
                    state.responses_tools.retain(|(id, _)| id != item_id);
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or(accumulated.provider_call_id);
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or(accumulated.tool_id);
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .filter(|args| !args.is_empty())
                        .unwrap_or(accumulated.arguments);
                    if !call_id.is_empty() && !name.is_empty() {
                        events.push(fluxdb_core::ModelEvent::ToolCallRequested(
                            fluxdb_core::ToolCallRequest {
                                provider_call_id: call_id,
                                tool_id: name,
                                arguments_json: arguments,
                            },
                        ));
                    }
                }
            }
        }
        "response.completed" => {
            if !state.usage_seen {
                if let Some(usage) = value.get("response").and_then(|r| r.get("usage")) {
                    if let Some(usage) = token_usage(
                        usage.get("input_tokens").and_then(|v| v.as_i64()),
                        usage.get("output_tokens").and_then(|v| v.as_i64()),
                        usage
                            .get("input_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(|v| v.as_i64()),
                    )? {
                        state.usage_seen = true;
                        events.push(fluxdb_core::ModelEvent::Usage(usage));
                    }
                }
            }
            events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Stop));
        }
        "response.incomplete" => {
            events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Length));
        }
        "response.failed" | "error" => {
            events.push(fluxdb_core::ModelEvent::Finished(fluxdb_core::FinishReason::Error));
        }
        _ => {}
    }
    Ok(events)
}
