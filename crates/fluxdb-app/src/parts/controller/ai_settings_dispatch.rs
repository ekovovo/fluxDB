// AI 管理命令独立职责；从既有 dispatch 结构拆出，公共状态仍通过 AppEvent 表达。
impl AppController {
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
