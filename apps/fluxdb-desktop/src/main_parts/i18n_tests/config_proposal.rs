#[gpui::test]
fn configuration_confirmation_persists_only_after_real_click(cx: &mut TestAppContext) {
    use crate::{AppMessageKind, SettingsPanelSection};
    use fluxdb_app::{AppCommand, ConfigProposal, ConfigProposalKind};
    let _guard = locale_guard();
    let temp = tempfile::tempdir().expect("isolated profile");
    let storage = FileStorage::new(temp.path().join("profile"));
    let saved = Settings {
        locale: "en".into(),
        editor_font_size: 12,
        ..Default::default()
    };
    storage.save_settings(&saved).expect("save baseline");
    storage
        .with_ai_connection(|conn| {
            use fluxdb_storage::ai_platform::{agents, sessions};
            sessions::create_session(
                conn,
                crate::now_millis(),
                "config-session",
                agents::BUILTIN_AGENT_ID,
                "UI test",
            )?;
            sessions::start_turn(
                conn,
                crate::now_millis(),
                "config-session",
                "config-turn",
                "{}",
                "{}",
            )?;
            Ok(())
        })
        .expect("turn");
    init_i18n(
        &app_assets_base_path().join("locales"),
        &storage.root_dir().join("locales"),
        "en",
    );
    let mut controller = AppController::new();
    controller.set_ai_storage(storage.clone());
    controller.dispatch(AppCommand::SaveSettings(saved.clone()));
    let proposal = ConfigProposal {
        request_id: "ui-config-call".into(),
        turn_id: "config-turn".into(),
        expected_revision: fluxdb_app::settings_fingerprint(&saved),
        created_at_ms: crate::now_millis(),
        kind: ConfigProposalKind::SettingUpdate(
            fluxdb_app::parse_settings_prefill(
                &serde_json::json!({"section":"editor","fields":{"editor_font_size":18}}),
            )
            .expect("prefill"),
        ),
    };
    cx.update(|cx| {
        extend_component_i18n();
        gpui_component::init(cx);
    });
    let profile = storage.clone();
    let (window, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            let mut app = navicat_main_new(
                window,
                cx,
                controller,
                profile,
                gpui_component::ThemeMode::Light,
            );
            // 准备工具已生成的提案，不直接调用确认处理函数。
            app.pending_config_proposal = Some(proposal);
            app.settings_panel_section = SettingsPanelSection::Editor;
            app
        });
        Root::new(view, window, cx)
    });
    let view = cx.update(|window, cx| {
        window
            .root::<Root>()
            .expect("root")
            .expect("root view")
            .read(cx)
            .view()
            .clone()
            .downcast::<NavicatMain>()
            .expect("NavicatMain")
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert_eq!(
        storage
            .load_settings()
            .expect("not confirmed")
            .editor_font_size,
        12
    );
    let cancel = cx
        .debug_bounds("config-proposal-cancel")
        .expect("cancel button");
    cx.simulate_click(cancel.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|_, cx| assert!(view.read(cx).pending_config_proposal.is_none()));
    assert_eq!(
        storage.load_settings().expect("cancelled").editor_font_size,
        12
    );
    // 准备另一个提案，随后只通过生产按钮触发保存。
    cx.update(|_, cx| {
        view.update(cx, |app, cx| {
            app.pending_config_proposal = Some(ConfigProposal {
                request_id: "ui-config-apply".into(),
                turn_id: "config-turn".into(),
                expected_revision: fluxdb_app::settings_fingerprint(&saved),
                created_at_ms: crate::now_millis(),
                kind: ConfigProposalKind::SettingUpdate(
                    fluxdb_app::parse_settings_prefill(
                        &serde_json::json!({"section":"editor","fields":{"editor_font_size":18}}),
                    )
                    .expect("prefill"),
                ),
            });
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let apply = cx
        .debug_bounds("config-proposal-apply")
        .expect("apply button");
    cx.simulate_click(apply.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        storage.load_settings().expect("confirmed").editor_font_size,
        18
    );
    cx.update(|_, cx| {
        let app = view.read(cx);
        assert!(!app.config_proposal_busy);
        assert!(app.pending_config_proposal.is_none());
        assert_eq!(app.controller.state().settings.editor_font_size, 18);
        let message = app
            .app_message
            .as_ref()
            .expect("observable success feedback");
        assert_eq!(message.kind, AppMessageKind::Success);
        assert_eq!(
            message.text,
            crate::tr("app.ai.config_proposal.applied").to_string()
        );
    });
    // 用户已有不同的未保存草稿时，真实确认按钮不能覆盖草稿或写盘。
    cx.update(|_, cx| {
        view.update(cx, |app, cx| {
            app.settings_editor_draft.editor_font_size = 20;
            app.pending_config_proposal = Some(ConfigProposal {
                request_id: "ui-config-dirty".into(),
                turn_id: "config-turn".into(),
                expected_revision: fluxdb_app::settings_fingerprint(
                    &app.controller.state().settings,
                ),
                created_at_ms: crate::now_millis(),
                kind: ConfigProposalKind::SettingUpdate(
                    fluxdb_app::parse_settings_prefill(&serde_json::json!({
                        "section":"editor","fields":{"editor_font_size":16}
                    }))
                    .expect("conflicting settings"),
                ),
            });
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let apply = cx
        .debug_bounds("config-proposal-apply")
        .expect("conflicting apply button");
    cx.simulate_click(apply.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        storage.load_settings().expect("conflict unchanged").editor_font_size,
        18
    );
    cx.update(|_, cx| {
        let app = view.read(cx);
        assert!(app.pending_config_proposal.is_some());
        assert!(!app.config_proposal_busy);
        assert_eq!(app.settings_editor_draft.editor_font_size, 20);
        assert_eq!(app.controller.state().settings.editor_font_size, 18);
        let message = app.app_message.as_ref().expect("conflict feedback");
        assert_eq!(message.kind, AppMessageKind::Warning);
        assert_eq!(
            message.text,
            crate::tr("app.ai.config_proposal.settings_draft_dirty").to_string()
        );
    });
    // 放弃测试草稿后，旧 revision 的提案通过真实点击也必须失败。
    cx.update(|_, cx| {
        view.update(cx, |app, cx| {
            app.settings_editor_draft = app.controller.state().settings.clone();
            app.pending_config_proposal = Some(ConfigProposal {
                request_id: "ui-config-stale".into(),
                turn_id: "config-turn".into(),
                expected_revision: fluxdb_app::settings_fingerprint(&saved),
                created_at_ms: crate::now_millis(),
                kind: ConfigProposalKind::SettingUpdate(
                    fluxdb_app::parse_settings_prefill(&serde_json::json!({
                        "section":"editor","fields":{"editor_font_size":20}
                    }))
                    .expect("stale prefill"),
                ),
            });
            cx.notify();
        })
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let apply = cx
        .debug_bounds("config-proposal-apply")
        .expect("stale apply button");
    cx.simulate_click(apply.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        storage
            .load_settings()
            .expect("stale unchanged")
            .editor_font_size,
        18
    );
    cx.update(|_, cx| {
        let app = view.read(cx);
        let message = app
            .app_message
            .as_ref()
            .expect("observable failure feedback");
        assert_eq!(message.kind, AppMessageKind::Error);
        assert!(
            message
                .text
                .contains(crate::tr("app.ai.config_proposal.stale").as_ref())
        );
        assert_eq!(app.controller.state().settings.editor_font_size, 18);
    });
    let _ = window;
}
