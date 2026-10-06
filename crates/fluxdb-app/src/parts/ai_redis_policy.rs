// Redis 工具的风险分类与输入契约（设计 §22，T025）。
//
// 原则（与 SQL 侧一致）：
// - **不按 argv[1] 拍脑袋判断安全**：读白名单之外的命令一律按"需要授权"处理；
// - 管理/危险命令直接拒绝（CONFIG/ACL/DEBUG/MONITOR/SHUTDOWN/FLUSH*…），不给审批机会绕过；
// - 长连接类命令（SUBSCRIBE/PSUBSCRIBE/MONITOR）拒绝：Agent 调用不能挂成长订阅；
// - EVAL/EVALSHA/FUNCTION 视为脚本执行 → 需授权（不做脚本内容分析，无法保证只读）。

/// Redis 命令风险。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedisRisk {
    /// 已知只读命令：可在只读路径执行。
    ReadOnly,
    /// 需要授权/审批：写命令，或无法判定只读的命令。
    RequiresAuthorization { reasons: Vec<String> },
    /// 直接拒绝：管理/危险/长连接命令。
    Rejected { reasons: Vec<String> },
}

impl RedisRisk {
    pub fn is_read_only(&self) -> bool {
        matches!(self, RedisRisk::ReadOnly)
    }

    pub fn reasons(&self) -> &[String] {
        match self {
            RedisRisk::ReadOnly => &[],
            RedisRisk::RequiresAuthorization { reasons } | RedisRisk::Rejected { reasons } => reasons,
        }
    }
}

/// 明确只读的命令（Redis 只读集合的保守子集）。
const REDIS_READ_ONLY: &[&str] = &[
    "get", "mget", "strlen", "type", "ttl", "pttl", "exists", "hexists", "hget", "hmget",
    "hgetall", "hkeys", "hvals", "hlen", "hstrlen", "llen", "lrange", "lindex", "lpos",
    "scard", "sismember", "smembers", "srandmember", "zcard", "zcount", "zrange",
    "zrangebyscore", "zscore", "zrank", "zrevrange", "xlen", "xrange", "xrevrange",
    "dbsize", "info", "memory", "object", "time", "lolwut", "geodist", "geopos", "geohash",
    "bitcount", "bitpos", "getrange", "dump", "pfcount", "randomkey", "scan", "sscan",
    "hscan", "zscan", "keys",
];

/// 需要授权但不能直接拒绝的（写、脚本、键空间变更）。
const REDIS_REQUIRES_AUTHORIZATION: &[&str] = &[
    "set", "setnx", "setex", "psetex", "mset", "msetnx", "append", "setrange", "incr", "decr",
    "incrby", "decrby", "incrbyfloat", "getset", "getdel", "getex", "del", "unlink", "expire",
    "pexpire", "expireat", "persist", "rename", "renamenx", "move", "hset", "hsetnx", "hmset",
    "hdel", "hincrby", "hincrbyfloat", "lpush", "rpush", "lpop", "rpop", "lset", "ltrim",
    "linsert", "lrem", "sadd", "srem", "spop", "smove", "zadd", "zrem", "zincrby", "zremrangebyrank",
    "zremrangebyscore", "xadd", "xtrim", "xdel", "xack", "xgroup", "pfadd", "pfmerge",
    "setbit", "bitop", "bitfield", "geoadd", "copy", "restore", "migrate", "evict",
    "eval", "evalsha", "eval_ro", "evalsha_ro", "fcall", "fcall_ro", "function", "script",
    "publish", "spublish", "setrange",
];

/// 直接拒绝：管理/危险/长连接/阻塞命令。
const REDIS_REJECTED: &[&str] = &[
    "config", "acl", "debug", "monitor", "shutdown", "slaveof", "replicaof", "failover",
    "cluster", "bgsave", "bgrewriteaof", "save", "lastsave", "flushall", "flushdb", "swapdb",
    "client", "module", "migrate", "restore-asking", "subscribe", "psubscribe", "ssubscribe",
    "unsubscribe", "punsubscribe", "sunsubscribe", "psync", "sync", "replconf", "wait",
    "blpop", "brpop", "blmove", "brpoplpush", "blmpop", "bzpopmin", "bzpopmax", "bzmpop",
    "xread", "xreadgroup", "auth", "hello", "reset", "select",
];

/// 按 argv 分类一条 Redis 命令。
///
/// `argv` 是**未拼接**的参数列表（第一条是命令名）；不做 shell/CLI 文本解析。
/// 参数数量与语义由工具 schema 与连接器再做边界校验。
pub fn classify_redis_command(argv: &[String]) -> RedisRisk {
    let Some(command) = argv.first().map(|value| value.to_ascii_lowercase()) else {
        return RedisRisk::Rejected {
            reasons: vec!["empty_command".into()],
        };
    };
    if REDIS_REJECTED.contains(&command.as_str()) {
        return RedisRisk::Rejected {
            reasons: vec![format!("blocked_command:{command}")],
        };
    }
    if REDIS_READ_ONLY.contains(&command.as_str()) {
        // KEYS/SCAN 之类全库遍历仍要限流边界，但不属于写风险。
        return RedisRisk::ReadOnly;
    }
    if REDIS_REQUIRES_AUTHORIZATION.contains(&command.as_str()) {
        return RedisRisk::RequiresAuthorization {
            reasons: vec![format!("mutating_or_script:{command}")],
        };
    }
    // 未知命令一律按需授权处理：宁可多问一次，也不放过未识别的副作用。
    RedisRisk::RequiresAuthorization {
        reasons: vec![format!("unknown_command:{command}")],
    }
}

/// `redis.scan` 输入 schema。
pub const REDIS_SCAN_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "pattern"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "pattern": { "type": "string", "minLength": 1, "maxLength": 512 },
    "cursor": { "type": "string", "maxLength": 32 },
    "count": { "type": "integer", "minimum": 1, "maximum": 1000 }
  }
}"##;

/// `redis.inspect` 输入 schema。
pub const REDIS_INSPECT_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "key"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "key": { "type": "string", "minLength": 1, "maxLength": 1024 },
    "view": { "type": "string", "enum": ["metadata", "value"], "default": "metadata" },
    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
  }
}"##;

/// `redis.execute` 输入 schema：argv 原样传参，不接受拼接后的命令文本。
pub const REDIS_EXECUTE_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "argv"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "argv": {
      "type": "array",
      "minItems": 1,
      "maxItems": 64,
      "items": { "type": "string", "maxLength": 4096 }
    },
    "intent": { "type": "string", "maxLength": 512 }
  }
}"##;

/// `redis.server_info` 输入 schema。
pub const REDIS_SERVER_INFO_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "sections": {
      "type": "array",
      "maxItems": 8,
      "items": { "type": "string", "maxLength": 64 }
    }
  }
}"##;

/// 四个 Redis 工具的定义：scan/inspect/server_info 为 Read，execute 为 Write（逐次审批）。
pub fn standard_redis_tool_specs() -> fluxdb_core::Result<Vec<ToolSpec>> {
    let descriptors = [
        fluxdb_core::ToolDescriptor {
            id: "redis.scan".into(),
            version: 1,
            description: "按 pattern 渐进扫描 Key（游标分页，有界）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: REDIS_SCAN_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "redis.inspect".into(),
            version: 1,
            description: "查看单个 Key 的类型/TTL/有界值预览".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: REDIS_INSPECT_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "redis.execute".into(),
            version: 1,
            description: "按 argv 执行一条 Redis 命令（写命令需逐次审批）".into(),
            risk: fluxdb_core::ToolRisk::Write,
            input_schema_json: REDIS_EXECUTE_INPUT_SCHEMA.into(),
        },
        fluxdb_core::ToolDescriptor {
            id: "redis.server_info".into(),
            version: 1,
            description: "读取限定 section 的服务诊断信息".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: REDIS_SERVER_INFO_INPUT_SCHEMA.into(),
        },
    ];
    descriptors
        .iter()
        .map(ToolSpec::from_descriptor)
        .collect::<fluxdb_core::Result<Vec<_>>>()
}
