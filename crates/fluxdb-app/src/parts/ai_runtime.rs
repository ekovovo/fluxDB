// AI 运行时 actor：控制队列 + 事件桥（设计 §12，T016）。
//
// 边界与保证：
// - UI 线程只做 `try_send`（控制通道**有界**，满时立即返回错误，绝不阻塞 UI）。
// - 取消走 `CancelFlag` **旁路**（`handle.cancel()` 直接置位），因此不会被控制队列里堆积的
//   大文本/大量事件挤掉；actor 在每个事件之间检查取消。
// - actor 独占 Provider 与 turn 循环，不持有 AppController/SQLite 锁，也不跨越 await 持锁。
// - 事件经 `AiEventSink` 回传 UI 线程（桌面端用 gpui 的 update/notify 实现它），事件自带 `seq`。
// - 关闭窗口：`AiRuntimeHandle::drop` → `shutdown()`，等 actor 在安全边界退出（带超时兜底）。

use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Instant;

use fluxdb_core::{
    CancelFlag, FinishReason, GenerationProvider, ModelRequest, ToolCallRequest, ToolReceipt,
};

/// 每个 Turn 允许排队的 steering 条数（超出丢弃最旧的，保证有界）。
const MAX_PENDING_STEERING: usize = 64;

/// 控制命令（UI → actor）。
#[derive(Clone, Debug)]
pub enum AiControl {
    StartTurn {
        session_id: String,
        turn_id: String,
        spec: TurnSpec,
    },
    /// 用户补充（在下一个安全边界被吸收进下一轮请求）。
    Steering {
        turn_id: String,
        text: String,
    },
    /// 请求取消某个 Turn（正常情况下直接用 `handle.cancel()` 旁路更快）。
    Cancel {
        turn_id: String,
    },
    Shutdown,
}

/// 启动一轮 Turn 所需的全部输入（不含凭据）。
#[derive(Clone, Debug)]
pub struct TurnSpec {
    pub model: String,
    pub messages: Vec<fluxdb_core::ModelMessage>,
    pub max_output_tokens: Option<i64>,
    /// 本轮固定目标（`TargetRef::to_json()`），参与 plan hash。
    pub target_json: String,
    /// Agent 白名单 + 目标/内核权限。
    pub permission: TurnPermissionContext,
}

impl TurnSpec {
    /// 只给定模型与消息（目标为空、权限取内核默认：可读不可写）。
    pub fn from_request(request: ModelRequest) -> Self {
        Self {
            model: request.model,
            messages: request.messages,
            max_output_tokens: request.max_output_tokens,
            target_json: String::new(),
            permission: TurnPermissionContext::default(),
        }
    }
}

/// actor → UI 的事件（带会话/Turn/序号，旧的迟到事件不会污染新 Turn）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiEvent {
    TurnStarted {
        session_id: String,
        turn_id: String,
        seq: u64,
    },
    TextDelta {
        session_id: String,
        turn_id: String,
        seq: u64,
        text: String,
    },
    ReasoningSummaryDelta {
        session_id: String,
        turn_id: String,
        seq: u64,
        text: String,
    },
    ToolCallRequested {
        session_id: String,
        turn_id: String,
        seq: u64,
        call: ToolCallRequest,
    },
    /// 工具开始真实执行（已通过 schema 校验 + 审计 + 授权）。
    ToolStarted {
        session_id: String,
        turn_id: String,
        seq: u64,
        call: ToolCallRequest,
    },
    /// 工具真实回执（不可修改的事实；UI 只渲染，不重算）。
    ToolFinished {
        session_id: String,
        turn_id: String,
        seq: u64,
        receipt: ToolReceipt,
    },
    /// 需要用户审批：本 Turn 暂停，**没有执行任何工具**。
    ApprovalRequired {
        session_id: String,
        turn_id: String,
        seq: u64,
        plan_hash: String,
        scope: ApprovalScope,
    },
    /// 运行时的可见说明（未知工具、参数不合法、无进展暂停等）。
    Notice {
        session_id: String,
        turn_id: String,
        seq: u64,
        text: String,
    },
    /// Turn 安全暂停（等审批/达到限制）；之后不会再有该 Turn 的事件。
    Paused {
        session_id: String,
        turn_id: String,
        seq: u64,
        reason: String,
    },
    Finished {
        session_id: String,
        turn_id: String,
        seq: u64,
        reason: FinishReason,
    },
    Error {
        session_id: String,
        turn_id: String,
        seq: u64,
        message: String,
    },
}

impl AiEvent {
    pub fn turn_id(&self) -> &str {
        match self {
            AiEvent::TurnStarted { turn_id, .. }
            | AiEvent::TextDelta { turn_id, .. }
            | AiEvent::ReasoningSummaryDelta { turn_id, .. }
            | AiEvent::ToolCallRequested { turn_id, .. }
            | AiEvent::ToolStarted { turn_id, .. }
            | AiEvent::ToolFinished { turn_id, .. }
            | AiEvent::ApprovalRequired { turn_id, .. }
            | AiEvent::Notice { turn_id, .. }
            | AiEvent::Paused { turn_id, .. }
            | AiEvent::Finished { turn_id, .. }
            | AiEvent::Error { turn_id, .. } => turn_id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            AiEvent::TurnStarted { seq, .. }
            | AiEvent::TextDelta { seq, .. }
            | AiEvent::ReasoningSummaryDelta { seq, .. }
            | AiEvent::ToolCallRequested { seq, .. }
            | AiEvent::ToolStarted { seq, .. }
            | AiEvent::ToolFinished { seq, .. }
            | AiEvent::ApprovalRequired { seq, .. }
            | AiEvent::Notice { seq, .. }
            | AiEvent::Paused { seq, .. }
            | AiEvent::Finished { seq, .. }
            | AiEvent::Error { seq, .. } => *seq,
        }
    }
}

/// UI 事件接收端（桌面端用 gpui 的 update/notify 实现）。
pub trait AiEventSink: Send + 'static {
    fn emit(&self, event: AiEvent);
}

/// 运行时错误（面向 UI 的可读信息，不含凭据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// 控制队列已满：UI 不应阻塞等待，直接提示稍后重试。
    ControlQueueFull,
    /// 运行时已经关闭。
    Closed,
    /// 事件回调未被消费（actor 已退出）。
    NotRunning,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::ControlQueueFull => f.write_str("AI 运行时繁忙，请稍后重试"),
            RuntimeError::Closed => f.write_str("AI 运行时已关闭"),
            RuntimeError::NotRunning => f.write_str("AI 运行时未在运行"),
        }
    }
}

/// 运行时句柄：UI 线程持有它，只做 `try_send` / 置取消位。
pub struct AiRuntimeHandle {
    control: SyncSender<AiControl>,
    /// 旁路取消位：`cancel()` 直接置位，不排队。
    cancel: Arc<Mutex<Option<(String, CancelFlag)>>>,
    /// 旁路关闭位：控制队列满时也能可靠停止 actor（actor 用 `recv_timeout` 周期检查）。
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AiRuntimeHandle {
    /// 放弃投递而不是阻塞 UI（bounded channel 满 = 运行时不健康）。
    /// 兼容入口：只用模型与消息启动一轮（目标为空、权限取内核默认）。
    pub fn start_turn(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        request: ModelRequest,
    ) -> Result<(), RuntimeError> {
        self.start_turn_with_spec(session_id, turn_id, TurnSpec::from_request(request))
    }

    /// 完整入口：携带固定目标与权限上下文（审批/授权按它判定）。
    pub fn start_turn_with_spec(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        spec: TurnSpec,
    ) -> Result<(), RuntimeError> {
        self.send(AiControl::StartTurn {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            spec,
        })
    }

    pub fn steering(&self, turn_id: impl Into<String>, text: impl Into<String>) -> Result<(), RuntimeError> {
        self.send(AiControl::Steering {
            turn_id: turn_id.into(),
            text: text.into(),
        })
    }

    /// 取消：直接置位对应 Turn 的取消标志（旁路，不等队列）。
    pub fn cancel(&self, turn_id: &str) -> Result<(), RuntimeError> {
        let guard = self.cancel.lock().map_err(|_| RuntimeError::NotRunning)?;
        match guard.as_ref() {
            Some((id, flag)) if id == turn_id => {
                flag.request();
                Ok(())
            }
            // 还没开始（或已结束）的 Turn：走控制队列，让 actor 在启动时立刻看到取消。
            _ => self.send(AiControl::Cancel {
                turn_id: turn_id.to_string(),
            }),
        }
    }

    fn send(&self, control: AiControl) -> Result<(), RuntimeError> {
        match self.control.try_send(control) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(RuntimeError::ControlQueueFull),
            Err(TrySendError::Disconnected(_)) => Err(RuntimeError::Closed),
        }
    }

    /// 请求关闭并等待 actor 退出（最多 `timeout`）。
    pub fn shutdown(&mut self, timeout: Duration) -> bool {
        // 先把已启动的 Turn 取消，避免 actor 卡在长任务里。
        if let Ok(guard) = self.cancel.lock() {
            if let Some((_, flag)) = guard.as_ref() {
                flag.request();
            }
        }
        // 旁路置位：即使控制队列已满（try_send 会 Full）也能让 actor 在下一个轮询周期退出。
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // 快路径：队列有空位时立即唤醒 actor，不必等轮询。
        let _ = self.control.try_send(AiControl::Shutdown);
        let Some(join) = self.join.take() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !join.is_finished() {
            if Instant::now() >= deadline {
                // 超时：把句柄放回去，让调用方决定（不 detach 掩盖泄漏）。
                self.join = Some(join);
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = join.join();
        true
    }
}

impl Drop for AiRuntimeHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            // 窗口关闭路径：尽力安全结束，超时也不阻塞进程退出。
            let _ = self.shutdown(Duration::from_secs(2));
        }
    }
}

/// 启动 actor：独占 Provider，控制通道容量 `control_capacity`。
/// 默认审计 Hook：**拒绝一切**工具调用（fail closed）。
///
/// 只有显式接入规则化审计的实现才放行；"没接审计"= 工具一律不执行，
/// 而不是默默放行。
pub struct DenyAllAudit;

impl SecurityAuditHook for DenyAllAudit {
    fn audit(&self, _input: &AuditInput) -> fluxdb_core::Result<AuditOutcome> {
        Ok(AuditOutcome::Deny)
    }
}

/// 简易入口：不注册任何工具（因此任何工具调用都会被审计拒绝）。
pub fn spawn_ai_runtime(
    provider: Box<dyn GenerationProvider>,
    sink: Box<dyn AiEventSink>,
    control_capacity: usize,
) -> AiRuntimeHandle {
    spawn_ai_runtime_with_tools(
        provider,
        sink,
        control_capacity,
        ToolCatalog::new(Vec::new()),
        Box::new(DenyAllAudit),
    )
}

/// 启动 actor：独占 Provider、工具目录与强制审计 Hook。
pub fn spawn_ai_runtime_with_tools(
    provider: Box<dyn GenerationProvider>,
    sink: Box<dyn AiEventSink>,
    control_capacity: usize,
    tools: ToolCatalog,
    audit: Box<dyn SecurityAuditHook>,
) -> AiRuntimeHandle {
    let capacity = control_capacity.max(1);
    let (tx, rx) = sync_channel::<AiControl>(capacity);
    let cancel: Arc<Mutex<Option<(String, CancelFlag)>>> = Arc::new(Mutex::new(None));
    let cancel_for_thread = cancel.clone();
    let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_for_thread = shutdown_flag.clone();
    let join = std::thread::Builder::new()
        .name("fluxdb-ai-runtime".to_string())
        .spawn(move || {
            run_actor(
                provider,
                sink,
                rx,
                cancel_for_thread,
                shutdown_for_thread,
                tools,
                audit,
            )
        })
        .ok();
    AiRuntimeHandle {
        control: tx,
        cancel,
        shutdown_flag,
        join,
    }
}

fn run_actor(
    provider: Box<dyn GenerationProvider>,
    sink: Box<dyn AiEventSink>,
    control: std::sync::mpsc::Receiver<AiControl>,
    cancel_slot: Arc<Mutex<Option<(String, CancelFlag)>>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    tools: ToolCatalog,
    audit: Box<dyn SecurityAuditHook>,
) {
    // 每个 Turn 的 steering 缓冲（有界）。
    let mut pending_steering: Vec<String> = Vec::new();
    // 待取消的 Turn（还没启动的）。
    let mut cancelled_before_start: Vec<String> = Vec::new();

    loop {
        if shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let control = match control.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(control) => control,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match control {
            AiControl::Shutdown => break,
            AiControl::Steering { turn_id, text } => {
                let _ = turn_id;
                pending_steering.push(text);
                if pending_steering.len() > MAX_PENDING_STEERING {
                    pending_steering.remove(0);
                }
            }
            AiControl::Cancel { turn_id } => {
                let mut guard = match cancel_slot.lock() {
                    Ok(guard) => guard,
                    Err(_) => break,
                };
                match guard.as_ref() {
                    Some((id, flag)) if id == &turn_id => flag.request(),
                    _ => cancelled_before_start.push(turn_id),
                }
            }
            AiControl::StartTurn {
                session_id,
                turn_id,
                spec,
            } => {
                // 吸收上一轮排队的 steering（安全边界，不改执行目标）。
                let mut steering: Vec<String> = Vec::new();
                steering.extend(pending_steering.drain(..));

                let flag = CancelFlag::new();
                if cancelled_before_start.iter().any(|id| id == &turn_id) {
                    flag.request();
                    cancelled_before_start.retain(|id| id != &turn_id);
                }
                if let Ok(mut guard) = cancel_slot.lock() {
                    *guard = Some((turn_id.clone(), flag.clone()));
                }

                let mut seq: u64 = 0;
                seq += 1;
                sink.emit(AiEvent::TurnStarted {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    seq,
                });

                if flag.is_cancelled() {
                    seq += 1;
                    sink.emit(AiEvent::Finished {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                        seq,
                        reason: FinishReason::Cancelled,
                    });
                } else {
                    let runner = TurnRunner {
                        provider: provider.as_ref(),
                        tools: &tools,
                        limits: LoopLimits::default(),
                        permission: spec.permission.clone(),
                        audit: audit.as_ref(),
                    };
                    let state = TurnState {
                        model: spec.model.clone(),
                        messages: spec.messages.clone(),
                        max_output_tokens: spec.max_output_tokens,
                        target_json: spec.target_json.clone(),
                    };
                    let outcome = runner.run(state, &mut steering, &flag, &mut |event| {
                        if let Some(event) =
                            runtime_event_from_loop(&session_id, &turn_id, &mut seq, event)
                        {
                            sink.emit(event);
                        }
                    });

                    match outcome {
                        TurnOutcome::Completed { .. } => {
                            seq += 1;
                            sink.emit(AiEvent::Finished {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                seq,
                                reason: FinishReason::Stop,
                            });
                        }
                        TurnOutcome::Cancelled => {
                            seq += 1;
                            sink.emit(AiEvent::Finished {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                seq,
                                reason: FinishReason::Cancelled,
                            });
                        }
                        TurnOutcome::Failed { message } => {
                            seq += 1;
                            sink.emit(AiEvent::Error {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                seq,
                                message,
                            });
                            seq += 1;
                            sink.emit(AiEvent::Finished {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                seq,
                                reason: FinishReason::Error,
                            });
                        }
                        // 暂停（等审批/达到限制）：这是该 Turn 的终态事件，不再补 Finished，
                        // 避免 UI 把"等待用户决定"误判成"本轮已完成"。
                        TurnOutcome::Paused { reason } => {
                            seq += 1;
                            sink.emit(AiEvent::Paused {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                seq,
                                reason,
                            });
                        }
                    }
                }
                if let Ok(mut guard) = cancel_slot.lock() {
                    *guard = None;
                }
            }
        }
    }
}

/// `LoopEvent` → UI 事件；纯内部事件（迭代开始）不投影给 UI。
fn runtime_event_from_loop(
    session_id: &str,
    turn_id: &str,
    seq: &mut u64,
    event: LoopEvent,
) -> Option<AiEvent> {
    let session_id = session_id.to_string();
    let turn_id = turn_id.to_string();
    match event {
        LoopEvent::IterationStarted { .. } => None,
        LoopEvent::TextDelta(text) => {
            *seq += 1;
            Some(AiEvent::TextDelta {
                session_id,
                turn_id,
                seq: *seq,
                text,
            })
        }
        LoopEvent::ReasoningSummaryDelta(text) => {
            *seq += 1;
            Some(AiEvent::ReasoningSummaryDelta {
                session_id,
                turn_id,
                seq: *seq,
                text,
            })
        }
        LoopEvent::ToolCallRequested(call) => {
            *seq += 1;
            Some(AiEvent::ToolCallRequested {
                session_id,
                turn_id,
                seq: *seq,
                call,
            })
        }
        LoopEvent::ToolStarted { call, .. } => {
            *seq += 1;
            Some(AiEvent::ToolStarted {
                session_id,
                turn_id,
                seq: *seq,
                call,
            })
        }
        LoopEvent::ToolFinished { receipt } => {
            *seq += 1;
            Some(AiEvent::ToolFinished {
                session_id,
                turn_id,
                seq: *seq,
                receipt,
            })
        }
        LoopEvent::ApprovalRequired { plan_hash, scope } => {
            *seq += 1;
            Some(AiEvent::ApprovalRequired {
                session_id,
                turn_id,
                seq: *seq,
                plan_hash,
                scope,
            })
        }
        LoopEvent::Notice(text) => {
            *seq += 1;
            Some(AiEvent::Notice {
                session_id,
                turn_id,
                seq: *seq,
                text,
            })
        }
    }
}

/// 就绪 future 的最小阻塞器。
///
/// 设计 §6 明确 app 在生产中不引入 tokio；本切片的 Provider（scripted、以及内部用同步 ureq
/// 的 HTTP provider）在单次 poll 内即可完成。若 Provider 真的返回 `Pending`（需要外部 IO 反应堆），
/// 这里返回可读错误而不是自旋挂死——引入执行器后再替换本函数。
fn block_on_ready<F: std::future::Future>(future: F) -> Result<F::Output, fluxdb_core::Error> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    for _ in 0..64 {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => continue,
        }
    }
    Err(fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        "Provider 未在就绪轮次内完成：需要引入异步执行器（当前切片不支持挂起等待）",
    ))
}

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}
