// System One / Jev 决策 Provider（设计 §18、T036）。
//
// 只实现 TypeSafe/OpenRouter 共用的 System One 分类协议（`POST <base>/systemone`），
// **不走聊天协议、不把生成模型冒充成决策模型**。协议形状参考 Pi 的
// `typesafe-system-one.ts` / `system-one-shared.ts`，但落点是本仓库自己的 HTTP 边界。
//
// 分层：
// - `DecisionWireResponse` / `parse_systemone_answers`：纯函数，把响应 JSON 投影成
//   `DecisionResponse`，集中做数值/概率/置信度校验；非法数值一律拒绝，缺字段不伪造。
// - `DecisionTransport`：HTTP 抽象，生产走 ureq（受控超时 + 不跟随重定向），
//   测试注入假实现即可在无网络下断言请求体、认证与解析。
// - `SystemOneDecisionProvider`：`fluxdb_core::DecisionProvider` 实现，负责候选/提示映射、
//   凭据租约与请求体构造，认证头只在一次请求内短暂存在。
//
// Jev 决策请求只包含任务/结构摘要，绝不携带凭据、SQL 参数或私有推理。

use fluxdb_core::{DecisionRequest, DecisionResponse, Usage};

/// System One 请求顶层字段：`state` 公开状态 + `questions` 判别问题集。
///
/// Jev 只用单个 `decision` Choice 问题；答案键与候选 ID 一一对应，路由层据此取
/// `chosen`。不发送候选之外的问题，避免模型返回无法映射的答案。
pub const SYSTEMONE_QUESTION_ID: &str = "decision";

/// 决策协议错误（对外只给固定中文原因，不透出响应正文/URL/凭据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionProtocolError {
    /// HTTP 非 2xx。
    HttpStatus(u16),
    /// 响应不是合法 JSON 或结构不符合 System One 协议。
    Malformed(String),
    /// 请求地址无效（非 HTTP/HTTPS、含凭据等）。
    BadEndpoint,
    /// 凭据不可用（缺失或无法解密）。
    MissingCredential,
    /// 已取消。
    Cancelled,
}

impl DecisionProtocolError {
    /// 面向 UI/日志的脱敏原因（不含响应正文）。
    pub fn message(&self) -> String {
        match self {
            DecisionProtocolError::HttpStatus(status) => format!("决策模型返回 HTTP {status}"),
            DecisionProtocolError::Malformed(reason) => format!("决策响应不符合协议: {reason}"),
            DecisionProtocolError::BadEndpoint => "决策模型请求地址无效".to_string(),
            DecisionProtocolError::MissingCredential => "决策模型缺少可用凭据".to_string(),
            DecisionProtocolError::Cancelled => "决策请求已取消".to_string(),
        }
    }

    pub fn to_core_error(self) -> fluxdb_core::Error {
        let kind = match self {
            DecisionProtocolError::Cancelled => fluxdb_core::ErrorKind::Cancelled,
            _ => fluxdb_core::ErrorKind::Internal,
        };
        fluxdb_core::Error::new(kind, self.message())
    }
}

/// 把 System One 的 `answers` 投影成决策响应。
///
/// 只接受 `type=choice` 或 `type=score`：
/// - Choice：`choice` 必须是字符串，`probabilities` 的每个值必须在 0–1；
///   `confidence` 若存在必须在 0–1，缺失为 `None`。
/// - Score：`score` 必须是有限数，`confidence` 同上。
/// - 其它类型（含 `noul`/bool）在本 Provider 用途下视为协议不匹配，明确拒绝，
///   不猜语义；非法概率（负数、>1、NaN/Inf）或数值一律拒绝。
pub fn parse_systemone_answers(
    answers: &serde_json::Value,
    candidates: &[String],
    usage: Option<Usage>,
) -> Result<DecisionResponse, DecisionProtocolError> {
    let answer = answers
        .get(SYSTEMONE_QUESTION_ID)
        .ok_or_else(|| DecisionProtocolError::Malformed("缺少 decision 答案".into()))?;
    let kind = answer
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| DecisionProtocolError::Malformed("答案缺少 type".into()))?;
    match kind {
        "choice" => parse_choice_answer(answer, candidates, usage),
        "score" => parse_score_answer(answer, candidates, usage),
        other => Err(DecisionProtocolError::Malformed(format!(
            "不支持的答案类型 {other}"
        ))),
    }
}

fn parse_choice_answer(
    answer: &serde_json::Value,
    candidates: &[String],
    usage: Option<Usage>,
) -> Result<DecisionResponse, DecisionProtocolError> {
    let choice = answer
        .get("choice")
        .and_then(|value| value.as_str())
        .ok_or_else(|| DecisionProtocolError::Malformed("choice 不是字符串".into()))?
        .to_string();
    // 所选必须在候选集合内：模型编造的 ID 不能进入路由/过滤。
    if !candidates.iter().any(|candidate| candidate == &choice) {
        return Err(DecisionProtocolError::Malformed(
            "choice 不在候选集合内".into(),
        ));
    }
    let confidence = optional_confidence(answer)?;
    let probabilities = answer
        .get("probabilities")
        .and_then(|value| value.as_object())
        .ok_or_else(|| DecisionProtocolError::Malformed("probabilities 不是对象".into()))?;
    let mut scores = Vec::with_capacity(probabilities.len());
    for (id, raw) in probabilities {
        let value = finite_number(raw)
            .ok_or_else(|| DecisionProtocolError::Malformed("概率不是有限数".into()))?;
        if !(0.0..=1.0).contains(&value) {
            return Err(DecisionProtocolError::Malformed("概率超出 0–1".into()));
        }
        scores.push((id.clone(), value));
    }
    Ok(DecisionResponse {
        chosen: Some(choice),
        scores,
        reason: None,
        confidence,
        usage,
    })
}

fn parse_score_answer(
    answer: &serde_json::Value,
    candidates: &[String],
    usage: Option<Usage>,
) -> Result<DecisionResponse, DecisionProtocolError> {
    let score = answer
        .get("score")
        .and_then(finite_number)
        .ok_or_else(|| DecisionProtocolError::Malformed("score 不是有限数".into()))?;
    let confidence = optional_confidence(answer)?;
    // Score 不给稳定 ID；按候选顺序给出原始分数，路由层再决定是否采用。
    let scores = candidates
        .iter()
        .map(|candidate| (candidate.clone(), score))
        .collect();
    Ok(DecisionResponse {
        chosen: None,
        scores,
        reason: None,
        confidence,
        usage,
    })
}

/// `confidence` 可选但存在时必须合法（有限且 0–1）；缺失返回 `None`，不伪造 0。
fn optional_confidence(answer: &serde_json::Value) -> Result<Option<f64>, DecisionProtocolError> {
    match answer.get("confidence") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(raw) => {
            let value = finite_number(raw)
                .ok_or_else(|| DecisionProtocolError::Malformed("confidence 不是有限数".into()))?;
            if !(0.0..=1.0).contains(&value) {
                return Err(DecisionProtocolError::Malformed("confidence 超出 0–1".into()));
            }
            Ok(Some(value))
        }
    }
}

/// 只接受 JSON 数字且为有限值（拒绝字符串数字、NaN/Inf）。
fn finite_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .map(|number| number)
}

/// 构造 System One 请求体（**不含任何凭据**）。
///
/// `state` 只放公开任务摘要；`questions.decision` 用 Choice 形式，`criteria` 的键就是
/// 候选 ID，值为候选的公开说明，保证模型只能从中挑选。
pub fn build_systemone_body(model: &str, request: &DecisionRequest) -> serde_json::Value {
    let criteria: serde_json::Map<String, serde_json::Value> = request
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                candidate.clone(),
                serde_json::Value::String(format!("候选 {}", index + 1)),
            )
        })
        .collect();
    serde_json::json!({
        "model": model,
        "state": { "text": request.prompt },
        "questions": {
            SYSTEMONE_QUESTION_ID: {
                "type": "choice",
                "instructions": "从候选中选择一个最合适的 ID",
                "criteria": criteria,
            }
        }
    })
}

/// System One HTTP 边界（生产走 ureq；测试注入假实现）。
pub trait DecisionTransport: Send + Sync {
    /// 发送一次 System One 请求，返回原始响应 JSON。
    ///
    /// `auth` 只在本调用内有效；实现不得把凭据写入日志或持久化。
    fn call(
        &self,
        endpoint: &str,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &CancelFlag,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, DecisionProtocolError>;
}

/// 生产实现：ureq，禁重定向、有界超时、可取消。
pub struct UreqDecisionTransport;

impl DecisionTransport for UreqDecisionTransport {
    fn call(
        &self,
        endpoint: &str,
        auth: &crate::TransportAuth,
        body_json: &str,
        cancel: &CancelFlag,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, DecisionProtocolError> {
        if cancel.is_cancelled() {
            return Err(DecisionProtocolError::Cancelled);
        }
        // 复用连接测试同款 Agent：不跟随重定向 + 优先 IPv4 解析。
        // 系统 DNS 可能返回不可用的 IPv6 地址，默认解析器会在真实决策模型上挂起。
        let agent = crate::ai_agent(timeout);
        let mut request = agent
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        request = match auth {
            crate::TransportAuth::Bearer(key) => request.header("Authorization", &format!("Bearer {key}")),
            crate::TransportAuth::Header { name, value } => request.header(*name, value),
            crate::TransportAuth::None => request,
        };
        let mut response = match request.send(body_json) {
            Ok(response) => response,
            Err(ureq::Error::Timeout(_)) => {
                return Err(DecisionProtocolError::Malformed("请求超时".into()));
            }
            Err(ureq::Error::StatusCode(status)) => {
                return Err(DecisionProtocolError::HttpStatus(status));
            }
            Err(_) => return Err(DecisionProtocolError::Malformed("网络请求失败".into())),
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(DecisionProtocolError::HttpStatus(status));
        }
        // 决策响应应很小；限制读取字节，避免无界内存。
        let body = response
            .body_mut()
            .with_config()
            .limit(256 * 1024)
            .read_to_string()
            .map_err(|_| DecisionProtocolError::Malformed("读取响应失败".into()))?;
        serde_json::from_str(&body)
            .map_err(|_| DecisionProtocolError::Malformed("响应不是合法 JSON".into()))
    }
}

/// 凭据租约边界：请求前才申请一次性明文。
pub trait DecisionCredentialLease: Send + Sync {
    fn lease(&self, model_profile_id: &str) -> fluxdb_core::Result<SecretLease>;
}

/// 生产决策 Provider：System One 协议 + 受控传输 + 凭据租赁。
pub struct SystemOneDecisionProvider {
    model_profile_id: String,
    model: String,
    endpoint: String,
    auth_style: crate::AuthStyle,
    lease: Arc<dyn DecisionCredentialLease>,
    transport: Arc<dyn DecisionTransport>,
    timeout: std::time::Duration,
}

impl std::fmt::Debug for SystemOneDecisionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemOneDecisionProvider")
            .field("model_profile_id", &self.model_profile_id)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl SystemOneDecisionProvider {
    pub fn new(
        model_profile_id: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
        auth_style: crate::AuthStyle,
        lease: Arc<dyn DecisionCredentialLease>,
        transport: Arc<dyn DecisionTransport>,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            model_profile_id: model_profile_id.into(),
            model: model.into(),
            endpoint: endpoint.into(),
            auth_style,
            lease,
            transport,
            timeout,
        }
    }
}

impl fluxdb_core::DecisionProvider for SystemOneDecisionProvider {
    fn decide(
        &self,
        request: DecisionRequest,
        cancel: CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, DecisionResponse> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(DecisionProtocolError::Cancelled.to_core_error());
            }
            if request.candidates.is_empty() {
                return Err(DecisionProtocolError::Malformed("决策请求没有候选".into())
                    .to_core_error());
            }
            let body = build_systemone_body(&self.model, &request);
            let body_json = serde_json::to_string(&body).map_err(|e| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("决策请求序列化失败: {e}"),
                )
            })?;
            // 凭据只在一次请求内存在：租约 → 构造认证头 → 请求 → 作用域结束即丢弃。
            let auth = match self.auth_style {
                crate::AuthStyle::Bearer => {
                    let lease = self.lease.lease(&self.model_profile_id)?;
                    let key = lease.expose().to_string();
                    crate::TransportAuth::Bearer(key)
                }
                crate::AuthStyle::AnthropicApiKey => {
                    let lease = self.lease.lease(&self.model_profile_id)?;
                    crate::TransportAuth::Header {
                        name: "x-api-key",
                        value: lease.expose().to_string(),
                    }
                }
            };
            let body = self
                .transport
                .call(&self.endpoint, &auth, &body_json, &cancel, self.timeout)
                .map_err(DecisionProtocolError::to_core_error)?;
            drop(auth);
            if cancel.is_cancelled() {
                return Err(DecisionProtocolError::Cancelled.to_core_error());
            }
            let usage = parse_systemone_usage(body.get("usage"));
            let answers = body
                .get("answers")
                .ok_or_else(|| {
                    DecisionProtocolError::Malformed("响应缺少 answers".into()).to_core_error()
                })?;
            parse_systemone_answers(answers, &request.candidates, usage)
                .map_err(DecisionProtocolError::to_core_error)
        })
    }
}

/// System One 用量：`{input_tokens, output_tokens}`；缺失或非法时不报 usage，
/// 由调用方按 `unknown` 计量（不伪造零消耗）。
fn parse_systemone_usage(value: Option<&serde_json::Value>) -> Option<Usage> {
    let value = value?.as_object()?;
    let input = token_count(value.get("input_tokens"));
    let output = token_count(value.get("output_tokens"));
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        cached_input_tokens: 0,
    })
}

fn token_count(value: Option<&serde_json::Value>) -> Option<i64> {
    value
        .and_then(|value| value.as_i64())
        .filter(|count| *count >= 0)
}

/// 生产凭据租赁：请求前才从存储解密模型 API Key。
///
/// 明文只在 `lease()` 返回的一次性 `SecretLease` 内存在，不进入 DTO/日志/事件；
/// 解密失败映射为 `MissingCredential`，不覆盖或重建用户密钥。
pub struct StorageDecisionLease {
    storage: fluxdb_storage::FileStorage,
}

impl StorageDecisionLease {
    pub fn new(storage: fluxdb_storage::FileStorage) -> Self {
        Self { storage }
    }
}

impl DecisionCredentialLease for StorageDecisionLease {
    fn lease(&self, model_profile_id: &str) -> fluxdb_core::Result<crate::SecretLease> {
        let sealed = self.storage.with_ai_connection(|conn| {
            fluxdb_storage::ai_platform::models::load_encrypted_secret(conn, model_profile_id)
        })?;
        let sealed = sealed.ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "决策模型缺少 API Key")
        })?;
        let (cipher, _source) = fluxdb_storage::ai_platform::load_ai_cipher(self.storage.root_dir())
            .map_err(|_| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型凭据不可用")
            })?;
        let key = fluxdb_storage::ai_platform::model_secret::open(&cipher, model_profile_id, &sealed)
            .map_err(|_| {
                fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型凭据不可用")
            })?;
        Ok(crate::SecretLease::new(key))
    }
}

/// 从落盘模型档案构造 System One 决策 Provider。
///
/// 只接受 `api_protocol = system-one` 且用途为 decision 的模型；不按模型名猜协议，
/// 也不把生成协议（chat/responses/anthropic）当成决策协议。endpoint 缺省时使用
/// `FLUXDB_DECISION_DEFAULT_ENDPOINT`（供本地/自建部署），无默认则明确报错。
pub fn decision_provider_from_storage(
    storage: &fluxdb_storage::FileStorage,
    model_profile_id: &str,
) -> fluxdb_core::Result<SystemOneDecisionProvider> {
    decision_provider_from_storage_with_timeout(
        storage,
        model_profile_id,
        std::time::Duration::from_millis(DECISION_DEADLINE_MS),
    )
}

/// 单次决策默认截止时间。
///
/// 设计 §18 原起始值为 800ms；2026-10-12 真实验收（用户本机 Jev 模型
/// `inception/mercury-decide:free`）实测单次决策约 1978ms，800ms 会稳定超时，
/// 故按用户决定上调为 3000ms。无自动重试；重试/熔断/「不每轮等待」由上层 T037 承担。
pub const DECISION_DEADLINE_MS: u64 = 3_000;

/// 显式 deadline 版本；供验收在不同网络时延下核对协议，生产默认走 800ms。
pub fn decision_provider_from_storage_with_timeout(
    storage: &fluxdb_storage::FileStorage,
    model_profile_id: &str,
    timeout: std::time::Duration,
) -> fluxdb_core::Result<SystemOneDecisionProvider> {
    let (model, policy) = storage.with_ai_connection(|conn| {
        Ok((
            fluxdb_storage::ai_platform::models::load_model(conn, model_profile_id)?,
            crate::EgressPolicy {
                enabled: fluxdb_storage::ai_platform::settings::load_privacy_policy(conn)?
                    .model_egress_enabled,
            },
        ))
    })?;
    // 复用 T033 候选过滤：停用 / 用途错配 / 全局出站闸门 / 单模型出站许可 / 凭据。
    // 这里再要求 System One 协议，避免把生成协议当成决策协议。
    crate::candidate_check(&model, crate::CandidateRole::Decision, policy)
        .map_err(|rejection| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Unsupported,
                rejection.message(crate::CandidateRole::Decision),
            )
        })?;
    if model.api_protocol != "system-one" {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Unsupported,
            "该模型不是 System One 决策协议",
        ));
    }
    let default_base = std::env::var("FLUXDB_DECISION_DEFAULT_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let base = model
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(default_base)
        .ok_or_else(|| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "决策模型未配置请求地址",
            )
        })?;
    let endpoint = ai_request_url(&base, &model.api_protocol, model.endpoint_is_full_url)
        .map_err(|_| DecisionProtocolError::BadEndpoint.to_core_error())?;
    let auth_style = if model.auth_mode == fluxdb_storage::ai_platform::models::AuthMode::ApiKey {
        crate::AuthStyle::Bearer
    } else {
        // System One 协议需要凭据；无凭据模型不能作为决策 Provider。
        return Err(DecisionProtocolError::MissingCredential.to_core_error());
    };
    Ok(SystemOneDecisionProvider::new(
        model.id.clone(),
        model.model.clone(),
        endpoint,
        auth_style,
        Arc::new(StorageDecisionLease::new(storage.clone())),
        Arc::new(UreqDecisionTransport),
        timeout,
    ))
}

/// 记账包装：每次决策请求在 `ai_usage_ledger` 登记/完成一次 `purpose=decision`。
///
/// 决策请求也占用会话并发名额并计入 token 账本；usage 缺失时记为 `unknown`，
/// 不伪造零消耗。登记失败返回错误，不静默继续。
pub struct MeteredDecisionProvider<'a> {
    inner: &'a dyn fluxdb_core::DecisionProvider,
    storage: &'a fluxdb_storage::FileStorage,
    session_id: String,
    turn_id: String,
    model_profile_id: String,
}

impl<'a> MeteredDecisionProvider<'a> {
    pub fn new(
        inner: &'a dyn fluxdb_core::DecisionProvider,
        storage: &'a fluxdb_storage::FileStorage,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        model_profile_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            storage,
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            model_profile_id: model_profile_id.into(),
        }
    }
}

impl fluxdb_core::DecisionProvider for MeteredDecisionProvider<'_> {
    fn decide(
        &self,
        request: DecisionRequest,
        cancel: CancelFlag,
    ) -> fluxdb_core::AiFuture<'_, DecisionResponse> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(DecisionProtocolError::Cancelled.to_core_error());
            }
            let request_id = uuid::Uuid::new_v4().to_string();
            let descriptor = fluxdb_storage::ai_platform::usage_ledger::UsageRequest {
                request_id: request_id.clone(),
                session_id: self.session_id.clone(),
                turn_id: self.turn_id.clone(),
                model_profile_id: self.model_profile_id.clone(),
                purpose: "decision".into(),
                title: None,
            };
            if !with_ai_storage(self.storage, |conn| {
                fluxdb_storage::ai_platform::usage_ledger::start_request(
                    conn,
                    ai_chat_now(),
                    &descriptor,
                )
            })? {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    "重复决策请求，拒绝再次发送",
                ));
            }
            let result = self.inner.decide(request, cancel).await;
            let usage = result.as_ref().ok().and_then(|response| response.usage);
            with_ai_storage(self.storage, |conn| {
                fluxdb_storage::ai_platform::usage_ledger::finish_request(
                    conn,
                    ai_chat_now(),
                    &request_id,
                    usage,
                )
            })?;
            result
        })
    }
}
