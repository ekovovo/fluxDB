// 启动参数（设计 §28.5.2）。参数**不含**任何数据库密码、API Key、任意 SQL 或未授权 URI。
use std::path::PathBuf;

use fluxdb_storage::FileStorage;

pub const USAGE: &str = "\
fluxdb-mcp —— fluxDB 对外本地 MCP Server（stdio）

用法:
    fluxdb-mcp --server-profile <id> [--config-root <dir>] [--transport stdio]

选项:
    --server-profile <id>   要服务的 Server Profile ID（必填；Profile 不存在/禁用则拒绝）
    --config-root <dir>     隔离的配置根；默认复用 FileStorage 平台目录
    --transport <stdio>     传输方式，当前仅支持 stdio（默认）
    -h, --help              显示本帮助
";

/// 解析后的启动参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub server_profile: String,
    pub config_root: Option<PathBuf>,
}

pub enum Parsed {
    Serve(Args),
    Help,
}

/// 解析命令行；未知参数直接报错（不做"忽略未知参数"的宽松解析）。
pub fn parse_cli<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut server_profile: Option<String> = None;
    let mut config_root: Option<PathBuf> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--server-profile" => {
                let value = iter.next().ok_or("--server-profile 缺少取值")?;
                server_profile = Some(value);
            }
            "--config-root" => {
                let value = iter.next().ok_or("--config-root 缺少取值")?;
                config_root = Some(PathBuf::from(value));
            }
            "--transport" => {
                let value = iter.next().ok_or("--transport 缺少取值")?;
                if value != "stdio" {
                    return Err(format!("暂不支持的传输方式: {value}（当前仅支持 stdio）"));
                }
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    let server_profile = server_profile.ok_or("必须提供 --server-profile <id>")?;
    Ok(Parsed::Serve(Args {
        server_profile,
        config_root,
    }))
}

impl Args {
    /// 解析配置根：显式 `--config-root` 优先，否则复用平台默认目录。
    pub fn storage(&self) -> Result<FileStorage, String> {
        match &self.config_root {
            Some(root) => Ok(FileStorage::new(root)),
            None => FileStorage::default_root()
                .map(FileStorage::new)
                .map_err(|error| format!("无法解析默认配置根: {}", error.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_profile_and_root() {
        let parsed = parse_cli(args(&[
            "--server-profile",
            "p1",
            "--config-root",
            "/tmp/x",
            "--transport",
            "stdio",
        ]))
        .unwrap();
        match parsed {
            Parsed::Serve(args) => {
                assert_eq!(args.server_profile, "p1");
                assert_eq!(args.config_root, Some(PathBuf::from("/tmp/x")));
            }
            Parsed::Help => panic!("应为 Serve"),
        }
    }

    #[test]
    fn missing_profile_is_rejected() {
        assert!(parse_cli(args(&["--config-root", "/tmp/x"])).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse_cli(args(&["--password", "x"])).is_err());
    }

    #[test]
    fn non_stdio_transport_is_rejected() {
        assert!(parse_cli(args(&["--server-profile", "p", "--transport", "http"])).is_err());
    }
}
