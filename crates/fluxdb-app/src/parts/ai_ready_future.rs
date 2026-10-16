// 兼容别名：执行器已升级为在真实 waker 上停放/唤醒（见 `ai_executor.rs`）。
//
// 保留此模块名是因为多处历史调用点仍写着 `block_on_ready`；新代码应优先使用
// `ai_executor::block_on` / `block_on_cancellable`，需要取消语义的调用点尤其
// 必须迁移，否则等待期间取消不会生效。

/// 不带取消语义的阻塞等待（等价于 [`crate::ai_executor::block_on`]）。
pub fn block_on_ready<F: std::future::Future>(future: F) -> Result<F::Output, fluxdb_core::Error> {
    crate::ai_executor::block_on(future)
}
