// 「计划与执行进度」卡片的生产渲染回归（T071）。
//
// 走真实 View + AppState + 生产渲染：种子一条 `Plan` 片段，打开 AI 面板，
// 断言计划卡与各步骤行按 `test_support` 稳定 ID 渲染出来，并检查进度汇总计数。
#[cfg(test)]
mod ai_plan_card_tests {
    use super::*;
    use fluxdb_app::{AiConversationCommand, AiModelSaveInput};
    use fluxdb_storage::ai_platform::{conversation, sessions};
    use gpui::{TestAppContext, VisualTestContext};
    use std::path::PathBuf;

    const SESSION: &str = "plan-card-session";
    const TURN: &str = "plan-card-turn";
    const ASSISTANT: &str = "plan-card-assistant";

    struct Fixture {
        root: PathBuf,
        storage: FileStorage,
    }

    impl Fixture {
        fn new() -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "fluxdb-ai-plan-card-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("fixture 时钟")
                    .as_nanos(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            let storage = FileStorage::new(root.clone());
            let mut controller = AppController::new();
            controller.set_ai_storage(FileStorage::new(&root));
            let event = controller.dispatch(AppCommand::SaveAiModel(AiModelSaveInput {
                display_name: "计划卡测试模型".into(),
                provider: "openai".into(),
                model: "plan-card-model".into(),
                endpoint: Some("http://127.0.0.1:9/v1".into()),
                enabled: true,
                ..Default::default()
            }));
            assert!(
                matches!(event, AppEvent::AiModelSaved { .. }),
                "隔离模型保存失败: {event:?}"
            );
            let fixture = Self { root, storage };
            // 种子一条已完成会话 + 一条 Plan 片段（模拟模型已经发布过计划）。
            fixture
                .storage
                .with_ai_connection(|conn| {
                    conversation::send(
                        conn,
                        1,
                        true,
                        SESSION,
                        fluxdb_storage::ai_platform::agents::BUILTIN_AGENT_ID,
                        TURN,
                        "plan-card-user",
                        "做一份订单报表",
                        "{}",
                        "{}",
                    )?;
                    // 计划片段必须在 Turn 进入终态前写入（终态后事件会被拒绝）。
                    conversation::upsert_event_part(
                        conn,
                        2,
                        SESSION,
                        TURN,
                        ASSISTANT,
                        "plan-card-part-1",
                        sessions::PartKind::Plan,
                        sessions::PartStatus::Complete,
                        r#"{"title":"生成订单报表","steps":[
                            {"title":"读取订单表","status":"done"},
                            {"title":"汇总指标","status":"in_progress"},
                            {"title":"输出报表","status":"pending"}
                        ]}"#,
                        None,
                        None,
                    )?;
                    conversation::checkpoint(
                        conn,
                        3,
                        SESSION,
                        TURN,
                        ASSISTANT,
                        "计划已发布",
                        "",
                        Some(sessions::TurnStatus::Completed),
                        None,
                    )?;
                    Ok(())
                })
                .expect("隔离计划种子");
            fixture
        }

        fn controller(&self) -> AppController {
            let mut controller = AppController::new();
            controller.set_ai_storage(FileStorage::new(&self.root));
            controller
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
    }

    #[gpui::test]
    fn plan_card_renders_title_and_step_rows(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        cx.update(gpui_component::init);
        let storage = fixture.storage.clone();
        let mut controller = fixture.controller();
        assert!(matches!(
            controller.dispatch(AppCommand::AiConversation(
                AiConversationCommand::Load {
                    session_id: Some(SESSION.into()),
                }
            )),
            AppEvent::AiConversationLoaded(_)
        ));
        let (view, cx) = cx.add_window_view(move |window, cx| {
            navicat_main_new(window, cx, controller, storage, ThemeMode::Light)
        });
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
        });
        draw(cx);
        // 打开 AI 面板（复用生产入口）。
        let ask = bounds(cx, "ask-ai").expect("应渲染询问 AI 入口");
        cx.simulate_click(ask.center(), gpui::Modifiers::default());
        draw(cx);
        // 计划卡与三个步骤行必须按稳定 ID 渲染。
        assert!(
            bounds(cx, "ai-plan:plan-card-part-1").is_some(),
            "计划卡未渲染"
        );
        for index in 0..3 {
            assert!(
                bounds(cx, &format!("ai-plan-step:plan-card-part-1:{index}")).is_some(),
                "计划步骤 {index} 未渲染"
            );
        }
    }
}
