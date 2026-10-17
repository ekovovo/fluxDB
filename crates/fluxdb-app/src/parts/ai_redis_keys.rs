// Redis 命令的 Key 解析与范围校验（设计 §22.4 / §28.6.1，T025）。
//
// 原则：
// - **不按 argv[1] 猜 key**：`MSET k1 v1 k2 v2`、`RENAME src dst`、`BITOP op dst src...`
//   的 key 位置各不相同，必须按命令的 key spec 解析；
// - **解析不出来就不放行**：脚本（EVAL/FCALL）、跨节点迁移（MIGRATE）、跨库搬移（MOVE）、
//   模块与未知命令无法静态确定 key，一律拒绝，不能靠审批绕过范围边界；
// - **范围是显式类型**：`RedisKeyScope` 没有「空 = 全部」的隐式含义，必须显式构造
//   `AllKeys` 或 `Prefixes`，避免把未配置的空规则当全库通行（§28.6.1）。

/// 单条命令允许触碰的 Key 数量上限；超出按拒绝处理，而不是截断后假装覆盖完整。
pub const REDIS_MAX_KEYS_PER_COMMAND: usize = 128;

/// 单个 Key 名长度上限，与 `redis.inspect` 的入参上限一致。
pub const REDIS_MAX_KEY_LEN: usize = 1024;

/// 命令 key 的解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedisKeyExtraction {
    /// 命令不涉及 key（`SCAN`、`INFO`、`DBSIZE`；Pub/Sub 的频道不是 key）。
    NoKeys,
    /// 静态解析出的全部 key（含源与目标）。
    Keys(Vec<String>),
    /// key 位置无法静态确定：必须拒绝，不能在无法核对范围的情况下执行。
    Unresolvable { reason: String },
}

/// Key 访问范围（由授权资源给定）。
///
/// 刻意不实现 `Default`：空配置不等于「全部 Key」，必须显式表达授权意图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedisKeyScope {
    /// 授权指定 DB 的全部 Key。
    AllKeys { database: u32 },
    /// 仅授权指定 DB 内匹配任一 prefix 的 Key。
    Prefixes { database: u32, prefixes: Vec<String> },
}

impl RedisKeyScope {
    pub fn database(&self) -> u32 {
        match self {
            RedisKeyScope::AllKeys { database } | RedisKeyScope::Prefixes { database, .. } => {
                *database
            }
        }
    }

    /// prefix 授权为空时不能当成「全部 Key」：没有授权就没有通行范围。
    pub fn allows(&self, key: &str) -> bool {
        match self {
            RedisKeyScope::AllKeys { .. } => true,
            RedisKeyScope::Prefixes { prefixes, .. } => {
                !prefixes.is_empty() && prefixes.iter().any(|prefix| key.starts_with(prefix))
            }
        }
    }
}

/// 按命令的 key spec 解析 argv 中的全部 key。
///
/// `argv[0]` 是命令名。返回 `Unresolvable` 的命令族一律不能进入执行路径——
/// 无法静态核对范围，就不能声称范围受限。
pub fn extract_redis_command_keys(argv: &[String]) -> RedisKeyExtraction {
    let Some(command) = argv.first().map(|name| name.to_ascii_lowercase()) else {
        return RedisKeyExtraction::Unresolvable {
            reason: "empty_command".into(),
        };
    };
    let arg = |index: usize| argv.get(index).cloned();
    let first = || RedisKeyExtraction::Keys(arg(1).into_iter().collect());
    let second = || RedisKeyExtraction::Keys(arg(2).into_iter().collect());
    let all_from = |start: usize| RedisKeyExtraction::Keys(argv.iter().skip(start).cloned().collect());
    let stepped = |start: usize, step: usize| {
        RedisKeyExtraction::Keys(argv.iter().skip(start).step_by(step).cloned().collect())
    };
    match command.as_str() {
        // ---- 不涉及 key ----
        "info" | "time" | "lolwut" | "dbsize" | "keys" | "randomkey" | "scan" => {
            RedisKeyExtraction::NoKeys
        }
        // 频道不是 key：Pub/Sub 命名空间不受 key prefix 范围约束。
        "publish" | "spublish" => RedisKeyExtraction::NoKeys,
        // 子命令决定是否带 key。
        "memory" => match arg(1).map(|value| value.to_ascii_lowercase()).as_deref() {
            Some("usage") => second(),
            _ => RedisKeyExtraction::NoKeys,
        },
        "object" => match arg(1).map(|value| value.to_ascii_lowercase()).as_deref() {
            Some("encoding") | Some("freq") | Some("idletime") | Some("refcount") => second(),
            _ => RedisKeyExtraction::NoKeys,
        },
        // ---- 固定单 key ----
        "get" | "strlen" | "type" | "ttl" | "pttl" | "exists" | "dump" | "getrange"
        | "bitcount" | "bitpos" | "set" | "setnx" | "setex" | "psetex" | "append"
        | "setrange" | "incr" | "decr" | "incrby" | "decrby" | "incrbyfloat" | "getset"
        | "getdel" | "getex" | "expire" | "pexpire" | "expireat" | "persist" | "hexists"
        | "hget" | "hgetall" | "hkeys" | "hvals" | "hlen" | "hstrlen" | "hset" | "hsetnx"
        | "hmset" | "hdel" | "hincrby" | "hincrbyfloat" | "llen" | "lrange" | "lindex"
        | "lpos" | "lpush" | "rpush" | "lset" | "ltrim" | "linsert" | "lrem" | "lpop"
        | "rpop" | "scard" | "sismember" | "smembers" | "srandmember" | "sadd" | "srem"
        | "spop" | "zcard" | "zcount" | "zrange" | "zrangebyscore" | "zscore" | "zrank"
        | "zrevrange" | "zadd" | "zrem" | "zincrby" | "zremrangebyrank" | "zremrangebyscore"
        | "xlen" | "xrange" | "xrevrange" | "xadd" | "xtrim" | "xdel" | "xack" | "xgroup"
        | "pfadd" | "setbit" | "bitfield" | "geoadd" | "geodist" | "geopos" | "geohash"
        | "restore" | "evict" | "sscan" | "hscan" | "zscan" => first(),
        // ---- 多 key：从某位置起全部是 key ----
        "mget" | "del" | "unlink" | "pfcount" | "pfmerge" => all_from(1),
        // 源与目标都是 key。
        "rename" | "renamenx" | "copy" => all_from(1),
        // SMOVE 的第三个参数是目标集合内的成员，不是 key：只取前两个。
        "smove" => {
            let keys: Vec<String> = argv.iter().skip(1).take(2).cloned().collect();
            RedisKeyExtraction::Keys(keys)
        }
        // BITOP op destkey srckey...：argv[1] 是操作名，key 从 argv[2] 起。
        "bitop" => all_from(2),
        // ---- 成对 key ----
        "mset" | "msetnx" => stepped(1, 2),
        // ---- 无法静态确定 ----
        "eval" | "evalsha" | "eval_ro" | "evalsha_ro" | "fcall" | "fcall_ro" | "function"
        | "script" => RedisKeyExtraction::Unresolvable {
            reason: "script_declared_keys".into(),
        },
        "migrate" => RedisKeyExtraction::Unresolvable {
            reason: "cross_node_migration".into(),
        },
        // 跨库搬移：即使 key 名可解析，也改变目标 DB 的键空间。
        "move" => RedisKeyExtraction::Unresolvable {
            reason: "cross_database_move".into(),
        },
        // 未知命令：无法确定 key 位置，不能声称范围受限。
        other => RedisKeyExtraction::Unresolvable {
            reason: format!("unknown_command_key_spec:{other}"),
        },
    }
}

/// 按授权范围校验一条命令将要触碰的 key；返回实际会触碰的 key 列表。
///
/// 执行前调用。`scope` 来自宿主授权资源，不接受模型自带的范围声明。
pub fn check_redis_command_scope(
    argv: &[String],
    database: u32,
    scope: &RedisKeyScope,
) -> Result<Vec<String>, SqlExecError> {
    if scope.database() != database {
        return Err(SqlExecError::new(
            "scope_mismatch",
            format!(
                "目标 DB {database} 不在授权范围（仅授权 DB {}）",
                scope.database()
            ),
        ));
    }
    let keys = match extract_redis_command_keys(argv) {
        RedisKeyExtraction::NoKeys => Vec::new(),
        RedisKeyExtraction::Keys(keys) => keys,
        RedisKeyExtraction::Unresolvable { reason } => {
            return Err(SqlExecError::new(
                "redis_key_scope_unresolvable",
                format!("无法静态确定该命令的 Key 范围，已拒绝执行（{reason}）"),
            ))
        }
    };
    if keys.len() > REDIS_MAX_KEYS_PER_COMMAND {
        return Err(SqlExecError::new(
            "redis_too_many_keys",
            format!(
                "单条命令涉及 {} 个 Key，超过上限 {REDIS_MAX_KEYS_PER_COMMAND}",
                keys.len()
            ),
        ));
    }
    for key in &keys {
        if key.len() > REDIS_MAX_KEY_LEN {
            return Err(SqlExecError::new(
                "redis_key_too_long",
                format!("Key 长度 {} 超过上限 {REDIS_MAX_KEY_LEN}", key.len()),
            ));
        }
        if !scope.allows(key) {
            return Err(SqlExecError::new(
                "scope_mismatch",
                format!("Key「{key}」不在授权的 prefix 范围内"),
            ));
        }
    }
    Ok(keys)
}

/// 按授权范围过滤 Key 列表（扫描/查看类工具的强制边界）。
///
/// `pattern` 是模型的检索条件而非授权：返回的 Key 必须同时落在授权 prefix 内。
pub fn filter_redis_keys_by_scope(keys: &[String], scope: &RedisKeyScope) -> Vec<String> {
    keys.iter()
        .filter(|key| scope.allows(key))
        .cloned()
        .collect()
}
