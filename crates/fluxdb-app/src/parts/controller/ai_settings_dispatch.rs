// AI 管理命令独立职责；从既有 dispatch 结构拆出，公共状态仍通过 AppEvent 表达。
impl AppController {
    /// 确保用于一键注册的 Server Profile 存在且启用；仅在默认只读 Profile 缺失时创建。
    fn ensure_mcp_profile(&self, profile_id: &str) -> fluxdb_core::Result<()> {
        use fluxdb_storage::ai_platform::mcp_server::{
            ServerProfileEdit, WriteMode, load_server_profile, save_server_profile,
        };
        let storage = self.ai_storage()?;
        with_ai_storage(&storage, |conn| {
            let now = ai_chat_now();
            match load_server_profile(conn, profile_id)? {
                Some(profile) if profile.enabled => Ok(()),
                Some(_) => Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("Server Profile 已存在但处于禁用状态: {profile_id}；请先在本地 MCP 设置里启用"),
                )),
                None if profile_id == crate::DEFAULT_MCP_PROFILE_ID => {
                    let edit = ServerProfileEdit {
                        id: profile_id.to_string(),
                        display_name: "本地只读（自动创建）".into(),
                        enabled: true,
                        write_mode: WriteMode::Disabled,
                        allowed_tool_ids: crate::DEFAULT_READONLY_TOOLS
                            .iter()
                            .map(|t| t.to_string())
                            .collect(),
                        options_json: "{}".into(),
                        max_inflight: 4,
                        max_request_ms: 30_000,
                        max_request_bytes: 262_144,
                        max_response_bytes: 131_072,
                    };
                    // 新建默认禁用，二次保存才启用（与存储层“保存即生效”防护一致）。
                    let created = save_server_profile(conn, now, &edit, None)?;
                    save_server_profile(conn, now, &edit, Some(created.revision))?;
                    Ok(())
                }
                None => Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("Server Profile 不存在: {profile_id}"),
                )),
            }
        })
    }

    fn dispatch_ai_settings(&mut self, command: AppCommand) -> AppEvent {
        match command {
            AppCommand::SaveAiModel(input) => {
                let storage = self.ai_storage();
                let result = storage.and_then(|st| save_ai_model(&mut self.state, &st, &input));
                match result {
                    Ok((id, revision)) => {
                        return AppEvent::AiModelSaved { id, revision };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::DeleteAiModel {
                id,
                expected_revision,
            } => {
                let result = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        models::delete_model(conn, &id, expected_revision)
                    })
                });
                match result {
                    Ok(()) => {
                        self.state.ai_models.retain(|model| model.id != id);
                        tracing::info!(model_id = %id, "AI 模型已删除");
                        return AppEvent::AiModelDeleted { id };
                    }
                    Err(error) => {
                        tracing::warn!(model_id = %id, reason = %error.message, "删除 AI 模型失败");
                        return AppEvent::AiSettingsError(error.message);
                    }
                }
            }
            AppCommand::CompleteAiModelDelete { id, result } => match result {
                Ok(()) => {
                    self.state.ai_models.retain(|model| model.id != id);
                    return AppEvent::AiModelDeleted { id };
                }
                Err(message) => return AppEvent::AiSettingsError(message),
            },
            AppCommand::CompleteAiAgentDelete {
                id,
                runtime_revision,
                result,
            } => {
                return match result {
                    Ok(()) => {
                        self.state.ai_agents.retain(|a| a.id != id);
                        self.state.ai_runtime_revision =
                            self.state.ai_runtime_revision.max(runtime_revision);
                        AppEvent::AiAgentDeleted { id }
                    }
                    Err(message) => AppEvent::AiSettingsError(message),
                };
            }
            AppCommand::DeleteAiAgent {
                id,
                expected_revision,
            } => {
                let result = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::agents::delete_agent_with_runtime_revision(
                            conn,
                            &id,
                            expected_revision,
                        )
                    })
                });
                match result {
                    Ok(revision) => {
                        self.state.ai_agents.retain(|a| a.id != id);
                        self.state.ai_runtime_revision = revision;
                        return AppEvent::AiAgentDeleted { id };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::SaveAiAgent(input) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| save_ai_agent(&mut self.state, &st, input, now));
                match result {
                    Ok(id) => {
                        let revision = self
                            .state
                            .ai_agents
                            .iter()
                            .find(|a| a.id == id)
                            .map(|a| a.revision)
                            .unwrap_or(0);
                        return AppEvent::AiAgentSaved { id, revision };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::RefreshAiSettings => {
                let storage = self.ai_storage();
                let result = storage.and_then(|st| refresh_ai_settings(&mut self.state, &st));
                match result {
                    Ok(()) => return AppEvent::AiSettingsLoaded,
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::InstallMcpClient {
                client,
                profile_id,
                binary_path,
                config_root,
                preapprove_claude_tools,
            } => {
                let home = match crate::home_dir() {
                    Ok(home) => home,
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                };
                let Some(client_kind) = crate::client_from_key(&client) else {
                    return AppEvent::AiSettingsError(format!("未知 MCP 客户端: {client}"));
                };
                // “一键注册”要求目标 Profile 存在且已启用；默认只读 Profile 缺失时按需创建。
                // 已存在但被禁用则报错，不静默改用户已保存的 Profile。
                if let Err(error) = self.ensure_mcp_profile(&profile_id) {
                    return AppEvent::AiSettingsError(error.message);
                }
                let config_root = match config_root {
                    Some(root) => std::path::PathBuf::from(root),
                    None => match self.ai_storage().and_then(|storage| {
                        std::fs::canonicalize(storage.root_dir()).map_err(|error| {
                            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, format!("无法解析 MCP 数据目录：{error}"))
                        })
                    }) {
                        Ok(root) => root,
                        Err(error) => return AppEvent::AiSettingsError(error.message),
                    },
                };
                let registration = crate::McpRegistration {
                    server_name: "fluxdb".into(),
                    binary_path: std::path::PathBuf::from(&binary_path),
                    profile_id: profile_id.clone(),
                    config_root: Some(config_root),
                    preapprove_claude_tools,
                };
                match crate::register(client_kind, &home, &registration) {
                    Ok(fact) => {
                        // Claude 预批准规则（仅只读）；失败只告警，不算注册失败。
                        if client_kind == crate::McpClientKind::ClaudeCode
                            && preapprove_claude_tools
                        {
                            if let Err(error) = crate::set_claude_tool_preapproval(
                                &home,
                                &registration.server_name,
                                true,
                            ) {
                                tracing::warn!(%error, "写入 Claude MCP 预批准规则失败");
                            }
                        }
                        let _ = self.ai_storage().and_then(|storage| {
                            with_ai_storage(&storage, |conn| {
                                fluxdb_storage::ai_platform::mcp_server::save_auto_register_enabled(conn, true)
                            })
                        });
                        tracing::info!(
                            client = %client,
                            path = %fact.config_path.display(),
                            changed = fact.changed,
                            "本地 MCP Server 已注册到客户端"
                        );
                        return AppEvent::AiMcpClientRegistration {
                            client,
                            installed: true,
                            config_path: fact.config_path.display().to_string(),
                            restart_hint: true,
                        };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::UninstallMcpClient { client } => {
                let home = match crate::home_dir() {
                    Ok(home) => home,
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                };
                let Some(client_kind) = crate::client_from_key(&client) else {
                    return AppEvent::AiSettingsError(format!("未知 MCP 客户端: {client}"));
                };
                match crate::unregister(client_kind, &home, "fluxdb") {
                    Ok(fact) => {
                        if client_kind == crate::McpClientKind::ClaudeCode {
                            if let Err(error) = crate::set_claude_tool_preapproval(
                                &home, "fluxdb", false,
                            ) {
                                tracing::warn!(%error, "移除 Claude MCP 预批准规则失败");
                            }
                        }
                        let _ = self.ai_storage().and_then(|storage| {
                            with_ai_storage(&storage, |conn| {
                                fluxdb_storage::ai_platform::mcp_server::save_auto_register_enabled(conn, false)
                            })
                        });
                        tracing::info!(
                            client = %client,
                            path = %fact.config_path.display(),
                            changed = fact.changed,
                            "本地 MCP Server 已从客户端移除"
                        );
                        return AppEvent::AiMcpClientRegistration {
                            client,
                            installed: false,
                            config_path: fact.config_path.display().to_string(),
                            restart_hint: false,
                        };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::AutoRegisterMcpClients { binary_path } => {
                // 开关关闭 → 不再自动注册（用户显式卸载即关闭）。
                let enabled = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::mcp_server::load_auto_register_enabled(conn)
                    })
                });
                match enabled {
                    Ok(true) => {}
                    Ok(false) => return AppEvent::AiMcpAutoRegistered { clients: Vec::new() },
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
                if binary_path.trim().is_empty() {
                    return AppEvent::AiSettingsError("MCP 自动注册失败：找不到本地 Server 可执行文件".into());
                }
                // 确保默认只读 Profile 存在（缺失则创建），使注册即用。
                if let Err(error) = self.ensure_mcp_profile(crate::DEFAULT_MCP_PROFILE_ID) {
                    tracing::warn!(reason = %error.message, "MCP 自动注册：准备 Profile 失败");
                    return AppEvent::AiSettingsError(error.message);
                }
                // CLI 不会继承桌面进程的 FLUXDB_PROFILE；必须绑定 Profile 所在的真实存储根。
                let config_root = match self.ai_storage().and_then(|storage| {
                    std::fs::canonicalize(storage.root_dir()).map_err(|error| {
                        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, format!("无法解析 MCP 数据目录：{error}"))
                    })
                }) {
                    Ok(root) => root,
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                };
                let home = match crate::home_dir() {
                    Ok(home) => home,
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                };
                let registration = crate::McpRegistration {
                    server_name: crate::DEFAULT_MCP_SERVER_NAME.into(),
                    binary_path: std::path::PathBuf::from(&binary_path),
                    profile_id: crate::DEFAULT_MCP_PROFILE_ID.into(),
                    config_root: Some(config_root),
                    preapprove_claude_tools: true,
                };
                let results = crate::auto_register_installed_clients(&home, &registration);
                let mut clients = Vec::new();
                let mut errors = Vec::new();
                for (client, outcome) in results {
                    match outcome {
                        Ok(fact) => {
                            tracing::info!(
                                client = client.key(),
                                path = %fact.config_path.display(),
                                changed = fact.changed,
                                "启动自动注册本地 MCP Server"
                            );
                            if fact.changed {
                                clients.push(client.key().to_string());
                            }
                        }
                        Err(message) => {
                            tracing::warn!(client = client.key(), %message, "自动注册客户端失败");
                            errors.push(format!("{}：{message}", client.label()));
                        }
                    }
                }
                if !errors.is_empty() {
                    return AppEvent::AiSettingsError(format!("MCP 自动注册失败：{}", errors.join("；")));
                }
                return AppEvent::AiMcpAutoRegistered { clients };
            }
            AppCommand::LoadMoreAiSessions => {
                let storage = self.ai_storage();
                let result = storage.and_then(|st| load_more_ai_sessions(&mut self.state, &st));
                match result {
                    Ok(appended) => {
                        return AppEvent::AiSessionsAppended {
                            appended,
                            has_more: self.state.ai_sessions_has_more,
                        };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::RenameAiSession {
                id,
                title,
                expected_revision,
            } => {
                let title = title.trim().to_string();
                if title.is_empty() {
                    return AppEvent::AiSettingsError("会话名称不能为空".into());
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let result = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::sessions::rename_session(
                            conn,
                            now,
                            &id,
                            &title,
                            expected_revision,
                        )
                    })
                });
                match result {
                    Ok(revision) => {
                        if let Some(item) = self.state.ai_sessions.iter_mut().find(|s| s.id == id) {
                            item.title = title.clone();
                            item.revision = revision;
                        }
                        return AppEvent::AiSessionRenamed {
                            id,
                            title,
                            revision,
                        };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::ArchiveAiSession {
                id,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let result = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::sessions::archive_session(
                            conn,
                            now,
                            &id,
                            expected_revision,
                        )
                    })
                });
                match result {
                    Ok(revision) => {
                        if let Some(item) = self.state.ai_sessions.iter_mut().find(|s| s.id == id) {
                            item.status = "archived".into();
                            item.revision = revision;
                        }
                        return AppEvent::AiSessionArchived { id, revision };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::RestoreAiSession {
                id,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let result = self.ai_storage().and_then(|storage| {
                    with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::sessions::restore_session(
                            conn,
                            now,
                            &id,
                            expected_revision,
                        )
                    })
                });
                match result {
                    Ok(revision) => {
                        if let Some(item) = self.state.ai_sessions.iter_mut().find(|s| s.id == id) {
                            item.status = "active".into();
                            item.revision = revision;
                        }
                        return AppEvent::AiSessionRestored { id, revision };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::PurgeAiSession {
                id,
                expected_revision,
            } => {
                let result = self.ai_storage().and_then(|storage| {
                    // CAS 复核：归档后的 revision 必须与调用方一致，避免误删并发变化的会话。
                    let current = with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::sessions::load_session(conn, &id)
                    })?;
                    if current.revision != expected_revision {
                        return Err(fluxdb_core::Error::new(
                            fluxdb_core::ErrorKind::Internal,
                            "会话已被其他窗口修改，请重新加载",
                        ));
                    }
                    let plan = with_ai_storage(&storage, |conn| {
                        fluxdb_storage::ai_platform::sessions::purge_archived_session(conn, &id)
                    })?;
                    // 数据库删除已提交，文件清理失败不回滚；仅记录告警。
                    purge_session_files(&storage, &plan);
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        self.state.ai_sessions.retain(|session| session.id != id);
                        self.state.ai_sessions_total = (self.state.ai_sessions_total - 1).max(0);
                        return AppEvent::AiSessionPurged { id };
                    }
                    Err(error) => return AppEvent::AiSettingsError(error.message),
                }
            }
            AppCommand::CompleteAiSessionPurge { id, result } => {
                return match result {
                    Ok(()) => {
                        self.state.ai_sessions.retain(|session| session.id != id);
                        AppEvent::AiSessionPurged { id }
                    }
                    Err(message) => AppEvent::AiSettingsError(message),
                };
            }
            AppCommand::CompleteAiSessionRename {
                id,
                title,
                revision,
                result,
            } => {
                return match result {
                    Ok(()) => {
                        if let Some(item) = self.state.ai_sessions.iter_mut().find(|s| s.id == id) {
                            item.title = title.clone();
                            item.revision = revision;
                        }
                        AppEvent::AiSessionRenamed {
                            id,
                            title,
                            revision,
                        }
                    }
                    Err(message) => AppEvent::AiSettingsError(message),
                };
            }
            AppCommand::CompleteAiSessionStatus {
                id,
                archived,
                revision,
                result,
            } => {
                return match result {
                    Ok(()) => {
                        if let Some(item) = self.state.ai_sessions.iter_mut().find(|s| s.id == id) {
                            item.status = if archived { "archived" } else { "active" }.into();
                            item.revision = revision;
                        }
                        if archived {
                            AppEvent::AiSessionArchived { id, revision }
                        } else {
                            AppEvent::AiSessionRestored { id, revision }
                        }
                    }
                    Err(message) => AppEvent::AiSettingsError(message),
                };
            }
            AppCommand::SetAiModelEnabled {
                id,
                enabled,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| {
                    set_ai_model_enabled(&mut self.state, &st, &id, enabled, expected_revision, now)
                });
                match result {
                    Ok(()) => {
                        return AppEvent::AiModelEnabled {
                            id,
                            enabled,
                            revision: expected_revision + 1,
                        };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::SetAiPluginInstanceEnabled {
                id,
                enabled,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| {
                    set_ai_plugin_enabled(
                        &mut self.state,
                        &st,
                        &id,
                        enabled,
                        expected_revision,
                        now,
                    )
                });
                match result {
                    Ok(()) => {
                        return AppEvent::AiPluginInstanceEnabled {
                            id,
                            enabled,
                            revision: expected_revision + 1,
                        };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::SetAiModelEgressEnabled {
                enabled,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| {
                    set_ai_model_egress_enabled(
                        &mut self.state,
                        &st,
                        enabled,
                        expected_revision,
                        now,
                    )
                });
                match result {
                    Ok(()) => {
                        return AppEvent::AiModelEgressEnabledChanged {
                            enabled,
                            revision: expected_revision + 1,
                        };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::SetAiToolEnabled {
                tool_id,
                enabled,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| {
                    set_ai_tool_enabled(
                        &mut self.state,
                        &st,
                        &tool_id,
                        enabled,
                        expected_revision,
                        now,
                    )
                });
                match result {
                    Ok(_) => {
                        tracing::info!(
                            tool_id = %tool_id,
                            enabled,
                            "标准工具全局启停已更新"
                        );
                        return AppEvent::AiToolEnabled {
                            tool_id,
                            enabled,
                            revision: expected_revision + 1,
                        };
                    }
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::SaveAiDefaultModels {
                default_model_id,
                default_decision_model_id,
                expected_revision,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let storage = self.ai_storage();
                let result = storage.and_then(|st| {
                    save_ai_default_models(
                        &mut self.state,
                        &st,
                        default_model_id,
                        default_decision_model_id,
                        expected_revision,
                        now,
                    )
                });
                match result {
                    Ok(revision) => return AppEvent::AiDefaultModelsSaved { revision },
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::TestAiModel { id } => {
                let storage = self.ai_storage();
                let result = storage.and_then(|st| test_ai_model(&self.state, &st, &id));
                match result {
                    Ok((id, status)) => return AppEvent::AiModelTested { id, status },
                    Err(e) => return AppEvent::AiSettingsError(e.message),
                }
            }
            AppCommand::FetchAiModels {
                provider,
                endpoint,
                endpoint_is_full_url,
                api_key,
            } => {
                if endpoint_is_full_url {
                    return AppEvent::AiModelsFetchFailed {
                        message: "完整 URL 模式不自动获取模型列表，请手工填写模型 ID".into(),
                    };
                }
                // 纯网络读取：不触碰 storage，api_key 仅用于本次请求头。
                match fetch_ai_model_capabilities(&provider, endpoint.as_deref(), &api_key) {
                    Ok(capabilities) => {
                        return AppEvent::AiModelsFetched {
                            models: capabilities.iter().map(|m| m.id.clone()).collect(),
                            capabilities,
                        };
                    }
                    Err(e) => return AppEvent::AiModelsFetchFailed { message: e.message },
                }
            }
            _ => unreachable!("仅 AI 管理命令进入此分发器"),
        }
    }
}
