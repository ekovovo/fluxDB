// 「生命周期诊断」内置插件（设计 §23.2，`fluxdb.lifecycle.telemetry`）。
//
// 记录一轮执行中各阶段的**脱敏**耗时/错误/诊断汇总：模型请求、工具调用等阶段的
// 耗时与受控错误码，写入 `ai_run_events`（`kind = "stage_timing"`），并在回执位置
// 追加一条汇总事件。只写角色/阶段/ID/脱敏码与耗时，不含正文、参数、结果或凭据；
// 关键的执行事实与审计仍由内核独立落盘，不依赖本插件是否启用。
//
// 启用边界（设计 §23.2「可开关；关键执行/审计落盘不依赖它」）：
// - 关闭后不再写诊断事件，但安全审计、工具回执、审批事实照常落盘；
// - 历史诊断事件保留，不因关闭而删除。

/// 插件实例 ID；与目录登记一致，禁止改名以保持实例/绑定稳定。
pub const LIFECYCLE_TELEMETRY_INSTANCE_ID: &str = "builtin.telemetry";
const LIFECYCLE_TELEMETRY_PACKAGE: &str = "fluxdb.lifecycle.telemetry";

/// 诊断事件的 `ai_run_events.kind`。
pub const KIND_STAGE_TIMING: &str = "stage_timing";
/// 一轮结束时的汇总事件 kind。
pub const KIND_TURN_DIAGNOSTICS: &str = "turn_diagnostics";

/// 单轮内每个阶段的耗时累计；汇总时写入一条事件，避免每阶段一行刷事件表。
#[derive(Default)]
pub struct TurnDiagnostics {
    /// 该 Turn 是否启用诊断（关闭时所有方法直接跳过，不写事件）。
    enabled: bool,
    model_requests: u64,
    model_ms: u64,
    model_failures: u64,
    tool_calls: u64,
    tool_ms: u64,
    tool_failures: u64,
    /// 首次出现的错误码（去重后按出现顺序）；只保留脱敏码。
    error_codes: Vec<String>,
}

impl TurnDiagnostics {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 记一次阶段耗时；关闭时为空操作。
    pub fn record(
        &mut self,
        stage: crate::loop_stage::Stage,
        duration_ms: u64,
        ok: bool,
        error_code: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        match stage {
            crate::loop_stage::Stage::ModelRequest => {
                self.model_requests += 1;
                self.model_ms += duration_ms;
                if !ok {
                    self.model_failures += 1;
                }
            }
            crate::loop_stage::Stage::ToolCall => {
                self.tool_calls += 1;
                self.tool_ms += duration_ms;
                if !ok {
                    self.tool_failures += 1;
                }
            }
        }
        if let Some(code) = error_code.filter(|code| !code.is_empty()) {
            if !self.error_codes.iter().any(|existing| existing == code) {
                self.error_codes.push(code.to_string());
            }
        }
    }

    /// 汇总事件载荷；只含计数、耗时与脱敏码。
    pub fn summary_json(&self) -> String {
        serde_json::json!({
            "model_requests": self.model_requests,
            "model_ms": self.model_ms,
            "model_failures": self.model_failures,
            "tool_calls": self.tool_calls,
            "tool_ms": self.tool_ms,
            "tool_failures": self.tool_failures,
            "error_codes": self.error_codes,
        })
        .to_string()
    }

    /// 落一条阶段事件 + 一条回合汇总；写失败只告警，不阻断本轮（诊断是证据，不是控制点）。
    pub fn persist(
        &self,
        storage: &fluxdb_storage::FileStorage,
        turn_id: &str,
        now_ms: i64,
    ) {
        if !self.enabled {
            return;
        }
        let summary = self.summary_json();
        if let Err(error) = with_ai_storage(storage, |conn| {
            fluxdb_storage::ai_platform::run_events::append(
                conn,
                turn_id,
                KIND_TURN_DIAGNOSTICS,
                &summary,
                now_ms,
            )?;
            fluxdb_storage::ai_platform::run_events::append(
                conn,
                turn_id,
                KIND_STAGE_TIMING,
                &self.summary_json(),
                now_ms,
            )
        }) {
            tracing::warn!(%error, turn_id, "生命周期诊断事件写入失败");
        }
    }
}

/// 该会话是否启用「生命周期诊断」插件。
///
/// 与 BI 能力同一套规则：全局实例开启 **且** 当前 Agent 没有把它绑定为禁用。
pub fn lifecycle_telemetry_enabled(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> fluxdb_core::Result<bool> {
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .iter()
        .any(|instance| {
            instance.id == LIFECYCLE_TELEMETRY_INSTANCE_ID
                && instance.enabled
                && instance.runtime_kind == "builtin"
                && instance.package_key == LIFECYCLE_TELEMETRY_PACKAGE
        });
    if !enabled {
        return Ok(false);
    }
    let session = fluxdb_storage::ai_platform::sessions::load_session(conn, session_id)?;
    let bindings =
        fluxdb_storage::ai_platform::plugins::list_agent_plugin_bindings(conn, &session.agent_id)?;
    Ok(!bindings.iter().any(|binding| {
        binding.plugin_instance_id == LIFECYCLE_TELEMETRY_INSTANCE_ID && !binding.enabled
    }))
}

#[cfg(test)]
mod ai_diagnostics_tests {
    use super::*;

    #[test]
    fn disabled_accumulates_nothing() {
        let mut diagnostics = TurnDiagnostics::new(false);
        diagnostics.record(crate::loop_stage::Stage::ModelRequest, 120, false, Some("timeout"));
        diagnostics.record(crate::loop_stage::Stage::ToolCall, 30, true, None);
        let value: serde_json::Value =
            serde_json::from_str(&diagnostics.summary_json()).unwrap();
        // 关闭时全为零：不写诊断，也不影响执行事实。
        assert_eq!(value["model_requests"], 0);
        assert_eq!(value["tool_calls"], 0);
        assert_eq!(value["error_codes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn enabled_accumulates_counts_durations_and_dedupes_codes() {
        let mut diagnostics = TurnDiagnostics::new(true);
        diagnostics.record(crate::loop_stage::Stage::ModelRequest, 100, true, None);
        diagnostics.record(crate::loop_stage::Stage::ModelRequest, 250, false, Some("http_429"));
        diagnostics.record(crate::loop_stage::Stage::ToolCall, 40, false, Some("timeout"));
        diagnostics.record(crate::loop_stage::Stage::ToolCall, 10, false, Some("timeout"));
        let value: serde_json::Value =
            serde_json::from_str(&diagnostics.summary_json()).unwrap();
        assert_eq!(value["model_requests"], 2);
        assert_eq!(value["model_ms"], 350);
        assert_eq!(value["model_failures"], 1);
        assert_eq!(value["tool_calls"], 2);
        assert_eq!(value["tool_ms"], 50);
        assert_eq!(value["tool_failures"], 2);
        // 去重且保持出现顺序。
        assert_eq!(value["error_codes"], serde_json::json!(["http_429", "timeout"]));
    }

    #[test]
    fn summary_json_contains_no_message_text() {
        let mut diagnostics = TurnDiagnostics::new(true);
        diagnostics.record(crate::loop_stage::Stage::ToolCall, 5, false, Some("permission"));
        let summary = diagnostics.summary_json();
        // 只有计数/耗时/脱敏码：不得出现 message 字段或业务值。
        assert!(!summary.contains("message"));
        assert!(summary.contains("permission"));
    }
}
