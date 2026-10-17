// 写入风险评分的确定性部分：形状分类、连接器能力与合成规则。

fn score(sql: &str, dialect: SqlDialectKind, bounded: bool) -> f64 {
    sql_write_rule_score(sql, dialect, bounded)
}

fn codes(sql: &str, dialect: SqlDialectKind, bounded: bool) -> Vec<String> {
    sql_write_signal_codes(sql, dialect, bounded)
}

/// 如果这张卡上的 MySQL INSERT 也满足形状与能力，就应当自动放行；
/// 库不支持时按普通执行路径评分，必须高于阈值（需要评审模型背书）。
#[test]
fn single_row_literal_insert_needs_model_backing_on_any_connector() {
    let sql = "INSERT INTO countries (id,name) VALUES ('ZZ-test','Testland')";
    // INSERT 不在有界提交覆盖范围内（那个能力只做 UPDATE），因此即使连接声明了能力，
    // 规则分也高于阈值：需要评审模型背书才自动放行。
    for bounded in [false, true] {
        assert!(
            score(sql, SqlDialectKind::MySql, bounded) > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
            "bounded={bounded}"
        );
    }
    assert!(codes(sql, SqlDialectKind::MySql, true).contains(&"single_row_insert".to_string()));
    let facts = sql_write_risk_facts(sql, SqlDialectKind::MySql, true);
    assert!(!facts.bounded_capable, "INSERT 不能走有界提交路径");
    // 接入"当前对话模型"背书后落到阈值下 —— 这就是把权限交给评审模型的档位。
    let decision = fluxdb_core::combine_write_risk(
        &facts,
        Some(&fluxdb_core::ModelRisk::new(0.10, "mock")),
        fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
        0.5,
    );
    assert!(decision.allows_automatic(), "{}", decision.summary());
}

#[test]
fn insert_from_query_is_a_hard_signal() {
    let facts = sql_write_risk_facts(
        "INSERT INTO t (a) SELECT a FROM other",
        SqlDialectKind::Postgres,
        true,
    );
    assert!(facts.hard_floor() >= 0.5, "查询来源应属硬信号");
    assert!(facts.codes().contains(&"insert_from_query"));
}

#[test]
fn multi_row_values_outranks_single_row() {
    let one = score("INSERT INTO t (a) VALUES (1)", SqlDialectKind::MySql, true);
    let many = score("INSERT INTO t (a) VALUES (1),(2),(3)", SqlDialectKind::MySql, true);
    assert!(many > one, "{many} 应高于 {one}");
}

#[test]
fn whole_table_changes_are_hard_but_unbounded_predicates_are_soft() {
    // 语句本身给不出边界：评审模型无权下穿。
    for sql in [
        "UPDATE t SET a=1",
        "UPDATE t SET a=a+1 WHERE id=1",
        "DELETE FROM t",
    ] {
        let facts = sql_write_risk_facts(sql, SqlDialectKind::MySql, true);
        assert!(
            facts.hard_floor() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
            "{sql} 应落在硬信号上：{:?}",
            facts.codes()
        );
    }
    // 谓词定位不到确定行数属于"不确定度"：规则分高于阈值，
    // 但按用户要求，评审模型可以背书把它降到阈值以下（把权限交给 Jev）。
    let facts = sql_write_risk_facts("UPDATE t SET a=1 WHERE id>0", SqlDialectKind::MySql, true);
    assert!(
        facts.rule_score() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
        "没有评审时批量更新必须转人工"
    );
    assert_eq!(facts.hard_floor(), 0.0, "非唯一谓词不是硬信号");
    let decision = fluxdb_core::combine_write_risk(
        &facts,
        Some(&fluxdb_core::ModelRisk::new(0.05, "mock")),
        fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
        0.5,
    );
    assert!(decision.allows_automatic(), "{}", decision.summary());
}

#[test]
fn unique_key_equality_update_is_low_risk_only_when_bounded() {
    let sql = "UPDATE items SET value='x' WHERE id=1";
    // 形状确定 + 连接器能在事务内复核真实影响：机制上已证明，无模型也可放行。
    assert!(score(sql, SqlDialectKind::Sqlite, true) <= fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD);
    // 形状相同但连接器只能普通执行：规则分高于阈值，需要模型背书。
    assert!(score(sql, SqlDialectKind::MySql, false) > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD);
}

#[test]
fn delete_stays_uncertain_because_bounded_commit_covers_update_only() {
    let sql = "DELETE FROM items WHERE id=1";
    for bounded in [false, true] {
        let facts = sql_write_risk_facts(sql, SqlDialectKind::Sqlite, bounded);
        assert!(
            facts.rule_score() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
            "删除不可逆，bounded={bounded}"
        );
        assert!(facts.codes().contains(&"delete_unverified_key"));
        assert!(!facts.bounded_capable, "有界提交只覆盖 UPDATE，不能用于删除");
    }
}

#[test]
fn structural_changes_are_unrecoverable() {
    for sql in ["ALTER TABLE t ADD COLUMN c INT", "DROP TABLE t", "GRANT ALL ON t TO u"] {
        let facts = sql_write_risk_facts(sql, SqlDialectKind::Postgres, true);
        assert!(facts.hard_floor() >= 0.9, "{sql}");
    }
}

#[test]
fn unparseable_or_multi_statement_is_unproven() {
    for sql in ["", "SELECT 1; SELECT 2", "NOT SQL AT ALL"] {
        let facts = sql_write_risk_facts(sql, SqlDialectKind::MySql, true);
        assert!(facts.hard_floor() >= 1.0, "{sql:?}");
    }
}

#[test]
fn evidence_json_never_carries_business_values_or_object_names() {
    let facts = sql_write_risk_facts(
        "UPDATE secret_users SET api_token='abc' WHERE email='a@b.c'",
        SqlDialectKind::Postgres,
        true,
    );
    for needle in ["secret_users", "api_token", "abc", "a@b.c", "UPDATE"] {
        assert!(
            !facts.evidence_json.contains(needle),
            "结构事实泄漏了 {needle}：{}",
            facts.evidence_json
        );
    }
}

#[test]
fn combine_never_lets_the_model_cross_the_hard_floor() {
    let facts = sql_write_risk_facts("UPDATE t SET a=1", SqlDialectKind::MySql, true);
    let model = fluxdb_core::ModelRisk::new(0.0, "mock");
    let decision = fluxdb_core::combine_write_risk(&facts, Some(&model), fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, 0.5);
    assert!(decision.final_score >= decision.hard_floor);
    assert!(!decision.allows_automatic());
}

#[test]
fn combine_can_lower_soft_uncertainty_with_model_backing() {
    let mut facts = fluxdb_core::WriteRiskFacts::new("{}");
    facts.soft("determined_shape", 0.40, "形状确定但未核实行数");
    let model = fluxdb_core::ModelRisk::new(0.10, "mock");
    let decision = fluxdb_core::combine_write_risk(&facts, Some(&model), fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, 0.5);
    // 0.5*0.40 + 0.5*0.10 = 0.25
    assert!((decision.final_score - 0.25).abs() < 1e-9, "{}", decision.summary());
    assert!(decision.allows_automatic());
    // 没有模型时退回规则分：0.40 高于阈值。
    let rule_only = fluxdb_core::combine_write_risk(&facts, None, fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, 0.5);
    assert!(!rule_only.allows_automatic());
}

#[test]
fn weight_is_capped_so_the_model_cannot_override_the_rules() {
    let mut facts = fluxdb_core::WriteRiskFacts::new("{}");
    facts.soft("determined_shape", 1.0, "最保守估计");
    let model = fluxdb_core::ModelRisk::new(0.0, "mock");
    // 权重即使传入 1.0 也被限制在 0.5：规则分必须保留一半话语权。
    let decision = fluxdb_core::combine_write_risk(&facts, Some(&model), fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, 1.0);
    assert!((decision.final_score - 0.5).abs() < 1e-9, "{}", decision.summary());
}

#[test]
fn summary_lists_signals_and_threshold() {
    let mut facts = fluxdb_core::WriteRiskFacts::new("{}");
    facts.soft("determined_shape", 0.40, "形状确定");
    facts.hard("no_predicate", 0.60, "无谓词");
    let decision = fluxdb_core::combine_write_risk(&facts, None, 0.35, 0.5);
    let summary = decision.summary();
    assert!(summary.contains("determined_shape=0.40"), "{summary}");
    assert!(summary.contains("!no_predicate=0.60"), "{summary}");
    assert!(summary.contains("阈值 0.35"), "{summary}");
}

/// Redis 侧的确定性评分：命令族决定不可逆性，Key 范围受限只降低"影响面不可知"。
#[test]
fn redis_single_key_overwrite_is_low_risk_only_within_scope() {
    let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    let set = argv(&["SET", "session:1", "value"]);
    let bounded = redis_write_rule_score(&set, true);
    assert!(
        bounded <= fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
        "{bounded}"
    );
    let unbounded = redis_write_rule_score(&set, false);
    assert!(unbounded > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD, "{unbounded}");
}

#[test]
fn redis_destructive_and_multi_key_commands_need_human() {
    let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    for command in [
        vec!["DEL", "session:1"],
        vec!["UNLINK", "session:1"],
        vec!["FLUSHDB"],
        vec!["MSET", "a", "1", "b", "2"],
        vec!["RENAME", "a", "b"],
    ] {
        let facts = redis_write_risk_facts(&argv(&command), true);
        assert!(
            facts.rule_score() > fluxdb_core::DEFAULT_WRITE_RISK_THRESHOLD,
            "{command:?} 应高于阈值：{:?}",
            facts.codes()
        );
        assert!(
            facts.hard_floor() > 0.0 || facts.rule_score() > 0.5,
            "{command:?} 不能是低风险形状"
        );
    }
}

#[test]
fn redis_evidence_json_hides_key_names_and_values() {
    let argv = vec![
        "SET".to_string(),
        "prod:session:42".to_string(),
        "secret-token".to_string(),
    ];
    let facts = redis_write_risk_facts(&argv, true);
    for needle in ["prod:session:42", "secret-token", "SET"] {
        assert!(
            !facts.evidence_json.contains(needle),
            "Redis 结构事实泄漏了 {needle}：{}",
            facts.evidence_json
        );
    }
    assert!(facts.evidence_json.contains("\"command\":\"set\""));
}

#[test]
fn redis_no_ttl_overwrite_is_flagged() {
    let with_ttl = vec!["SET".to_string(), "k".into(), "v".into(), "EX".into(), "60".into()];
    let without = vec!["SET".to_string(), "k".into(), "v".into()];
    assert!(
        redis_write_rule_score(&with_ttl, true) < redis_write_rule_score(&without, true),
        "带 TTL 的覆盖写应低于不带的"
    );
}
