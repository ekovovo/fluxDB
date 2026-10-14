// fluxdb-mcp：对外本地 MCP Server 的 stdio 入口（设计 §28.5，T058）。
//
// 职责边界：
// - 解析启动参数（--server-profile / --config-root / --transport），不读数据库密码；
// - 从存储根加载 Server Profile、资源授权与连接配置（凭据解密在连接器层）；
// - 装配 `McpExecutionService`（与内部 Agent 共用同一条执行链）；
// - over stdio 提供 initialize / tools/list / tools/call，stdout 只写协议，日志走 stderr；
// - 不启动 GPUI、不读模型 API Key/Jev/Agent prompt，不依赖桌面是否运行。

// 参数解析、日志与进程入口；业务在 parts/server.rs 与 parts/tools.rs。
mod parts;

use parts::cli::{Args, Parsed, parse_cli};
use parts::server::{FluxDbMcpServer, ServeTarget};

/// 运行 stdio MCP 进程入口，返回退出码；不得在调用前初始化 UI 或 stdout 日志。
pub fn run_cli(args: impl IntoIterator<Item = String>) -> i32 {
    // stdout 是协议通道：所有日志/诊断必须走 stderr，避免污染 JSON-RPC。
    parts::logging::init_stderr_logging();

    let args = match parse_cli(args) {
        Ok(Parsed::Serve(args)) => args,
        Ok(Parsed::Help) => {
            eprintln!("{}", parts::cli::USAGE);
            return 0;
        }
        Err(error) => {
            eprintln!("参数错误: {error}");
            eprintln!("{}", parts::cli::USAGE);
            return 2;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("无法启动运行时: {error}");
            return 1;
        }
    };
    let exit = runtime.block_on(async move { run(args).await });
    match exit {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("fluxdb-mcp 退出: {error}");
            1
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let target = ServeTarget::load(&args)?;
    tracing::info!(
        profile = %target.profile_id(),
        tools = target.tool_names().len(),
        "fluxdb-mcp stdio Server 启动"
    );
    let server = FluxDbMcpServer::new(target)?;
    parts::server::serve_stdio(server).await
}
