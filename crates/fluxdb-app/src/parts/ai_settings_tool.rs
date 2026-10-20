// `page.prefill_form`：把经白名单校验的设置字段提交给宿主设置页草稿。
//
// 工具本身不保存设置、不读凭据、不持有 UI；桌面端消费 `UiAction` 后才写入
// `settings_editor_draft`，用户仍需点击设置页保存。

fn page_prefill_form_schema() -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "section", "fields"],
        "properties": {
            "target": {
                "type": "string",
                "minLength": 1,
                "description": "当前页面引用，来自 current_page.id"
            },
            "section": {
                "type": "string",
                "enum": ["editor", "appearance", "data", "system"]
            },
            "fields": crate::settings_fields_schema()
        }
    })
    .to_string()
}

struct PagePrefillFormTool {
    descriptor: fluxdb_core::ToolDescriptor,
    page_ref: String,
    actions: std::sync::Arc<dyn crate::UiActionSink>,
}

impl PagePrefillFormTool {
    fn new(
        page_ref: String,
        actions: std::sync::Arc<dyn crate::UiActionSink>,
    ) -> fluxdb_core::Result<Self> {
        Ok(Self {
            descriptor: fluxdb_core::ToolDescriptor {
                id: "page.prefill_form".into(),
                version: 1,
                description: "把非敏感设置字段预填到当前设置页草稿；不保存、不执行数据库操作，用户确认后再点击保存。仅用于设置页。".into(),
                risk: fluxdb_core::ToolRisk::Read,
                input_schema_json: page_prefill_form_schema(),
            },
            page_ref,
            actions,
        })
    }
}

impl fluxdb_core::AiTool for PagePrefillFormTool {
    fn descriptor(&self) -> &fluxdb_core::ToolDescriptor {
        &self.descriptor
    }

    fn execute(
        &self,
        ctx: fluxdb_core::ToolExecutionContext,
        arguments_json: String,
    ) -> fluxdb_core::AiFuture<'_, fluxdb_core::ToolReceipt> {
        Box::pin(async move {
            let now = now_ms();
            let value = serde_json::from_str::<serde_json::Value>(&arguments_json)
                .map_err(|error| ui_error(format!("参数不是合法 JSON: {error}")));
            let (status, result, error_code) = match value.and_then(|value| {
                if ctx.cancel.is_cancelled() {
                    return Err(ui_error("页面预填已取消"));
                }
                let target = value
                    .get("target")
                    .and_then(|target| target.as_str())
                    .ok_or_else(|| ui_error("缺少 target"))?;
                if target != self.page_ref {
                    return Err(ui_error(format!(
                        "target 不属于本轮页面：仅可预填 {}",
                        self.page_ref
                    )));
                }
                let prefill = crate::parse_settings_prefill(&value).map_err(ui_error)?;
                self.actions
                    .submit(crate::UiAction::PrefillSettings {
                        request_id: ctx.call_id.clone(),
                        page_ref: self.page_ref.clone(),
                        prefill: prefill.clone(),
                    })
                    .map_err(|error| {
                        let retryable = error.retryable();
                        let message = error.message().to_string();
                        ui_error_with_retryable(message, retryable)
                    })?;
                Ok(serde_json::json!({
                    "status": "submitted",
                    "target": self.page_ref,
                    "section": prefill.section.as_str(),
                    "fields": prefill.fields.keys().collect::<Vec<_>>(),
                    "note": "已提交到设置页草稿；尚未保存，等待用户确认"
                }))
            }) {
                Ok(value) => (fluxdb_core::ToolReceiptStatus::Succeeded, value, None),
                Err(error) => (
                    if ctx.cancel.is_cancelled() {
                        fluxdb_core::ToolReceiptStatus::Cancelled
                    } else {
                        fluxdb_core::ToolReceiptStatus::Failed
                    },
                    serde_json::json!({
                        "error": error.message,
                        "retryable": error.retryable,
                    }),
                    Some("ui_action_failed".to_string()),
                ),
            };
            Ok(fluxdb_core::ToolReceipt {
                call_id: ctx.call_id,
                tool_id: self.descriptor.id.clone(),
                status,
                result_json: result.to_string(),
                error_code,
                affected_rows: None,
                started_at_ms: now,
                finished_at_ms: now_ms(),
            })
        })
    }
}

#[derive(Debug)]
struct UiToolError {
    message: String,
    retryable: bool,
}

fn ui_error(message: impl Into<String>) -> UiToolError {
    UiToolError {
        message: message.into(),
        retryable: false,
    }
}

fn ui_error_with_retryable(message: impl Into<String>, retryable: bool) -> UiToolError {
    UiToolError {
        message: message.into(),
        retryable,
    }
}

pub fn add_ai_settings_tools(
    catalog: &mut crate::ToolCatalog,
    page_ref: String,
    actions: std::sync::Arc<dyn crate::UiActionSink>,
) -> fluxdb_core::Result<()> {
    catalog.add_tool_with_shape_and_effect(
        Box::new(PagePrefillFormTool::new(page_ref, actions)?),
        crate::ToolOutputShape::Opaque,
        Some(crate::EffectClass::LocalUiMutation),
    )
}

#[cfg(test)]
mod ai_settings_tool_tests {
    use super::*;
    use fluxdb_core::AiTool;

    #[derive(Default)]
    struct RecordingSink {
        actions: std::sync::Mutex<Vec<crate::UiAction>>,
    }

    impl crate::UiActionSink for RecordingSink {
        fn submit(&self, action: crate::UiAction) -> Result<(), crate::UiActionError> {
            self.actions.lock().expect("actions").push(action);
            Ok(())
        }
    }

    fn context() -> fluxdb_core::ToolExecutionContext {
        fluxdb_core::ToolExecutionContext {
            automatic_approval: false,
            bounded_write: false,
            session_id: "session".into(),
            turn_id: "turn".into(),
            iteration: 1,
            call_id: "call".into(),
            cancel: fluxdb_core::CancelFlag::new(),
            progress: None,
        }
    }

    fn run(tool: &dyn AiTool, args: &str) -> fluxdb_core::ToolReceipt {
        let ctx = context();
        let cancel = ctx.cancel.clone();
        crate::ai_executor::block_on_cancellable(tool.execute(ctx, args.to_string()), &cancel)
            .expect("drive")
            .expect("receipt")
    }

    #[test]
    fn submits_a_validated_settings_prefill() {
        let sink = std::sync::Arc::new(RecordingSink::default());
        let tool = PagePrefillFormTool::new(
            "page:7".into(),
            sink.clone() as std::sync::Arc<dyn crate::UiActionSink>,
        )
        .expect("tool");
        let receipt = run(
            &tool,
            r#"{"target":"page:7","section":"appearance","fields":{"theme":"dark"}}"#,
        );
        assert!(receipt.is_success());
        let actions = sink.actions.lock().expect("actions");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            crate::UiAction::PrefillSettings { page_ref, prefill, .. } => {
                assert_eq!(page_ref, "page:7");
                assert_eq!(prefill.section, crate::SettingsSection::Appearance);
            }
            _ => panic!("expected prefill action"),
        }
    }

    #[test]
    fn rejects_a_target_from_another_page() {
        let sink = std::sync::Arc::new(RecordingSink::default());
        let tool = PagePrefillFormTool::new(
            "page:7".into(),
            sink.clone() as std::sync::Arc<dyn crate::UiActionSink>,
        )
        .expect("tool");
        let receipt = run(
            &tool,
            r#"{"target":"page:8","section":"appearance","fields":{"theme":"dark"}}"#,
        );
        assert!(!receipt.is_success());
        assert!(sink.actions.lock().expect("actions").is_empty());
    }

}
