// Redis 命令 Key spec 与授权范围校验测试（设计 §22.4 / §28.6.1，T025）。

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|value| (*value).to_string()).collect()
}

fn keys(parts: &[&str]) -> Vec<String> {
    match extract_redis_command_keys(&argv(parts)) {
        RedisKeyExtraction::Keys(keys) => keys,
        other => panic!("{parts:?} 应解析出 key，实际 {other:?}"),
    }
}

fn unresolvable(parts: &[&str]) -> String {
    match extract_redis_command_keys(&argv(parts)) {
        RedisKeyExtraction::Unresolvable { reason } => reason,
        other => panic!("{parts:?} 必须不可解析，实际 {other:?}"),
    }
}

#[test]
fn ai_redis_keys_single_key_commands_take_argv_1() {
    assert_eq!(keys(&["GET", "user:1"]), ["user:1"]);
    assert_eq!(keys(&["set", "user:1", "v"]), ["user:1"]);
    assert_eq!(keys(&["EXPIRE", "user:1", "60"]), ["user:1"]);
    assert_eq!(keys(&["HGET", "h", "f"]), ["h"]);
    // 大小写不敏感。
    assert_eq!(keys(&["ZADD", "z", "1", "m"]), ["z"]);
}

#[test]
fn ai_redis_keys_multi_key_commands_are_not_read_from_argv_1_only() {
    // DEL/MGET 的每个参数都是 key。
    assert_eq!(keys(&["DEL", "a", "b", "c"]), ["a", "b", "c"]);
    assert_eq!(keys(&["MGET", "a", "b"]), ["a", "b"]);
    // MSET 是 key/value 成对：value 不能被当成 key。
    assert_eq!(keys(&["MSET", "k1", "v1", "k2", "v2"]), ["k1", "k2"]);
    assert_eq!(keys(&["MSETNX", "k1", "v1"]), ["k1"]);
    // 源与目标都是 key：漏掉任一端都会让越权目标通过。
    assert_eq!(keys(&["RENAME", "src", "dst"]), ["src", "dst"]);
    assert_eq!(keys(&["SMOVE", "src", "dst", "member"]), ["src", "dst"]);
    assert_eq!(keys(&["COPY", "src", "dst"]), ["src", "dst"]);
    // BITOP 的 argv[1] 是操作名，不是 key。
    assert_eq!(keys(&["BITOP", "AND", "dest", "a", "b"]), ["dest", "a", "b"]);
}

#[test]
fn ai_redis_keys_commands_without_keys_are_explicit() {
    for parts in [
        vec!["SCAN", "0", "MATCH", "user:*"],
        vec!["INFO", "server"],
        vec!["DBSIZE"],
        // 频道不是 key，不受 prefix 范围约束。
        vec!["PUBLISH", "order:events", "payload"],
    ] {
        assert_eq!(
            extract_redis_command_keys(&argv(&parts)),
            RedisKeyExtraction::NoKeys,
            "{parts:?} 不应带 key"
        );
    }
    // MEMORY/OBJECT 的 key 由子命令决定。
    assert_eq!(keys(&["MEMORY", "USAGE", "user:1"]), ["user:1"]);
    assert_eq!(
        extract_redis_command_keys(&argv(&["MEMORY", "STATS"])),
        RedisKeyExtraction::NoKeys
    );
    assert_eq!(keys(&["OBJECT", "ENCODING", "user:1"]), ["user:1"]);
}

#[test]
fn ai_redis_keys_unresolvable_commands_fail_closed() {
    // 脚本内的 key 由运行时决定，静态不可知：必须拒绝而不是放行。
    assert_eq!(
        unresolvable(&["EVAL", "return 1", "0"]),
        "script_declared_keys"
    );
    assert_eq!(unresolvable(&["FCALL", "fn", "0"]), "script_declared_keys");
    // 跨节点与跨库搬移改变的是别的键空间。
    assert_eq!(
        unresolvable(&["MIGRATE", "127.0.0.1", "6379", "k", "0", "1"]),
        "cross_node_migration"
    );
    assert_eq!(unresolvable(&["MOVE", "k", "1"]), "cross_database_move");
    // 未知命令不能声称范围受限。
    assert_eq!(
        unresolvable(&["BF.ADD", "k", "v"]),
        "unknown_command_key_spec:bf.add"
    );
    assert_eq!(unresolvable(&[]), "empty_command");
}

#[test]
fn ai_redis_keys_scope_accepts_only_granted_prefixes() {
    let scope = RedisKeyScope::Prefixes {
        database: 0,
        prefixes: vec!["app:".into()],
    };
    assert_eq!(
        check_redis_command_scope(&argv(&["SET", "app:u", "v"]), 0, &scope).unwrap(),
        ["app:u"]
    );
    let error = check_redis_command_scope(&argv(&["SET", "secret:x", "v"]), 0, &scope).unwrap_err();
    assert_eq!(error.code, "scope_mismatch");
    // 多 key 命令只要有一个越权就整体拒绝，不能部分执行。
    let error =
        check_redis_command_scope(&argv(&["DEL", "app:a", "secret:b"]), 0, &scope).unwrap_err();
    assert_eq!(error.code, "scope_mismatch");
    // 目标键也要校验：RENAME 把授权内的 key 搬到授权外同样越权。
    let error = check_redis_command_scope(&argv(&["RENAME", "app:a", "secret:b"]), 0, &scope)
        .unwrap_err();
    assert_eq!(error.code, "scope_mismatch");
}

#[test]
fn ai_redis_keys_scope_database_must_match_target() {
    let scope = RedisKeyScope::Prefixes {
        database: 0,
        prefixes: vec!["app:".into()],
    };
    // 授权 DB 0，却对 DB 1 执行：范围不匹配。
    let error = check_redis_command_scope(&argv(&["GET", "app:a"]), 1, &scope).unwrap_err();
    assert_eq!(error.code, "scope_mismatch");
    let all = RedisKeyScope::AllKeys { database: 0 };
    assert_eq!(check_redis_command_scope(&argv(&["GET", "any:key"]), 0, &all).unwrap(), ["any:key"]);
}

#[test]
fn ai_redis_keys_empty_prefix_list_is_not_all_keys() {
    // 空授权不等于放行：§28.6.1 要求显式选择整库或限定 prefix。
    let scope = RedisKeyScope::Prefixes {
        database: 0,
        prefixes: Vec::new(),
    };
    assert!(!scope.allows("anything"));
    let error = check_redis_command_scope(&argv(&["GET", "k"]), 0, &scope).unwrap_err();
    assert_eq!(error.code, "scope_mismatch");
}

#[test]
fn ai_redis_keys_unresolvable_command_is_rejected_before_execution() {
    let all = RedisKeyScope::AllKeys { database: 0 };
    // 即使是全 Key 授权，解析不出 key 的命令仍然拒绝：范围无法核对。
    let error = check_redis_command_scope(&argv(&["EVAL", "return 1", "0"]), 0, &all).unwrap_err();
    assert_eq!(error.code, "redis_key_scope_unresolvable");
    let error =
        check_redis_command_scope(&argv(&["BF.ADD", "k", "v"]), 0, &all).unwrap_err();
    assert_eq!(error.code, "redis_key_scope_unresolvable");
}

#[test]
fn ai_redis_keys_limits_reject_oversized_operations() {
    let all = RedisKeyScope::AllKeys { database: 0 };
    let many: Vec<String> = (0..=REDIS_MAX_KEYS_PER_COMMAND)
        .map(|index| format!("k{index}"))
        .collect();
    let mut command = vec!["DEL".to_string()];
    command.extend(many);
    let error = check_redis_command_scope(&command, 0, &all).unwrap_err();
    assert_eq!(error.code, "redis_too_many_keys");

    let long_key = "k".repeat(REDIS_MAX_KEY_LEN + 1);
    let error =
        check_redis_command_scope(&argv(&["GET", &long_key]), 0, &all).unwrap_err();
    assert_eq!(error.code, "redis_key_too_long");
}

#[test]
fn ai_redis_keys_scope_filter_drops_out_of_scope_keys() {
    let scope = RedisKeyScope::Prefixes {
        database: 0,
        prefixes: vec!["app:".into()],
    };
    let filtered = filter_redis_keys_by_scope(
        &["app:a".into(), "secret:b".into(), "app:c".into()],
        &scope,
    );
    assert_eq!(filtered, ["app:a", "app:c"]);
}

#[test]
fn ai_redis_keys_prefix_option_restricts_every_tool() {
    // 连接配置里的 ai_key_prefixes 是人员可控的显式授权，不是模型可以改的。
    let mut options = std::collections::BTreeMap::new();
    options.insert("ai_key_prefixes".to_string(), "app:, cache:".to_string());
    let config = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(7),
        name: "scoped".into(),
        kind: fluxdb_core::DatabaseKind::Redis,
        endpoint: fluxdb_core::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port: 6379,
            database: Some("0".into()),
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    let resolver = MapSqlResolver::new([config]);
    let scope = resolver.redis_key_scope(fluxdb_core::ConnectionId(7), 0);
    assert_eq!(
        scope,
        RedisKeyScope::Prefixes {
            database: 0,
            prefixes: vec!["app:".into(), "cache:".into()],
        }
    );
    // 未配置的连接仍保持既有全 Key 行为。
    let mut options = std::collections::BTreeMap::new();
    options.insert("host".to_string(), "127.0.0.1".to_string());
    let config = fluxdb_core::ConnectionConfig {
        id: fluxdb_core::ConnectionId(8),
        name: "unscoped".into(),
        kind: fluxdb_core::DatabaseKind::Redis,
        endpoint: fluxdb_core::Endpoint::Tcp {
            host: "127.0.0.1".into(),
            port: 6379,
            database: Some("0".into()),
        },
        options,
        redis_profile: None,
        mysql_profile: None,
        postgres_profile: None,
    };
    let resolver = MapSqlResolver::new([config]);
    assert_eq!(
        resolver.redis_key_scope(fluxdb_core::ConnectionId(8), 0),
        RedisKeyScope::AllKeys { database: 0 }
    );
}

