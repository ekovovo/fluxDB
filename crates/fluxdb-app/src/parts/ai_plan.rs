// 「计划与执行进度」内置插件（设计 §23.2，`fluxdb.progress.plan`）。
//
// 这个插件向模型暴露一个 `plan.update` 工具：模型用它发布/更新**公开的结构化任务
// 计划**（步骤 + 状态 + 检查点说明）。计划落成 `PartKind::Plan` 片段，重启后仍可
// 查看；历史不被删除。
//
// 边界（设计 §23.2「只显示真实事件/模型公开内容」）：
// - 步骤状态由模型显式声明，或由步骤引用的**真实工具回执**驱动，不做关键词/工具名
//   匹配来"自动推进"；
// - 关闭插件（或取消 Agent 绑定）后工具不进入目录，模型既发现不到也无法直接调用；
//   已发布的计划片段保留，不删除历史。

/// 插件实例 ID；与目录登记一致，禁止改名以保持实例/绑定稳定。
pub const PROGRESS_PLAN_INSTANCE_ID: &str = "builtin.progress";
const PROGRESS_PLAN_PACKAGE: &str = "fluxdb.progress.plan";

/// 单个计划允许的最大步骤数；超出拒绝而不是截断，避免"看起来完整"的假象。
const MAX_PLAN_STEPS: usize = 40;
/// 单步标题/说明的 UTF-8 字节上限。
const MAX_STEP_TITLE_BYTES: usize = 200;
const MAX_STEP_NOTE_BYTES: usize = 500;
pub const MAX_PLAN_BYTES: usize = 32 * 1024;

/// 计划步骤状态。模型只能声明这几种；展示层据此渲染，不自行推断完成。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
    Skipped,
}

impl PlanStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStepStatus::Pending => "pending",
            PlanStepStatus::InProgress => "in_progress",
            PlanStepStatus::Done => "done",
            PlanStepStatus::Blocked => "blocked",
            PlanStepStatus::Skipped => "skipped",
        }
    }
}

/// 计划中的一步。
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    pub title: String,
    pub status: PlanStepStatus,
    /// 检查点/说明；可为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 可选：本步骤对应的真实工具调用 ID。只作为"引用"，不据此伪造状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// 一次计划发布的规范化结果；校验失败返回受控错误，不落盘半成品。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanUpdate {
    pub title: String,
    pub steps: Vec<PlanStep>,
}

impl PlanUpdate {
    pub fn to_payload_json(&self) -> String {
        serde_json::json!({
            "title": self.title,
            "steps": self.steps,
        })
        .to_string()
    }
}

/// 校验并规范化模型的 `plan.update` 参数。
///
/// 严格拒绝而不是"尽量修复"：未知状态、超长标题、步骤过多都返回受控错误，
/// 让模型改用合法参数重发，而不是把半截计划写成看起来完整的卡片。
pub fn parse_plan_update(arguments_json: &str) -> fluxdb_core::Result<PlanUpdate> {
    let value: serde_json::Value = serde_json::from_str(arguments_json).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, "计划参数不是合法 JSON")
    })?;
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, "计划缺少非空 title")
        })?
        .to_string();
    if title.len() > MAX_STEP_TITLE_BYTES {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Query,
            "计划标题过长",
        ));
    }
    let steps_value = value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Query, "计划缺少 steps 数组")
        })?;
    if steps_value.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Query,
            "计划至少需要一个步骤",
        ));
    }
    if steps_value.len() > MAX_PLAN_STEPS {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Query,
            format!("计划步骤数超过上限 {MAX_PLAN_STEPS}"),
        ));
    }
    let mut steps = Vec::with_capacity(steps_value.len());
    for (index, raw) in steps_value.iter().enumerate() {
        let step_title = raw
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Query,
                    format!("第 {} 步缺少非空 title", index + 1),
                )
            })?;
        if step_title.len() > MAX_STEP_TITLE_BYTES {
            return Err(fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Query,
                format!("第 {} 步标题过长", index + 1),
            ));
        }
        let status = match raw
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending")
        {
            "pending" => PlanStepStatus::Pending,
            "in_progress" => PlanStepStatus::InProgress,
            "done" => PlanStepStatus::Done,
            "blocked" => PlanStepStatus::Blocked,
            "skipped" => PlanStepStatus::Skipped,
            other => {
                return Err(fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Query,
                    format!("第 {} 步状态非法：{other}", index + 1),
                ));
            }
        };
        let note = raw
            .get("note")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(|note| {
                if note.len() > MAX_STEP_NOTE_BYTES {
                    note.chars().take(MAX_STEP_NOTE_BYTES).collect::<String>()
                } else {
                    note.to_string()
                }
            });
        let tool_call_id = raw
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        steps.push(PlanStep {
            title: step_title.to_string(),
            status,
            note,
            tool_call_id,
        });
    }
    Ok(PlanUpdate { title, steps })
}

/// 该会话是否启用「计划与执行进度」插件。
///
/// 与 BI 能力同一套规则：全局实例开启 **且** 当前 Agent 没有把它绑定为禁用。
fn builtin_progress_plan_enabled(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> fluxdb_core::Result<bool> {
    let enabled = fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?
        .iter()
        .any(|instance| {
            instance.id == PROGRESS_PLAN_INSTANCE_ID
                && instance.enabled
                && instance.runtime_kind == "builtin"
                && instance.package_key == PROGRESS_PLAN_PACKAGE
        });
    if !enabled {
        return Ok(false);
    }
    let session = fluxdb_storage::ai_platform::sessions::load_session(conn, session_id)?;
    let bindings =
        fluxdb_storage::ai_platform::plugins::list_agent_plugin_bindings(conn, &session.agent_id)?;
    Ok(!bindings.iter().any(|binding| {
        binding.plugin_instance_id == PROGRESS_PLAN_INSTANCE_ID && !binding.enabled
    }))
}

/// `plan.update` 工具：把模型的公开计划写成一个 `Plan` 片段。
///
/// 工具只负责**校验 + 生成规范化载荷**；真正的落盘由主循环的 `LoopEvent::PlanUpdated`
/// 分支完成——这样计划与其它会话片段走同一条持久化路径，可被同一套回桥消费。
struct PlanUpdateTool {
    descriptor: fluxdb_core::ToolDescriptor,
}

impl PlanUpdateTool {
    fn new() -> Self {
        Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "plan.update".into(),
                version: 1,
                description: "发布或更新本次任务的公开计划：给出计划标题与有序步骤（每步含标题与状态 pending/in_progress/done/blocked/skipped，可选检查点说明 note）。这是给用户看的计划，不授予任何执行权限；步骤状态必须如实填写，不要为了好看提前标 done。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: r#"{
                    "type":"object",
                    "properties":{
                        "title":{"type":"string","minLength":1,"maxLength":200},
                        "steps":{"type":"array","minItems":1,"maxItems":40,"items":{
                            "type":"object",
                            "properties":{
                                "title":{"type":"string","minLength":1,"maxLength":200},
                                "status":{"type":"string","enum":["pending","in_progress","done","blocked","skipped"]},
                                "note":{"type":"string","maxLength":500},
                                "tool_call_id":{"type":"string"}
                            },
                            "required":["title"],
                            "additionalProperties":false
                        }}
                    },
                    "required":["title","steps"],
                    "additionalProperties":false
                }"#
                .into(),
            },
        }
    }
}

impl fluxdb_core::AiTool for PlanUpdateTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let started_at_ms = now_ms();
            let cancelled = ctx.cancel.is_cancelled();
            let (status, result_json, error_code) = if cancelled {
                (
                    fluxdb_core::ToolReceiptStatus::Cancelled,
                    serde_json::json!({"ok": false, "code": "cancelled", "message": "计划更新已取消", "retryable": false}).to_string(),
                    Some("cancelled".to_string()),
                )
            } else {
                match parse_plan_update(&arguments_json) {
                    Ok(update) => (
                        fluxdb_core::ToolReceiptStatus::Succeeded,
                        serde_json::json!({
                            "ok": true,
                            "title": update.title,
                            "step_count": update.steps.len(),
                            "plan": serde_json::from_str::<serde_json::Value>(&update.to_payload_json()).unwrap_or_default(),
                        })
                        .to_string(),
                        None,
                    ),
                    Err(error) => (
                        fluxdb_core::ToolReceiptStatus::Failed,
                        serde_json::json!({
                            "ok": false,
                            "code": "invalid_arguments",
                            "message": error.message,
                            "retryable": true,
                        })
                        .to_string(),
                        Some("invalid_arguments".to_string()),
                    ),
                }
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json,
                error_code,
                affected_rows: None,
                started_at_ms,
                finished_at_ms: now_ms(),
            })
        })
    }
}

/// 按插件/绑定门控把 `plan.update` 加入目录；未启用时不装配，模型发现不到。
pub fn add_ai_plan_tool(
    catalog: &mut ToolCatalog,
    storage: &fluxdb_storage::FileStorage,
    session_id: &str,
) -> fluxdb_core::Result<bool> {
    let enabled = with_ai_storage(storage, |conn| {
        builtin_progress_plan_enabled(conn, session_id)
    })?;
    if !enabled {
        return Ok(false);
    }
    catalog.add_tool(Box::new(PlanUpdateTool::new()))?;
    Ok(true)
}

/// 规范化后的计划载荷 → 计划片段。供主循环在 `PlanUpdated` 事件落盘时使用。
pub fn plan_part_payload(update: &PlanUpdate) -> String {
    update.to_payload_json()
}

#[cfg(test)]
mod ai_plan_tests {
    use super::*;

    #[test]
    fn accepts_valid_plan_and_normalizes_status() {
        let update = parse_plan_update(
            r#"{"title":"  分析订单  ","steps":[
                {"title":"读取表结构","status":"done"},
                {"title":"查询数据"},
                {"title":"生成报表","status":"in_progress","note":"  正在汇总  ","tool_call_id":"call-1"}
            ]}"#,
        )
        .expect("合法计划应通过");
        assert_eq!(update.title, "分析订单");
        assert_eq!(update.steps.len(), 3);
        assert_eq!(update.steps[0].status, PlanStepStatus::Done);
        // 缺省状态是 pending，而不是"已完成"。
        assert_eq!(update.steps[1].status, PlanStepStatus::Pending);
        assert_eq!(update.steps[2].note.as_deref(), Some("正在汇总"));
        assert_eq!(update.steps[2].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn rejects_unknown_status_without_guessing() {
        let error =
            parse_plan_update(r#"{"title":"t","steps":[{"title":"s","status":"finished"}]}"#)
                .expect_err("未知状态必须拒绝而不是猜成 done");
        assert_eq!(error.kind, fluxdb_core::ErrorKind::Query);
    }

    #[test]
    fn rejects_empty_steps_and_missing_title() {
        assert!(parse_plan_update(r#"{"title":"t","steps":[]}"#).is_err(), "空步骤计划必须拒绝");
        assert!(parse_plan_update(r#"{"title":"   ","steps":[{"title":"s"}]}"#).is_err(), "空白标题必须拒绝");
        assert!(parse_plan_update(r#"{"title":"t","steps":[{"title":"  "}]}"#).is_err(), "空白步骤标题必须拒绝");
        assert!(parse_plan_update("not json").is_err(), "非法 JSON 必须拒绝");
    }

    #[test]
    fn rejects_too_many_steps() {
        let steps = (0..=MAX_PLAN_STEPS)
            .map(|index| serde_json::json!({"title": format!("步骤 {index}")}))
            .collect::<Vec<_>>();
        let payload = serde_json::json!({"title": "超长计划", "steps": steps}).to_string();
        assert!(parse_plan_update(&payload).is_err(), "超出步骤上限必须拒绝");
    }

    #[test]
    fn payload_round_trips_title_and_steps() {
        let update =
            parse_plan_update(r#"{"title":"T","steps":[{"title":"A","status":"blocked"}]}"#)
                .unwrap();
        let payload = plan_part_payload(&update);
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["title"], "T");
        assert_eq!(value["steps"][0]["status"], "blocked");
    }
}
