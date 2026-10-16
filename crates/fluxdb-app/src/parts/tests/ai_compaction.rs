// 上下文压缩纯逻辑测试（设计 §16.1 / T069）。
// 只测触发判定、合法切点、摘要校验与来源 hash；模型调用与 checkpoint 落库由集成测试覆盖。

use crate::{
    CompactionCut, CompactionReason, CompactionTrigger, build_summary_prompt, compute_cut,
    cut_candidates, estimate_messages_tokens, estimate_tokens, exceeds_input_budget,
    resolve_output_token_limit, should_compact, source_hash, validate_summary,
};

#[test]
fn configured_output_budget_is_used_instead_of_a_fixed_ceiling() {
    // 曾经这里被封顶到 1024：推理模型思考用光额度后正文为空。
    assert_eq!(
        resolve_output_token_limit(Some(1_000_000), Some(16_000)),
        16_000
    );
    assert_eq!(resolve_output_token_limit(Some(32_768), Some(4_096)), 4_096);
}

#[test]
fn missing_output_budget_is_derived_from_window_and_never_starves_input() {
    // 窗口未知：给固定默认值，不能退化成 0 或极小值。
    assert_eq!(resolve_output_token_limit(None, None), 8_192);
    assert_eq!(resolve_output_token_limit(Some(0), None), 8_192);
    // 大窗口按 1/4 推导但不超过默认值；小窗口不超出窗口本身。
    assert_eq!(resolve_output_token_limit(Some(1_000_000), None), 8_192);
    assert_eq!(resolve_output_token_limit(Some(8_192), None), 2_048);
    // 输出必须给输入留位置：显式配置顶满窗口也要收敛。
    assert_eq!(resolve_output_token_limit(Some(4_096), Some(4_096)), 3_072);
    assert_eq!(resolve_output_token_limit(Some(1_024), Some(4_096)), 1);
    // 非法的 0 / 负数按未配置处理。
    assert_eq!(resolve_output_token_limit(Some(1_000_000), Some(0)), 8_192);
    assert_eq!(resolve_output_token_limit(Some(1_000_000), Some(-5)), 8_192);
}

fn trigger(percent: u8, window: i64, output: i64) -> CompactionTrigger {
    CompactionTrigger {
        context_window_tokens: Some(window),
        max_output_tokens: Some(output),
        trigger_percent: percent,
    }
}

#[test]
fn percent_threshold_triggers_before_output_reserve_for_large_windows() {
    // 200k 窗口，80% = 160k；输出 8k，余量 10k ⇒ 可用 182k。应先命中百分比。
    assert_eq!(should_compact(159_999, trigger(80, 200_000, 8_000)), None);
    assert_eq!(
        should_compact(160_000, trigger(80, 200_000, 8_000)),
        Some(CompactionReason::Percent)
    );
}

#[test]
fn output_reserve_triggers_for_small_windows() {
    // 32k 窗口，80% = 25.6k；输出 16k，余量 1.6k ⇒ 可用 ≈ 14.4k。输出预留先命中。
    assert_eq!(
        should_compact(15_000, trigger(80, 32_768, 16_384)),
        Some(CompactionReason::OutputReserve)
    );
}

#[test]
fn unknown_window_never_triggers_and_is_optional() {
    assert_eq!(
        should_compact(999_999, CompactionTrigger {
            context_window_tokens: None,
            max_output_tokens: Some(1024),
            trigger_percent: 80,
        }),
        None
    );
}

#[test]
fn window_smaller_than_output_always_triggers() {
    assert_eq!(
        should_compact(1, trigger(80, 1_000, 5_000)),
        Some(CompactionReason::OutputReserve)
    );
}

#[test]
fn estimate_tokens_counts_cjk_as_one_and_ascii_as_quarter() {
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
    assert_eq!(estimate_tokens("你好"), 2);
    assert_eq!(estimate_tokens("你好abcd"), 3);
}

#[test]
fn estimate_messages_tokens_adds_structure_overhead() {
    let messages = vec![
        ModelMessage::new(ModelRole::User, "abcd"),
        ModelMessage::new(ModelRole::Assistant, "你好"),
    ];
    // 1 + 4 与 2 + 4
    assert_eq!(estimate_messages_tokens(&messages), 11);
}

#[test]
fn cut_keeps_recent_messages_and_system_boundary() {
    let messages: Vec<ModelMessage> = (0..10)
        .map(|i| ModelMessage::new(ModelRole::User, format!("m{i}")))
        .collect();
    let cut: CompactionCut = compute_cut(&messages, 4, |index| index as i64 + 1).unwrap();
    assert_eq!(cut.cut, 6);
    assert_eq!(cut.last_message_seq, 6);
}

#[test]
fn cut_never_splits_tool_call_from_its_results() {
    let messages = vec![
        ModelMessage::new(ModelRole::User, "q"),
        ModelMessage::new(ModelRole::Assistant, "a"),
        ModelMessage::new(ModelRole::User, "q2"),
        ModelMessage::new(ModelRole::Assistant, "a2"),
        ModelMessage::new(ModelRole::User, "q3"),
        ModelMessage::assistant_with_tool_calls("call", Vec::new()),
        ModelMessage::tool_result("call", "read", "{}"),
        ModelMessage::new(ModelRole::Assistant, "done"),
    ];
    let cut = compute_cut(&messages, 2, |index| index as i64 + 1).unwrap();
    // 切点必须落在成对边界之前，不能把 tool_result 与被压掉/保留的调用拆开。
    assert!(cut.cut < 6, "切点不应落在工具调用与其回执之间：{:?}", cut);
    // 切点之后不能出现"孤立的 tool_result"。
    if let Some((_, first_kept)) = messages.iter().enumerate().nth(cut.cut) {
        assert_ne!(first_kept.role, ModelRole::Tool);
    }
}

#[test]
fn cut_returns_none_when_nothing_safe_to_compact() {
    let messages = vec![
        ModelMessage::new(ModelRole::User, "a"),
        ModelMessage::new(ModelRole::Assistant, "b"),
    ];
    assert_eq!(compute_cut(&messages, 4, |index| index as i64 + 1), None);
}

#[test]
fn validate_summary_requires_protected_items() {
    assert!(validate_summary(r#"{"goal":"x","constraints":"none","verified_facts":"f","decisions":"d","next_steps":"n"}"#).is_ok());
    assert!(validate_summary("goal: x").is_err());
    assert!(validate_summary("").is_err());
    assert!(validate_summary("goal constraints verified_facts decisions next_steps [truncated]").is_err());
}

#[test]
fn build_summary_prompt_includes_previous_summary_incrementally() {
    let prompt = build_summary_prompt(Some("旧摘要"), "新增历史");
    assert!(prompt.contains("旧摘要"));
    assert!(prompt.contains("新增历史"));
    let prompt_without = build_summary_prompt(None, "只有新增");
    assert!(!prompt_without.contains("已有摘要"));
    assert!(prompt_without.contains("只有新增"));
}

#[test]
fn source_hash_is_stable_and_sensitive_to_input() {
    assert_eq!(source_hash(&["a", "b"]), source_hash(&["a", "b"]));
    assert_ne!(source_hash(&["a", "b"]), source_hash(&["b", "a"]));
}

#[test]
fn exceeds_input_budget_only_when_window_truly_cannot_hold_input() {
    // 200k 窗口、输出 8k、余量 10k ⇒ 可用 182k：只有真正超过可用预算才算装不下。
    assert!(!exceeds_input_budget(160_000, trigger(80, 200_000, 8_000)));
    assert!(!exceeds_input_budget(182_000, trigger(80, 200_000, 8_000)));
    assert!(exceeds_input_budget(182_001, trigger(80, 200_000, 8_000)));
}

#[test]
fn exceeds_input_budget_is_false_when_window_unknown() {
    // 窗口未知时既不能判定压缩，也不能据此暂停。
    assert!(!exceeds_input_budget(999_999, CompactionTrigger {
        context_window_tokens: None,
        max_output_tokens: Some(1024),
        trigger_percent: 80,
    }));
}

#[test]
fn exceeds_input_budget_treats_window_smaller_than_output_as_over_budget() {
    assert!(exceeds_input_budget(1, trigger(80, 1_000, 5_000)));
}

#[test]
fn cut_candidates_always_include_the_deterministic_cut() {
    let messages: Vec<ModelMessage> = (0..12)
        .map(|i| ModelMessage::new(ModelRole::User, format!("m{i}")))
        .collect();
    let candidates = cut_candidates(&messages, 4, |index| index as i64 + 1).expect("有可压历史");
    let deterministic = compute_cut(&messages, 4, |index| index as i64 + 1).unwrap();
    assert_eq!(candidates.deterministic, deterministic);
    assert!(candidates.all.contains(&deterministic));
    assert!(candidates.all.len() >= 2, "应有可决策的多个合法切点");
}

#[test]
fn cut_candidates_keep_tool_pairs_intact_for_every_candidate() {
    let messages = vec![
        ModelMessage::new(ModelRole::User, "q"),
        ModelMessage::assistant_with_tool_calls("call-1", Vec::new()),
        ModelMessage::tool_result("call-1", "read", "{}"),
        ModelMessage::new(ModelRole::Assistant, "a"),
        ModelMessage::new(ModelRole::User, "q2"),
        ModelMessage::assistant_with_tool_calls("call-2", Vec::new()),
        ModelMessage::tool_result("call-2", "read", "{}"),
        ModelMessage::new(ModelRole::Assistant, "done"),
    ];
    let candidates = cut_candidates(&messages, 2, |index| index as i64 + 1).expect("有可压历史");
    // 每个候选都必须是合法切点：保留段的第一条不能是孤立 Tool 回执。
    for candidate in &candidates.all {
        assert_ne!(
            messages[candidate.cut].role,
            ModelRole::Tool,
            "候选切点不得落在工具调用与回执之间：{candidate:?}"
        );
        assert_eq!(candidate.last_message_seq, candidate.cut as i64);
    }
}

#[test]
fn cut_candidates_are_none_when_nothing_is_safe_to_compact() {
    let messages = vec![
        ModelMessage::new(ModelRole::User, "a"),
        ModelMessage::new(ModelRole::Assistant, "b"),
    ];
    assert!(cut_candidates(&messages, 4, |index| index as i64 + 1).is_none());
}
