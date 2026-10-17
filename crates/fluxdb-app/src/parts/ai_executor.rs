// AI 运行时异步执行器（设计 §6、§12）。
//
// 早期切片用"有限就绪轮询"阻塞 future：轮询次数耗尽即报错，因此只支持
// 单次 poll 内完成的 Provider/工具。这里换成在**真实 waker** 上停放/唤醒
// 当前线程的执行器：
// - 由 AI 运行时 actor 线程调用；停放的是该后台线程，不阻塞 UI 主线程；
// - 等待期间持续检查 `CancelFlag` 与 deadline，超出上限即返回受控错误，
//   不会因为 future 永不被唤醒而无限期挂起；
// - 不依赖外部 IO 反应堆：future 由自身唤醒源（线程、通道等）唤醒。
//
// 有界性：单次 `poll` 内同步完成的工作仍在该线程上执行；跨 poll 的等待按
// `WAIT_SLICE` 分片停放，因此取消标志的延迟上限就是一个分片 + 一次唤醒。

// 用模块包裹是因为 `include!` 会把本文件内容拼到 crate 根，避免与根作用域
// 已有的 `Arc`/`Mutex`/`Duration`/`Instant` 等导入重名。
pub mod ai_executor {
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

/// 停放分片：即使没有唤醒源，也在该间隔后重新检查取消与总时限。
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// 无显式 deadline 时的兜底上限，避免 future 永不被唤醒时线程永久占用。
const DEFAULT_WAIT_CAP: Duration = Duration::from_secs(600);

/// 在真实 waker 上把当前线程停放，直到 future 完成。
///
/// 仅用于**异步执行器线程**（AI 运行时 actor）；不要在任何 UI 回调或窗口
/// 更新路径里调用。
pub fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, fluxdb_core::Error> {
    drive(future, None, None)
}

/// 同上，但在等待期间响应取消：`CancelFlag` 置位后尽快返回 `Cancelled`。
pub fn block_on_cancellable<F: std::future::Future>(
    future: F,
    cancel: &fluxdb_core::CancelFlag,
) -> Result<F::Output, fluxdb_core::Error> {
    drive(future, Some(cancel), None)
}

/// 同上，但显式给出等待上限；超过 `deadline` 返回 `Timeout`。
pub fn block_on_until<F: std::future::Future>(
    future: F,
    cancel: &fluxdb_core::CancelFlag,
    deadline: Instant,
) -> Result<F::Output, fluxdb_core::Error> {
    drive(future, Some(cancel), Some(deadline))
}

fn drive<F: std::future::Future>(
    future: F,
    cancel: Option<&fluxdb_core::CancelFlag>,
    deadline: Option<Instant>,
) -> Result<F::Output, fluxdb_core::Error> {
    let parker = Arc::new(Parker::default());
    let waker = Waker::from(Arc::new(ThreadWaker(Arc::clone(&parker))));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    let start = Instant::now();
    let cap = deadline.unwrap_or(start + DEFAULT_WAIT_CAP);
    loop {
        if cancel.is_some_and(fluxdb_core::CancelFlag::is_cancelled) {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Cancelled,
                "已取消执行",
            ));
        }
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return Ok(output),
            // 未就绪：先看是否已被唤醒，否则分片停放，随后重新检查取消与时限。
            Poll::Pending => parker.park_until(cap),
        }
        if Instant::now() >= cap {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Timeout,
                "等待结果超时，已停止等待；请重试或检查目标可用性",
            ));
        }
    }
}

/// 线程停放器：把 `std::thread::park`/`unpark` 语义搬到 condvar 上，
/// 以便同时支持"被 waker 唤醒"与"分片超时后自行醒来重查取消"。
#[derive(Default)]
struct Parker {
    signalled: Mutex<bool>,
    condvar: Condvar,
}

impl Parker {
    /// 等待唤醒；`until` 之前未收到唤醒也会返回，让调用方重查取消/时限。
    fn park_until(&self, until: Instant) {
        let Ok(mut signalled) = self.signalled.lock() else {
            // 锁中毒说明有其他线程在停放路径 panic；退化为分片睡眠，不静默成功。
            std::thread::sleep(WAIT_SLICE);
            return;
        };
        if *signalled {
            // 唤醒先于停放到达：消费唤醒标记，立即返回重新 poll。
            *signalled = false;
            return;
        }
        let now = Instant::now();
        let wait = until.saturating_duration_since(now).min(WAIT_SLICE);
        if wait.is_zero() {
            return;
        }
        let Ok((mut signalled, _)) = self.condvar.wait_timeout(signalled, wait) else {
            return;
        };
        // 超时或被唤醒都消费标记，下一轮 poll 会决定是否继续等待。
        *signalled = false;
    }

    fn unpark(&self) {
        let Ok(mut signalled) = self.signalled.lock() else {
            return;
        };
        *signalled = true;
        self.condvar.notify_one();
    }
}

struct ThreadWaker(Arc<Parker>);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

}
