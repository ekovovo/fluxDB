// AI 运行时 actor：控制队列 + 事件桥（设计 §12，T016）。
//
// 边界与保证：
// - UI 线程只做 `try_send`（控制通道**有界**，满时立即返回错误，绝不阻塞 UI）。
// - 取消走 `CancelFlag` **旁路**（`handle.cancel()` 直接置位），因此不会被控制队列里堆积的
//   大文本/大量事件挤掉；actor 在每个事件之间检查取消。
// - actor 串行执行会话任务；只有一个任务执行入口，没有独立的 Provider 原型分支。
// - 控制锁只保护取消位与有界补充队列，不覆盖 Provider、工具或 SQLite 调用。
// - 析构只请求关闭，不等待外部调用；显式 shutdown 可等待收尾并报告超时。

use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use fluxdb_core::CancelFlag;

/// 补充缓冲的数量上限；溢出显式拒绝，不丢弃已接受的输入。
const MAX_PENDING_STEERING: usize = 64;

enum RuntimeCommand {
    Execute {
        turn_id: String,
        cancel: CancelFlag,
        execute: RuntimeExecution,
    },
    Shutdown,
}

type RuntimeExecution = Box<dyn FnOnce(&RuntimeTurn) + Send>;

/// 单次执行的控制上下文；补充按 Turn 隔离，网络与数据库调用不持控制锁。
struct RuntimeTurn {
    id: String,
    cancel: CancelFlag,
    controls: Arc<Mutex<RuntimeControls>>,
}

impl RuntimeTurn {
    fn take_steering(&self) -> Vec<String> {
        let Ok(mut controls) = self.controls.lock() else {
            return Vec::new();
        };
        let mut texts = Vec::new();
        controls.steering.retain(|(id, text)| {
            if id == &self.id {
                texts.push(text.clone());
                false
            } else {
                true
            }
        });
        texts
    }
}

#[derive(Default)]
struct RuntimeControls {
    active: Option<(String, CancelFlag)>,
    queued: Vec<(String, CancelFlag)>,
    steering: std::collections::VecDeque<(String, String)>,
    cancelled: BTreeSet<String>,
}

/// 运行时错误（面向 UI 的可读信息，不含凭据）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// 任务队列或补充缓冲达到容量/字节限制，调用方不应阻塞等待。
    ControlQueueFull,
    /// 运行时已经关闭。
    Closed,
    /// 运行时无法启动，或控制/执行能力不可用。
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

/// 有界任务队列的所有者；取消与关闭不依赖队列容量。
pub struct AiRuntimeHandle {
    control: SyncSender<RuntimeCommand>,
    controls: Arc<Mutex<RuntimeControls>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    control_capacity: usize,
}

impl AiRuntimeHandle {
    /// 创建串行 actor；实际工作在提交后执行，不在创建时读取配置或凭据。
    pub fn new(control_capacity: usize) -> Result<Self, RuntimeError> {
        let capacity = control_capacity.max(1);
        let (tx, rx) = sync_channel(capacity);
        let controls = Arc::new(Mutex::new(RuntimeControls::default()));
        let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_controls = controls.clone();
        let thread_shutdown = shutdown_flag.clone();
        let join = std::thread::Builder::new()
            .name("fluxdb-ai-runtime".into())
            .spawn(move || run_actor(rx, thread_controls, thread_shutdown))
            .map_err(|error| {
                tracing::warn!(error=%error, "AI actor 启动失败");
                RuntimeError::NotRunning
            })?;
        Ok(Self {
            control: tx,
            controls,
            shutdown_flag,
            join: Some(join),
            control_capacity: capacity,
        })
    }

    pub fn steering(
        &self,
        turn_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        if self.shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(RuntimeError::Closed);
        }
        let text = text.into();
        let mut controls = self.controls.lock().map_err(|_| RuntimeError::NotRunning)?;
        // 数量与字节双限额；拒绝溢出，不静默丢弃用户补充。
        if controls.steering.len() >= self.control_capacity.min(MAX_PENDING_STEERING)
            || controls
                .steering
                .iter()
                .map(|(_, text)| text.len())
                .sum::<usize>()
                + text.len()
                > 64 * 1024
        {
            return Err(RuntimeError::ControlQueueFull);
        }
        controls.steering.push_back((turn_id.into(), text));
        Ok(())
    }

    pub fn cancel(&self, turn_id: &str) -> Result<(), RuntimeError> {
        let mut controls = self.controls.lock().map_err(|_| RuntimeError::NotRunning)?;
        if self.shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(RuntimeError::Closed);
        }
        let mut found = false;
        if let Some((id, flag)) = &controls.active {
            if id == turn_id {
                flag.request();
                found = true;
            }
        }
        for (id, flag) in &controls.queued {
            if id == turn_id {
                flag.request();
                found = true;
            }
        }
        if found || controls.cancelled.contains(turn_id) {
            return Ok(());
        }
        if controls.cancelled.len() >= self.control_capacity {
            return Err(RuntimeError::ControlQueueFull);
        }
        controls.cancelled.insert(turn_id.to_owned());
        Ok(())
    }

    fn submit(
        &self,
        turn_id: String,
        cancel: CancelFlag,
        execute: RuntimeExecution,
    ) -> Result<(), RuntimeError> {
        self.send(RuntimeCommand::Execute {
            turn_id,
            cancel,
            execute,
        })
    }

    fn send(&self, command: RuntimeCommand) -> Result<(), RuntimeError> {
        // 与关闭标志切换串行化，避免“已接受”的任务落在 actor 最后一次排空之后。
        let mut controls = self.controls.lock().map_err(|_| RuntimeError::NotRunning)?;
        if self.shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(RuntimeError::Closed);
        }
        let queued = match &command {
            RuntimeCommand::Execute {
                turn_id, cancel, ..
            } => Some((turn_id.clone(), cancel.clone())),
            RuntimeCommand::Shutdown => None,
        };
        match self.control.try_send(command) {
            Ok(()) => {
                if let Some((id, flag)) = queued {
                    if controls.cancelled.remove(&id) {
                        flag.request();
                    }
                    controls.queued.push((id, flag));
                }
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(RuntimeError::ControlQueueFull),
            Err(TrySendError::Disconnected(_)) => Err(RuntimeError::Closed),
        }
    }

    /// 只请求关闭，不等待网络/数据库返回，也不等待线程 join。
    pub fn request_shutdown(&self) {
        if let Ok(controls) = self.controls.lock() {
            self.shutdown_flag
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some((_, flag)) = &controls.active {
                flag.request();
            }
            for (_, flag) in &controls.queued {
                flag.request();
            }
        } else {
            self.shutdown_flag
                .store(true, std::sync::atomic::Ordering::SeqCst);
            tracing::warn!("AI 控制状态不可用，正在关闭运行时");
        }
        let _ = self.control.try_send(RuntimeCommand::Shutdown);
    }

    /// 等待真实线程结束；超时保留句柄，调用方可稍后再次等待。
    pub fn shutdown(&mut self, timeout: Duration) -> bool {
        self.request_shutdown();
        let Some(join) = self.join.take() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !join.is_finished() {
            if Instant::now() >= deadline {
                self.join = Some(join);
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if join.join().is_err() {
            tracing::warn!("AI actor 异常退出");
        }
        true
    }
}

impl Drop for AiRuntimeHandle {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(join) = self.join.take() {
            // 析构不等待外部调用；线程在取消边界收尾，后台等待并报告异常退出。
            if let Err(error) = std::thread::Builder::new()
                .name("fluxdb-ai-runtime-exit".into())
                .spawn(move || {
                    if join.join().is_err() {
                        tracing::warn!("AI actor 异常退出");
                    }
                })
            {
                tracing::warn!(error=%error, "无法启动 AI 退出等待线程；取消请求已发送");
            }
        }
    }
}

fn run_actor(
    receiver: std::sync::mpsc::Receiver<RuntimeCommand>,
    controls: Arc<Mutex<RuntimeControls>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        let closing = shutdown.load(std::sync::atomic::Ordering::SeqCst);
        let command = if closing {
            match receiver.try_recv() {
                Ok(command) => command,
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => command,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        let RuntimeCommand::Execute {
            turn_id,
            cancel: flag,
            execute,
        } = command
        else {
            continue;
        };
        if let Ok(mut state) = controls.lock() {
            if let Some(index) = state.queued.iter().position(|(id, _)| id == &turn_id) {
                state.queued.remove(index);
            }
            if state.cancelled.remove(&turn_id) {
                flag.request();
            }
            state.active = Some((turn_id.clone(), flag.clone()));
        }
        // 安装取消位后再查关闭标志，覆盖 shutdown 与开始执行之间的竞态。
        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            flag.request();
        }
        let turn = RuntimeTurn {
            id: turn_id.clone(),
            cancel: flag.clone(),
            controls: controls.clone(),
        };
        execute(&turn);
        if let Ok(mut state) = controls.lock() {
            state.active = None;
            state.steering.retain(|(id, _)| id != &turn_id);
        }
    }
}
