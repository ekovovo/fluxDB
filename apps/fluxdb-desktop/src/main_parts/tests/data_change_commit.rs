// 「提交更改」不得在 UI 线程做数据库 I/O 的回归测试。
//
// 复现条件：demo 连接被设置为提交耗时 1500ms（模拟数据库响应很慢）。
// 旧实现直接在 UI 线程同步 `dispatch`，用户点「提交」后整个界面卡住 1.5s。
// 现在提交放到后台线程，`confirm_apply_data_changes` 只派发即返回，
// 因此函数返回瞬间标签仍处于「提交中」。
#[cfg(test)]
mod data_change_commit_tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    /// demo 连接的模拟提交耗时：足够长，能与「同步执行」明确区分。
    const APPLY_DELAY_MS: u64 = 1500;

    /// 用生产装配路径打开一个带未提交改动的数据编辑器标签。
    fn open_dirty_data_editor<'a>(
        cx: &'a mut TestAppContext,
        extra_options: &[(&str, &str)],
    ) -> (Entity<NavicatMain>, &'a mut VisualTestContext) {
        let root = std::env::temp_dir().join(format!(
            "fluxdb-data-commit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("测试时钟")
                .as_nanos()
        ));
        let storage = FileStorage::new(root.clone());
        let mut controller = AppController::with_mock_data();
        let mut slow_config = controller.state().connections[0].config.clone();
        slow_config
            .options
            .insert("demo.apply_delay_ms".into(), APPLY_DELAY_MS.to_string());
        for (key, value) in extra_options {
            slow_config.options.insert((*key).into(), (*value).into());
        }
        controller.dispatch(AppCommand::UpdateConnection(slow_config));
        controller.dispatch(AppCommand::OpenConnection(ConnectionId(1)));
        let database = controller.state().connections[0].objects[0].path.clone();
        controller.dispatch(AppCommand::LoadObjectChildren(database));
        let object = controller.state().connections[0].objects[1].path.clone();
        controller.dispatch(AppCommand::OpenDataEditor(object));
        controller.dispatch(AppCommand::LoadDataPage(TabId(1)));
        // 造一处未提交改动，走真实「提交更改」确认框。
        controller.dispatch(AppCommand::EditDataCell {
            tab_id: TabId(1),
            row: 0,
            column: 1,
            value: CellValue::Text("异步提交标记".to_string()),
        });
        let _ = cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            navicat_main_new(window, cx, controller, storage, ThemeMode::Light)
        });
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| Root::new(view.clone(), window, cx));
            let _ = window.draw(cx).clear(cx);
        });
        let _ = &root;
        (view, cx)
    }

    /// 提交确认处理函数必须在数据库返回前就返回——这正是「界面不卡死」的契约。
    ///
    /// 用 `view.update` 而不是模拟点击：`simulate_event` 内部会
    /// `run_until_parked` 把后台任务抽干，等待时间会被算进点击耗时，
    /// 无法区分「UI 线程同步阻塞」与「后台执行后回桥」。
    #[gpui::test]
    fn data_change_commit_returns_before_database_responds(cx: &mut TestAppContext) {
        let (view, cx) = open_dirty_data_editor(cx, &[]);
        view.update(cx, |this, cx| {
            this.request_apply_data_changes(TabId(1), cx);
        });

        // 函数返回瞬间：提交仍在后台进行，标签处于「提交中」。
        let returned_early = view.update_in(cx, |this, window, cx| {
            this.confirm_apply_data_changes(window, cx);
            let editor_submitting = match this.controller.state().active_tab().map(|t| &t.kind)
            {
                Some(TabKind::DataEditor(editor)) => editor.submitting,
                _ => false,
            };
            (
                this.data_change_apply_in_flight(TabId(1)),
                editor_submitting,
            )
        });
        assert!(
            returned_early.0,
            "确认处理函数返回时提交仍应在后台进行：提交被放回 UI 线程同步执行了"
        );
        assert!(
            returned_early.1,
            "提交期间数据编辑器必须进入 submitting 状态供 UI 显示 loading"
        );

        // 有界等待后台提交回桥：确认框关闭、标签不再提交中、提交真正生效。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while view.read_with(cx, |main, _| {
            main.data_change_apply_in_flight(TabId(1))
                || main.pending_apply_data_changes == Some(TabId(1))
        }) {
            assert!(std::time::Instant::now() < deadline, "后台提交 30s 未回桥");
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (changes_empty, dirty) = view.read_with(cx, |main, _| {
            let tab = main
                .controller
                .state()
                .tabs
                .iter()
                .find(|tab| tab.id == TabId(1))
                .expect("数据编辑器标签仍存在");
            let changes_empty = match &tab.kind {
                TabKind::DataEditor(editor) => {
                    editor.changes.as_ref().is_none_or(|changes| changes.is_empty())
                }
                _ => false,
            };
            (changes_empty, tab.dirty)
        });
        assert!(changes_empty, "提交成功后改动集必须清空");
        assert!(!dirty, "提交成功后标签不再是 dirty");
    }

    /// 提交失败必须弹 Alert（而不是几秒后自动消失的轻提示）：
    /// 写入失败时改动仍留在编辑器里，是用户必须处理的结果。
    #[gpui::test]
    fn data_change_commit_failure_shows_alert_and_keeps_changes(cx: &mut TestAppContext) {
        let _dialog_guard = ai_ui_tests::dialog_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // 前置：提交确认框是自绘 overlay（不进官方 Dialog 栈），
        // 所以 `has_active_dialog` 在失败前应为 false，失败后才为 true。
        let (view, cx) = open_dirty_data_editor(cx, &[("demo.fail_submit", "true")]);
        view.update(cx, |this, cx| {
            this.request_apply_data_changes(TabId(1), cx);
        });
        assert!(
            !cx.update(|window, cx| {
                let _ = window.draw(cx).clear(cx);
                window.has_active_dialog(cx)
            }),
            "前置：提交确认框是自绘 overlay，不应占用官方 Dialog 栈"
        );
        let _ = view.update_in(cx, |this, window, cx| {
            this.confirm_apply_data_changes(window, cx);
        });

        // 有界等待后台失败回桥。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while view.read_with(cx, |main, _| main.data_change_apply_in_flight(TabId(1))) {
            assert!(std::time::Instant::now() < deadline, "后台提交 30s 未回桥");
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // 失败后：改动仍在（用户可重试或改写），提交态已清除，错误写入编辑器。
        let (changes_kept, submitting, error) = view.read_with(cx, |main, _| {
            let Some(TabKind::DataEditor(editor)) =
                main.controller.state().active_tab().map(|tab| &tab.kind)
            else {
                panic!("数据编辑器标签应仍存在");
            };
            (
                editor.changes.as_ref().is_some_and(|c| !c.is_empty()),
                editor.submitting,
                editor.error.clone(),
            )
        });
        assert!(changes_kept, "提交失败必须保留改动集，不能丢数据");
        assert!(!submitting, "失败回桥后必须清除提交中状态");
        assert!(
            error.as_ref().is_some_and(|e| e.title == "保存失败"),
            "失败原因必须落在编辑器错误态：{error:?}"
        );

        // Alert 真的打开了：`open_alert_dialog` 走官方 Dialog 栈，
        // `AlertDialog` 未暴露 debug_selector，因此用活动弹框计数断言。
        let alert_open = cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
            window.has_active_dialog(cx)
        });
        assert!(alert_open, "提交失败必须弹出 Alert，而不是轻量 toast");
        assert!(
            cx.debug_bounds("apply-data-changes-confirm").is_none(),
            "提交确认框必须先关闭，剩下的活动弹框只能是失败 Alert"
        );
        // 收尾关闭 Alert：GPUI 测试里官方 Dialog 栈跨用例共享，
        // 残留弹框会让并行的其他弹框测试误判。
        cx.update(|window, cx| {
            window.close_all_dialogs(cx);
        });
        println!("failure evidence: changes kept, submitting cleared, editor error set");
    }
}
