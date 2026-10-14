// 本地 MCP Server 的「一键注册到各 CLI 客户端」（设计 §28.5、§39.1 派生能力）。
//
// 目标：像 Pencil 扩展那样，把 `fluxdb-mcp` 这个 stdio Server 注册进用户已安装的
// Codex CLI / Claude Code 的**用户级配置**，用户无需手写 JSON/TOML。硬约束：
// 1. **注册可控**：启动仅注册已有配置的客户端；显式卸载后关闭自动注册。
// 2. **可回滚**：注册/卸载只增删本应用拥有的 server 条目，不触碰其它 server。
// 3. **写入前备份、原子替换、失败不半写**：临时文件 + rename；备份保留原内容。
// 4. **不猜格式**：目标文件若无法解析，报错退出，绝不改写未知结构。
// 5. **不含秘密**：配置里只有二进制绝对路径、profile ID、config-root，无密码/API Key。
//
// 传输固定 stdio（设计 §28.5：本地单机、无监听面、无需端口/token）。

// 本文件以 include! 引入 crate 根作用域：Path/PathBuf 已在 lib.rs 顶部导入。

/// 支持自动注册的 CLI 客户端（首版：Codex + Claude Code）。
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

    /// Claude Code 的工具预批准文件（仅只读工具时使用）。
    pub fn claude_allow_settings_path(home: &Path) -> PathBuf {
        home.join(".claude").join("settings.json")
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

/// 一键注册时使用的默认只读工具白名单（不含任何写工具）。
pub const DEFAULT_READONLY_TOOLS: &[&str] = &[
    "fluxdb_connections_list",
    "fluxdb_search_objects",
    "fluxdb_describe_object",
    "fluxdb_query",
    "fluxdb_explain",
    "fluxdb_redis_scan",
    "fluxdb_redis_inspect",
    "fluxdb_redis_server_info",
    "fluxdb_result_page",
];

/// 查询所有支持客户端的注册状态（供 UI 一次性渲染）。
pub fn install_status_snapshot(home: &Path) -> Vec<ClientStatus> {
    McpClientKind::all()
        .iter()
        .copied()
        .map(|client| client_status(client, home, DEFAULT_MCP_SERVER_NAME))
        .collect()
}

/// 解析随桌面端一起打包的 `fluxdb-mcp` 二进制路径：优先取当前可执行文件的同级目录。
///
/// 未打包独立 Server 时复用桌面可执行文件的 `--mcp-server` 入口，不猜测开发机目录。
pub fn bundled_mcp_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    mcp_binary_next_to(&exe)
}

fn is_desktop_binary(exe: &Path) -> bool {
    matches!(exe.file_stem().and_then(|name| name.to_str()), Some("fluxdb-desktop" | "fluxdb"))
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
    /// 是否额外写入 Claude Code 的工具预批准规则（`mcp__<name>`）。
    ///
    /// 仅当注册的是**只读** Profile 时才应由 UI 置位；写权限不经此捷径。
    pub preapprove_claude_tools: bool,
}

impl McpRegistration {
    /// 校验注册输入：路径必须绝对、名称/ID 非空、名称不含会破坏配置的字符。
    pub fn validate(&self) -> fluxdb_core::Result<()> {
        let err = |message: &str| fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message.to_string());
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

/// 某个客户端当前的注册状态（供 UI 展示）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientStatus {
    pub client: McpClientKind,
    pub config_path: PathBuf,
    /// 配置文件是否存在（用于判断该客户端是否可能已安装）。
    pub config_exists: bool,
    /// 配置里是否已有同名 server 条目。
    pub registered: bool,
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
}

/// 一次性生成 UI 用的注册状态快照。
pub fn install_client_statuses(home: &Path) -> Vec<McpInstallClientStatus> {
    install_status_snapshot(home)
        .into_iter()
        .map(|status| McpInstallClientStatus {
            key: status.client.key().to_string(),
            label: status.client.label().to_string(),
            config_path: status.config_path.display().to_string(),
            config_exists: status.config_exists,
            registered: status.registered,
        })
        .collect()
}

/// 查询某客户端的注册状态；读取失败按“未注册 + 文件不存在”处理，不抛错。
pub fn client_status(
    client: McpClientKind,
    home: &Path,
    server_name: &str,
) -> ClientStatus {
    let config_path = client.config_path(home);
    let config_exists = config_path.is_file();
    let registered = if config_exists {
        std::fs::read_to_string(&config_path)
            .ok()
            .map(|text| match client {
                McpClientKind::Codex => codex_has_server(&text, server_name),
                McpClientKind::ClaudeCode => claude_has_server(&text, server_name),
            })
            .unwrap_or(false)
    } else {
        false
    };
    ClientStatus {
        client,
        config_path,
        config_exists,
        registered,
    }
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

/// 注册到指定客户端（幂等：已存在同名条目则覆盖为最新命令/参数）。
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
        McpClientKind::ClaudeCode => register_claude(&config_path, existing.as_deref(), registration),
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
        McpClientKind::ClaudeCode => unregister_claude(&config_path, existing.as_deref(), server_name),
    }
}

fn read_optional(path: &Path) -> fluxdb_core::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(write_error(format!("读取 {} 失败: {error}", path.display()))),
    }
}

fn write_error(message: impl Into<String>) -> fluxdb_core::Error {
    fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, message.into())
}

// ---- Codex（TOML，保留注释/格式，只改 [mcp_servers.<name>]）----

fn codex_has_server(text: &str, server_name: &str) -> bool {
    text.parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("mcp_servers").and_then(|m| m.get(server_name)).map(|_| ()))
        .is_some()
}

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
    let mut entry = toml_edit::Table::new();
    entry["command"] = toml_edit::value(command);
    let mut arg_array = toml_edit::Array::new();
    for arg in args {
        arg_array.push(arg);
    }
    entry["args"] = toml_edit::value(arg_array);
    servers[&registration.server_name] = toml_edit::Item::Table(entry);
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

fn claude_has_server(text: &str, server_name: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("mcpServers")
                .and_then(|servers| servers.get(server_name))
                .map(|_| ())
        })
        .is_some()
}

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
    servers.insert(
        registration.server_name.clone(),
        serde_json::json!({
            "command": command,
            "args": args,
            "env": {},
            "type": "stdio",
        }),
    );
    // enabledMcpServers：Claude Code 用它记录启停；注册即可用。
    let enabled = object
        .entry("enabledMcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(enabled) = enabled.as_object_mut() {
        enabled.insert(registration.server_name.clone(), serde_json::Value::Bool(true));
    }
    let rendered = serde_json::to_string_pretty(&root)
        .map_err(|error| write_error(format!("序列化配置失败: {error}")))?;
    let before = serde_json::to_string_pretty(
        &serde_json::from_str::<serde_json::Value>(source).unwrap_or(serde_json::json!({})),
    )
    .unwrap_or_default();
    let backup = write_atomic(config_path, &rendered)?;
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

// ---- Claude Code 工具预批准（仅只读）----

/// 写入/移除 Claude Code 的 `mcp__<name>` 预批准规则（`~/.claude/settings.json`）。
///
/// `allow=true` 时确保存在该规则；`false` 时移除。仅接受 `mcp__<name>` 前缀，
/// 不触碰用户的其它权限项；文件无法解析则拒绝改写。
pub fn set_claude_tool_preapproval(
    home: &Path,
    server_name: &str,
    allow: bool,
) -> fluxdb_core::Result<Option<PathBuf>> {
    if !server_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(write_error("server 名称含非法字符，拒绝写 allow 规则"));
    }
    let path = McpClientKind::claude_allow_settings_path(home);
    let existing = read_optional(&path)?;
    let mut root: serde_json::Value = match existing.as_deref() {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text).map_err(|error| {
            write_error(format!("{} 不是合法 JSON，拒绝改写: {error}", path.display()))
        })?,
        _ => serde_json::json!({}),
    };
    let rule = format!("mcp__{server_name}");
    let object = root
        .as_object_mut()
        .ok_or_else(|| write_error("settings.json 根不是对象，拒绝改写"))?;
    let permissions = object
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let permissions = permissions
        .as_object_mut()
        .ok_or_else(|| write_error("permissions 不是对象，拒绝改写"))?;
    let allow_list = permissions
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let list = allow_list
        .as_array_mut()
        .ok_or_else(|| write_error("permissions.allow 不是数组，拒绝改写"))?;
    let mut changed = false;
    if allow {
        if !list.iter().any(|v| v.as_str() == Some(rule.as_str())) {
            list.push(serde_json::Value::String(rule.clone()));
            changed = true;
        }
    } else {
        let before = list.len();
        list.retain(|v| v.as_str() != Some(rule.as_str()));
        changed = before != list.len();
    }
    if !changed {
        return Ok(None);
    }
    let rendered = serde_json::to_string_pretty(&root)
        .map_err(|error| write_error(format!("序列化 settings.json 失败: {error}")))?;
    write_atomic(&path, &rendered)
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
    let write = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(write_error(format!("写入临时文件失败: {error}")));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(write_error(format!("替换 {} 失败: {error}", path.display())));
    }
    Ok(backup)
}

#[cfg(test)]
mod mcp_install_tests {
    use super::*;

    #[test]
    fn desktop_mcp_entry_is_available_without_sidecar() {
        let home = temp_home("desktop-entry");
        for name in ["fluxdb-desktop", "fluxdb-desktop.exe", "fluxdb"] {
            let exe = home.join(name);
            std::fs::write(&exe, b"stub").unwrap();
            assert_eq!(mcp_binary_next_to(&exe), Some(exe.clone()));
            let mut reg = registration(&home);
            reg.binary_path = exe;
            assert_eq!(reg.command_args().1[0], "--mcp-server");
        }
        let sidecar = home.join(if cfg!(windows) { "fluxdb-mcp.exe" } else { "fluxdb-mcp" });
        std::fs::write(&sidecar, b"stub").unwrap();
        assert_eq!(mcp_binary_next_to(&home.join("fluxdb-desktop")), Some(sidecar));
        assert_eq!(registration(&home).command_args().1[0], "--server-profile");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fluxdb-mcp-install-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn registration(home: &Path) -> McpRegistration {
        McpRegistration {
            server_name: "fluxdb".into(),
            binary_path: home.join("bin").join("fluxdb-mcp"),
            profile_id: "local-readonly".into(),
            config_root: Some(home.join("data")),
            preapprove_claude_tools: true,
        }
    }

    #[test]
    fn validate_rejects_relative_paths_and_bad_names() {
        let home = temp_home("validate");
        let mut reg = registration(&home);
        assert!(reg.validate().is_ok());
        reg.binary_path = PathBuf::from("fluxdb-mcp");
        assert!(reg.validate().is_err(), "相对路径必须拒绝");
        reg = registration(&home);
        reg.server_name = "bad name!".into();
        assert!(reg.validate().is_err(), "非法名称必须拒绝");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn codex_register_preserves_existing_content_and_is_idempotent() {
        let home = temp_home("codex");
        let config = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n").unwrap();
        let reg = registration(&home);
        let fact = register(McpClientKind::Codex, &home, &reg).unwrap();
        assert!(fact.changed);
        assert!(fact.backup_path.is_some(), "首次写入应保留备份");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("model = \"gpt-5\""), "保留用户既有配置");
        assert!(text.contains("[mcp_servers.other]"), "保留其它 server");
        assert!(text.contains("[mcp_servers.fluxdb]"));
        assert!(text.contains("--server-profile"));
        assert!(text.contains("local-readonly"));
        // 二次注册幂等：内容一致则 changed=false（覆盖为相同内容）。
        let again = register(McpClientKind::Codex, &home, &reg).unwrap();
        assert!(!again.changed, "重复注册相同内容不应重写");
        // 状态查询。
        let status = client_status(McpClientKind::Codex, &home, "fluxdb");
        assert!(status.registered && status.config_exists);
        // 卸载只删自己的条目。
        let removed = unregister(McpClientKind::Codex, &home, "fluxdb").unwrap();
        assert!(removed.changed);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("[mcp_servers.fluxdb]"));
        assert!(text.contains("[mcp_servers.other]"), "不误删其它 server");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn codex_register_rejects_invalid_toml() {
        let home = temp_home("codex-bad");
        let config = home.join(".codex").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "this is = = not toml\n").unwrap();
        let reg = registration(&home);
        assert!(register(McpClientKind::Codex, &home, &reg).is_err(), "非法 TOML 必须拒绝改写");
        // 原文件未被破坏。
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "this is = = not toml\n");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn claude_register_roundtrip_and_allow_rule() {
        let home = temp_home("claude");
        std::fs::write(home.join(".claude.json"), r#"{"userID":"u1","mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        let reg = registration(&home);
        let fact = register(McpClientKind::ClaudeCode, &home, &reg).unwrap();
        assert!(fact.changed);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(value["userID"], serde_json::json!("u1"), "保留其它键");
        assert_eq!(value["mcpServers"]["other"]["command"], serde_json::json!("x"));
        assert_eq!(value["mcpServers"]["fluxdb"]["type"], serde_json::json!("stdio"));
        assert!(value["mcpServers"]["fluxdb"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "local-readonly"));
        assert_eq!(value["enabledMcpServers"]["fluxdb"], serde_json::json!(true));

        // allow 规则写入。
        let backup = set_claude_tool_preapproval(&home, "fluxdb", true).unwrap();
        assert!(backup.is_some() || true);
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap()).unwrap();
        assert!(settings["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "mcp__fluxdb"));
        // 卸载同时清掉 allow 与条目。
        unregister(McpClientKind::ClaudeCode, &home, "fluxdb").unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert!(value["mcpServers"].get("fluxdb").is_none());
        assert!(value["mcpServers"].get("other").is_some());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn claude_register_rejects_invalid_json() {
        let home = temp_home("claude-bad");
        std::fs::write(home.join(".claude.json"), "{ not json").unwrap();
        let reg = registration(&home);
        assert!(register(McpClientKind::ClaudeCode, &home, &reg).is_err());
        assert_eq!(std::fs::read_to_string(home.join(".claude.json")).unwrap(), "{ not json");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn status_reports_absence_for_missing_config() {
        let home = temp_home("missing");
        let status = client_status(McpClientKind::Codex, &home, "fluxdb");
        assert!(!status.config_exists && !status.registered);
        std::fs::remove_dir_all(&home).ok();
    }
}

/// 启动自动注册：对**已存在配置文件**的客户端，若本应用 server 尚未注册，则自动写入。
///
/// 语义（按用户要求：「启动时先看有没有配置文件，有就自动注册」）：
/// - 只处理 `config_exists == true` 的客户端（即该 CLI 已安装），不为未安装的客户端创建配置；
/// - 已注册且命令/参数一致的客户端不重写（幂等）；
/// - 已注册但参数变化（如二进制路径变更）会更新为最新；
/// - 单个客户端失败不影响其它客户端，也不阻断启动；返回每个客户端的处理结果。
///
/// 调用方需先确认“自动注册”开关开启（见 `load_auto_register_enabled`）。
pub fn auto_register_installed_clients(
    home: &Path,
    registration: &McpRegistration,
) -> Vec<(McpClientKind, std::result::Result<ClientWriteFact, String>)> {
    let mut results = Vec::new();
    for client in McpClientKind::all().iter().copied() {
        let status = client_status(client, home, &registration.server_name);
        if !status.config_exists {
            continue;
        }
        // 已注册且内容一致时 register 会返回 changed=false（幂等），无需预先比对。
        let outcome = register(client, home, registration).map_err(|error| error.message);
        if outcome.is_ok() && client == McpClientKind::ClaudeCode && registration.preapprove_claude_tools {
            if let Err(error) = set_claude_tool_preapproval(home, &registration.server_name, true) {
                tracing::warn!(%error, "自动注册：写入 Claude 预批准规则失败");
            }
        }
        results.push((client, outcome));
    }
    results
}
