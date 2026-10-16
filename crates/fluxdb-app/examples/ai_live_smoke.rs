//! 显式启用的真实模型冒烟：只读真实配置、只发一次最小请求，**绝不打印凭据**。
//!
//! 用法：`FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_MODEL="<display_name 或 model>" cargo run -p fluxdb-app --example ai_live_smoke`
use fluxdb_app::AiConversationProvider;
use fluxdb_core::GenerationProvider;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_LIVE").ok().as_deref() != Some("1") {
        return Err("需要显式设置 FLUXDB_AI_LIVE=1（真实请求，会计费）".into());
    }
    let want = std::env::var("FLUXDB_AI_LIVE_MODEL").unwrap_or_else(|_| "公司自建".to_string());
    let storage = match std::env::var_os("FLUXDB_AI_LIVE_ROOT") {
        Some(root) => fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root)),
        None => fluxdb_storage::FileStorage::try_default()?,
    };
    let model_id = storage.with_ai_connection(|conn| {
        let models = fluxdb_storage::ai_platform::models::list_models_page(conn, 200, 0)?;
        models
            .into_iter()
            .find(|model| {
                model.enabled
                    && (model.display_name == want || model.model == want || model.id == want)
                    && model.kind == fluxdb_storage::ai_platform::models::ModelKind::Generation
            })
            .map(|model| model.id)
            .ok_or_else(|| {
                fluxdb_core::Error::new(
                    fluxdb_core::ErrorKind::Internal,
                    format!("未找到启用且可用的生成模型: {want}"),
                )
            })
    })?;
    let provider = AiConversationProvider::from_storage(&storage, &model_id)?;
    println!("provider 就绪（凭据已在进程内解密，不外显）");

    if std::env::var("FLUXDB_AI_LIVE_TOOLS").ok().as_deref() == Some("1") {
        return live_tools_smoke(&provider);
    }

    let cancel = fluxdb_core::CancelFlag::new();
    let request = fluxdb_core::ModelRequest {
        model: model_id,
        messages: vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            "只回复两个字：就绪",
        )],
        tools: Vec::new(),
        // 注意：Responses 协议下推理模型会先花 token 做思考，上限太小会被思考吃光而拿不到正文。
        max_output_tokens: Some(
            std::env::var("FLUXDB_AI_LIVE_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(256),
        ),
    };
    let events = ai_block_on(provider.stream(request, cancel.clone()))??;
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            fluxdb_core::ModelEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let finish = events.iter().find_map(|event| match event {
        fluxdb_core::ModelEvent::Finished(reason) => Some(reason),
        _ => None,
    });
    let usage = events.iter().find_map(|event| match event {
        fluxdb_core::ModelEvent::Usage(usage) => Some(usage),
        _ => None,
    });
    println!("finish={finish:?} usage={usage:?} text={text:?}");
    if text.trim().is_empty() {
        return Err("真实模型未返回正文".into());
    }
    Ok(())
}

/// 就绪 future 的最小阻塞器（与运行时同款，仅用于冒烟脚本）。
fn ai_block_on<F: std::future::Future>(future: F) -> Result<F::Output, fluxdb_core::Error> {
    use std::task::{Context, Poll, Wake, Waker};
    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: std::sync::Arc<Self>) {}
    }
    let waker = Waker::from(std::sync::Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    for _ in 0..64 {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => continue,
        }
    }
    Err(fluxdb_core::Error::new(
        fluxdb_core::ErrorKind::Internal,
        "真实 Provider 未在就绪轮次内完成",
    ))
}

/// 真实模型的工具回合：合成一个内存 demo 连接（不写用户库），走完整
/// schema 校验 → 审计 → 授权 → 执行 → 回执 → 回灌模型 链路。
fn live_tools_smoke(provider: &AiConversationProvider) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = std::collections::BTreeMap::new();
    options.insert("demo".to_string(), "true".to_string());
    let config = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(1),
        name: "live-smoke demo".into(),
        kind: fluxdb_core::DatabaseKind::Sqlite,
        endpoint: fluxdb_core::Endpoint::SqliteFile {
            path: "live-smoke.db".into(),
            read_only: true,
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    let catalog = fluxdb_app::standard_sql_catalog(
        std::sync::Arc::new(fluxdb_app::MapSqlResolver::new([config.clone()])),
        false,
    )?;
    println!(
        "工具目录: {:?}",
        catalog
            .specs()
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>()
    );

    let runner = fluxdb_app::TurnRunner {
        provider,
        tools: &catalog,
        limits: fluxdb_app::LoopLimits::default(),
        permission: fluxdb_app::TurnPermissionContext {
            agent_allowed_tools: Vec::new(),
            disabled_tools: Default::default(),
            policy: fluxdb_app::kernel_policy_for_connection(Some(&config)),
            plugin_digest: None,
        },
        audit: &fluxdb_app::KernelAuditHook,
        progress: None,
    };
    let state = fluxdb_app::TurnState {
        session_id: "live-smoke-session".into(),
        turn_id: "live-smoke-turn".into(),
        model: "live-smoke".into(),
        messages: vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            "请用工具查询 product 表的行，然后只用一句话说明你查到了什么。",
        )],
        max_output_tokens: Some(512),
        target_json: "{\"connection_id\":1}".into(),
    };
    let cancel = fluxdb_core::CancelFlag::new();
    let mut steering = Vec::new();
    let mut text = String::new();
    let mut tools_used: Vec<String> = Vec::new();
    let outcome = runner.run(state, &mut steering, &cancel, &mut |event| match event {
        fluxdb_app::LoopEvent::TextDelta(delta) => text.push_str(&delta),
        fluxdb_app::LoopEvent::ToolStarted { call, .. } => {
            tools_used.push(format!("start:{}", call.tool_id))
        }
        fluxdb_app::LoopEvent::ToolFinished { receipt, .. } => tools_used.push(format!(
            "done:{}:{}:affected={:?}",
            receipt.tool_id,
            if receipt.is_success() { "ok" } else { "fail" },
            receipt.affected_rows
        )),
        fluxdb_app::LoopEvent::Notice(notice) => tools_used.push(format!("notice:{notice}")),
        _ => {}
    });
    println!("outcome={outcome:?}");
    println!("tools={tools_used:?}");
    println!("final_text={text:?}");
    if !tools_used.iter().any(|entry| entry.starts_with("done:")) {
        return Err("真实模型没有走完工具调用链路".into());
    }
    Ok(())
}
