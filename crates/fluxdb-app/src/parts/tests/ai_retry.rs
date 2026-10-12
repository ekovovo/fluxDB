// 模型请求重试层测试（设计 §18.1）。
//
// 全部用闭包计数，不发网络、不读配置：断言"是否重试"和"重试几次"，
// 而不是重试的耗时。

fn timeout_error() -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Timeout, "网络错误: 超时")
}

#[test]
fn ai_retry_retries_retryable_error_once_then_succeeds() {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancel = fluxdb_core::CancelFlag::new();
    let result = retry_model_call(&cancel, || {
        let attempt = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if attempt == 0 {
            Err(timeout_error())
        } else {
            Ok("ok")
        }
    });
    assert_eq!(result.expect("第二次尝试应成功"), "ok");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "瞬时超时应重试一次");
}

#[test]
fn ai_retry_does_not_retry_deterministic_failures() {
    for error in [
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Authentication, "鉴权失败"),
        fluxdb_core::Error::with_http_status(fluxdb_core::ErrorKind::Internal, "HTTP 401", 401),
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "响应格式错误"),
    ] {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let cancel = fluxdb_core::CancelFlag::new();
        let result: fluxdb_core::Result<()> = retry_model_call(&cancel, || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(error.clone())
        });
        assert!(result.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "确定性失败不得重试: {error:?}");
    }
}

#[test]
fn ai_retry_retries_429_and_5xx_but_not_cancelled() {
    for status in [429u16, 500, 503] {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let cancel = fluxdb_core::CancelFlag::new();
        let _: fluxdb_core::Result<()> = retry_model_call(&cancel, || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(fluxdb_core::Error::with_http_status(
                fluxdb_core::ErrorKind::Internal,
                format!("HTTP {status}"),
                status,
            ))
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "HTTP {status} 属瞬时故障");
    }

    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancel = fluxdb_core::CancelFlag::new();
    cancel.request();
    let _: fluxdb_core::Result<()> = retry_model_call(&cancel, || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(timeout_error())
    });
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "已取消不得重试");
}

/// 传输错误映射必须保留类别，否则 `is_retryable` 会把超时判成不可重试。
#[test]
fn transport_error_mapping_preserves_retryable_kind() {
    let timeout = TransportError::Timeout("超时".into()).to_core_error();
    assert_eq!(timeout.kind, fluxdb_core::ErrorKind::Timeout);
    assert!(timeout.is_retryable());
    assert_eq!(timeout.message, "网络错误: 超时");

    let network = TransportError::Network("连接失败".into()).to_core_error();
    assert_eq!(network.kind, fluxdb_core::ErrorKind::Connection);
    assert!(network.is_retryable());

    let throttled = TransportError::HttpStatus(429).to_core_error();
    assert!(throttled.is_retryable(), "429 应可重试");

    let unauthorized = TransportError::HttpStatus(401).to_core_error();
    assert_eq!(unauthorized.kind, fluxdb_core::ErrorKind::Authentication);
    assert!(!unauthorized.is_retryable());

    let malformed = TransportError::Malformed("不是合法 JSON".into()).to_core_error();
    assert!(!malformed.is_retryable());

    let cancelled = TransportError::Cancelled.to_core_error();
    assert_eq!(cancelled.kind, fluxdb_core::ErrorKind::Cancelled);
    assert!(!cancelled.is_retryable());
}
