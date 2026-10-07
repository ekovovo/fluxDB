#[gpui::test]
fn drawer_preserves_pages_and_embedded_keeps_sidebar_visible_light(cx: &mut TestAppContext) {
    check_workspace_modes(cx, ThemeMode::Light);
}

#[gpui::test]
fn drawer_preserves_pages_and_embedded_keeps_sidebar_visible_dark(cx: &mut TestAppContext) {
    check_workspace_modes(cx, ThemeMode::Dark);
}

fn check_workspace_modes(cx: &mut TestAppContext, theme: ThemeMode) {
    // 编辑器组件用真实 smol Timer 做防抖；异步 io 反应堆线程的唤醒会被测试
    // 调度器判定为"不确定性"（Markdown 正文渲染拉长本用例时长后更易命中）。
    // 允许 parking 后，调度器不再把后台唤醒当失败（与本文件其他流式用例同一处理）。
    cx.executor().allow_parking();
    let database = ObjectPath {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
        name: "main".into(),
        kind: ObjectKind::Database,
    };
    let table = ObjectPath {
        name: "Product".into(),
        kind: ObjectKind::Table,
        ..database.clone()
    };
    for (page, command) in [
        ("home", AppCommand::DeactivateTab),
        ("settings", AppCommand::OpenSettings),
        (
            "objects",
            AppCommand::OpenObjectList(Some(database.clone())),
        ),
        ("query", AppCommand::OpenQueryEditor(ConnectionId(1))),
        ("table", AppCommand::OpenDataEditor(table.clone())),
    ] {
        let fixture = Fixture::seeded();
        let mut controller = fixture.controller();
        controller.dispatch(AppCommand::ReplaceConnections(vec![ConnectionConfig {
            id: ConnectionId(1),
            name: "demo".into(),
            kind: DatabaseKind::Sqlite,
            endpoint: fluxdb_core::Endpoint::SqliteFile {
                path: "demo.db".into(),
                read_only: true,
            },
            options: [("demo".into(), "true".into())].into_iter().collect(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
        }]));
        controller.dispatch(AppCommand::OpenConnection(ConnectionId(1)));
        controller.dispatch(AppCommand::LoadObjectChildren(database.clone()));
        controller.dispatch(command);
        let (view, cx) = open_main(cx, &fixture, controller, theme);
        cx.simulate_resize(size(px(1400.), px(800.)));
        cx.run_until_parked();
        draw(cx);
        let workspace_before = cx.debug_bounds("database-workspace").unwrap();
        let page_before = cx.debug_bounds("workspace-page").unwrap();
        let sidebar_before = cx.debug_bounds("connection-browser").unwrap();
        let tree_before = cx.debug_bounds("connection-tree-viewport").unwrap();
        assert!(tree_before.size.height > px(100.));
        open_panel(&view, cx);
        assert_eq!(
            view.read_with(cx, |main, _| main.ai_chat.mode),
            settings::PanelMode::Drawer
        );
        assert_eq!(
            cx.debug_bounds("database-workspace").unwrap(),
            workspace_before,
            "抽屉不能改变底层工作区宽高: {page}"
        );
        assert_eq!(
            cx.debug_bounds("workspace-page").unwrap(),
            page_before,
            "抽屉不能压缩底层页面: {page}"
        );
        assert_eq!(
            cx.debug_bounds("connection-browser").unwrap(),
            sidebar_before
        );
        assert_eq!(
            cx.debug_bounds("connection-tree-viewport").unwrap(),
            tree_before
        );
        let row = cx.debug_bounds("connection-row:1").unwrap();
        assert!(row.top() >= tree_before.top() && row.bottom() <= tree_before.bottom());
        let count = view.read_with(cx, |main, _| {
            main.controller.state().connections[0].objects.len()
        });
        assert!(count > 0);
        click_selector(cx, "ai-chat-mode");
        wait_idle(&view, cx);
        let embedded = cx.debug_bounds("database-workspace").unwrap();
        let pane = cx.debug_bounds("ai-chat-panel").unwrap();
        assert!(embedded.size.width < workspace_before.size.width);
        assert_eq!(embedded.size.height, workspace_before.size.height);
        assert!(embedded.right() <= pane.left() + px(1.));
        let tree = cx.debug_bounds("connection-tree-viewport").unwrap();
        assert_eq!(
            tree.size.height, tree_before.size.height,
            "内嵌只分配横向空间，不压缩连接树高度: {page}"
        );
        assert!(cx.debug_bounds("workspace-page").unwrap().size.height > px(100.));
        assert_eq!(
            view.read_with(cx, |main, _| main.controller.state().connections[0]
                .objects
                .len()),
            count
        );
        click_selector(cx, "ai-chat-mode");
        wait_idle(&view, cx);
        assert_eq!(
            cx.debug_bounds("database-workspace").unwrap(),
            workspace_before
        );
        assert_eq!(cx.debug_bounds("workspace-page").unwrap(), page_before);
        // 点遮罩关闭抽屉，底层连接行不能被穿透触发。
        click_selector(cx, "connection-row:1");
        wait_idle(&view, cx);
        assert!(cx.debug_bounds("ai-chat-panel").is_none());
        assert_eq!(
            cx.debug_bounds("database-workspace").unwrap(),
            workspace_before
        );
        assert!(view.read_with(cx, |main, _| {
            main.controller.state().connections[0].connected
        }));
        // 释放编辑器语法/光标 Timer，不能让上一个窗口的任务进入下一轮测试调度器。
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
    }
}
