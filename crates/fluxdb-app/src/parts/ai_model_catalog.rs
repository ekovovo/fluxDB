// 模型容量目录：Provider 的 `/models` 只负责列出可用模型 ID，
// 上下文窗口与最大输出来自 models.dev 公共目录（pi/goose 也使用同类目录）。
//
// 只在 Provider 响应缺少容量时读取公开目录；不发送 API Key，不猜测未知模型。
// 目录获取失败不阻断模型列表，容量留空供用户手工填写。

type ModelCapacityCatalog =
    std::collections::HashMap<(String, String), (i64, i64)>;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MODELS_DEV_MAX_BYTES: u64 = 16 * 1024 * 1024;

static MODELS_DEV_CATALOG: std::sync::OnceLock<std::sync::Arc<ModelCapacityCatalog>> =
    std::sync::OnceLock::new();

/// 把已知官方端点映射到 models.dev 的 Provider 键；未知端点不做猜测。
fn models_dev_provider(endpoint: &str) -> Option<&'static str> {
    let url = url::Url::parse(endpoint.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path();
    Some(match host.as_str() {
        "api.openai.com" => "openai",
        "api.anthropic.com" => "anthropic",
        "openrouter.ai" => "openrouter",
        "api.deepseek.com" => "deepseek",
        "api.mistral.ai" => "mistral",
        "api.groq.com" => "groq",
        "generativelanguage.googleapis.com" => "google",
        "api.x.ai" => "xai",
        "api.z.ai" => "zai",
        "open.bigmodel.cn" if path.contains("/coding/") => "zhipuai-coding-plan",
        "open.bigmodel.cn" => "zhipuai",
        _ => return None,
    })
}

fn parse_capacity_catalog(body: &serde_json::Value) -> ModelCapacityCatalog {
    let mut catalog = ModelCapacityCatalog::new();
    let Some(providers) = body.as_object() else {
        return catalog;
    };
    for (provider, provider_data) in providers {
        let Some(models) = provider_data.get("models").and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (model_id, model) in models {
            let Some(limit) = model.get("limit") else {
                continue;
            };
            let context = limit.get("context").and_then(serde_json::Value::as_i64);
            let output = limit.get("output").and_then(serde_json::Value::as_i64);
            if let (Some(context), Some(output)) = (context, output)
                && context > 0
                && output > 0
                && output <= context
            {
                catalog.insert(
                    (provider.to_ascii_lowercase(), model_id.to_ascii_lowercase()),
                    (context, output),
                );
            }
        }
    }
    catalog
}

fn fetch_models_dev_catalog() -> fluxdb_core::Result<std::sync::Arc<ModelCapacityCatalog>> {
    if let Some(catalog) = MODELS_DEV_CATALOG.get() {
        return Ok(catalog.clone());
    }
    let agent = ai_agent(std::time::Duration::from_millis(
        crate::MODEL_REQUEST_TIMEOUT_MS,
    ));
    let mut response = agent
        .get(MODELS_DEV_URL)
        .call()
        .map_err(|error| {
            fluxdb_core::Error::new(
                fluxdb_core::ErrorKind::Connection,
                ai_net_error_reason(&error),
            )
        })?;
    if !(200..300).contains(&response.status().as_u16()) {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            format!("模型目录请求失败：HTTP {}", response.status()),
        ));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MODELS_DEV_MAX_BYTES)
        .read_to_string()
        .map_err(|_| {
            fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "读取模型目录失败")
        })?;
    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal, "模型目录不是合法 JSON")
    })?;
    let catalog = std::sync::Arc::new(parse_capacity_catalog(&parsed));
    if catalog.is_empty() {
        return Err(fluxdb_core::Error::new(
            fluxdb_core::ErrorKind::Internal,
            "模型目录没有可用容量",
        ));
    }
    let _ = MODELS_DEV_CATALOG.set(catalog.clone());
    Ok(catalog)
}

fn apply_catalog_capacities(
    provider: &str,
    catalog: &ModelCapacityCatalog,
    models: &mut [AiDiscoveredModel],
) {
    let provider = provider.to_ascii_lowercase();
    for model in models {
        if model.context_window_tokens.is_some() && model.max_output_tokens.is_some() {
            continue;
        }
        let key = (provider.clone(), model.id.to_ascii_lowercase());
        let Some((context, output)) = catalog.get(&key) else {
            continue;
        };
        model.context_window_tokens.get_or_insert(*context);
        model.max_output_tokens.get_or_insert(*output);
    }
}

/// 用公开模型目录补齐 Provider 响应中缺失的容量。
///
/// 已知端点且确有缺失时才访问目录；目录失败只记录调试日志，
/// 仍返回 Provider 的模型 ID，容量留空，不伪造默认值。
pub fn enrich_model_capacities(endpoint: &str, models: &mut [AiDiscoveredModel]) {
    let Some(provider) = models_dev_provider(endpoint) else {
        return;
    };
    if models.iter().all(|model| {
        model.context_window_tokens.is_some() && model.max_output_tokens.is_some()
    }) {
        return;
    }
    match fetch_models_dev_catalog() {
        Ok(catalog) => apply_catalog_capacities(provider, &catalog, models),
        Err(error) => tracing::debug!(error=%error.message, "模型容量目录不可用"),
    }
}

#[cfg(test)]
mod ai_model_catalog_tests {
    use super::*;

    #[test]
    fn known_provider_endpoints_map_without_guessing_custom_hosts() {
        assert_eq!(
            models_dev_provider("https://api.openai.com/v1"),
            Some("openai"),
        );
        assert_eq!(
            models_dev_provider("https://api.anthropic.com"),
            Some("anthropic"),
        );
        assert_eq!(
            models_dev_provider("https://open.bigmodel.cn/api/coding/paas/v4"),
            Some("zhipuai-coding-plan"),
        );
        assert_eq!(models_dev_provider("https://proxy.example.com/v1"), None);
    }

    #[test]
    fn catalog_only_fills_missing_capacities() {
        let body = serde_json::json!({
            "openai": {"models": {
                "gpt-test": {"limit": {"context": 1000, "output": 200}},
            }},
            "anthropic": {"models": {
                "claude-test": {"limit": {"context": 2000, "output": 300}},
            }},
        });
        let catalog = parse_capacity_catalog(&body);
        let mut models = [
            AiDiscoveredModel {
                id: "gpt-test".into(),
                context_window_tokens: Some(900),
                max_output_tokens: None,
            },
            AiDiscoveredModel {
                id: "claude-test".into(),
                context_window_tokens: None,
                max_output_tokens: None,
            },
        ];
        apply_catalog_capacities("openai", &catalog, &mut models[..1]);
        apply_catalog_capacities("anthropic", &catalog, &mut models[1..]);

        assert_eq!(models[0].context_window_tokens, Some(900));
        assert_eq!(models[0].max_output_tokens, Some(200));
        assert_eq!(models[1].context_window_tokens, Some(2000));
        assert_eq!(models[1].max_output_tokens, Some(300));
    }

    #[test]
    fn invalid_limits_are_ignored() {
        let body = serde_json::json!({
            "openai": {"models": {
                "zero": {"limit": {"context": 0, "output": 10}},
                "too-large": {"limit": {"context": 100, "output": 200}},
                "missing": {"limit": {"context": 100}},
            }},
        });
        assert!(parse_capacity_catalog(&body).is_empty());
    }
}
