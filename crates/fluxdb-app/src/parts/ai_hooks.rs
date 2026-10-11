// 插件 Hook 契约：有序绑定、显式冲突、不可改变目标/回执，第三方加载仍不实现。
#[derive(Clone, Debug)]
pub enum AgentHookInput {
    BeforeTool(fluxdb_core::ToolCallRequest),
    AfterTool(fluxdb_core::ToolReceipt),
}
#[derive(Clone, Debug)]
pub enum AgentHookDecision {
    Observe,
    Arguments(String),
    Deny(String),
    RequireApproval(String),
}
pub trait AgentHook: Send + Sync {
    /// 回调不得阻塞或递归派发；必须检查 cancel/deadline。宿主再次核对耗时/体积。
    fn invoke(&self,input:&AgentHookInput,cancel:&fluxdb_core::CancelFlag)->fluxdb_core::Result<AgentHookDecision>;
}
pub struct HookBinding {
    pub id:String,
    pub priority:i64,
    pub enabled:bool,
    pub best_effort:bool,
    pub hook:Box<dyn AgentHook>,
}
#[derive(Default)]
pub struct HookPipeline { bindings:Vec<HookBinding> }
impl HookPipeline {
    pub fn new(mut bindings:Vec<HookBinding>)->Self {
        bindings.sort_by(|a,b|(a.priority,&a.id).cmp(&(b.priority,&b.id)));
        Self{bindings}
    }
    pub fn before(&self,call:&fluxdb_core::ToolCallRequest,cancel:&fluxdb_core::CancelFlag)->fluxdb_core::Result<(fluxdb_core::ToolCallRequest,Option<String>,Option<String>)> {
        let decisions=self.invoke(&AgentHookInput::BeforeTool(call.clone()),cancel)?;
        let mut patch=None; let mut deny=None; let mut approval=None;
        for decision in decisions {
            match decision {
                AgentHookDecision::Observe=>{},
                AgentHookDecision::Arguments(raw)=> {
                    if patch.as_ref().is_some_and(|previous|previous!=&raw) {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"Hook 参数补丁冲突"));}
                    if raw.len()>DEFAULT_MAX_ARGUMENT_BYTES {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"Hook 参数超过上限"));}
                    patch=Some(raw);
                }
                AgentHookDecision::Deny(reason)=>deny=Some(reason),
                AgentHookDecision::RequireApproval(reason)=>approval=Some(reason),
            }
        }
        let mut final_call=call.clone();
        if let Some(raw)=patch {final_call.arguments_json=raw;}
        Ok((final_call,deny,approval))
    }
    pub fn after(&self,receipt:&fluxdb_core::ToolReceipt,cancel:&fluxdb_core::CancelFlag)->fluxdb_core::Result<()> {
        for decision in self.invoke(&AgentHookInput::AfterTool(receipt.clone()),cancel)? {
            if !matches!(decision,AgentHookDecision::Observe) {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"after-tool Hook 不允许改写执行事实"));}
        }
        Ok(())
    }
    fn invoke(&self,input:&AgentHookInput,cancel:&fluxdb_core::CancelFlag)->fluxdb_core::Result<Vec<AgentHookDecision>> {
        thread_local! {static ACTIVE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
        if ACTIVE.with(|v|v.replace(true)) {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"禁止递归调用 Hook"));}
        struct Reset;
        impl Drop for Reset {fn drop(&mut self){ACTIVE.with(|v|v.set(false));}}
        let _reset=Reset;
        if self.bindings.len()>64 {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"Hook 绑定超过上限"));}
        let mut decisions=Vec::new();
        for binding in self.bindings.iter().filter(|b|b.enabled) {
            if cancel.is_cancelled() {return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"Hook 已取消"));}
            let start=std::time::Instant::now();
            let result=binding.hook.invoke(input,cancel);
            let result=if start.elapsed()>std::time::Duration::from_millis(100) {Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"Hook 超过执行时限"))}else{result};
            match result {
                Ok(decision)=>decisions.push(decision),
                Err(error) if binding.best_effort=>tracing::warn!(binding_id=%binding.id,"可选 Hook 已降级：{}",error.message),
                Err(error)=>return Err(error),
            }
        }
        Ok(decisions)
    }
}

/// 内置 telemetry 只读事件；它不执行任何外部代码，禁用绑定不会被调用。
struct BuiltinTelemetryHook;
impl AgentHook for BuiltinTelemetryHook {
    fn invoke(&self,input:&AgentHookInput,_:&fluxdb_core::CancelFlag)->fluxdb_core::Result<AgentHookDecision> {
        let tool_id=match input {AgentHookInput::BeforeTool(call)=>&call.tool_id,AgentHookInput::AfterTool(receipt)=>&receipt.tool_id};
        tracing::debug!(tool_id=%tool_id,"内置工具生命周期事件");
        Ok(AgentHookDecision::Observe)
    }
}
pub fn ai_builtin_hooks(storage:&fluxdb_storage::FileStorage,job:&AiConversationJob)->fluxdb_core::Result<HookPipeline> {
    with_ai_storage(storage,|conn| {
        let registry = BuiltinRegistry::default();
        let session=fluxdb_storage::ai_platform::sessions::load_session(conn,&job.session_id)?;
        let instances=fluxdb_storage::ai_platform::plugins::list_plugin_instances(conn)?;
        let bindings=fluxdb_storage::ai_platform::plugins::list_agent_plugin_bindings(conn,&session.agent_id)?;
        let hooks=bindings.iter().filter(|b|b.enabled).filter_map(|b| {
            let instance=instances.iter().find(|i|i.id==b.plugin_instance_id&&i.enabled&&i.package_key=="fluxdb.lifecycle.telemetry")?;
            let descriptor = registry.find_by_package(&instance.package_key)?;
            if !descriptor.capabilities.contains(&BuiltinCapability::AgentHook) {
                return None;
            }
            Some(HookBinding{id:b.id.clone(),priority:b.priority,enabled:instance.enabled,best_effort:true,hook:Box::new(BuiltinTelemetryHook) as Box<dyn AgentHook>})
        }).collect();
        Ok(HookPipeline::new(hooks))
    })
}
