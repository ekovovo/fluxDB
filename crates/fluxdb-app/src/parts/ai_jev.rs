// 内置 Jev 装配与故障策略（设计 §18/§18.1，T037）。
//
// 这个模块只做两件事，都不参与 SQL/文件权限裁判：
// 1. `reconcile_jev_builtins`：按用户在插件管理里显式选择的决策模型重建 system 来源的
//    `decision` 角色绑定与插件绑定；未选择就撤掉旧 system 行、不擅自挑模型，永不覆盖/删除
//    `user` 绑定；
// 2. 决策建议的降级闸门：低置信度/非法返回/超时/无凭据/网络错一律放弃建议走基础策略，
//    连续失败熔断，不重试、不阻塞每一轮。
//
// 协议与凭据边界完全复用 T036 的 `SystemOneDecisionProvider`；本模块不自己拼 HTTP，
// 也不按模型名猜协议（资格只看 `api_protocol=system-one` + T033 候选过滤）。

use fluxdb_storage::ai_platform::model_bindings::{
    self, BindingSource, ModelBindingView, ModelRole,
};

/// 内置 Jev 的插件实例与包标识（与 T032 注册表、T012 种子表保持同一套 ID）。
pub const JEV_INSTANCE_ID: &str = "builtin.jev";
pub const JEV_PACKAGE_KEY: &str = "fluxdb.decision.jev";

/// 上下文压缩插件实例 ID（压缩决策建议归属的能力）。
pub const COMPACTION_INSTANCE_ID: &str = "builtin.compaction";

/// system 决策绑定的优先级；用户绑定同角色由调用方优先取用，二者不混排。
pub const JEV_SYSTEM_BINDING_PRIORITY: i64 = 100;

/// 熔断参数（设计 §18 起始值：连续 3 次失败熔断 60s，无自动重试）。
pub const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
pub const CIRCUIT_COOLDOWN_MS: i64 = 60_000;

/// 采用建议所需的最低候选概率。
///
/// 真实 System One 决策模型返回的顶层 `confidence` 常在 0.02–0.05（它表达"这个问题
/// 值不值得判定"，不是"我选中的把握"，见 T036 真实验收 confidence≈0.0425），因此
/// 顶层 confidence 不能直接当门槛，否则真实模型永远不可用。这里改用被选候选在
/// `probabilities` 中的位置判定：必须唯一最高且不低于该值，否则按低置信度放弃建议。
pub const MIN_CHOICE_PROBABILITY: f64 = 0.34;

/// 触发相关性判断的最少候选数：更少时没有真实的多候选决策点，直接全部带上。
pub const MIN_RELEVANCE_CANDIDATES: usize = 3;

/// 一次决策请求的候选上限，避免 System One 请求体随引用数量无界放大。
pub const MAX_RELEVANCE_CANDIDATES: usize = 12;

/// 触发压缩切点判断的最少候选数：只有一个合法切点时没有可决策空间。
pub const MIN_COMPACTION_CANDIDATES: usize = 2;

/// 决策角色在候选筛选中要求的模型用途。
const DECISION_ROLE: ModelRole = ModelRole::Decision;

/// Jev 声明里的决策模型字段：角色、协议要求与候选口径都由声明决定，
/// 运行期不再自己维护一套「合格候选」判定。
fn jev_decision_model_field() -> fluxdb_core::Result<&'static PluginConfigField> {
    jev_config_declaration()
        .field(JEV_DECISION_MODEL_FIELD)
        .ok_or_else(|| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "Jev 配置声明缺少决策模型字段",
            )
        })
}

/// 计算当前状态（只读，不写库）。UI 与运行期共用，避免两套判定漂移。
pub fn jev_status(
    conn: &rusqlite::Connection,
    now_ms: i64,
) -> fluxdb_core::Result<JevStatus> {
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .into_iter()
        .any(|instance| instance.id == JEV_INSTANCE_ID && instance.enabled);
    if !enabled {
        return Ok(JevStatus {
            kind: JevStatusKind::Disabled,
            ..JevStatus::default()
        });
    }
    let field = jev_decision_model_field()?;
    let resolved = resolve_field(conn, field)?;
    let status = match resolved.selection.as_deref() {
        // 只有用户在插件管理里选过决策模型，Jev 才运行。
        Some(model_profile_id) if resolved.selection_is_usable() => JevStatus {
            kind: JevStatusKind::Available,
            model_profile_id: Some(model_profile_id.to_string()),
            ..JevStatus::default()
        },
        // 选过但现在不合格：不静默换成别的模型，只给出可见原因。
        Some(model_profile_id) => JevStatus {
            kind: JevStatusKind::Unavailable,
            model_profile_id: Some(model_profile_id.to_string()),
            detail: Some(selection_rejection(conn, field, model_profile_id)),
            ..JevStatus::default()
        },
        None if !resolved.choices.is_empty() => JevStatus {
            kind: JevStatusKind::NeedsChoice,
            detail: Some(format!(
                "有 {} 个合格决策模型，请在「Jev 决策优化 · 管理」中选择决策模型",
                resolved.choices.len()
            )),
            ..JevStatus::default()
        },
        None if resolved.protocol_rejected > 0 => JevStatus {
            kind: JevStatusKind::Unavailable,
            detail: Some(format!(
                "已配置的 {} 个决策模型不支持该插件声明的决策协议",
                resolved.protocol_rejected
            )),
            ..JevStatus::default()
        },
        // 没有合格候选：零请求、零等待。
        None => JevStatus::default(),
    };
    // 熔断是运行期事实：即便配置齐全，处于熔断期也显示"暂不可用"而不是"可用"。
    if status.kind == JevStatusKind::Available {
        if let Some(model_profile_id) = status.model_profile_id.clone() {
            let circuit = circuit_snapshot(&model_profile_id, now_ms);
            if circuit.open_until_ms > now_ms {
                return Ok(JevStatus {
                    kind: JevStatusKind::Unavailable,
                    detail: circuit.last_reason.clone().map(|reason| {
                        format!("Jev 决策连续失败已熔断：{reason}")
                    }),
                    circuit_until_ms: Some(circuit.open_until_ms),
                    ..status
                });
            }
        }
    }
    Ok(status)
}

/// 统一 reconcile 入口（设计 §18.1）。
///
/// 触发点：启动、模型保存/启停、凭据 Set/Clear、Jev 决策模型（插件管理弹框）变更、
/// Agent 新建/编辑、插件启停。幂等：只有真的需要变更时才写库。
pub fn reconcile_jev_builtins(
    conn: &rusqlite::Connection,
    now_ms: i64,
) -> fluxdb_core::Result<JevStatus> {
    let status = jev_status(conn, now_ms)?;
    let model_profile_id = status.model_profile_id.clone();
    let usable = status.is_usable();
    for agent in fluxdb_storage::ai_platform::agents::list_agents(conn)? {
        // 插件绑定固定 best_effort：Jev 永不 required，绑定本身失败不阻断 Agent。
        fluxdb_storage::ai_platform::plugins::bind_plugin_system(
            conn,
            now_ms,
            &format!("bind-{}-{}", JEV_INSTANCE_ID, agent.id),
            &agent.id,
            JEV_INSTANCE_ID,
        )?;
        // 用户手动绑定优先：既不覆盖，也不再挂 system 候选去和它竞争。
        if has_enabled_user_binding(conn, &agent.id)? {
            model_bindings::prune_system_role_bindings(
                conn,
                now_ms,
                &agent.id,
                DECISION_ROLE,
                None,
            )?;
            continue;
        }
        if usable {
            let model_id = model_profile_id.as_deref().unwrap_or_default();
            model_bindings::ensure_system_binding(
                conn,
                now_ms,
                // ID 含模型档案：换决策模型时不会与旧行撞主键，旧行随后被 prune 停用。
                &format!("bind-decision-{}-{}", agent.id, model_id),
                &agent.id,
                model_id,
                DECISION_ROLE,
                JEV_SYSTEM_BINDING_PRIORITY,
            )?;
            model_bindings::enable_system_role_bindings(
                conn,
                now_ms,
                &agent.id,
                DECISION_ROLE,
                model_id,
            )?;
            model_bindings::prune_system_role_bindings(
                conn,
                now_ms,
                &agent.id,
                DECISION_ROLE,
                Some(model_id),
            )?;
        } else {
            // 未配置/关闭/待选择：撤掉自动选择过的 system 行，避免旧模型继续被采用。
            model_bindings::prune_system_role_bindings(
                conn,
                now_ms,
                &agent.id,
                DECISION_ROLE,
                None,
            )?;
        }
    }
    Ok(status)
}

fn has_enabled_user_binding(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> fluxdb_core::Result<bool> {
    Ok(model_bindings::list_agent_bindings(conn, agent_id)?
        .iter()
        .any(|binding: &ModelBindingView| {
            binding.role == DECISION_ROLE
                && binding.binding_source == BindingSource::User
                && binding.enabled
        }))
}

/// 压缩决策模型：用户在「上下文压缩 · 管理」里显式选择的槽位，与 Jev 决策模型互不影响。
///
/// 返回 `None` 表示能力已关闭、未配置，或已选择的模型当前不合格（停用/用途错配/禁止出站/
/// 缺凭据/协议不符）；调用方一律回退确定性切点，不静默换用别的模型，也不因它失败而暂停压缩。
pub fn resolve_compaction_decision_model(
    conn: &rusqlite::Connection,
) -> fluxdb_core::Result<Option<String>> {
    // 插件关闭等于关掉这层优化：确定性压缩与保护约束照常，只是不再发决策请求。
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .into_iter()
        .any(|instance| instance.id == COMPACTION_INSTANCE_ID && instance.enabled);
    if !enabled {
        return Ok(None);
    }
    let field = compaction_decision_model_field()?;
    let resolved = resolve_field(conn, field)?;
    let usable = resolved.selection_is_usable();
    Ok(if usable { resolved.selection } else { None })
}

/// 上下文压缩声明里的压缩决策模型字段：角色、协议与候选口径都由声明决定。
fn compaction_decision_model_field() -> fluxdb_core::Result<&'static PluginConfigField> {
    compaction_config_declaration()
        .field(COMPACTION_DECISION_MODEL_FIELD)
        .ok_or_else(|| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Internal,
                "上下文压缩配置声明缺少压缩决策模型字段",
            )
        })
}

/// 该 Agent 本轮实际会用哪个决策模型。
///
/// 顺序：用户手动 `decision` 绑定 → system 绑定 → 全局 reconcile 选中的模型。
/// 前两者都不可用时返回 `None`，调用方走基础路径而不是"找一个差不多的"。
pub fn resolve_decision_model(
    conn: &rusqlite::Connection,
    agent_id: Option<&str>,
    now_ms: i64,
) -> fluxdb_core::Result<Option<String>> {
    if let Some(agent_id) = agent_id {
        let bindings = model_bindings::list_agent_bindings(conn, agent_id)?;
        if let Some(bound) = bindings
            .iter()
            .find(|binding| {
                binding.role == DECISION_ROLE
                    && binding.enabled
                    && binding.binding_source == BindingSource::User
            })
            .map(|binding| binding.model_profile_id.clone())
        {
            return Ok(Some(bound));
        }
        if let Some(bound) = bindings
            .iter()
            .find(|binding| {
                binding.role == DECISION_ROLE
                    && binding.enabled
                    && binding.binding_source == BindingSource::System
            })
            .map(|binding| binding.model_profile_id.clone())
        {
            return Ok(Some(bound));
        }
    }
    Ok(jev_status(conn, now_ms)?.model_profile_id)
}

/// 熔断状态：按模型档案隔离，重启后自然重置（配置一变就该重新试一次）。
#[derive(Clone, Debug, Default)]
pub(crate) struct CircuitState {
    consecutive_failures: u32,
    open_until_ms: i64,
    last_reason: Option<String>,
}

fn circuit_registry() -> &'static Mutex<HashMap<String, CircuitState>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, CircuitState>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn circuit_snapshot(model_profile_id: &str, now_ms: i64) -> CircuitState {
    let mut registry = match circuit_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    let entry = registry.entry(model_profile_id.to_string()).or_default();
    if entry.open_until_ms <= now_ms && entry.open_until_ms != 0 {
        // 熔断窗口结束：清零计数，下一轮允许重新尝试（不做退避加长，也不重试本轮）。
        entry.consecutive_failures = 0;
        entry.open_until_ms = 0;
        entry.last_reason = None;
    }
    entry.clone()
}

/// 成功即清零连续失败计数。
pub(crate) fn record_circuit_success(model_profile_id: &str) {
    let mut registry = match circuit_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(entry) = registry.get_mut(model_profile_id) {
        entry.consecutive_failures = 0;
        entry.open_until_ms = 0;
        entry.last_reason = None;
    }
}

/// 失败累加；达到阈值则熔断 `CIRCUIT_COOLDOWN_MS`，返回本次是否新开熔断。
pub(crate) fn record_circuit_failure(model_profile_id: &str, reason: &str, now_ms: i64) -> bool {
    let mut registry = match circuit_registry().lock() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    let entry = registry.entry(model_profile_id.to_string()).or_default();
    entry.consecutive_failures += 1;
    entry.last_reason = Some(reason.to_string());
    if entry.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD {
        entry.open_until_ms = now_ms + CIRCUIT_COOLDOWN_MS;
        entry.consecutive_failures = 0;
        return true;
    }
    false
}

/// 一次相关性决策的候选（只含公开标签与类别，不含路径正文、SQL 或凭据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelevanceCandidate {
    pub id: String,
    pub label: String,
    pub kind: String,
}

/// 决策建议的采用结果；任何非 `Adopted` 都意味着"按基础策略继续"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JevAdvice {
    /// 未发请求：未配置/关闭/候选不足/熔断中。`reason` 用于日志与断言。
    NotAttempted(String),
    /// 采用建议。
    Adopted { chosen: String },
    /// 发了请求但放弃建议（失败/低置信度/非法返回），走基础路径。
    Degraded { reason: String },
}

impl JevAdvice {
    pub fn adopted_id(&self) -> Option<&str> {
        match self {
            JevAdvice::Adopted { chosen } => Some(chosen),
            _ => None,
        }
    }
}

/// 相关性决策提示词：只描述任务与候选结构，不带凭据、SQL 参数或私有推理。
pub(crate) fn relevance_prompt(candidates: &[RelevanceCandidate]) -> String {
    let mut prompt = String::from("为当前任务选择唯一最相关的一个候选引用。未列出任何其他信息。\n");
    for candidate in candidates {
        prompt.push_str(&format!("- {}：{}（{}）\n", candidate.id, candidate.label, candidate.kind));
    }
    prompt.push_str("若都不相关，也必须在给定候选中挑出最接近的一个。");
    prompt
}

/// 判断一次决策响应是否值得采用。
///
/// 非法返回由 T036 解析层拒绝；这里只判断"有没有足够把握"：被选候选必须是
/// `probabilities` 里的唯一最高且不低于 [`MIN_CHOICE_PROBABILITY`]，否则按低置信度
/// 放弃。缺少概率信息同样不采用（不猜语义）。
pub fn advice_is_usable(
    response: &fluxdb_core::DecisionResponse,
    candidates: &[String],
) -> Result<(), &'static str> {
    let Some(chosen) = response.chosen.as_deref() else {
        return Err("决策未给出稳定选择");
    };
    if !candidates.iter().any(|id| id == chosen) {
        return Err("决策选择不在候选集合内");
    }
    let Some((_, chosen_probability)) = response.scores.iter().find(|(id, _)| id == chosen) else {
        return Err("决策缺少被选候选的概率");
    };
    if response
        .scores
        .iter()
        .any(|(id, probability)| id != chosen && *probability >= *chosen_probability)
    {
        return Err("候选概率无明显优势");
    }
    if *chosen_probability < MIN_CHOICE_PROBABILITY {
        return Err("决策置信度不足");
    }
    Ok(())
}

/// 决策建议的来源标注：决定诊断事件里的 `role` / `source`，两种用途共用一个决策通道。
#[derive(Clone, Copy)]
struct AdviceOrigin {
    role: &'static str,
    source: &'static str,
    circuit_reason: &'static str,
}

const RELEVANCE_ORIGIN: AdviceOrigin = AdviceOrigin {
    role: "decision",
    source: "builtin.jev",
    circuit_reason: "Jev 熔断中，跳过本轮建议",
};

const COMPACTION_ORIGIN: AdviceOrigin = AdviceOrigin {
    role: "compaction_decision",
    source: "builtin.compaction",
    circuit_reason: "压缩决策模型熔断中，跳过本轮建议",
};

/// 发起一次相关性建议；调用方必须已经有 Turn（计量账本要求真实 turn 归属）。
pub fn advise_relevance(
    storage: &fluxdb_storage::FileStorage,
    agent_id: Option<&str>,
    session_id: &str,
    turn_id: &str,
    candidates: &[RelevanceCandidate],
    cancel: &fluxdb_core::CancelFlag,
) -> JevAdvice {
    if candidates.len() < MIN_RELEVANCE_CANDIDATES {
        return JevAdvice::NotAttempted("候选不足，无需决策".into());
    }
    let bounded: Vec<RelevanceCandidate> = candidates
        .iter()
        .take(MAX_RELEVANCE_CANDIDATES)
        .cloned()
        .collect();
    let now_ms = ai_chat_now();

    let (model_profile_id, status) = match with_ai_storage(storage, |conn| {
        let status = jev_status(conn, now_ms)?;
        // Agent 的用户绑定优先于全局 reconcile 选择。
        let model = match resolve_decision_model(conn, agent_id, now_ms)? {
            Some(model) => model,
            None => return Ok((String::new(), status)),
        };
        Ok((model, status))
    }) {
        Ok(value) => value,
        Err(error) => return JevAdvice::Degraded { reason: error.message },
    };
    if !status.is_usable() || model_profile_id.is_empty() {
        return JevAdvice::NotAttempted(match status.detail {
            Some(detail) => format!("Jev 不可用：{detail}"),
            None => format!("Jev 不可用：{}", status.label()),
        });
    }
    let ids: Vec<String> = bounded.iter().map(|candidate| candidate.id.clone()).collect();
    advise_with_model(
        storage,
        session_id,
        turn_id,
        &model_profile_id,
        &ids,
        relevance_prompt(&bounded),
        RELEVANCE_ORIGIN,
        cancel,
        now_ms,
    )
}

/// 压缩决策候选：宿主已经算好的一个合法切点，只暴露公开标签与取舍说明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionCutCandidate {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// 压缩决策提示词：只说"保留多少历史"，不带消息正文、SQL 或凭据。
fn compaction_prompt(candidates: &[CompactionCutCandidate]) -> String {
    let mut prompt = String::from(
        "为一次上下文压缩选择唯一最合适的一个切点。切点决定多少历史进入摘要、多少保留原文；         越靠后的切点压缩越多、保留的原文越少。未列出任何其他信息。\n",
    );
    for candidate in candidates {
        prompt.push_str(&format!(
            "- {}：{}（{}）\n",
            candidate.id, candidate.label, candidate.detail
        ));
    }
    prompt.push_str("必须从给定候选中挑出一个；无法判断时选择保留原文最多的那个。");
    prompt
}

/// 发起一次压缩切点建议；调用方必须已经有 Turn（计量账本要求真实 turn 归属）。
///
/// 与相关性建议共用同一个决策通道与降级口径：未配置/关闭/熔断/失败/低置信度都返回非
/// `Adopted`，由调用方回退到确定性切点。这里的建议只影响哪些历史进入摘要，不改变权限、
/// 保护项与 checkpoint 校验。
pub fn advise_compaction_cut(
    storage: &fluxdb_storage::FileStorage,
    session_id: &str,
    turn_id: &str,
    candidates: &[CompactionCutCandidate],
    cancel: &fluxdb_core::CancelFlag,
) -> JevAdvice {
    if candidates.len() < MIN_COMPACTION_CANDIDATES {
        return JevAdvice::NotAttempted("候选不足，无需决策".into());
    }
    let now_ms = ai_chat_now();
    let model_profile_id = match with_ai_storage(storage, resolve_compaction_decision_model) {
        Ok(Some(model)) => model,
        Ok(None) => {
            return JevAdvice::NotAttempted(
                "压缩决策模型未配置或不可用，按确定性切点压缩".into(),
            );
        }
        Err(error) => return JevAdvice::Degraded { reason: error.message },
    };
    let ids: Vec<String> = candidates.iter().map(|candidate| candidate.id.clone()).collect();
    advise_with_model(
        storage,
        session_id,
        turn_id,
        &model_profile_id,
        &ids,
        compaction_prompt(candidates),
        COMPACTION_ORIGIN,
        cancel,
        now_ms,
    )
}

/// 决策建议的公共执行路径：熔断闸门 → Provider → 计量重试 → 采用门槛 → 诊断事件。
#[allow(clippy::too_many_arguments)]
fn advise_with_model(
    storage: &fluxdb_storage::FileStorage,
    session_id: &str,
    turn_id: &str,
    model_profile_id: &str,
    ids: &[String],
    prompt: String,
    origin: AdviceOrigin,
    cancel: &fluxdb_core::CancelFlag,
    now_ms: i64,
) -> JevAdvice {
    let circuit = circuit_snapshot(model_profile_id, now_ms);
    if circuit.open_until_ms > now_ms {
        return JevAdvice::NotAttempted(origin.circuit_reason.into());
    }
    let request = fluxdb_core::DecisionRequest {
        model: model_profile_id.to_string(),
        candidates: ids.to_vec(),
        prompt,
    };
    let provider = match decision_provider_from_storage(storage, model_profile_id) {
        Ok(provider) => provider,
        Err(error) => {
            record_circuit_failure(model_profile_id, &error.message, now_ms);
            return JevAdvice::Degraded {
                reason: error.message,
            };
        }
    };
    if cancel.is_cancelled() {
        return JevAdvice::NotAttempted("任务已取消，跳过决策建议".into());
    }
    let metered =
        MeteredDecisionProvider::new(&provider, storage, session_id, turn_id, model_profile_id);
    let response = match decide_with_retry(&metered, request, cancel) {
        Ok(response) => response,
        Err(error) => {
            record_circuit_failure(model_profile_id, &error.message, now_ms);
            record_decision_event(
                storage,
                turn_id,
                model_profile_id,
                origin,
                "error",
                Some(&error.message),
            );
            return JevAdvice::Degraded {
                reason: error.message,
            };
        }
    };
    if let Err(reason) = advice_is_usable(&response, ids) {
        record_circuit_failure(model_profile_id, reason, now_ms);
        record_decision_event(
            storage,
            turn_id,
            model_profile_id,
            origin,
            "rejected",
            Some(reason),
        );
        return JevAdvice::Degraded {
            reason: reason.to_string(),
        };
    }
    record_circuit_success(model_profile_id);
    record_decision_event(storage, turn_id, model_profile_id, origin, "adopted", None);
    JevAdvice::Adopted {
        chosen: response.chosen.unwrap_or_default(),
    }
}

/// 按错误类别重试决策请求。
///
/// 复用 `retry_model_call`：重试判定在 `fluxdb_core::Error::is_retryable`
/// （口径同 Pi 复刻的 SDK 策略），退避带抖动且可被取消打断。**每次失败都经
/// 计量 Provider 返回并计入熔断**——重试不绕过熔断。
fn decide_with_retry(
    provider: &dyn fluxdb_core::DecisionProvider,
    request: fluxdb_core::DecisionRequest,
    cancel: &fluxdb_core::CancelFlag,
) -> fluxdb_core::Result<fluxdb_core::DecisionResponse> {
    retry_model_call(cancel, || {
        let current = request.clone();
        block_on_ready(fluxdb_core::DecisionProvider::decide(
            provider,
            current,
            cancel.clone(),
        ))
        .and_then(|result| result)
    })
}

/// 组装"用户显式引用"系统消息，并按可选的 Jev 建议把最相关引用排到最前。
///
/// 只调整顺序，不删除、不改写任何引用：Jev 是排序建议而非权限裁判，拿不到建议时
/// 基础策略照旧把它们全部带上。返回值是新的系统消息列表（入参不被修改）。
pub fn ai_conversation_reference_message(
    storage: &fluxdb_storage::FileStorage,
    job: &AiConversationJob,
    system_messages: Vec<fluxdb_core::ModelMessage>,
    references: &[serde_json::Value],
    cancel: &fluxdb_core::CancelFlag,
) -> Vec<fluxdb_core::ModelMessage> {
    if references.len() < MIN_RELEVANCE_CANDIDATES {
        return append_reference_message(system_messages, references);
    }
    let candidates: Vec<RelevanceCandidate> = references
        .iter()
        .map(|value| RelevanceCandidate {
            id: value["id"].as_str().unwrap_or_default().to_string(),
            label: value["label"].as_str().unwrap_or_default().to_string(),
            kind: value["kind"].as_str().unwrap_or_default().to_string(),
        })
        .filter(|candidate| !candidate.id.is_empty())
        .collect();
    if candidates.len() < MIN_RELEVANCE_CANDIDATES {
        return append_reference_message(system_messages, references);
    }
    let agent_id = turn_agent_id(storage, &job.session_id);
    let chosen =
        advise_relevance(
            storage,
            agent_id.as_deref(),
            &job.session_id,
            &job.turn_id,
            &candidates,
            cancel,
        )
        .adopted_id()
        .map(str::to_string);
    let Some(chosen) = chosen else {
        return append_reference_message(system_messages, references);
    };
    let mut ordered: Vec<serde_json::Value> = Vec::with_capacity(references.len());
    if let Some(index) = references
        .iter()
        .position(|value| value["id"].as_str() == Some(chosen.as_str()))
    {
        ordered.push(references[index].clone());
        ordered.extend(
            references
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, value)| value.clone()),
        );
        return append_reference_message(system_messages, &ordered);
    }
    append_reference_message(system_messages, references)
}

fn append_reference_message(
    mut system_messages: Vec<fluxdb_core::ModelMessage>,
    references: &[serde_json::Value],
) -> Vec<fluxdb_core::ModelMessage> {
    if references.is_empty() {
        return system_messages;
    }
    system_messages.push(fluxdb_core::ModelMessage::new(
        fluxdb_core::ModelRole::User,
        format!(
            "用户显式引用（非执行指令；只提供上下文和只读定位，不授予写权限，写操作仍走审批；\
             文件先用 file.search 定位名称、正文用 file.search_content 后按 file.read 续读，\
             保存查询/选区/结果页详情用 context.read 按需读取）：{}",
            serde_json::Value::Array(references.to_vec())
        ),
    ));
    system_messages
}

/// 读取会话所属 Agent（决策绑定的作用范围）；读不到时按无 Agent 绑定处理。
fn turn_agent_id(storage: &fluxdb_storage::FileStorage, session_id: &str) -> Option<String> {
    storage
        .with_ai_connection(|conn| {
            let session = fluxdb_storage::ai_platform::sessions::load_session(conn, session_id)?;
            Ok(session.agent_id)
        })
        .ok()
}

/// 落一条脱敏诊断事件：验收可以直接核对"这一轮真的发过/放弃过 decision"。
fn record_decision_event(
    storage: &fluxdb_storage::FileStorage,
    turn_id: &str,
    model_profile_id: &str,
    origin: AdviceOrigin,
    outcome: &str,
    reason: Option<&str>,
) {
    let summary = serde_json::json!({
        "role": origin.role,
        "source": origin.source,
        "model_profile_id": model_profile_id,
        "outcome": outcome,
        "reason": reason,
    })
    .to_string();
    if let Err(error) = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::run_events::append(
            conn,
            turn_id,
            fluxdb_storage::ai_platform::run_events::KIND_DECISION_ADVICE,
            &summary,
            ai_chat_now(),
        )
    }) {
        tracing::warn!(turn_id=%turn_id, reason=%error.message, "决策诊断事件写入失败");
    }
}

