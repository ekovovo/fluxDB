// E09 文件/@：附件授权在真实 View 链路上的可见反馈。
// 安全打开、CSV 分段、二进制反馈的生产逻辑由 fluxdb-app 集成测试覆盖，
// 这里只验证 UI 侧的 chip、摘要与错误提示，不重复断言文件内容解析。

#[gpui_kit::test]
fn e09_disabled_file_plugin_disables_upload_but_keeps_context_available(
    cx: &mut TestAppContext,
) {
    for theme in [ThemeMode::Light, ThemeMode::Dark] {
        let fixture = Fixture::seeded();
        let mut controller = fixture.controller();
        let plugin = controller.state().ai_plugin_instances.iter()
            .find(|plugin| plugin.id == "builtin.files").unwrap().clone();
        assert!(matches!(controller.dispatch(AppCommand::SetAiPluginInstanceEnabled {
            id: plugin.id, enabled: false, expected_revision: plugin.revision,
        }), AppEvent::AiPluginInstanceEnabled { .. }));
        let (view, cx) = open_main(cx, &fixture, controller, theme);
        open_panel(&view, cx);
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-add", cx);
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-upload", cx);
        });
        assert!(!cx.did_prompt_for_paths());
        view.read_with(cx, |main, _| {
            assert!(!main.ai_chat.attachment_loading);
            assert!(main.controller.state().ai_conversation.context_references.is_empty());
            assert!(main.app_message.is_none(), "禁用项不应调用上传回调并弹出提示");
        });
        let draft = view.read_with(cx, |main, cx| main.ai_chat.input.read(cx).value().to_string());
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("ai-chat-add-context", cx);
        });
        view.read_with(cx, |main, cx| {
            let value = main.ai_chat.input.read(cx).value();
            assert!(value.starts_with(&draft) && value.ends_with('@'));
            assert_ne!(value.as_str(), draft.as_str());
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.remove_window();
        });
    }
}

/// 走生产附件入口：点击「添加」→附件菜单→路径面板返回路径，等待授权完成。
async fn authorize_attachment(
    view: &Entity<NavicatMain>,
    cx: &mut VisualTestContext,
    path: &std::path::Path,
    expect_added: bool,
) -> Option<String> {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("ai-chat-add", cx);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("ai-chat-upload", cx);
    });
    assert!(cx.did_prompt_for_paths(), "附件入口必须打开系统路径选择");
    assert!(
        view.read_with(cx, |main, _| main.ai_chat.attachment_loading),
        "授权期间必须显示 loading，避免重复提交"
    );
    cx.simulate_path_prompt_response(|_| Some(vec![path.to_path_buf()]));
    wait_source(view, cx, "attachment authorization", |main| {
        !main.ai_chat.attachment_loading && main.ai_chat._io.is_none()
    })
    .await;
    let references = view.read_with(cx, |main, _| {
        main.controller.state().ai_conversation.context_references.clone()
    });
    if expect_added {
        assert_eq!(
            references.len(),
            1,
            "授权成功必须新增引用；错误: {:?}",
            view.read_with(cx, |main, _| (
                main.ai_chat.error.clone(),
                main.app_message.clone()
            ))
        );
        Some(references[0].id.clone())
    } else {
        assert!(
            references.is_empty(),
            "被拒文件不得留下引用: {references:?}"
        );
        assert!(
            view.read_with(cx, |main, _| main.app_message.is_some()),
            "拒绝必须给出用户可见消息"
        );
        None
    }
}

#[gpui_kit::test]
async fn e09_authorized_attachment_shows_chip_with_content_type_and_survives_send(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let csv = fixture.root.with_extension("csv");
    // 含引号转义与嵌入换行：摘要必须体现 CSV 类型，不能笼统称「文本文件」。
    std::fs::write(&csv, "id,name\n1,alice\n2,\"bob,\nline2\"\n").unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    let id = authorize_attachment(&view, cx, &csv, true).await.unwrap();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(SharedString::from(format!("ai-reference-chip:{id}")))
                .is_some(),
            "授权后 chip 必须立即可见"
        );
    });
    let summary = view.read_with(cx, |main, _| {
        main.controller
            .state()
            .ai_conversation
            .context_references[0]
            .summary
            .clone()
    });
    assert!(
        summary.contains("CSV") || summary.contains("TSV"),
        "摘要必须体现内容类型: {summary}"
    );
    assert!(summary.contains("字节"), "摘要必须给出大小: {summary}");
    // 引用随发送转入消息标签，chip 不随草稿清空而消失。
    let job = start_scripted_send(&fixture, &view, cx, "解释这个附件");
    assert!(cx.debug_bounds("ai-chat-references-scroll").is_none());
    // 运行快照只带不透明 id/标签/摘要：路径与正文都不得进入模型上下文。
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            // file_grants 是宿主侧授权数据（含真实路径），按设计保留在快照中；
            // 面向模型的公开段 context_references 必须只带 opaque id/标签/摘要。
            assert!(
                !snapshot["context_references"]
                    .to_string()
                    .contains(&csv.to_string_lossy().to_string()),
                "模型上下文不得包含附件路径"
            );
            assert!(
                !snapshot["context_references"]
                    .to_string()
                    .contains("line2"),
                "模型上下文不得包含附件正文"
            );
            assert_eq!(snapshot["file_grants"].as_array().unwrap().len(), 1);
            Ok(())
        })
        .unwrap();
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
    std::fs::remove_file(csv).unwrap();
}

#[cfg(unix)]
#[gpui_kit::test]
async fn e09_oversized_and_credential_like_attachments_are_rejected_with_visible_error(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let oversized = fixture.root.with_extension("sql");
    // 超过 16 MiB 上限：授权阶段即拒绝，不进入引用列表。
    std::fs::write(&oversized, vec![b'a'; 17 * 1024 * 1024]).unwrap();
    let credential = fixture.root.with_extension("credentials");
    std::fs::write(&credential, "secret=1").unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Dark);
    open_panel(&view, cx);
    authorize_attachment(&view, cx, &oversized, false).await;
    authorize_attachment(&view, cx, &credential, false).await;
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
    std::fs::remove_file(oversized).unwrap();
    std::fs::remove_file(credential).unwrap();
}

#[cfg(unix)]
#[gpui_kit::test]
async fn e09_symlink_attachment_is_rejected_and_plain_file_still_authorizes(
    cx: &mut TestAppContext,
) {
    use gpui_kit::test::TestWindowExt as _;
    use std::os::unix::fs::symlink;
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let plain = fixture.root.with_extension("sql");
    std::fs::write(&plain, "SELECT 'plain';").unwrap();
    // 链接指向存储目录：扩展名合法也必须拒绝，不能凭链接读取授权点之外的内容。
    let link = fixture.root.with_extension("lnk");
    symlink(fixture.storage.root_dir(), &link).unwrap();
    let (view, cx) = open_main(cx, &fixture, fixture.controller(), ThemeMode::Light);
    open_panel(&view, cx);
    assert!(authorize_attachment(&view, cx, &link, false).await.is_none());
    // 对照：普通文件仍能授权，说明拒绝来自链接本身而不是面板状态。
    assert!(authorize_attachment(&view, cx, &plain, true).await.is_some());
    click_selector(cx, "ai-chat-close");
    wait_idle(&view, cx);
    cx.update(|window, _| window.remove_window());
    std::fs::remove_file(plain).unwrap();
    std::fs::remove_file(link).unwrap();
}
