/// 通用 stdio 接入信息；不包含连接凭据或客户端原有配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConnectionDetails {
    pub command: String,
    pub args: Vec<String>,
    pub command_line: String,
    pub json_example: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpClientsSnapshot {
    pub clients: Vec<McpInstallClientStatus>,
    pub connection: Option<McpConnectionDetails>,
    pub connection_error: Option<String>,
}

impl McpRegistration {
    pub fn connection_details(&self) -> fluxdb_core::Result<McpConnectionDetails> {
        self.validate()?;
        let (command, args) = self.command_args();
        // Windows 输出 PowerShell 语法，Unix 输出 POSIX shell 语法；参数原值另行提供。
        let quote = |value: &str| {
            if cfg!(windows) {
                format!("'{}'", value.replace('\'', "''"))
            } else {
                format!("'{}'", value.replace('\'', "'\\''"))
            }
        };
        let command_line = std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ");
        let command_line = if cfg!(windows) {
            format!("& {command_line}")
        } else {
            command_line
        };
        let json_example = serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                &self.server_name: {"command":command, "args":args}
            }
        }))
        .map_err(|_| write_error("生成 MCP 配置示例失败"))?;
        Ok(McpConnectionDetails {
            command,
            args,
            command_line,
            json_example,
        })
    }

    fn launch(&self) -> fluxdb_storage::McpClientLaunch {
        let (command, args) = self.command_args();
        fluxdb_storage::McpClientLaunch { command, args }
    }
}

/// 把 Codex TOML 的 `[mcp_servers.<name>]` 条目转成 JSON 值，让两种配置格式
/// 共用同一份条目校验（`url` / `type` / `command` / `args`）。
fn codex_entry_to_json(entry: &toml_edit::Item) -> fluxdb_core::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for key in ["url", "type", "command"] {
        if let Some(value) = entry.get(key) {
            map.insert(
                key.to_string(),
                value
                    .as_str()
                    .map(|text| serde_json::Value::String(text.to_string()))
                    // 非字符串值同样要留下"该键存在"的痕迹，交由共享校验拒绝。
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    if let Some(args) = entry.get("args") {
        let items = args
            .as_array()
            .ok_or_else(|| write_error("MCP 参数不是数组"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(|text| serde_json::Value::String(text.to_string()))
                    .ok_or_else(|| write_error("MCP 参数必须是字符串"))
            })
            .collect::<fluxdb_core::Result<Vec<_>>>()?;
        map.insert("args".to_string(), serde_json::Value::Array(items));
    }
    Ok(serde_json::Value::Object(map))
}

/// 只提取非敏感的启动信息；绝不把客户端整个配置带入状态、日志或持久化记录。
///
/// `None` 表示没有同名条目；同名条目存在但不是合法 stdio 服务时返回错误，
/// 由调用方展示原因而不是当作"未注册"。
fn client_launch(
    client: McpClientKind,
    home: &Path,
) -> fluxdb_core::Result<Option<fluxdb_storage::McpClientLaunch>> {
    let Some(text) = read_optional(&client.config_path(home))? else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let entry = match client {
        McpClientKind::Codex => {
            let value = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| write_error("客户端配置不是合法 TOML，未改写"))?;
            if value
                .get("mcp_servers")
                .is_some_and(|v| v.as_table_like().is_none())
            {
                return Err(write_error("mcp_servers 不是表，未改写"));
            }
            let Some(entry) = value
                .get("mcp_servers")
                .and_then(|v| v.get(DEFAULT_MCP_SERVER_NAME))
            else {
                return Ok(None);
            };
            Some(codex_entry_to_json(entry)?)
        }
        McpClientKind::ClaudeCode => {
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|_| write_error("客户端配置不是合法 JSON，未改写"))?;
            if !value.is_object() || value.get("mcpServers").is_some_and(|v| !v.is_object()) {
                return Err(write_error("客户端配置或 mcpServers 不是对象，未改写"));
            }
            value
                .get("mcpServers")
                .and_then(|v| v.get(DEFAULT_MCP_SERVER_NAME))
                .cloned()
        }
    };
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.get("url").is_some()
        || entry
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v != "stdio")
    {
        return Err(write_error("同名 fluxdb 条目不是 stdio 服务，未改写"));
    }
    let command = entry
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| write_error("同名 fluxdb 条目缺少启动程序，未改写"))?
        .to_string();
    let args: Vec<String> = serde_json::from_value(
        entry
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|_| write_error("同名 fluxdb 条目的参数不是字符串数组，未改写"))?;
    Ok(Some(fluxdb_storage::McpClientLaunch { command, args }))
}

/// 列出全部已适配客户端的展示行（不含注册判定，由调用方用权威解析结果填充）。
fn install_client_statuses(home: &Path) -> Vec<McpInstallClientStatus> {
    McpClientKind::all()
        .iter()
        .copied()
        .map(|client| {
            let config_path = client.config_path(home);
            McpInstallClientStatus {
                key: client.key().to_string(),
                label: client.label().to_string(),
                config_path: config_path.display().to_string(),
                config_exists: config_path.is_file(),
                registered: false,
                managed: false,
                up_to_date: false,
                error: None,
            }
        })
        .collect()
}

pub fn mcp_clients_snapshot(
    storage: &fluxdb_storage::FileStorage,
    home: &Path,
    registration: fluxdb_core::Result<McpRegistration>,
) -> fluxdb_core::Result<McpClientsSnapshot> {
    let managed = storage.load_mcp_client_registrations()?;
    let desired = registration.as_ref().ok().map(|r| r.launch());
    let clients = install_client_statuses(home)
        .into_iter()
        .map(|mut status| {
            status.managed = managed.contains_key(&status.key);
            if let Some(client) = client_from_key(&status.key) {
                match client_launch(client, home) {
                    Ok(current) => {
                        status.registered = current.is_some();
                        status.up_to_date = desired.is_some() && current == desired;
                        if let Some(expected) = managed.get(&status.key) {
                            if current.as_ref().is_some_and(|v| v != expected) {
                                status.error = Some(
                                    "同名配置已被修改，已停止自动维护；可重新安装覆盖为本应用命令".into(),
                                );
                            }
                        }
                    }
                    Err(error) => status.error = Some(error.message),
                }
            }
            status
        })
        .collect();
    let (connection, connection_error) = match registration.and_then(|r| r.connection_details()) {
        Ok(details) => (Some(details), None),
        Err(error) => (None, Some(error.message)),
    };
    Ok(McpClientsSnapshot {
        clients,
        connection,
        connection_error,
    })
}

/// 显式安装即接管客户端配置里的同名键：写入本应用当前实例的启动命令并登记管理权。
///
/// 同名键是本应用在客户端配置中的固定槽位，用户点击安装表达的是「这里归 fluxdb」，
/// 因此不再与上次记录比对；无法解析的配置和同名非 stdio 条目（可能是用户自有的远端
/// 服务）仍然拒绝改写。
pub fn connect_mcp_client_registration(
    storage: &fluxdb_storage::FileStorage,
    home: &Path,
    client: McpClientKind,
    registration: &McpRegistration,
) -> fluxdb_core::Result<()> {
    registration.validate()?;
    let mut managed = storage.load_mcp_client_registrations()?;
    // 先解析同名条目：配置不可解析或同名条目不是 stdio 服务时拒绝改写。
    client_launch(client, home)?;
    register(client, home, registration)?;
    managed.insert(client.key().into(), registration.launch());
    storage.save_mcp_client_registrations(&managed)
}

/// 撤销接入：删除同名键并放弃管理权，其它 server 与客户端原有配置不动。
pub fn remove_mcp_client_registration(
    storage: &fluxdb_storage::FileStorage,
    home: &Path,
    client: McpClientKind,
) -> fluxdb_core::Result<()> {
    let mut managed = storage.load_mcp_client_registrations()?;
    unregister(client, home, DEFAULT_MCP_SERVER_NAME)?;
    managed.remove(client.key());
    storage.save_mcp_client_registrations(&managed)
}

/// 启动只更新已选择、仍存在且与上次写入一致的条目，不接管手动配置。
pub fn sync_mcp_client_registrations(
    storage: &fluxdb_storage::FileStorage,
    home: &Path,
    registration: &McpRegistration,
) -> fluxdb_core::Result<Vec<String>> {
    let mut managed = storage.load_mcp_client_registrations()?;
    let mut changed = Vec::new();
    let mut errors = Vec::new();
    for client in McpClientKind::all().iter().copied() {
        let Some(expected) = managed.get(client.key()).cloned() else {
            continue;
        };
        let result = (|| {
            let Some(current) = client_launch(client, home)? else {
                // 用户在客户端移除了条目，不在后台重新添加。
                return Ok(());
            };
            if current != expected {
                return Err(write_error("同名配置已改变，未自动覆盖"));
            }
            let fact = register(client, home, registration)?;
            managed.insert(client.key().into(), registration.launch());
            if fact.changed {
                changed.push(client.label().to_string());
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(client = client.key(), "MCP 客户端维护失败，保留原配置");
            errors.push(format!("{}：{}", client.label(), error.message));
        }
    }
    storage.save_mcp_client_registrations(&managed)?;
    if !errors.is_empty() {
        return Err(write_error(errors.join("；")));
    }
    Ok(changed)
}

impl AppController {
    fn mcp_registration(&self, binary_path: &str) -> fluxdb_core::Result<McpRegistration> {
        if binary_path.trim().is_empty() {
            return Err(write_error("找不到本地 MCP Server 可执行文件"));
        }
        let storage = self.ai_storage()?;
        let config_root = std::fs::canonicalize(storage.root_dir())
            .map_err(|_| write_error("无法解析 MCP 数据目录"))?;
        let registration = McpRegistration {
            server_name: DEFAULT_MCP_SERVER_NAME.into(),
            binary_path: PathBuf::from(binary_path),
            profile_id: DEFAULT_MCP_PROFILE_ID.into(),
            config_root: Some(config_root),
        };
        registration.validate()?;
        Ok(registration)
    }

    fn dispatch_mcp_clients(&mut self, command: AppCommand) -> AppEvent {
        let result = (|| {
            let storage = self.ai_storage()?;
            let home = home_dir()?;
            match command {
                AppCommand::RefreshMcpClients { binary_path } => {
                    // 首次打开时先初始化隔离存储根，生成的命令与实际注册使用同一份参数。
                    storage.load_mcp_client_registrations()?;
                    let snapshot =
                        mcp_clients_snapshot(&storage, &home, self.mcp_registration(&binary_path))?;
                    Ok(AppEvent::AiMcpClientsLoaded(snapshot))
                }
                AppCommand::SetMcpClientRegistration {
                    client_key,
                    registered,
                    binary_path,
                } => {
                    let client = client_from_key(&client_key)
                        .ok_or_else(|| write_error("未适配的客户端，请使用通用接入"))?;
                    if registered {
                        let registration = self.mcp_registration(&binary_path)?;
                        connect_mcp_client_registration(&storage, &home, client, &registration)?;
                    } else {
                        remove_mcp_client_registration(&storage, &home, client)?;
                    }
                    Ok(AppEvent::AiMcpClientsSynced {
                        clients: vec![client.label().into()],
                    })
                }
                AppCommand::SyncMcpClients { binary_path } => {
                    if storage.load_mcp_client_registrations()?.is_empty() {
                        return Ok(AppEvent::AiMcpClientsSynced { clients: vec![] });
                    }
                    let registration = self.mcp_registration(&binary_path)?;
                    let clients = sync_mcp_client_registrations(&storage, &home, &registration)?;
                    Ok(AppEvent::AiMcpClientsSynced { clients })
                }
                _ => unreachable!(),
            }
        })();
        result.unwrap_or_else(|error: fluxdb_core::Error| AppEvent::AiSettingsError(error.message))
    }
}
