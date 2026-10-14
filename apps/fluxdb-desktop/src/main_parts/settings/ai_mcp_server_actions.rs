// MCP Server 管理页动作（T060）：打开编辑器、校验、派发保存命令。

impl NavicatMain {
    /// 打开 Profile 编辑器：`Some(id)` 选中已有 Profile，`None` 新建草稿。
    fn open_ai_mcp_profile_editor(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = id.as_ref().and_then(|id| {
            self.controller
                .state()
                .ai_mcp_server_profiles
                .iter()
                .find(|p| &p.id == id)
                .cloned()
        });
        if id.is_some() && existing.is_none() {
            self.show_message(
                "Server Profile 不存在，可能已被删除",
                AppMessageKind::Warning,
                cx,
            );
            return;
        }
        let inputs = AiMcpServerProfileEditorInputs::new(window, cx);
        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        // 限额输入框的 placeholder 不是值；新建草稿必须预填可保存的默认值，
        // 否则用户不填限额就保存会被正整数校验挡住。
        set(&inputs.max_inflight, "4".to_string(), window, cx);
        set(
            &inputs.max_request_ms,
            "30000".to_string(),
            window,
            cx,
        );
        set(
            &inputs.max_request_bytes,
            "262144".to_string(),
            window,
            cx,
        );
        set(
            &inputs.max_response_bytes,
            "131072".to_string(),
            window,
            cx,
        );
        let mut editor = AiMcpServerProfileEditor {
            profile_id: None,
            expected_revision: 0,
            // 新建默认关闭：保存配置本身不应意外对外开放服务。
            enabled: false,
            write_mode: "disabled".to_string(),
            write_mode_menu_open: false,
            // 新建默认只开列资源与只读查询两个最小工具，避免保存即全量开放。
            selected_tools: [
                "fluxdb_connections_list".to_string(),
                "fluxdb_query".to_string(),
            ]
            .into_iter()
            .collect(),
            tools_expanded: false,
            error: None,
            inputs,
        };
        if let Some(profile) = &existing {
            editor.profile_id = Some(profile.id.clone());
            editor.expected_revision = profile.revision;
            editor.enabled = profile.enabled;
            editor.write_mode = profile.write_mode.clone();
            editor.selected_tools = profile.allowed_tool_ids.iter().cloned().collect();
            set(
                &editor.inputs.display_name,
                profile.display_name.clone(),
                window,
                cx,
            );
            set(
                &editor.inputs.max_inflight,
                profile.max_inflight.to_string(),
                window,
                cx,
            );
            set(
                &editor.inputs.max_request_ms,
                profile.max_request_ms.to_string(),
                window,
                cx,
            );
            set(
                &editor.inputs.max_request_bytes,
                profile.max_request_bytes.to_string(),
                window,
                cx,
            );
            set(
                &editor.inputs.max_response_bytes,
                profile.max_response_bytes.to_string(),
                window,
                cx,
            );
        }
        self.ai_mcp_server_profile_editor = Some(editor);
        // 新建 Profile 时关闭可能残留的授权草稿，避免把授权挂到旧 Profile 上。
        if id.is_none() {
            self.ai_mcp_server_grant_editor = None;
        }
        cx.notify();
    }

    /// 提交 Profile 编辑器：先做 UI 校验，再派发保存命令；失败保留草稿。
    fn submit_ai_mcp_profile_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.ai_mcp_server_profile_editor.clone() else {
            return;
        };
        let display_name = ai_input_text(&editor.inputs.display_name, cx);
        if display_name.is_empty() {
            return self.set_ai_mcp_profile_error("名称不能为空", cx);
        }
        let positive = |input: &Entity<InputState>, label: &'static str| -> Result<i64, String> {
            match ai_input_text(input, cx).parse::<i64>() {
                Ok(value) if value > 0 => Ok(value),
                _ => Err(format!("{label}必须为正整数")),
            }
        };
        let max_inflight = match positive(&editor.inputs.max_inflight, "并发请求") {
            Ok(value) => value,
            Err(message) => return self.set_ai_mcp_profile_error(message, cx),
        };
        let max_request_ms = match positive(&editor.inputs.max_request_ms, "单请求时限") {
            Ok(value) => value,
            Err(message) => return self.set_ai_mcp_profile_error(message, cx),
        };
        let max_request_bytes = match positive(&editor.inputs.max_request_bytes, "请求字节上限") {
            Ok(value) => value,
            Err(message) => return self.set_ai_mcp_profile_error(message, cx),
        };
        let max_response_bytes = match positive(&editor.inputs.max_response_bytes, "响应字节上限") {
            Ok(value) => value,
            Err(message) => return self.set_ai_mcp_profile_error(message, cx),
        };
        // 工具白名单只允许目录内的稳定名称，避免写入不存在的工具导致 tools/list 恒空。
        let known: std::collections::HashSet<&str> =
            AI_MCP_SERVER_TOOLS.iter().map(|(name, _)| *name).collect();
        let mut allowed_tool_ids = editor
            .selected_tools
            .iter()
            .filter(|name| known.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        allowed_tool_ids.sort();
        let input = fluxdb_app::AiMcpServerProfileSaveInput {
            id: editor.profile_id.clone(),
            display_name,
            enabled: editor.enabled,
            write_mode: editor.write_mode.clone(),
            allowed_tool_ids,
            max_inflight,
            max_request_ms,
            max_request_bytes,
            max_response_bytes,
            expected_revision: editor.profile_id.as_ref().map(|_| editor.expected_revision),
        };
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpServerProfile(input))
        {
            AppEvent::AiMcpServerProfileSaved { id, revision } => {
                if let Some(state) = self.ai_mcp_server_profile_editor.as_mut() {
                    state.profile_id = Some(id);
                    state.expected_revision = revision;
                    state.error = None;
                }
                self.show_message("Server Profile 已保存", AppMessageKind::Success, cx);
            }
            AppEvent::AiSettingsError(message) => {
                self.set_ai_mcp_profile_error(message, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn set_ai_mcp_profile_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if let Some(editor) = self.ai_mcp_server_profile_editor.as_mut() {
            editor.error = Some(message.into());
        }
        cx.notify();
    }

    /// 就地切换授权启停（读回当前 revision 做对象级 CAS，冲突时刷新提示）。
    fn toggle_ai_mcp_grant(
        &mut self,
        id: &str,
        enabled: bool,
        expected_revision: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(grant) = self
            .controller
            .state()
            .ai_mcp_server_grants
            .iter()
            .find(|grant| grant.id == id)
            .cloned()
        else {
            self.show_message("资源授权不存在", AppMessageKind::Warning, cx);
            return;
        };
        let input = fluxdb_app::AiMcpServerGrantSaveInput {
            id: Some(grant.id.clone()),
            profile_id: grant.profile_id.clone(),
            connection_id: grant.connection_id,
            database_name: grant.database_name.clone(),
            namespace: grant.namespace.clone(),
            enabled,
            allow_read: grant.allow_read,
            allow_write: grant.allow_write,
            resource_kind: grant.resource_kind.clone(),
            object_policy_json: grant.object_policy_json.clone(),
            write_policy_json: grant.write_policy_json.clone(),
            max_rows: grant.max_rows,
            expected_revision: Some(expected_revision),
        };
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpServerGrant(input))
        {
            AppEvent::AiMcpServerGrantSaved { .. } => {
                self.show_message("资源授权已更新", AppMessageKind::Success, cx);
            }
            AppEvent::AiSettingsError(message) => {
                self.show_message(message, AppMessageKind::Error, cx);
                // CAS 冲突时重新加载，让 UI 与后端回到同一事实。
                let _ = self.controller.dispatch(AppCommand::RefreshAiSettings);
            }
            _ => {}
        }
        cx.notify();
    }

    /// 打开资源授权编辑器：`Some(id)` 编辑已有，`None` 在当前 Profile 下新建。
    fn open_ai_mcp_grant_editor(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.controller.state();
        let existing = id.as_ref().and_then(|id| {
            state
                .ai_mcp_server_grants
                .iter()
                .find(|grant| &grant.id == id)
                .cloned()
        });
        if id.is_some() && existing.is_none() {
            self.show_message("资源授权不存在", AppMessageKind::Warning, cx);
            return;
        }
        // 新建时归属当前选中的 Profile；没有选中则不打开（避免挂到未知 Profile）。
        let profile_id = existing
            .as_ref()
            .map(|grant| grant.profile_id.clone())
            .or_else(|| {
                self.ai_mcp_server_profile_editor
                    .as_ref()
                    .and_then(|editor| editor.profile_id.clone())
            });
        let Some(profile_id) = profile_id else {
            self.show_message(
                "请先保存并选中一个 Server Profile，再新增授权",
                AppMessageKind::Warning,
                cx,
            );
            return;
        };
        let inputs = AiMcpServerGrantEditorInputs::new(window, cx);
        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        // 同 Profile 编辑器：placeholder 不是值，新建草稿预填默认行数上限。
        set(&inputs.max_rows, "200".to_string(), window, cx);
        let mut editor = AiMcpServerGrantEditor {
            grant_id: None,
            expected_revision: 0,
            profile_id,
            connection_id: 0,
            connection_label: "选择连接".to_string(),
            resource_kind: "sql".to_string(),
            connection_menu_open: false,
            enabled: true,
            allow_read: true,
            allow_write: false,
            all_scope: true,
            error: None,
            inputs,
        };
        if let Some(grant) = &existing {
            editor.grant_id = Some(grant.id.clone());
            editor.expected_revision = grant.revision;
            editor.connection_id = grant.connection_id;
            editor.connection_label = grant.connection_name.clone();
            editor.resource_kind = grant.resource_kind.clone();
            editor.enabled = grant.enabled;
            editor.allow_read = grant.allow_read;
            editor.allow_write = grant.allow_write;
            let value: serde_json::Value =
                serde_json::from_str(&grant.object_policy_json).unwrap_or_default();
            editor.all_scope = if grant.resource_kind == "redis" {
                value.get("all_keys").and_then(serde_json::Value::as_bool) == Some(true)
            } else {
                value.get("all_objects").and_then(serde_json::Value::as_bool) == Some(true)
            };
            let list = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default()
            };
            set(&editor.inputs.database_name, grant.database_name.clone(), window, cx);
            set(&editor.inputs.namespace, grant.namespace.clone(), window, cx);
            set(
                &editor.inputs.table_allowlist,
                list("table_allowlist"),
                window,
                cx,
            );
            set(
                &editor.inputs.key_prefixes,
                list("key_prefixes"),
                window,
                cx,
            );
            set(&editor.inputs.max_rows, grant.max_rows.to_string(), window, cx);
            if grant.write_policy_json != "{}" {
                editor.inputs.write_policy.update(cx, |state, cx| {
                    state.set_value(grant.write_policy_json.clone(), window, cx)
                });
            }
        } else {
            set(&editor.inputs.max_rows, "200".to_string(), window, cx);
            // 新建默认选中第一个连接，减少一次无谓下拉操作。
            if let Some(first) = state.connections.first() {
                editor.connection_id = first.config.id.0 as i64;
                editor.connection_label = first.config.name.clone();
                editor.resource_kind = if first.config.kind == fluxdb_core::DatabaseKind::Redis {
                    "redis".into()
                } else {
                    "sql".into()
                };
            }
        }
        self.ai_mcp_server_grant_editor = Some(editor);
        cx.notify();
    }

    /// 提交资源授权编辑器：把 UI 字段转成受校验的对象/写规则 JSON。
    fn submit_ai_mcp_grant_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.ai_mcp_server_grant_editor.clone() else {
            return;
        };
        if editor.connection_id == 0 {
            return self.set_ai_mcp_grant_error("请选择授权的连接", cx);
        }
        let split_list = |input: &Entity<InputState>| -> Vec<String> {
            ai_input_text(input, cx)
                .split([',', '，'])
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let database_name = ai_input_text(&editor.inputs.database_name, cx);
        if database_name.is_empty() {
            return self.set_ai_mcp_grant_error("库名 / Redis DB 不能为空", cx);
        }
        if editor.resource_kind == "redis"
            && !database_name
                .chars()
                .all(|ch| ch.is_ascii_digit())
        {
            return self.set_ai_mcp_grant_error("Redis DB 必须是 0-15 的整数", cx);
        }
        let object_policy_json = if editor.all_scope {
            if editor.resource_kind == "redis" {
                r#"{"all_keys":true}"#.to_string()
            } else {
                r#"{"all_objects":true}"#.to_string()
            }
        } else {
            let items = if editor.resource_kind == "redis" {
                split_list(&editor.inputs.key_prefixes)
            } else {
                split_list(&editor.inputs.table_allowlist)
            };
            if items.is_empty() {
                return self.set_ai_mcp_grant_error(
                    "未选择整库放行时，必须明确列出表或 Key 前缀",
                    cx,
                );
            }
            let key = if editor.resource_kind == "redis" {
                "key_prefixes"
            } else {
                "table_allowlist"
            };
            serde_json::json!({ key: items }).to_string()
        };
        let write_policy_raw = editor
            .inputs
            .write_policy
            .read(cx)
            .value()
            .trim()
            .to_string();
        let write_policy_json = if write_policy_raw.is_empty() {
            "{}".to_string()
        } else {
            match serde_json::from_str::<serde_json::Value>(&write_policy_raw) {
                Ok(value) if value.is_object() => value.to_string(),
                _ => {
                    return self
                        .set_ai_mcp_grant_error("受限写规则必须是合法 JSON 对象", cx)
                }
            }
        };
        if editor.allow_write && write_policy_json == "{}" {
            return self.set_ai_mcp_grant_error(
                "允许写入时必须给出结构化写规则；意图文字不构成权限",
                cx,
            );
        }
        let max_rows = match ai_input_text(&editor.inputs.max_rows, cx).parse::<i64>() {
            Ok(value) if (1..=1000).contains(&value) => value,
            _ => return self.set_ai_mcp_grant_error("单次最大行数必须在 1–1000 之间", cx),
        };
        let input = fluxdb_app::AiMcpServerGrantSaveInput {
            id: editor.grant_id.clone(),
            profile_id: editor.profile_id.clone(),
            connection_id: editor.connection_id,
            database_name,
            namespace: ai_input_text(&editor.inputs.namespace, cx),
            enabled: editor.enabled,
            allow_read: editor.allow_read,
            allow_write: editor.allow_write,
            resource_kind: editor.resource_kind.clone(),
            object_policy_json,
            write_policy_json,
            max_rows,
            expected_revision: editor.grant_id.as_ref().map(|_| editor.expected_revision),
        };
        match self
            .controller
            .dispatch(AppCommand::SaveAiMcpServerGrant(input))
        {
            AppEvent::AiMcpServerGrantSaved { id, revision } => {
                if let Some(state) = self.ai_mcp_server_grant_editor.as_mut() {
                    state.grant_id = Some(id);
                    state.expected_revision = revision;
                    state.error = None;
                }
                self.show_message("资源授权已保存", AppMessageKind::Success, cx);
            }
            AppEvent::AiSettingsError(message) => {
                self.set_ai_mcp_grant_error(message, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn set_ai_mcp_grant_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if let Some(editor) = self.ai_mcp_server_grant_editor.as_mut() {
            editor.error = Some(message.into());
        }
        cx.notify();
    }
}
