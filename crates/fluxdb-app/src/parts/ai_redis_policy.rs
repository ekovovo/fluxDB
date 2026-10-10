// Redis 工具的风险分类与输入契约（设计 §22，T025）。
//
// 原则（与 SQL 侧一致）：
// - **不按 argv[1] 拍脑袋判断安全**：读白名单之外的命令一律按"需要授权"处理；
// - 管理/危险命令直接拒绝（CONFIG/ACL/DEBUG/MONITOR/SHUTDOWN/FLUSH*…），不给审批机会绕过；
// - 订阅命令不在 execute 路径放行：它会把连接挂成流式会话，由专门的 `redis.subscribe`
//   工具施加时限 / 消息数 / 字节上限后收集，不能用一条 argv 命令挂住普通执行连接；
// - Lua/function 默认拒绝，不能凭普通写审批开启任意脚本。

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

/// 直接拒绝：管理/危险/长连接/阻塞命令（含订阅；订阅走 `redis.subscribe` 工具）。
const REDIS_REJECTED: &[&str] = &[
    "config", "acl", "debug", "monitor", "shutdown", "slaveof", "replicaof", "failover",
    "cluster", "bgsave", "bgrewriteaof", "save", "lastsave", "flushall", "flushdb", "swapdb",
    "client", "module", "migrate", "restore-asking", "unsubscribe", "punsubscribe",
    "sunsubscribe", "psync", "sync", "replconf", "wait",
    "blpop", "brpop", "blmove", "brpoplpush", "blmpop", "bzpopmin", "bzpopmax", "bzmpop",
    "xread", "xreadgroup", "auth", "hello", "reset", "select",
    "eval", "evalsha", "eval_ro", "evalsha_ro", "fcall", "fcall_ro", "function", "script", "keys",
];

/// 订阅命令：不列入只读白名单，也不在 `redis.execute` 放行——它把连接挂成流式会话，
/// 必须走 `redis.subscribe` 工具施加时限 / 消息数 / 字节上限后收集。
pub const REDIS_SUBSCRIBE_COMMANDS: &[&str] = &["subscribe", "psubscribe", "ssubscribe"];


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
    // 订阅类命令有专门的 `redis.subscribe` 工具（有界收集），不能用一次性 argv 执行。
    if REDIS_SUBSCRIBE_COMMANDS.contains(&command.as_str()) {
        return RedisRisk::Rejected {
            reasons: vec![format!("use_redis_subscribe_tool:{command}")],
        };
    }
    if REDIS_REJECTED.contains(&command.as_str()) {
        return RedisRisk::Rejected {
            reasons: vec![format!("blocked_command:{command}")],
        };
    }
    if command == "memory" && !argv.get(1).is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "usage" | "stats" | "doctor" | "malloc-stats" | "help")) {
        return RedisRisk::Rejected { reasons: vec!["blocked_subcommand:memory".into()] };
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
    "count": { "type": "integer", "minimum": 1, "maximum": 1000 },
    "max_keys": { "type": "integer", "minimum": 1, "maximum": 1000 }
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
    "cursor": { "type": "string", "maxLength": 32 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 65536 }
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

/// `redis.subscribe` 输入 schema：订阅是有界的一次性收集，不是长会话。
pub const REDIS_SUBSCRIBE_INPUT_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["target", "channels"],
  "properties": {
    "target": { "type": "string", "minLength": 1 },
    "channels": {
      "type": "array",
      "minItems": 1,
      "maxItems": 16,
      "items": { "type": "string", "minLength": 1, "maxLength": 256 }
    },
    "pattern": { "type": "boolean", "default": false },
    "max_messages": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 },
    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 262144, "default": 65536 },
    "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 60000, "default": 10000 }
  }
}"##;

/// 五个 Redis 工具的定义：scan/inspect/server_info/subscribe 为 Read，execute 为 Write（逐次审批）。
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
        fluxdb_core::ToolDescriptor {
            id: "redis.subscribe".into(),
            version: 1,
            description: "有界订阅频道并收集消息（限时/限条数/限字节，可随时停止）".into(),
            risk: fluxdb_core::ToolRisk::Read,
            input_schema_json: REDIS_SUBSCRIBE_INPUT_SCHEMA.into(),
        },
    ];
    descriptors
        .iter()
        .map(ToolSpec::from_descriptor)
        .collect::<fluxdb_core::Result<Vec<_>>>()
}
