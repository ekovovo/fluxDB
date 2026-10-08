#[test]
fn ai_chat_tab_is_singleton_and_preserves_source_workspace() {
    let mut controller = AppController::with_mock_data();
    controller.dispatch(AppCommand::OpenQueryEditorInDatabase {
        connection_id: ConnectionId(1),
        database: Some("main".into()),
        schema: None,
    });
    let source = controller.state().active_tab.unwrap();
    let chat = controller.state().ai_conversation.clone();
    let AppEvent::TabOpened(ai_id) = controller.dispatch(AppCommand::OpenAiChatTab) else {
        panic!("首次打开应创建 AI 页签");
    };
    assert_eq!(
        controller.state().active_tab().unwrap().workspace(),
        Some(TabWorkspace {
            connection_id: ConnectionId(1),
            database: "main".into(),
        })
    );
    controller.dispatch(AppCommand::ActivateTab(source));
    assert_eq!(
        controller.dispatch(AppCommand::OpenAiChatTab),
        AppEvent::TabActivated(ai_id)
    );
    assert_eq!(controller.state().tabs.len(), 2);
    controller.dispatch(AppCommand::CloseTab(ai_id));
    assert_eq!(controller.state().active_tab, Some(source));
    assert_eq!(controller.state().ai_conversation, chat);
}

#[test]
fn ai_chat_tab_opened_from_home_has_no_database_scope() {
    let mut controller = AppController::new();
    controller.dispatch(AppCommand::OpenAiChatTab);
    assert_eq!(controller.state().active_tab().unwrap().workspace(), None);
}
