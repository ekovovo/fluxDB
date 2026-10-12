#[gpui_kit::test]
async fn current_page_large_sql_send_records_typed_excerpt_not_full_document(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
    });
    let tab = controller.state().active_tab.unwrap();
    let text = format!(
        "{}\nUNIQUE-MIDDLE-CONTENT\n{}",
        "a".repeat(300_000),
        "z".repeat(300_000)
    );
    controller.dispatch(AppCommand::UpdateQueryText { tab_id: tab, text });
    let (view, visual) = open_main(cx, &fixture, controller, ThemeMode::Light);
    open_panel(&view, visual);
    visual.update(|window, cx| {
        window.render_frame(cx);
        // 当前页面上下文默认附带，且发送框不再显示任何可见控件。
        assert!(window.try_find("ai-current-page").is_none());
    });
    let job = start_scripted_send(&fixture, &view, visual, "请搜索编辑器中段");
    fixture
        .storage
        .with_ai_connection(|conn| {
            let turn = sessions::load_turn(conn, &job.turn_id)?;
            let snapshot: serde_json::Value =
                serde_json::from_str(&turn.runtime_snapshot_json).unwrap();
            let page: serde_json::Value =
                serde_json::from_str(snapshot["ui_context"].as_str().unwrap()).unwrap();
            assert_eq!(page["page_kind"], "sql_editor");
            assert_eq!(page["delivery"], "excerpt");
            assert!(!snapshot.to_string().contains("UNIQUE-MIDDLE-CONTENT"));
            assert!(
                page["capabilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "editor.search")
            );
            Ok(())
        })
        .unwrap();
    wait_source(&view, visual, "page scripted send", |main| {
        main.ai_chat.running.is_none()
    })
    .await;
}

#[gpui_kit::test]
async fn current_page_capture_always_includes_loaded_page_data(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenDataEditor(ObjectPath {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
        kind: ObjectKind::Table,
        name: "users".into(),
    }));
    let first = controller.state().active_tab.unwrap();
    controller.dispatch(AppCommand::FinishDataPageLoad {
        tab_id: first,
        result: Ok(DataPage {
            columns: vec![fluxdb_core::Column {
                name: "email".into(),
                type_name: Some("TEXT".into()),
                nullable: true,
                primary_key: false,
                comment: None,
            }],
            rows: vec![Row {
                values: vec![CellValue::Text("PAGE-DATA-FIXTURE".into())],
            }],
            offset: 0,
            limit: 10,
            has_more: false,
        }),
    });
    let (view, visual) = open_main(cx, &fixture, controller, ThemeMode::Dark);
    open_panel(&view, visual);
    view.update(visual, |main, cx| {
        let page = main.capture_ai_page_context(cx, true);
        // 发送时自动固定当前页面与已加载数据快照。
        assert!(page.data_grant.is_some());
        assert!(
            !page.public_json().to_string().contains("PAGE-DATA-FIXTURE"),
            "数据值只经 page.read_rows 工具读取，不写首条消息"
        );
    });
    wait_source(&view, visual, "page toggle idle", |main| {
        main.ai_chat._io.is_none()
    })
    .await;
}

#[gpui_kit::test]
async fn sql_open_file_button_preserves_dirty_tab_and_sets_local_save_target(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let path = fixture.root.join("opened.sql");
    std::fs::write(&path, "\u{feff}SELECT 'FILE-FIXTURE';\r\n").unwrap();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: Some("scoped".into()),
    });
    let original = controller.state().active_tab.unwrap();
    controller.dispatch(AppCommand::UpdateQueryText {
        tab_id: original,
        text: "SELECT 'DIRTY-ORIGINAL';".into(),
    });
    let (view, visual) = open_main(cx, &fixture, controller, ThemeMode::Light);
    visual.update(|window, cx| {
        window.render_frame(cx);
        window.click("query-open-file", cx);
    });
    assert!(visual.did_prompt_for_paths());
    assert!(view.read_with(visual, |main, _| main.query_files.loading));
    visual.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    wait_source(&view, visual, "open local SQL", |main| {
        !main.query_files.loading
    })
    .await;
    view.read_with(visual,|main,_|{
        let state=main.controller.state();
        let old=state.tabs.iter().find(|t|t.id==original).unwrap();
        assert!(matches!(&old.kind,TabKind::QueryEditor(q) if q.text=="SELECT 'DIRTY-ORIGINAL';"));
        let current=state.active_tab().unwrap();
        assert_ne!(current.id,original);
        assert!(matches!(&current.kind,TabKind::QueryEditor(q) if q.text=="SELECT 'FILE-FIXTURE';\r\n"
            && q.schema.as_deref()==Some("scoped") && !q.running
            && q.origin==Some(QueryOrigin::File{path:path.clone()})));
        assert!(matches!(main.query_save_targets.get(&current.id),Some(QuerySaveTarget::Local(p)) if p==&path));
    });
}

#[gpui_kit::test]
async fn sql_open_file_cancel_and_invalid_file_leave_existing_text_unchanged(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let invalid = fixture.root.join("not-sql.txt");
    std::fs::write(&invalid, "not an SQL attachment").unwrap();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenQueryEditor(ConnectionId(1)));
    let original = controller.state().active_tab.unwrap();
    controller.dispatch(AppCommand::UpdateQueryText {
        tab_id: original,
        text: "SELECT 'KEEP';".into(),
    });
    let (view, visual) = open_main(cx, &fixture, controller, ThemeMode::Dark);
    for selected in [None, Some(vec![invalid.clone()])] {
        visual.update(|window, cx| {
            window.render_frame(cx);
            window.click("query-open-file", cx);
        });
        assert!(visual.did_prompt_for_paths());
        visual.simulate_path_prompt_response(|_| selected);
        wait_source(&view, visual, "cancel or reject SQL file", |main| {
            !main.query_files.loading
        })
        .await;
        view.read_with(visual, |main, _| {
            assert_eq!(main.controller.state().active_tab, Some(original));
            assert!(
                matches!(&main.controller.state().active_tab().unwrap().kind,
                TabKind::QueryEditor(q) if q.text=="SELECT 'KEEP';")
            );
        });
    }
    assert!(view.read_with(visual, |main, _| main.app_message.is_some()));
}

#[gpui_kit::test]
async fn current_page_table_metadata_carries_loaded_rows_without_putting_values_in_first_message(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = Fixture::seeded();
    let mut controller = fixture.controller();
    controller.dispatch(AppCommand::OpenDataEditor(ObjectPath {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
        kind: ObjectKind::Table,
        name: "users".into(),
    }));
    let tab = controller.state().active_tab.unwrap();
    controller.dispatch(AppCommand::FinishDataPageLoad {
        tab_id: tab,
        result: Ok(DataPage {
            columns: vec![fluxdb_core::Column {
                name: "email".into(),
                type_name: Some("TEXT".into()),
                nullable: true,
                primary_key: false,
                comment: None,
            }],
            rows: vec![Row {
                values: vec![CellValue::Text("DO-NOT-AUTO-SEND".into())],
            }],
            offset: 0,
            limit: 10,
            has_more: false,
        }),
    });
    let (view, visual) = open_main(cx, &fixture, controller, ThemeMode::Dark);
    open_panel(&view, visual);
    view.read_with(visual, |main, cx| {
        let page = main.capture_ai_page_context(cx, true);
        assert_eq!(page.kind, fluxdb_app::PageContextKind::TableData);
        assert_eq!(
            page.public_json()["details"]["columns"]["items"][0]["type"],
            "TEXT"
        );
        // 已加载数据值只经 page.read_rows 按行读取，不塞进首条消息。
        assert!(!page.public_json().to_string().contains("DO-NOT-AUTO-SEND"));
        assert!(page.data_grant.is_some());
        assert_eq!(
            page.public_json()["capabilities"],
            serde_json::json!(["page.read_rows"])
        );
        assert!(!page.public_json().to_string().contains("DO-NOT-AUTO-SEND"));
    });
    wait_source(&view, visual, "table context idle", |main| {
        main.ai_chat._io.is_none()
    })
    .await;
}
