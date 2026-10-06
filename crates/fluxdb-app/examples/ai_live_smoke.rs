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
    let storage = fluxdb_storage::FileStorage::try_default()?;
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

    let cancel = fluxdb_core::CancelFlag::new();
    let request = fluxdb_core::ModelRequest {
        model: model_id,
        messages: vec![fluxdb_core::ModelMessage::new(
            fluxdb_core::ModelRole::User,
            "只回复两个字：就绪",
        )],
        tools: Vec::new(),
        max_output_tokens: Some(16),
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
