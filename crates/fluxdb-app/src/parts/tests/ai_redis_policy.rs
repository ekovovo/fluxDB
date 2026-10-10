// Redis 工具风险分类与契约测试（设计 §22，T025）。

fn redis_argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn ai_redis_policy_read_only_allowlist_is_read_only() {
    for argv in [
        vec!["GET", "k"],
        vec!["get", "k"],
        vec!["HGETALL", "hash"],
        vec!["TTL", "k"],
        vec!["TYPE", "k"],
        vec!["SCAN", "0", "MATCH", "user:*", "COUNT", "10"],
        vec!["MEMORY", "USAGE", "k"],
        vec!["INFO", "server"],
        vec!["XRANGE", "stream", "-", "+"],
    ] {
        let risk = classify_redis_command(&redis_argv(&argv));
        assert!(
            risk.is_read_only(),
            "只读命令必须判为 ReadOnly: {argv:?} -> {risk:?}"
        );
    }
}

#[test]
fn ai_redis_policy_mutations_require_authorization() {
    for argv in [
        vec!["SET", "k", "v"],
        vec!["DEL", "k"],
        vec!["HSET", "h", "f", "v"],
        vec!["LPUSH", "l", "v"],
        vec!["ZADD", "z", "1", "m"],
        vec!["INCR", "counter"],
        vec!["EXPIRE", "k", "60"],
    ] {
        let risk = classify_redis_command(&redis_argv(&argv));
        assert!(
            matches!(risk, RedisRisk::RequiresAuthorization { .. }),
            "写/脚本命令必须要求授权: {argv:?} -> {risk:?}"
        );
    }
}

#[test]
fn ai_redis_policy_blocks_admin_and_long_lived_commands() {
    // 管理与危险命令：直接拒绝，不给审批绕过的机会。
    for argv in [
        vec!["CONFIG", "SET", "maxmemory", "1gb"],
        vec!["EVAL", "return 1", "0"],
        vec!["FCALL", "function", "0"],
        vec!["FUNCTION", "LIST"],
        vec!["MEMORY", "PURGE"],
        vec!["KEYS", "*"],
        vec!["ACL", "LIST"],
        vec!["DEBUG", "SLEEP", "1"],
        vec!["MONITOR"],
        vec!["SHUTDOWN"],
        vec!["FLUSHALL"],
        vec!["CLUSTER", "INFO"],
        vec!["CLIENT", "KILL", "ID", "1"],
        vec!["MODULE", "LIST"],
        vec!["AUTH", "pw"],
        vec!["SELECT", "1"],
        vec!["SUBSCRIBE", "ch"],
        vec!["BLPOP", "l", "1"],
    ] {
        let risk = classify_redis_command(&redis_argv(&argv));
        assert!(
            matches!(risk, RedisRisk::Rejected { .. }),
            "管理/长连接命令必须被拒绝: {argv:?} -> {risk:?}"
        );
    }
    assert!(matches!(
        classify_redis_command(&[]),
        RedisRisk::Rejected { .. }
    ));
}

#[test]
fn ai_redis_policy_unknown_commands_fail_closed_to_authorization() {
    // 未识别的命令绝不能当只读放过。
    for argv in [vec!["BF.ADD", "k", "v"], vec!["JSON.SET", "k", ".", "1"]] {
        let risk = classify_redis_command(&redis_argv(&argv));
        assert!(
            matches!(risk, RedisRisk::RequiresAuthorization { .. }),
            "未知命令必须要求授权: {argv:?} -> {risk:?}"
        );
    }
}

#[test]
fn ai_redis_policy_tool_specs_have_expected_risk_and_valid_schemas() {
    let specs = standard_redis_tool_specs().expect("Redis 工具定义");
    let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "redis.scan",
            "redis.inspect",
            "redis.execute",
            "redis.server_info"
        ]
    );
    for spec in &specs {
        assert!(
            spec.input_schema.is_object(),
            "{} 的 schema 必须是对象",
            spec.id
        );
        assert_eq!(
            spec.input_schema.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false)),
            "{} 必须禁止额外字段",
            spec.id
        );
        let expected = if spec.id == "redis.execute" {
            fluxdb_core::ToolRisk::Write
        } else {
            fluxdb_core::ToolRisk::Read
        };
        assert_eq!(spec.risk, expected, "{} 的风险等级", spec.id);
        // argv 必须是数组（不接受拼接后的命令文本）。
        if spec.id == "redis.execute" {
            let argv = spec
                .input_schema
                .get("properties")
                .and_then(|properties| properties.get("argv"))
                .expect("argv 定义");
            assert_eq!(argv.get("type"), Some(&serde_json::json!("array")));
        }
    }
    // schema 必须能被 ToolCatalog 的校验器接受（构造目录不报错）。
    let catalog = ToolCatalog::new(specs).with_argument_limit(64 * 1024);
    assert!(catalog
        .prepare(
            "redis.execute",
            r#"{"target":"cache-1","argv":["SET","k","v"],"intent":"写一个键"}"#
        )
        .is_ok());
    // 未知工具/缺 argv 必须被拒绝。
    assert!(catalog
        .prepare("redis.kill", r#"{"target":"cache-1"}"#)
        .is_err());
    assert!(catalog
        .prepare("redis.execute", r#"{"target":"cache-1"}"#)
        .is_err());
}
