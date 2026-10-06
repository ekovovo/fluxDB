//! 显式、离线、隔离的 T028 GUI fixture。只接收空目录，不读取用户配置或凭据。
use fluxdb_app::*;
use fluxdb_storage::ai_platform::{conversation,sessions};
use fluxdb_storage::Storage as _;
fn main() -> Result<(),Box<dyn std::error::Error>> {
    let root=std::env::args_os().nth(1).ok_or("必须传入新建 fixture root")?;
    let storage=fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root));
    let mut settings=fluxdb_core::Settings::default();settings.log_path=storage.root_dir().join("logs").to_string_lossy().into_owned();storage.save_settings(&settings)?;
    let mut app=AppController::new();app.set_ai_storage(storage.clone());
    let conn=storage.open_ai_connection()?;
    let existing:i64=conn.query_row("SELECT COUNT(*) FROM ai_sessions",[],|r|r.get(0))?;
    if existing!=0 {return Err("fixture root 非空，拒绝覆盖".into());}
    let input=AiModelSaveInput{id:None,display_name:"E2E 离线模型（不联网）".into(),provider:"openai".into(),model:"fixture".into(),endpoint:Some("http://127.0.0.1:1/v1".into()),endpoint_is_full_url:false,enabled:true,context_window_tokens:Some(32768),max_output_tokens:Some(1024),input_price_micros:None,output_price_micros:None,cached_input_price_micros:None,api_key:None,clear_key:false,expected_revision:None};
    let model_id=match app.dispatch(AppCommand::SaveAiModel(input)){AppEvent::AiModelSaved{id,..}=>id,other=>return Err(format!("fixture模型保存失败 {other:?}").into())};
    let agent_id=match app.dispatch(AppCommand::SaveAiAgent(AiAgentSaveInput{id:None,display_name:"E2E 数据库助手".into(),description:"测试 fixture".into(),system_prompt:"只展示离线夹具事件，不声称执行实际数据库操作。".into(),model_profile_id:Some(model_id),allowed_tools:Vec::new(),enabled:true,expected_revision:0})){AppEvent::AiAgentSaved{id,..}=>id,other=>return Err(format!("fixture Agent保存失败 {other:?}").into())};
    conversation::send(&conn,1001,true,"fixture-session",&agent_id,"fixture-turn","fixture-user","解释当前订单查询（E2E 夹具）","{\"connection_id\":7,\"database\":\"fixture_shop\"}",&serde_json::json!({"agent_id":agent_id}).to_string())?;
    conversation::checkpoint(&conn,1002,"fixture-session","fixture-turn","fixture-answer","这段查询按月份汇总订单金额。请核对付款时间与退款口径；此处是会话恢复夹具，不是实际查询结果。","已查看当前 SQL 摘录；这是公开摘要夹具。",Some(sessions::TurnStatus::Completed),None)?;
    sessions::append_part(&conn,1003,"fixture-plan","fixture-answer",sessions::PartKind::Plan,sessions::PartStatus::Complete,&serde_json::json!({"text":"1. 确认表字段与金额口径\n2. 检查时间筛选\n3. 解释聚合 SQL"}).to_string(),None,None,None)?;
    conn.execute("INSERT INTO ai_tool_calls(id,session_id,turn_id,iteration,source_ordinal,provider_call_id,tool_id,tool_version,arguments_redacted_json,target_json,plan_hash,status) VALUES('fixture-call','fixture-session','fixture-turn',1,0,'fixture-provider-call','db.query',1,'{}','{}','fixture-hash','prepared')",[])?;
    sessions::append_part(&conn,1004,"fixture-tool","fixture-answer",sessions::PartKind::ToolCallRef,sessions::PartStatus::Pending,&serde_json::json!({"text":"db.query · fixture_shop\nSELECT COUNT(*) FROM orders;\n准备状态；未执行（夹具）"}).to_string(),Some("fixture-call"),None,None)?;
    sessions::append_part(&conn,1005,"fixture-notice","fixture-answer",sessions::PartKind::Notice,sessions::PartStatus::Complete,"{\"text\":\"UI 夹具不使用真实模型、连接或凭据。\"}",None,None,None)?;
    println!("fixture session=fixture-session（无凭据；用于有数据窗口验收）");
    Ok(())
}
