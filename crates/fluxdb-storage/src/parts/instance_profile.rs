// 进程实例标识（profile）：让同一台机器上的多个 FluxDB 实例互相隔离、可并行运行。
//
// 标识来自环境变量 `FLUXDB_PROFILE`，未设置或为空时是默认实例，目录名与单实例锁
// 保持历史布局不变，现有用户数据不受影响。设置后，持久化根目录、日志目录与单实例
// 锁文件都追加 `-标识` 后缀，因此多个实例各自持有独立的配置、连接、历史与凭据，
// 互不阻塞也互不覆盖。Windows 侧的命名互斥量同样按标识区分。
//
// 标识会直接进入文件系统路径与 Windows 命名对象名，因此只接受 ASCII 字母、数字、
// `-` 和 `_`：其他字符可能写到预期之外的位置，或构造出非法的互斥量名。

use std::ffi::OsStr;

use crate::{Error, ErrorKind, Result};

/// 读取实例标识的环境变量名。
pub const PROFILE_ENV: &str = "FLUXDB_PROFILE";

/// 当前进程的实例标识；`None` 表示默认实例（与历史布局一致）。
///
/// 非法取值返回 Err，由启动路径给出可见错误并退出：静默回退到默认标识会让用户
/// 以为在用隔离实例，实际却写进了默认实例的数据目录。
pub fn instance_profile() -> Result<Option<String>> {
    parse_profile(std::env::var_os(PROFILE_ENV).as_deref())
}

/// 标识解析与校验。空值等价于未设置（默认实例），这样 `FLUXDB_PROFILE=` 与不设置
/// 行为一致，不会意外切出一个名为 `fluxdb-` 的目录。
fn parse_profile(raw: Option<&OsStr>) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let profile = raw.to_string_lossy().trim().to_string();
    if profile.is_empty() {
        return Ok(None);
    }
    let valid = profile
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("{PROFILE_ENV} 取值非法（只允许 ASCII 字母、数字、'-' 和 '_'）：{profile}"),
        ));
    }
    Ok(Some(profile))
}

/// 应用目录名（不含父目录）：默认实例沿用 `default_name`，设置标识后追加 `-标识`。
/// 持久化根目录与 Linux 日志目录共用，保证同一实例的目录前缀一致。
pub(crate) fn app_dir_name(default_name: &str) -> Result<String> {
    Ok(match instance_profile()? {
        Some(profile) => format!("{default_name}-{profile}"),
        None => default_name.to_string(),
    })
}
