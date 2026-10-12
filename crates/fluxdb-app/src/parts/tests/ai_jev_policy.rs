// T037 决策建议采用门槛与熔断的纯逻辑单测（设计 §18/§18.1）。
// 不触存储与网络：只覆盖"什么情况下采用建议、什么情况下放弃并熔断"。

fn jev_ids() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

fn jev_response(chosen: Option<&str>, scores: Vec<(&str, f64)>) -> fluxdb_core::DecisionResponse {
    fluxdb_core::DecisionResponse {
        chosen: chosen.map(str::to_string),
        scores: scores
            .into_iter()
            .map(|(id, probability)| (id.to_string(), probability))
            .collect(),
        reason: None,
        confidence: Some(0.04),
        usage: None,
    }
}

#[test]
fn jev_adopts_clear_choice_even_with_low_top_level_confidence() {
    // 真实验收里顶层 confidence 常在 0.02–0.05，不能拿它当门槛。
    let response = jev_response(Some("a"), vec![("a", 0.62), ("b", 0.38)]);
    assert_eq!(crate::advice_is_usable(&response, &jev_ids()), Ok(()));
}

#[test]
fn jev_rejects_flat_and_low_probability_choices() {
    let flat = jev_response(Some("a"), vec![("a", 0.5), ("b", 0.5)]);
    assert_eq!(
        crate::advice_is_usable(&flat, &jev_ids()),
        Err("候选概率无明显优势")
    );
    let low = jev_response(Some("a"), vec![("a", 0.2), ("b", 0.1)]);
    assert_eq!(
        crate::advice_is_usable(&low, &jev_ids()),
        Err("决策置信度不足")
    );
}

#[test]
fn jev_rejects_missing_or_out_of_range_choice() {
    let none = jev_response(None, vec![("a", 0.9)]);
    assert_eq!(
        crate::advice_is_usable(&none, &jev_ids()),
        Err("决策未给出稳定选择")
    );
    let outside = jev_response(Some("x"), vec![("x", 0.9)]);
    assert_eq!(
        crate::advice_is_usable(&outside, &jev_ids()),
        Err("决策选择不在候选集合内")
    );
    let no_probability = jev_response(Some("a"), vec![]);
    assert_eq!(
        crate::advice_is_usable(&no_probability, &jev_ids()),
        Err("决策缺少被选候选的概率")
    );
}

#[test]
fn jev_circuit_opens_after_three_failures_and_expires() {
    let id = format!("jev-circuit-{}", uuid::Uuid::new_v4());
    assert!(!crate::record_circuit_failure(&id, "超时", 1_000));
    assert!(!crate::record_circuit_failure(&id, "超时", 1_100));
    assert!(
        crate::record_circuit_failure(&id, "超时", 1_200),
        "连续 3 次失败应熔断"
    );
    let opened = crate::circuit_snapshot(&id, 1_300);
    assert!(opened.open_until_ms > 1_300);
    assert_eq!(
        opened.open_until_ms - 1_200,
        crate::CIRCUIT_COOLDOWN_MS,
        "熔断窗口应为 60s"
    );
    crate::record_circuit_success(&id);
    assert_eq!(crate::circuit_snapshot(&id, 1_400).open_until_ms, 0);

    // 成功后计数清零：下一次失败必须重新累计满阈值才再次熔断。
    assert!(!crate::record_circuit_failure(&id, "超时", 2_000));
    assert!(!crate::record_circuit_failure(&id, "超时", 2_100));
    assert!(crate::record_circuit_failure(&id, "超时", 2_200));
    // 熔断窗口自然过期后清零：允许重新尝试，而不是永久熔断。
    let expired = crate::circuit_snapshot(&id, 2_200 + crate::CIRCUIT_COOLDOWN_MS + 1);
    assert_eq!(expired.open_until_ms, 0);
    assert_eq!(expired.consecutive_failures, 0);
}

#[test]
fn jev_status_labels_cover_all_design_states() {
    use crate::JevStatus;
    use crate::JevStatusKind as K;
    assert_eq!(
        JevStatus {
            kind: K::NotConfigured,
            ..JevStatus::default()
        }
        .label(),
        "未配置"
    );
    assert_eq!(
        JevStatus {
            kind: K::Disabled,
            ..JevStatus::default()
        }
        .label(),
        "已禁用"
    );
    assert_eq!(
        JevStatus {
            kind: K::Available,
            model_profile_id: Some("m".into()),
            ..JevStatus::default()
        }
        .label(),
        "可用"
    );
    assert_eq!(
        JevStatus {
            kind: K::NeedsChoice,
            ..JevStatus::default()
        }
        .label(),
        "待选择模型"
    );
    assert_eq!(
        JevStatus {
            kind: K::Unavailable,
            ..JevStatus::default()
        }
        .label(),
        "暂不可用"
    );
}

#[test]
fn jev_relevance_prompt_carries_only_public_labels() {
    let prompt = crate::relevance_prompt(&[crate::RelevanceCandidate {
        id: "object:1".into(),
        label: "orders".into(),
        kind: "table".into(),
    }]);
    assert!(prompt.contains("orders"));
    assert!(!prompt.contains("api_key"));
    assert!(!prompt.contains("sk-"));
}
