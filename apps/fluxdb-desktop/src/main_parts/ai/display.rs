impl NavicatMain {
    fn capture_ai_chat_tab_source(&mut self, command: &AppCommand) {
        let activates_ai = matches!(command, AppCommand::OpenAiChatTab)
            || matches!(command, AppCommand::ActivateTab(id) if self.controller.state().tabs.iter()
                .any(|tab| tab.id == *id && matches!(tab.kind, TabKind::AiChat { .. })));
        if activates_ai
            && !self
                .controller
                .state()
                .active_tab()
                .is_some_and(|tab| matches!(tab.kind, TabKind::AiChat { .. }))
        {
            self.ai_chat.context_tab = self.controller.state().active_tab;
        }
    }

    fn ai_chat_tab_closed(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.ai_chat.tab_id != Some(tab_id) {
            return;
        }
        self.ai_chat.tab_id = None;
        if self.ai_chat.open
            && self.ai_chat.mode == fluxdb_storage::ai_platform::settings::PanelMode::Tab
        {
            self.ai_chat.open = false;
            self.ai_chat.history = false;
            self.ai_chat.pending.push_back(self.ai_chat_ui_command(cx));
        }
    }

    fn mount_ai_chat_tab(&mut self, cx: &mut Context<Self>) {
        if self.ai_chat.mode != fluxdb_storage::ai_platform::settings::PanelMode::Tab {
            return;
        }
        self.dispatch(AppCommand::OpenAiChatTab, cx);
        self.ai_chat.tab_id = self
            .controller
            .state()
            .tabs
            .iter()
            .find(|tab| matches!(tab.kind, TabKind::AiChat { .. }))
            .map(|tab| tab.id);
    }

    fn set_ai_chat_mode(
        &mut self,
        mode: fluxdb_storage::ai_platform::settings::PanelMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ai_chat.mode == mode {
            return;
        }
        self.ai_chat.mode = mode;
        if mode == fluxdb_storage::ai_platform::settings::PanelMode::Tab {
            self.mount_ai_chat_tab(cx);
        } else if let Some(tab_id) = self.ai_chat.tab_id.take() {
            // 移动宿主，不卸载会话；回到进入 AI 页签前的业务页，保留其上下文。
            self.dispatch(AppCommand::CloseTab(tab_id), cx);
            if let Some(source) = self
                .ai_chat
                .context_tab
                .filter(|id| self.controller.state().tabs.iter().any(|tab| tab.id == *id))
            {
                self.dispatch(AppCommand::ActivateTab(source), cx);
            }
        }
        self.save_ai_chat_ui(window, cx);
        self.ai_chat
            .input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn ai_chat_mode_button(
        &self,
        disabled: bool,
        colors: UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use fluxdb_storage::ai_platform::settings::PanelMode;
        let selected = self.ai_chat.mode;
        let owner = cx.entity().downgrade();
        let menu_owner = owner.clone();
        Button::new("ai-chat-mode")
            .debug_selector(|| "ai-chat-mode".into())
            .small()
            .ghost()
            .cursor_pointer()
            .disabled(disabled)
            .tooltip("切换显示方式")
            .child(
                div()
                    .id("ai-chat-mode-label")
                    .aria_label("切换显示方式")
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(app_icon(AppIcon::Layout, 14., colors.muted))
                    .child(app_icon(AppIcon::ChevronDown, 10., colors.muted)),
            )
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                let mut menu = menu.check_side(gpui_component::Side::Right).min_w(px(190.));
                for (mode, label, icon) in [
                    (PanelMode::Drawer, "侧边抽屉", AppIcon::PanelRight),
                    (PanelMode::Embedded, "内嵌面板", AppIcon::Layout),
                    (PanelMode::Tab, "独立标签页", AppIcon::PanelTop),
                ] {
                    let owner = menu_owner.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .id(SharedString::from(format!(
                                    "ai-chat-mode-label:{}",
                                    mode.as_str()
                                )))
                                .aria_label(label)
                                .debug_selector(move || {
                                    format!("ai-chat-mode-option:{}", mode.as_str())
                                })
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(app_icon(icon, 14., colors.muted))
                                .child(label)
                        })
                        .checked(mode == selected)
                        .on_click(move |_, window, cx| {
                            let _ = owner
                                .update(cx, |this, cx| this.set_ai_chat_mode(mode, window, cx));
                        }),
                    );
                }
                menu
            })
            .on_open_change(move |open, _, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.ai_chat.mode_menu_open = *open;
                    cx.notify();
                });
            })
            .into_any_element()
    }
}
