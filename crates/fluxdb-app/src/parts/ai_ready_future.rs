use std::task::{Context, Poll, Wake, Waker};

/// 就绪 future 的最小阻塞器。
///
/// 设计 §6 明确 app 在生产中不引入 tokio；调用方必须提供不依赖外部 IO 反应堆的
/// future。超出有限轮询仍 Pending 时返回错误，不自旋等待或假装已经完成。
pub fn block_on_ready<F: std::future::Future>(future: F) -> Result<F::Output, fluxdb_core::Error> {
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
