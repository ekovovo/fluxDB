// stderr 日志：stdout 必须只承载 JSON-RPC 帧，任何欢迎语都会污染协议。
use tracing_subscriber::{EnvFilter, fmt};

/// 初始化日志到 stderr；`FLUXDB_MCP_LOG` 控制级别，默认 `info`。
pub fn init_stderr_logging() {
    let filter =
        EnvFilter::try_from_env("FLUXDB_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
