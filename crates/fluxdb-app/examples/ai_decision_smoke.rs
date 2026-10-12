//! T036 §37.3 R02 真实“决策模型”冒烟：只读真实配置，只发一次 System One Choice 请求，
//! **绝不打印凭据**，不改配置/默认选择/插件开关。
//!
//! 用法：
//!   FLUXDB_AI_DECISION_LIVE=1 FLUXDB_AI_DECISION_MODEL='决策模型' \
//!   cargo run -p fluxdb-app --example ai_decision_smoke [-- 候选A 候选B ...]
//!
//! `FLUXDB_AI_DECISION_ROOT` 可指向隔离配置副本（默认用当前用户配置）。
//! 输出只包含脱敏协议结果（chosen/confidence/usage/耗时）与固定原因，不含正文/密钥。
use fluxdb_app::decision_provider_from_storage_with_timeout;
use fluxdb_core::{CancelFlag, DecisionProvider, DecisionRequest};
use fluxdb_storage::ai_platform::models::{ModelKind, list_models_page};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("FLUXDB_AI_DECISION_LIVE").ok().as_deref() != Some("1") {
        return Err("需要显式设置 FLUXDB_AI_DECISION_LIVE=1（真实请求，会计费）".into());
    }
    let storage = match std::env::var_os("FLUXDB_AI_DECISION_ROOT") {
        Some(root) => fluxdb_storage::FileStorage::new(std::path::PathBuf::from(root)),
        None => fluxdb_storage::FileStorage::try_default()
            .map_err(|e| format!("解析默认配置目录失败: {e}"))?,
    };
    let want = std::env::var("FLUXDB_AI_DECISION_MODEL").unwrap_or_else(|_| "决策模型".to_string());
    let candidates: Vec<String> = {
        let rest: Vec<String> = std::env::args().skip(1).collect();
        if rest.is_empty() {
            vec![
                "candidate-a".into(),
                "candidate-b".into(),
                "candidate-c".into(),
            ]
        } else {
            rest
        }
    };

    // 只读模型公开视图，核对唯一模型 ID/用途/协议/启用状态；重名或缺失明确受阻。
    let matches = storage.with_ai_connection(|conn| {
        Ok(list_models_page(conn, 500, 0)?
            .into_iter()
            .filter(|model| {
                model.enabled
                    && (model.display_name == want || model.model == want || model.id == want)
            })
            .collect::<Vec<_>>())
    })?;
    if matches.len() != 1 {
        return Err(format!(
            "未找到唯一启用模型“{want}”（命中 {} 个）；重名请在环境变量里用模型 ID 精确指定",
            matches.len()
        )
        .into());
    }
    let model = &matches[0];
    if model.kind != ModelKind::Decision {
        return Err(format!(
            "“{}”用途为 {:?}，不是决策模型；R02 不用生成模型冒充决策协议",
            model.display_name, model.kind
        )
        .into());
    }
    if model.api_protocol != "system-one" {
        return Err(format!(
            "“{}”协议为 {}，不是 System One；真实协议不匹配标记受阻",
            model.display_name, model.api_protocol
        )
        .into());
    }
    println!(
        "决策模型: id={} display={} model={} protocol={} revision={}",
        model.id, model.display_name, model.model, model.api_protocol, model.revision
    );
    // endpoint 是非敏感配置（不含凭据），打印出来便于核对请求目标。
    println!(
        "endpoint={:?} full_url={}",
        model.endpoint, model.endpoint_is_full_url
    );

    // 隔离计量上下文：没有会话/Turn 时用一次性合成 ID，账本会拒绝非真实归属。
    // R02 的“用量记账”应在真实会话里核对；本冒烟只打印 usage，不写账本。
    // deadline 可调只用于验收网络时延；生产默认 800ms（设计 §18）。
    let timeout_ms: u64 = std::env::var("FLUXDB_AI_DECISION_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fluxdb_app::MODEL_REQUEST_TIMEOUT_MS);
    let provider = decision_provider_from_storage_with_timeout(
        &storage,
        &model.id,
        std::time::Duration::from_millis(timeout_ms),
    )?;
    println!("deadline_ms={timeout_ms}");
    let request = DecisionRequest {
        model: model.id.clone(),
        candidates: candidates.clone(),
        prompt: "请从候选中选择一个最合适的 ID。".to_string(),
    };
    let started = std::time::Instant::now();
    let response = ai_block_on(DecisionProvider::decide(
        &provider,
        request,
        CancelFlag::new(),
    ))??;
    let elapsed = started.elapsed();
    println!(
        "decision ok: chosen={:?} confidence={:?} usage={:?} elapsed_ms={}",
        response.chosen,
        response.confidence,
        response.usage,
        elapsed.as_millis()
    );
    match response.chosen.as_deref() {
        Some(choice) if candidates.iter().any(|c| c == choice) => {}
        Some(other) => return Err(format!("返回的 choice 不在候选集合内: {other}").into()),
        None => return Err("决策响应未返回 Choice（可能只支持 Score），请在报告里如实记录".into()),
    }
    Ok(())
}

/// 就绪 future 的最小阻塞器（与运行时同款）。
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
        "决策 Provider 未在就绪轮次内完成",
    ))
}
