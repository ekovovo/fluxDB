// Redis 写入风险的确定性证据（与 SQL 侧同构，见 `ai_write_risk_sql.rs`）。
//
// Redis 没有"事务内复核影响"的有界提交能力：写命令发出去就生效，命令族本身的语义
// 就是它全部的风险。因此这里只用**命令族 + 参数形状**评分，不承诺回滚。
//
// 出站边界：`evidence_json` 只含命令名与结构标签，不含 Key 名、参数值、频道或凭据。

/// Redis 命令族对键空间的影响形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedisWriteShape {
    /// 单键覆盖写（`SET`/`HSET`/`LSET`…）：影响面就是那一个键。
    SingleKeyOverwrite,
    /// 单键追加/增量写（`APPEND`/`INCR`/`LPUSH`/`SADD`…）：不改别的键，但值与长度会累积。
    SingleKeyAccumulate,
    /// 有界单键删除（`DEL`/`UNLINK`/`GETDEL`）：键消失不可恢复。
    SingleKeyDestructive,
    /// 多键写入：影响面随参数个数放大。
    MultiKeyWrite,
    /// 键空间/结构级操作（`RENAME` 覆盖目标、`EXPIRE` 改 TTL 等）。
    KeySpaceMutation,
}

/// 生成 Redis 写入风险证据。
///
/// `key_scope_bounded` 表示本次调用的 Key 已落在用户授权的 prefix 范围内：
/// 范围受限能降低"影响面不可知"的不确定度，但不改变命令族本身的不可逆性。
pub fn redis_write_risk_facts(argv: &[String], key_scope_bounded: bool) -> WriteRiskFacts {
    let command = argv
        .first()
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let shape = redis_write_shape(&command);
    let mut facts = WriteRiskFacts::new(redis_evidence_json(shape, &command, argv, key_scope_bounded));
    redis_score_shape(shape, &command, argv, key_scope_bounded, &mut facts);
    facts
}

fn redis_score_shape(
    shape: RedisWriteShape,
    command: &str,
    argv: &[String],
    key_scope_bounded: bool,
    facts: &mut WriteRiskFacts,
) {
    match shape {
        RedisWriteShape::SingleKeyOverwrite => {
            // 值可被重写但旧值不可找回；范围受限时影响面明确。
            let score = if key_scope_bounded { 0.15 } else { 0.40 };
            facts.soft("single_key_overwrite", score, "单键覆盖写");
        }
        RedisWriteShape::SingleKeyAccumulate => {
            facts.soft("single_key_accumulate", 0.30, "单键追加或增量写");
        }
        RedisWriteShape::SingleKeyDestructive => {
            // 键删掉就回不来：与 SQL 的 DELETE 同理，不因范围受限而降级为可自动放行。
            facts.hard("single_key_destructive", 0.55, "删除 Key，内容不可恢复");
        }
        RedisWriteShape::MultiKeyWrite => {
            facts.soft(
                "multi_key_write",
                0.55,
                format!("多键写入（{} 个参数）", argv.len().saturating_sub(1)),
            );
        }
        RedisWriteShape::KeySpaceMutation => {
            facts.soft("key_space_mutation", 0.55, "改动键空间或 TTL 语义");
        }
    }
    // 大 value / 大成员数：单次写可能撑爆内存或阻塞服务端。
    let payload_bytes: usize = argv.iter().skip(1).map(String::len).sum();
    if payload_bytes > 64 * 1024 {
        facts.soft(
            "large_payload",
            0.50,
            format!("参数总量约 {} KiB", payload_bytes / 1024),
        );
    }
    // 无 TTL 的覆盖写会让键永久驻留；带 EX/PX/KEEPTTL 的写法排除这一条。
    if matches!(shape, RedisWriteShape::SingleKeyOverwrite)
        && !redis_has_ttl_clause(argv)
        && matches!(
            command,
            "set" | "setnx" | "mset" | "msetnx" | "setex" | "psetex" | "hset" | "hsetnx" | "hmset"
        )
    {
        // 永久驻留是真实风险但不是不可逆写入：给评审模型留出背书空间。
        facts.soft("no_ttl", 0.25, "未指定 TTL 的覆盖写，键会永久驻留");
    }
}

/// 参数里是否带 TTL 语义（`EX`/`PX`/`EXAT`/`PXAT`/`KEEPTTL`）。
fn redis_has_ttl_clause(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|arg| {
        matches!(
            arg.to_ascii_lowercase().as_str(),
            "ex" | "px" | "exat" | "pxat" | "keepttl"
        ) || arg.to_ascii_lowercase().starts_with("ex")
            && arg.len() > 2
            && arg[2..].chars().all(|c| c.is_ascii_digit())
    })
}

fn redis_write_shape(command: &str) -> RedisWriteShape {
    match command {
        "set" | "setnx" | "getset" | "hset" | "hsetnx" | "hmset" | "lset" => {
            RedisWriteShape::SingleKeyOverwrite
        }
        "append" | "setrange" | "incr" | "decr" | "incrby" | "decrby" | "incrbyfloat" | "hincrby"
        | "hincrbyfloat" | "lpush" | "rpush" | "linsert" | "sadd" | "zadd" | "zincrby"
        | "pfadd" | "setbit" | "bitfield" | "geoadd" | "xadd" | "xack" => {
            RedisWriteShape::SingleKeyAccumulate
        }
        "del" | "unlink" | "getdel" | "lpop" | "rpop" | "spop" | "lrem" | "ltrim"
        | "srem" | "zrem" | "zremrangebyrank" | "zremrangebyscore" | "hdel" | "xtrim" | "xdel" => {
            RedisWriteShape::SingleKeyDestructive
        }
        "mset" | "msetnx" | "mget" | "bitop" | "pfmerge" | "smove" | "copy" => {
            RedisWriteShape::MultiKeyWrite
        }
        // 键空间/TTL/元数据语义变更：影响面取决于当时的键状态。
        "rename" | "renamenx" | "expire" | "pexpire" | "expireat" | "persist" | "evict"
        | "restore" | "xgroup" => RedisWriteShape::KeySpaceMutation,
        // 其余命令要么被策略层直接拒绝，要么不在写路径上：
        // 统一按最激进的多键写处理，避免"没归类＝不危险"。
        _ => RedisWriteShape::MultiKeyWrite,
    }
}

fn redis_evidence_json(
    shape: RedisWriteShape,
    command: &str,
    argv: &[String],
    key_scope_bounded: bool,
) -> String {
    serde_json::json!({
        "command": command,
        "shape": match shape {
            RedisWriteShape::SingleKeyOverwrite => "single_key_overwrite",
            RedisWriteShape::SingleKeyAccumulate => "single_key_accumulate",
            RedisWriteShape::SingleKeyDestructive => "single_key_destructive",
            RedisWriteShape::MultiKeyWrite => "multi_key_write",
            RedisWriteShape::KeySpaceMutation => "key_space_mutation",
        },
        "arg_count": argv.len(),
        "key_scope_bounded": key_scope_bounded,
        "has_ttl_clause": redis_has_ttl_clause(argv),
        "key_names": "omitted",
        "values": "omitted",
    })
    .to_string()
}

