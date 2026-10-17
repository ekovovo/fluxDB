// 本地 MCP Server 的「一键注册到各 CLI 客户端」（设计 §28.5、§39.1 派生能力）。
//
// 目标：像 Pencil 扩展那样，把 `fluxdb-mcp` 这个 stdio Server 注册进用户已安装的
// Codex CLI / Claude Code 的**用户级配置**，用户无需手写 JSON/TOML。硬约束：
// 1. **注册可控**：显式接入，启动只维护已选择且归属核对通过的客户端。
// 2. **可回滚**：注册/卸载只增删本应用拥有的 server 条目，不触碰其它 server。
// 3. **写入前备份、原子替换、失败不半写**：临时文件 + rename；备份保留原内容。
// 4. **不猜格式**：目标文件若无法解析，报错退出，绝不改写未知结构。
// 5. **不含秘密**：配置里只有二进制绝对路径、profile ID、config-root，无密码/API Key。
//
// 传输固定 stdio（设计 §28.5：本地单机、无监听面、无需端口/token）。

// 本文件以 include! 引入 crate 根作用域：Path/PathBuf 已在 lib.rs 顶部导入。

/// 已适配配置格式的 CLI 客户端；未适配的客户端使用通用 stdio 启动信息。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpClientKind {
    /// Codex CLI：`~/.codex/config.toml` 的 `[mcp_servers.<name>]`。
    Codex,
    /// Claude Code：`~/.claude.json` 的 `mcpServers.<name>`（另可选写 allow 规则）。
    ClaudeCode,
}

impl McpClientKind {
    pub fn all() -> &'static [McpClientKind] {
        &[McpClientKind::Codex, McpClientKind::ClaudeCode]
    }

    /// 稳定小写标识（UI/命令/状态跨层传递用，不用 Debug 文本）。
    pub fn key(self) -> &'static str {
        match self {
            McpClientKind::Codex => "codex",
            McpClientKind::ClaudeCode => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            McpClientKind::Codex => "Codex CLI",
            McpClientKind::ClaudeCode => "Claude Code",
        }
    }

    /// 该客户端的用户级配置文件路径（跨平台：用 home 目录拼接，不硬编码分隔符）。
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            McpClientKind::Codex => home.join(".codex").join("config.toml"),
            McpClientKind::ClaudeCode => home.join(".claude.json"),
        }
    }

}

/// 解析当前用户主目录；失败时给出可读错误（不猜路径）。
pub fn home_dir() -> fluxdb_core::Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| {
        fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "无法解析用户主目录，无法定位客户端配置",
        )
    })
}

/// 由稳定小写标识解析客户端类型；未知标识返回 `None`。
pub fn client_from_key(key: &str) -> Option<McpClientKind> {
    McpClientKind::all()
        .iter()
        .copied()
        .find(|client| client.key() == key)
}

/// 默认注册的 server 名（用户级配置里的键）。
pub const DEFAULT_MCP_SERVER_NAME: &str = "fluxdb";

/// 默认 Server Profile ID（自动为“一键注册”创建一个只读、空授权的 Profile）。
pub const DEFAULT_MCP_PROFILE_ID: &str = "fluxdb.local";

/// 解析随桌面端一起打包的 `fluxdb-mcp` 二进制路径：优先取当前可执行文件的同级目录。
///
/// 未打包独立 Server 时复用桌面可执行文件的 `--mcp-server` 入口，不猜测开发机目录。
pub fn bundled_mcp_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    mcp_binary_next_to(&exe)
}

fn is_desktop_binary(exe: &Path) -> bool {
    matches!(
        exe.file_stem().and_then(|name| name.to_str()),
        Some("fluxdb-desktop" | "fluxdb")
    )
}

fn mcp_binary_next_to(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let candidate = dir.join(if cfg!(windows) {
        "fluxdb-mcp.exe"
    } else {
        "fluxdb-mcp"
    });
    if candidate.is_file() {
        Some(candidate)
    } else {
        (is_desktop_binary(exe) && exe.is_file()).then(|| exe.to_path_buf())
    }
}

/// 一次注册的目标描述（由 UI 传入，全部为非敏感信息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpRegistration {
    /// 写入客户端配置的 server 名（如 `fluxdb`）；同时用于 Claude 的 allow 规则前缀。
    pub server_name: String,
    /// 独立 MCP Server 或支持 `--mcp-server` 的桌面程序绝对路径。
    pub binary_path: PathBuf,
    /// Server Profile ID。
    pub profile_id: String,
    /// 可选隔离配置根；`None` 表示 Server 复用平台默认存储目录。
    pub config_root: Option<PathBuf>,
}

impl McpRegistration {
    /// 校验注册输入：路径必须绝对、名称/ID 非空、名称不含会破坏配置的字符。
    pub fn validate(&self) -> fluxdb_core::Result<()> {
        let err = |message: &str| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message.to_string())
        };
        if self.server_name.trim().is_empty() || self.server_name.len() > 64 {
            return Err(err("MCP server 名称不能为空且不超过 64 字符"));
        }
        if !self
            .server_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(err("MCP server 名称仅允许字母、数字、连字符与下划线"));
        }
        if self.profile_id.trim().is_empty() {
            return Err(err("Server Profile ID 不能为空"));
        }
        if !self.binary_path.is_absolute() {
            return Err(err("fluxdb-mcp 必须使用绝对路径"));
        }
        if let Some(root) = &self.config_root {
            if !root.is_absolute() {
                return Err(err("config-root 必须是绝对路径"));
            }
        }
        Ok(())
    }

    /// 该 server 在客户端里的启动命令与参数（stdio）。
    fn command_args(&self) -> (String, Vec<String>) {
        let mut args = Vec::new();
        if is_desktop_binary(&self.binary_path) {
            args.push("--mcp-server".to_string());
        }
        args.extend(["--server-profile".to_string(), self.profile_id.clone()]);
        if let Some(root) = &self.config_root {
            args.push("--config-root".to_string());
            args.push(root.to_string_lossy().into_owned());
        }
        (self.binary_path.to_string_lossy().into_owned(), args)
    }
}

/// UI 展示用的客户端注册状态投影（不含 PathBuf 之外的内部类型）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpInstallClientStatus {
    /// 稳定 key：`codex` / `claude`。
    pub key: String,
    /// 客户端展示名。
    pub label: String,
    /// 配置文件路径（展示 + 复制用）。
    pub config_path: String,
    /// 配置文件是否存在（该客户端是否可能已安装）。
    pub config_exists: bool,
    /// 是否已注册本应用的 server。
    pub registered: bool,
    pub managed: bool,
    /// 客户端里的同名条目与本应用将写入的启动命令一致；此时「卸载」才是对应用例行的撤销。
    pub up_to_date: bool,
    pub error: Option<String>,
}

/// 写入结果事实（供 UI 展示路径与备份位置）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientWriteFact {
    pub client: McpClientKind,
    pub config_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    /// 是否真的改动了文件（幂等重复注册/卸载时为 false）。
    pub changed: bool,
}

/// 写入指定客户端；调用方必须先核对同名条目的归属。
pub fn register(
    client: McpClientKind,
    home: &Path,
    registration: &McpRegistration,
) -> fluxdb_core::Result<ClientWriteFact> {
    registration.validate()?;
    let config_path = client.config_path(home);
    let existing = read_optional(&config_path)?;
    match client {
        McpClientKind::Codex => register_codex(&config_path, existing.as_deref(), registration),
        McpClientKind::ClaudeCode => {
            register_claude(&config_path, existing.as_deref(), registration)
        }
    }
}

/// 从指定客户端卸载（只删本应用的 server 条目；文件不存在视为已完成）。
pub fn unregister(
    client: McpClientKind,
    home: &Path,
    server_name: &str,
) -> fluxdb_core::Result<ClientWriteFact> {
    let config_path = client.config_path(home);
    let existing = read_optional(&config_path)?;
    match client {
        McpClientKind::Codex => unregister_codex(&config_path, existing.as_deref(), server_name),
        McpClientKind::ClaudeCode => {
            unregister_claude(&config_path, existing.as_deref(), server_name)
        }
    }
}

fn read_optional(path: &Path) -> fluxdb_core::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(write_error(format!(
            "读取 {} 失败: {error}",
            path.display()
        ))),
    }
}

fn write_error(message: impl Into<String>) -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message.into())
}

// ---- Codex（TOML，保留注释/格式，只改 [mcp_servers.<name>]）----

fn register_codex(
    config_path: &Path,
    existing: Option<&str>,
    registration: &McpRegistration,
) -> fluxdb_core::Result<ClientWriteFact> {
    // 空文件/不存在都按空文档处理；有内容但解析失败则拒绝（不猜结构）。
    let source = existing.unwrap_or("");
    let mut doc = if source.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        source.parse::<toml_edit::DocumentMut>().map_err(|error| {
            write_error(format!(
                "{} 不是合法 TOML，拒绝改写: {error}",
                config_path.display()
            ))
        })?
    };
    let (command, args) = registration.command_args();
    let before = doc.to_string();
    if !doc.contains_key("mcp_servers") {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| write_error("config.toml 里的 mcp_servers 不是表，拒绝改写"))?;
    if !servers.contains_key(&registration.server_name) {
        servers[&registration.server_name] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let entry = servers[&registration.server_name]
        .as_table_like_mut()
        .ok_or_else(|| write_error("同名 MCP 条目不是表，拒绝改写"))?;
    entry.insert("command", toml_edit::value(command));
    let mut arg_array = toml_edit::Array::new();
    for arg in args {
        arg_array.push(arg);
    }
    entry.insert("args", toml_edit::value(arg_array));
    let after = doc.to_string();
    if before == after {
        return Ok(ClientWriteFact {
            client: McpClientKind::Codex,
            config_path: config_path.to_path_buf(),
            backup_path: None,
            changed: false,
        });
    }
    let backup = write_atomic(config_path, &after)?;
    Ok(ClientWriteFact {
        client: McpClientKind::Codex,
        config_path: config_path.to_path_buf(),
        backup_path: backup,
        changed: true,
    })
}

fn unregister_codex(
    config_path: &Path,
    existing: Option<&str>,
    server_name: &str,
) -> fluxdb_core::Result<ClientWriteFact> {
    let Some(source) = existing else {
        return Ok(ClientWriteFact {
            client: McpClientKind::Codex,
            config_path: config_path.to_path_buf(),
            backup_path: None,
            changed: false,
        });
    };
    let mut doc = source.parse::<toml_edit::DocumentMut>().map_err(|error| {
        write_error(format!(
            "{} 不是合法 TOML，拒绝改写: {error}",
            config_path.display()
        ))
    })?;
    let removed = doc
        .get_mut("mcp_servers")
        .and_then(|m| m.as_table_mut())
        .and_then(|table| table.remove(server_name))
        .is_some();
    if !removed {
        return Ok(ClientWriteFact {
            client: McpClientKind::Codex,
            config_path: config_path.to_path_buf(),
            backup_path: None,
            changed: false,
        });
    }
    // 空 mcp_servers 表保留为 `[mcp_servers]`，避免删到父键影响其它内容。
    let backup = write_atomic(config_path, &doc.to_string())?;
    Ok(ClientWriteFact {
        client: McpClientKind::Codex,
        config_path: config_path.to_path_buf(),
        backup_path: backup,
        changed: true,
    })
}

// ---- Claude Code（JSON，保留其它键，只改 mcpServers.<name>）----

fn register_claude(
    config_path: &Path,
    existing: Option<&str>,
    registration: &McpRegistration,
) -> fluxdb_core::Result<ClientWriteFact> {
    let source = existing.unwrap_or("");
    let mut root: serde_json::Value = if source.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(source).map_err(|error| {
            write_error(format!(
                "{} 不是合法 JSON，拒绝改写: {error}",
                config_path.display()
            ))
        })?
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| write_error("~/.claude.json 根不是对象，拒绝改写"))?;
    let (command, args) = registration.command_args();
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| write_error("mcpServers 不是对象，拒绝改写"))?;
    let entry = servers
        .entry(registration.server_name.clone())
        .or_insert_with(|| serde_json::json!({"type":"stdio"}))
        .as_object_mut()
        .ok_or_else(|| write_error("同名 MCP 条目不是对象，拒绝改写"))?;
    entry.insert("command".into(), serde_json::json!(command));
    entry.insert("args".into(), serde_json::json!(args));
    // enabledMcpServers：Claude Code 用它记录启停；注册即可用。
    let enabled = object
        .entry("enabledMcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(enabled) = enabled.as_object_mut() {
        enabled.insert(
            registration.server_name.clone(),
            serde_json::Value::Bool(true),
        );
    }
    let rendered = serde_json::to_string_pretty(&root)
        .map_err(|error| write_error(format!("序列化配置失败: {error}")))?;
    let before = serde_json::to_string_pretty(
        &serde_json::from_str::<serde_json::Value>(source).unwrap_or(serde_json::json!({})),
    )
    .unwrap_or_default();
    let backup = if before != rendered {
        write_atomic(config_path, &rendered)?
    } else {
        None
    };
    Ok(ClientWriteFact {
        client: McpClientKind::ClaudeCode,
        config_path: config_path.to_path_buf(),
        backup_path: backup,
        changed: before != rendered,
    })
}

fn unregister_claude(
    config_path: &Path,
    existing: Option<&str>,
    server_name: &str,
) -> fluxdb_core::Result<ClientWriteFact> {
    let Some(source) = existing else {
        return Ok(ClientWriteFact {
            client: McpClientKind::ClaudeCode,
            config_path: config_path.to_path_buf(),
            backup_path: None,
            changed: false,
        });
    };
    let mut root: serde_json::Value = serde_json::from_str(source).map_err(|error| {
        write_error(format!(
            "{} 不是合法 JSON，拒绝改写: {error}",
            config_path.display()
        ))
    })?;
    let object = root
        .as_object_mut()
        .ok_or_else(|| write_error("~/.claude.json 根不是对象，拒绝改写"))?;
    let mut changed = false;
    if let Some(servers) = object.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        changed |= servers.remove(server_name).is_some();
    }
    if let Some(enabled) = object
        .get_mut("enabledMcpServers")
        .and_then(|s| s.as_object_mut())
    {
        changed |= enabled.remove(server_name).is_some();
    }
    if !changed {
        return Ok(ClientWriteFact {
            client: McpClientKind::ClaudeCode,
            config_path: config_path.to_path_buf(),
            backup_path: None,
            changed: false,
        });
    }
    let rendered = serde_json::to_string_pretty(&root)
        .map_err(|error| write_error(format!("序列化配置失败: {error}")))?;
    let backup = write_atomic(config_path, &rendered)?;
    Ok(ClientWriteFact {
        client: McpClientKind::ClaudeCode,
        config_path: config_path.to_path_buf(),
        backup_path: backup,
        changed: true,
    })
}

// ---- 原子写 + 备份 ----

/// 原子写：先写同目录临时文件，fsync 后 rename 覆盖；覆盖前把原文件复制为 `.bak`。
///
/// 返回备份路径（原文件不存在时为 `None`）。失败不留下半写文件。
fn write_atomic(path: &Path, content: &str) -> fluxdb_core::Result<Option<PathBuf>> {
    let parent = path
        .parent()
        .ok_or_else(|| write_error(format!("{} 缺少父目录", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| write_error(format!("创建目录 {} 失败: {error}", parent.display())))?;
    // 备份现有内容（非符号链接的正常文件才备份）。
    let backup = if path.is_file() {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(write_error("拒绝改写符号链接配置文件"));
            }
            Ok(_) => {}
            Err(error) => return Err(write_error(format!("读取属性失败: {error}"))),
        }
        let backup_path = {
            let mut name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("config")
                .to_string();
            name.push_str(".fluxdb.bak");
            path.with_file_name(name)
        };
        std::fs::copy(path, &backup_path)
            .map_err(|error| write_error(format!("备份 {} 失败: {error}", path.display())))?;
        Some(backup_path)
    } else {
        None
    };
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| write_error("配置文件名非法"))?;
    let temp = parent.join(format!(".{file_name}.fluxdb.{}.tmp", uuid::Uuid::new_v4()));
    let permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let write = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        // 客户端配置可能含其它服务的凭据，原子替换不能放宽原文件权限。
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(write_error(format!("写入临时文件失败: {error}")));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(write_error(format!(
            "替换 {} 失败: {error}",
            path.display()
        )));
    }
    Ok(backup)
}

#[cfg(test)]
include!("tests/ai_mcp_install.rs");
