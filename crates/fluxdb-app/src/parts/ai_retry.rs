// 模型请求的有限重试（设计 §18.1）。
//
// 只重试**瞬时故障**：网络层失败、超时、408/429/5xx。鉴权失败、参数/协议不合法、
// 载荷超限与用户取消都是确定性失败，重试只会把已注定的等待再乘一遍。
//
// 判定口径在 `fluxdb_core::Error::is_retryable`，因此调用方必须保留错误类别
// （`ErrorKind` / `http_status`）：把传输错误一律降级成 `Internal` 会让
// 超时与上游故障都变成"不可重试"。

/// 单次请求的最大尝试次数（含首次）：2 表示最多重试 1 次。
pub const MAX_REQUEST_ATTEMPTS: usize = 2;

/// 退避基数与上限（毫秒）。比 Pi 复刻的 `0.5s→8s` 收敛得多：重试只是兜底，
/// 不值得让运行时 actor 为退避长时间空转。
const RETRY_BACKOFF_BASE_MS: u64 = 200;
const RETRY_BACKOFF_CAP_MS: u64 = 2_000;

/// 按错误类别重试一次模型/网络请求。
///
/// `call` 每次都会真实执行，因此每次尝试都各自计量、各自计入熔断——重试不绕过任何一侧。
/// 调用方必须保证一次失败的尝试没有对外产生可见副作用（例如已累计到会话的正文）；
/// 缓冲整段响应再返回的 Provider 满足该前提，边收边落事实的调用方不能直接套用。
pub fn retry_model_call<T>(
    cancel: &fluxdb_core::CancelFlag,
    mut call: impl FnMut() -> fluxdb_core::Result<T>,
) -> fluxdb_core::Result<T> {
    let mut attempt = 0usize;
    loop {
        let is_last = attempt + 1 >= MAX_REQUEST_ATTEMPTS;
        let error = match call() {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if is_last || cancel.is_cancelled() || !error.is_retryable() {
            return Err(error);
        }
        attempt += 1;
        if !backoff_before_retry(attempt, cancel) {
            return Err(error);
        }
    }
}

/// 退避等待；返回 `false` 表示等待途中被取消，不应再发下一次请求。
fn backoff_before_retry(attempt: usize, cancel: &fluxdb_core::CancelFlag) -> bool {
    let base = RETRY_BACKOFF_BASE_MS.saturating_mul(1u64 << (attempt - 1).min(4));
    let capped = base.min(RETRY_BACKOFF_CAP_MS);
    // 抖动 ±25%，避免多轮同时重试打在同一时刻。
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter)
        + jitter.saturating_mul(u64::from(uuid::Uuid::new_v4().as_u128() as u8 % 2));
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(delay);
    // 分片睡眠以便及时响应取消；这是 runtime actor 线程，只能短暂占用。
    while std::time::Instant::now() < deadline {
        if cancel.is_cancelled() {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    !cancel.is_cancelled()
}
