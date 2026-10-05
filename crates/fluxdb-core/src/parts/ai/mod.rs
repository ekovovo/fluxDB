// AI 领域 DTO 与能力契约（设计 §8、§20，T002）。
//
// 分层约定：
// - 本文件只放**无副作用**的数据类型与 trait，不依赖 GPUI、HTTP 客户端或任何具体驱动；
// - 运行时是多实现能力（真实 Provider / scripted Provider / 各种工具），所以用 `dyn` 访问；
// - 凭据明文类型不在这里定义：`SecretInput` 已由 storage 的模型凭据模块提供
//   （不可序列化、Debug 脱敏），避免出现第二份"明文包装"实现。

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 异步能力返回类型；`Send` 让运行时可以把它交给后台线程池。
pub type AiFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

/// 跨线程取消标志：UI/控制队列置位，Provider 与工具在等待期间轮询。
#[derive(Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消（幂等）。
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for CancelFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelFlag")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// 模型能力声明（用于路由与校验，不允许"能力靠猜"）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCapabilities {
    /// 是否支持流式增量。
    pub streaming: bool,
    /// 是否支持 tool calling。
    pub tool_calling: bool,
    /// 是否支持 reasoning summary 之类的公开摘要。
    pub reasoning_summary: bool,
}

/// 对话角色（Provider 边界投影；应用内部消息不直接等于它）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 发给 Provider 的单条消息（只含公开内容，不含私有 CoT）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub text: String,
}

impl ModelMessage {
    pub fn new(role: ModelRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

/// 工具风险等级（与设计 §20 的风险分类一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    /// 只读、有界。
    Read,
    /// 会改数据，需要授权/审批。
    Write,
    /// 管理类操作，默认不暴露。
    Admin,
}

/// 工具描述（供模型发现；`input_schema_json` 是 JSON Schema 文本）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolDescriptor {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub risk: ToolRisk,
    pub input_schema_json: String,
}

/// 模型请求的一次工具调用。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRequest {
    /// Provider 在本次响应里给出的调用 ID（用于回填 tool 消息）。
    pub provider_call_id: String,
    pub tool_id: String,
    pub arguments_json: String,
}

/// 一次主模型推理的请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output_tokens: Option<i64>,
}

/// token 用量（费用与预算的事实来源）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_input_tokens: i64,
}

/// 结束原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Cancelled,
    Error,
}

/// 流式事件（Provider → 运行时；运行时再投影成 UI 事件与持久化片段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    /// 文本增量（UI 可按 30–60ms 合并，不进数据库逐字符写）。
    TextDelta(String),
    /// 公开推理摘要（绝不是私有 CoT）。
    ReasoningSummaryDelta(String),
    /// 模型请求调用某个工具。
    ToolCallRequested(ToolCallRequest),
    /// Provider 报出的用量。
    Usage(Usage),
    /// 本次推理结束。
    Finished(FinishReason),
}

/// 真实工具执行回执：不可变事实，Hook 不能改写成功/影响行数。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolReceipt {
    pub call_id: String,
    pub tool_id: String,
    pub status: ToolReceiptStatus,
    /// 面向 UI/模型的结果载荷（已脱敏；大结果只放引用）。
    pub result_json: String,
    pub error_code: Option<String>,
    pub affected_rows: Option<i64>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

impl ToolReceipt {
    /// 是否成功；Hook 不得改写。
    pub fn is_success(&self) -> bool {
        matches!(self.status, ToolReceiptStatus::Succeeded)
    }
}

/// 工具执行状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolReceiptStatus {
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    Unknown,
}

/// 工具执行上下文（由运行时提供；工具自身不接触 UI/SQLite 锁）。
#[derive(Clone, Debug)]
pub struct ToolExecutionContext {
    pub session_id: String,
    pub turn_id: String,
    pub iteration: i64,
    /// 本次调用的稳定 ID（与 `ToolReceipt.call_id` 对应）。
    pub call_id: String,
}

/// 决策（Jev）请求：只在需要做选择/排序/分类时调用，不参与主模型 tool calling。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionRequest {
    pub model: String,
    /// 候选项（模型/插件/条目的稳定 ID）。
    pub candidates: Vec<String>,
    /// 公开的决策提示（不含私有推理）。
    pub prompt: String,
}

/// 决策响应：稳定 ID + 分数；无法决策时给明确原因。
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionResponse {
    pub chosen: Option<String>,
    pub scores: Vec<(String, f64)>,
    pub reason: Option<String>,
}

/// 生成模型能力（运行时的多实现边界）。
pub trait GenerationProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;
    /// 流式推理；`cancel` 触发后必须尽快结束（返回 `Finished(Cancelled)` 或错误）。
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancelFlag,
    ) -> AiFuture<'_, Vec<ModelEvent>>;
}

/// 决策模型能力（Jev 等）。
pub trait DecisionProvider: Send + Sync {
    fn decide(
        &self,
        request: DecisionRequest,
        cancel: CancelFlag,
    ) -> AiFuture<'_, DecisionResponse>;
}

/// 工具能力。
pub trait AiTool: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;
    /// 执行一次调用；返回的回执是事实来源，不能被 Hook 改写。
    fn execute(
        &self,
        ctx: ToolExecutionContext,
        arguments_json: String,
    ) -> AiFuture<'_, ToolReceipt>;
}

impl std::fmt::Display for ToolRisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Admin => "admin",
        })
    }
}
