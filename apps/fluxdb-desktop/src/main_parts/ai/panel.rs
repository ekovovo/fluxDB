// T028 原生面板，唯一内容树共享 Input 实体，切换形式不创建运行时或重复订阅。
// 已核对 0.6.0：Sheet.resizable 未消费该属性；复用 ResizablePanelGroup 实现硬比例范围和 resize 回调。
fn ai_chat_workspace(workspace:gpui::AnyElement,this:&mut NavicatMain,window:&mut Window,colors:UiColors,cx:&mut Context<NavicatMain>)->gpui::AnyElement {
    if !this.ai_chat.open {return workspace;}
    let viewport=f32::from(window.viewport_size().width) as f64;
    let mode=this.ai_chat.mode;
    let width=fluxdb_storage::ai_platform::settings::resolve_panel_width(mode,this.ai_chat.desired_width(),viewport) as f32;
    if width<=0. {return workspace;}
    let max=(viewport*mode.max_ratio()).floor() as f32;
    let min=320_f32.min(max);
    let weak=cx.entity().downgrade();
    let pane=ai_chat_panel(this,window,colors,cx).into_any_element();
    let group=gpui_component::h_resizable(if mode==fluxdb_storage::ai_platform::settings::PanelMode::Embedded{"ai-chat-embedded"}else{"ai-chat-drawer"})
        .on_resize(move|state,window,cx|{
            if let Some(size)=state.read(cx).sizes().last().copied(){
                let _=weak.update(cx,|this,cx|{
                    let w=fluxdb_storage::ai_platform::settings::resolve_panel_width(this.ai_chat.mode,f32::from(size) as f64,f32::from(window.viewport_size().width) as f64);
                    if this.ai_chat.mode==fluxdb_storage::ai_platform::settings::PanelMode::Drawer{this.ai_chat.drawer=w}else{this.ai_chat.embedded=w;}
                    this.save_ai_chat_ui(window,cx);cx.notify();
                });
            }
        });
    if mode==fluxdb_storage::ai_platform::settings::PanelMode::Embedded {
        group.child(gpui_component::resizable_panel().child(workspace))
            .child(gpui_component::resizable_panel().size(px(width)).size_range(px(min)..px(max)).child(pane)).into_any_element()
    } else {
        let overlay=div().size_full().bg(colors.app_bg.alpha(0.14)).on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|{this.close_ai_chat(window,cx);cx.stop_propagation();}));
        div().relative().size_full().child(workspace).child(div().absolute().inset_0().child(
            group.child(gpui_component::resizable_panel().child(overlay))
                .child(gpui_component::resizable_panel().size(px(width)).size_range(px(min)..px(max)).child(pane))
        )).into_any_element()
    }
}
fn ai_chat_small_button(id:&'static str,icon:AppIcon,title:&'static str,colors:UiColors)->gpui_component::button::Button {
    Button::new(id).small().ghost().cursor_pointer().tooltip(title).child(app_icon(icon,14.,colors.muted))
}
fn ai_chat_part(part:&fluxdb_app::AiConversationPart,expanded:bool,colors:UiColors,cx:&mut Context<NavicatMain>)->gpui::AnyElement {
    let payload:serde_json::Value=serde_json::from_str(&part.payload_json).unwrap_or(serde_json::Value::Null);
    let text=payload.get("text").or_else(||payload.get("summary")).and_then(|s|s.as_str()).unwrap_or("").to_string();
    if part.kind=="text" {return div().text_size(px(12.)).line_height(px(18.)).text_color(colors.text).child(text).into_any_element();}
    // 审批卡：必须在这里决定（批准/拒绝），决定后由宿主按 plan_hash 复核再执行。
    if part.kind=="notice" {
        if let Some(approval_id)=payload.get("approval_id").and_then(|value|value.as_str()) {
            let revision=payload.get("approval_revision").and_then(serde_json::Value::as_i64).unwrap_or(1);
            let decided=payload.get("decision").and_then(|value|value.as_str()).is_some();
            if !decided && part.status=="pending" {
                let approve_id=approval_id.to_string();
                let deny_id=approval_id.to_string();
                return div().border_1().border_color(colors.danger).rounded(colors.radius).p_2().flex().flex_col().gap_2()
                    .child(div().text_size(px(11.)).line_height(px(16.)).text_color(colors.text).child(text))
                    .child(div().flex().items_center().gap_2()
                        .child(Button::new(SharedString::from(format!("ai-approve:{approval_id}"))).debug_selector({let id=approval_id.to_string();move||format!("ai-approve:{id}")}).small().primary().cursor_pointer().child("批准执行").on_click(cx.listener(move|this,_,window,cx|this.decide_ai_chat_approval(approve_id.clone(),true,revision,window,cx))))
                        .child(Button::new(SharedString::from(format!("ai-deny:{approval_id}"))).debug_selector({let id=approval_id.to_string();move||format!("ai-deny:{id}")}).small().ghost().cursor_pointer().child("拒绝").on_click(cx.listener(move|this,_,window,cx|this.decide_ai_chat_approval(deny_id.clone(),false,revision,window,cx))))
                        .child(div().text_size(px(10.)).text_color(colors.muted).child("批准后仍会复核 plan_hash，参数/目标变了就失效")))
                    .into_any_element();
            }
        }
    }
    let (icon,label)=match part.kind.as_str(){"reasoning_summary"=>(AppIcon::Workflow,"公开思考摘要"),"plan"=>(AppIcon::List,"计划"),"tool_call_ref"=>(AppIcon::Database,"工具调用"),"tool_result_ref"=>(AppIcon::Table,"结果引用"),"artifact_ref"=>(AppIcon::Activity,"任务产物"),"context_ref"=>(AppIcon::FileSearch,"上下文"),_=>(AppIcon::CircleSlash,"任务说明")};
    let id=part.id.clone();let status=match part.status.as_str(){"streaming"=>"进行中","interrupted"=>"已停止","error"=>"失败","pending"=>"准备中",_=>"已保存"};
    div().border_1().border_color(colors.border_soft).rounded(colors.radius).p_2().flex().flex_col().gap_2()
        .child(Button::new(SharedString::from(format!("ai-part:{}",part.id))).debug_selector(move||format!("ai-part:{id}")).small().ghost().cursor_pointer()
            .child(div().flex().items_center().gap_2().child(app_icon(icon,13.,colors.muted)).child(label).child(div().text_color(colors.muted).text_size(px(10.)).child(status)))
            .on_click(cx.listener({let id=part.id.clone();move|this,_,window,cx|{
                let mut chat=this.controller.state().ai_conversation.clone();if let Some(index)=chat.expanded_parts.iter().position(|p|p==&id){chat.expanded_parts.remove(index);}else{chat.expanded_parts.push(id.clone());}
                this.controller.dispatch(AppCommand::AiConversation(fluxdb_app::AiConversationCommand::Apply{view:chat}));this.save_ai_chat_ui(window,cx);cx.notify();
            }})))
        .when(expanded,|this|this.child(div().max_h(px(160.)).overflow_y_scrollbar().text_size(px(11.)).line_height(px(16.)).child(if text.is_empty(){part.payload_json.clone()}else{text})))
        .into_any_element()
}
fn ai_chat_panel(this:&mut NavicatMain,_window:&mut Window,colors:UiColors,cx:&mut Context<NavicatMain>)->Stateful<Div> {
    let chat=this.controller.state().ai_conversation.clone();
    let scope_label=chat.target_json.as_deref().and_then(|raw|serde_json::from_str::<serde_json::Value>(raw).ok()).map(|v|{
        if let Some(db)=v.get("database").and_then(|d|d.as_str()){format!("{db} · 本轮目标已固定")}else if let Some(id)=v.get("connection_id").and_then(|d|d.as_i64()){format!("连接 {id} · 本轮目标已固定")}else{"无执行目标 · 可解释与规划".into()}
    }).unwrap_or_else(||"发送时自动捕获当前页面".into());
    let loading=this.ai_chat.loading;let history=this.ai_chat.history;let running=this.ai_chat.running.is_some();let mode=this.ai_chat.mode;
    let error=this.ai_chat.error.clone().or(chat.error.clone());
    let cancel_requested=this.ai_chat.cancel.as_ref().is_some_and(|flag|flag.is_cancelled());
    let ready=!loading&&this.ai_chat._io.is_none();
    let input=this.ai_chat.input.clone();let scroll=this.ai_chat.scroll.clone();
    let mut timeline=div().id("ai-chat-timeline").debug_selector(||"ai-chat-timeline".into()).flex_1().min_h(px(0.)).overflow_y_scroll().track_scroll(&scroll).on_scroll_wheel(cx.listener(|this,_,_,cx|{this.ai_chat.follow_reply=false;cx.notify();})).px_4().py_3().flex().flex_col().gap_3();
    if loading {timeline=timeline.child(div().text_color(colors.muted).text_size(px(12.)).child("正在加载会话…"));}
    else if history {
        if chat.sessions.is_empty(){timeline=timeline.child(div().text_color(colors.muted).text_size(px(12.)).child("暂无会话历史"));}
        for session in &chat.sessions {
            let id=session.id.clone();
            timeline=timeline.child(Button::new(SharedString::from(format!("ai-session:{id}"))).debug_selector({let id=id.clone();move||format!("ai-session:{id}")}).ghost().cursor_pointer().child(div().w_full().text_size(px(12.)).child(session.title.clone()))
                .on_click(cx.listener(move|this,_,window,cx|{
                    this.save_ai_chat_ui(window,cx);this.ai_chat.epoch+=1;this.ai_chat.history=false;
                    this.ai_chat_request(fluxdb_app::AiConversationCommand::Load{session_id:Some(id.clone())},window,cx);
                })));
        }
    } else if chat.messages.is_empty() && error.is_none(){
        timeline=timeline.child(div().flex_1()).child(div().flex().flex_col().items_center().gap_2().py_6().child(app_icon(AppIcon::Bot,28.,colors.muted)).child(div().text_size(px(14.)).font_weight(gpui::FontWeight::SEMIBOLD).child("有什么可以帮你？")).child(div().text_size(px(11.)).text_color(colors.muted).child("解释 SQL · 数据分析 · Redis 排障"))).child(div().flex_1());
    } else {
        for message in &chat.messages {
            let user=message.role=="user";
            timeline=timeline.child(div().id(SharedString::from(format!("ai-message:{}",message.id))).debug_selector({let id=message.id.clone();move||format!("ai-message:{id}")}).flex().flex_col().gap_2().p_3().rounded(colors.radius_lg)
                .bg(if user{colors.panel_alt}else{colors.panel_bg})
                .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::MEDIUM).text_color(colors.muted).child(if user{"你"}else{"AI"}))
                .children(message.parts.iter().map(|p|ai_chat_part(p,chat.expanded_parts.contains(&p.id),colors,cx))));
        }
        if running{timeline=timeline.child(div().text_size(px(11.)).text_color(colors.muted).child("正在生成…"));}
    }
    if !running && chat.active_turn_id.is_some() {
        timeline=timeline.child(gpui_component::alert::Alert::warning("ai-chat-unfinished","存在上次未结束的任务记录。打开面板不会重放请求；恢复/核实执行结果将在 T029 接通。"));
    }
    if let Some(error)=error {timeline=timeline.child(gpui_component::alert::Alert::error("ai-chat-error",error).small()).child(Button::new("ai-chat-retry").debug_selector(||"ai-chat-retry".into()).small().ghost().child("重新加载会话").on_click(cx.listener(|this,_,window,cx|this.open_ai_chat(window,cx))));}
    div().id("ai-chat-panel").debug_selector(||"ai-chat-panel".into()).size_full().min_w(px(0.)).bg(colors.panel_bg).border_l_1().border_color(colors.border).occlude().flex().flex_col()
        .on_mouse_down(MouseButton::Left,|_,_,cx|cx.stop_propagation())
        .on_key_down(cx.listener(|this,event:&gpui::KeyDownEvent,window,cx|{if event.keystroke.key=="escape"{this.close_ai_chat(window,cx);cx.stop_propagation();}}))
        .child(div().h(px(48.)).flex_none().border_b_1().border_color(colors.border_soft).px_3().flex().items_center().gap_1()
            .child(div().flex_1().min_w(px(0.)).text_size(px(13.)).font_weight(gpui::FontWeight::SEMIBOLD).truncate().child(if history{"会话历史".into()}else{chat.title.clone()}))
            .child(ai_chat_small_button("ai-chat-new",AppIcon::Plus,"新建会话",colors).debug_selector(||"ai-chat-new".into()).disabled(!ready).on_click(cx.listener(|this,_,window,cx|this.new_ai_chat(window,cx))))
            .child(ai_chat_small_button("ai-chat-mode",AppIcon::PanelRight,if mode==fluxdb_storage::ai_platform::settings::PanelMode::Drawer{"切换为内嵌"}else{"切换为抽屉"},colors).debug_selector(||"ai-chat-mode".into()).disabled(!ready).on_click(cx.listener(|this,_,window,cx|{
                this.ai_chat.mode=if this.ai_chat.mode==fluxdb_storage::ai_platform::settings::PanelMode::Drawer{fluxdb_storage::ai_platform::settings::PanelMode::Embedded}else{fluxdb_storage::ai_platform::settings::PanelMode::Drawer};this.save_ai_chat_ui(window,cx);cx.notify();
            })))
            .child(ai_chat_small_button("ai-chat-history",AppIcon::CalendarClock,"会话历史",colors).debug_selector(||"ai-chat-history".into()).disabled(!ready).on_click(cx.listener(|this,_,_,cx|{this.ai_chat.history=!this.ai_chat.history;cx.notify();})))
            .child(ai_chat_small_button("ai-chat-close",AppIcon::Close,"关闭",colors).debug_selector(||"ai-chat-close".into()).on_click(cx.listener(|this,_,window,cx|this.close_ai_chat(window,cx)))))
        .child(div().px_3().py_2().border_b_1().border_color(colors.border_soft).flex().items_center().gap_2().child(app_icon(AppIcon::Database,12.,colors.muted)).child(div().text_size(px(10.)).text_color(colors.muted).child(scope_label)))
        .child(timeline)
        .when(this.ai_chat.new_reply,|panel|panel.child(Button::new("ai-chat-new-reply").small().ghost().child("查看新回复").on_click(cx.listener(|this,_,_,cx|{this.ai_chat.scroll.scroll_to_bottom();this.ai_chat.follow_reply=true;this.ai_chat.new_reply=false;cx.notify();}))))
        .child(div().p_3().border_t_1().border_color(colors.border_soft).flex_none().flex().flex_col().gap_2()
            .child(div().border_1().rounded(colors.radius_lg).border_color(colors.border).bg(colors.input_bg).p_2().debug_selector(||"ai-chat-input".into()).child(Input::new(&input).appearance(false).text_size(px(12.))).child(div().flex().items_center().justify_between().gap_2()
                .child(div().text_size(px(10.)).text_color(colors.muted).child("当前页面上下文"))
                .child(if running {Button::new("ai-chat-stop").debug_selector(||"ai-chat-stop".into()).small().ghost().cursor_pointer().disabled(cancel_requested).child(div().flex().items_center().gap_1().child(app_icon(AppIcon::Square,12.,colors.danger)).child(if cancel_requested{"停止中…"}else{"停止"})).on_click(cx.listener(|this,_,_,cx|{if let Some(flag)=&this.ai_chat.cancel{flag.request();}cx.notify();})).into_any_element()}
                else {Button::new("ai-chat-send").debug_selector(||"ai-chat-send".into()).small().primary().cursor_pointer().disabled(!ready).child(div().flex().items_center().gap_1().child(app_icon(AppIcon::ArrowUp,12.,colors.text)).child("发送")).on_click(cx.listener(|this,_,window,cx|this.send_ai_chat(window,cx))).into_any_element()})))
            .child(div().text_size(px(10.)).text_color(colors.muted).child("模型跟随 Agent · Jev 未配置不影响对话")))
}
