// 面板控件复用 gpui-component Input/ResizablePanel。仅宿主 GUI 状态，业务在 app。
struct AiChatUi {
    open:bool, loading:bool, history:bool, dirty:bool, error:Option<String>,
    mode:fluxdb_storage::ai_platform::settings::PanelMode, drawer:f64, embedded:f64,
    input:Entity<InputState>, _input_subscription:Subscription,
    scroll:gpui::ScrollHandle,
    pending:std::collections::VecDeque<fluxdb_app::AiConversationCommand>,
    _io:Option<Task<()>>, _pump:Option<Task<()>>,
    running:Option<fluxdb_app::AiConversationJob>, cancel:Option<fluxdb_core::CancelFlag>,
    epoch:u64, debug_open_pending:bool, follow_reply:bool, new_reply:bool, scripted_fixture:bool,
}
impl AiChatUi {
    fn new(window:&mut Window,cx:&mut Context<NavicatMain>)->Self {
        let input=cx.new(|cx|InputState::new(window,cx).multi_line(true).rows(3).placeholder("描述你的目标…"));
        let sub=cx.subscribe(&input,|this:&mut NavicatMain,_,event:&InputEvent,cx|{
            if matches!(event,InputEvent::Change){this.ai_chat.dirty=true;cx.notify();}
        });
        Self {open:false,loading:false,history:false,dirty:false,error:None,mode:fluxdb_storage::ai_platform::settings::PanelMode::Drawer,drawer:460.,embedded:420.,input,_input_subscription:sub,scroll:gpui::ScrollHandle::new(),pending:std::collections::VecDeque::new(),_io:None,_pump:None,running:None,cancel:None,epoch:0,follow_reply:true,new_reply:false,scripted_fixture:cfg!(debug_assertions)&&std::env::var("FLUXDB_AI_E2E_SCRIPTED").is_ok_and(|v|v=="1"),debug_open_pending:cfg!(debug_assertions)&&std::env::var("FLUXDB_DEBUG_AI_CHAT").is_ok_and(|v|v=="1")}
    }
    fn desired_width(&self)->f64 {if self.mode==fluxdb_storage::ai_platform::settings::PanelMode::Drawer{self.drawer}else{self.embedded}}
}
