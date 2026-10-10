/// 单个 RESP 回包的解码预算；普通桌面读取仍使用原有的大值边界。
#[derive(Clone, Copy, Debug)]
pub struct RedisReadBudget {
    pub max_bulk_bytes: usize,
    pub max_total_bytes: usize,
    pub max_elements: usize,
    pub max_depth: usize,
}

impl Default for RedisReadBudget {
    fn default() -> Self {
        Self {
            max_bulk_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_elements: 1000,
            max_depth: 16,
        }
    }
}

fn redis_read_value_with_budget(
    reader: &mut impl std::io::BufRead,
    budget: RedisReadBudget,
) -> fluxdb_core::Result<RedisValue> {
    let mut bytes_left = budget.max_total_bytes;
    let mut elements_left = budget.max_elements;
    redis_read_bounded_value(reader, budget, &mut bytes_left, &mut elements_left, 0)
}

fn redis_budget_error() -> Error {
    Error::new(ErrorKind::Connection, "Redis VALUE_TOO_LARGE：回包超出解码预算，请缩小读取范围")
}

fn redis_read_bounded_line(
    reader: &mut impl std::io::BufRead,
    budget: RedisReadBudget,
    bytes_left: &mut usize,
) -> fluxdb_core::Result<String> {
    use std::io::{BufRead, Read};
    let limit = (*bytes_left).min(budget.max_bulk_bytes).min(REDIS_MAX_LINE_LEN);
    let mut bytes = Vec::new();
    (&mut *reader).take(limit as u64 + 1).read_until(b'\n', &mut bytes).map_err(redis_io_error)?;
    if bytes.len() > limit {
        return Err(redis_budget_error());
    }
    if !bytes.ends_with(b"\r\n") {
        return Err(protocol_error("Redis 回包单行缺少 CRLF"));
    }
    *bytes_left -= bytes.len();
    bytes.truncate(bytes.len() - 2);
    String::from_utf8(bytes).map_err(|_| protocol_error("Redis 回包头不是 UTF-8"))
}

fn redis_read_bounded_value(
    reader: &mut impl std::io::BufRead,
    budget: RedisReadBudget,
    bytes_left: &mut usize,
    elements_left: &mut usize,
    depth: usize,
) -> fluxdb_core::Result<RedisValue> {
    use std::io::Read;
    if depth >= budget.max_depth || *elements_left == 0 || *bytes_left == 0 {
        return Err(redis_budget_error());
    }
    *elements_left -= 1;
    *bytes_left -= 1;
    let mut prefix = [0_u8; 1];
    reader.read_exact(&mut prefix).map_err(redis_io_error)?;
    let header = redis_read_bounded_line(reader, budget, bytes_left)?;
    match prefix[0] {
        b'+' => Ok(RedisValue::Simple(header)),
        b'-' => Err(Error::new(ErrorKind::Query, header)),
        b':' => header.parse::<i64>().map(RedisValue::Int)
            .map_err(|_| protocol_error("Redis 整数返回格式异常")),
        b'$' | b'*' => {
            let len = header.parse::<i64>()
                .map_err(|_| protocol_error("Redis 返回长度异常"))?;
            if len == -1 {
                return Ok(if prefix[0] == b'$' { RedisValue::Bulk(None) } else { RedisValue::Array(Vec::new()) });
            }
            let len = usize::try_from(len).map_err(|_| protocol_error("Redis 返回长度异常"))?;
            if prefix[0] == b'*' {
                // 在进入循环和分配元素之前拒绝超限声明。
                if len > *elements_left {
                    return Err(redis_budget_error());
                }
                let mut values = Vec::with_capacity(len.min(64));
                for _ in 0..len {
                    values.push(redis_read_bounded_value(reader, budget, bytes_left, elements_left, depth + 1)?);
                }
                Ok(RedisValue::Array(values))
            } else {
                if len > budget.max_bulk_bytes || len.checked_add(2).is_none_or(|size| size > *bytes_left) {
                    return Err(redis_budget_error());
                }
                *bytes_left -= len + 2;
                let mut bytes = Vec::new();
                (&mut *reader).take(len as u64).read_to_end(&mut bytes).map_err(redis_io_error)?;
                if bytes.len() != len {
                    return Err(protocol_error("Redis Bulk 返回长度与声明不一致"));
                }
                let mut crlf = [0_u8; 2];
                reader.read_exact(&mut crlf).map_err(redis_io_error)?;
                if crlf != *b"\r\n" {
                    return Err(protocol_error("Redis Bulk 缺少 CRLF"));
                }
                Ok(RedisValue::Bulk(Some(bytes)))
            }
        }
        other => Err(protocol_error(&format!("未收到 Redis 协议响应：0x{other:02x}"))),
    }
}

#[cfg(test)]
mod redis_read_budget_tests {
    use super::*;

    fn read(payload: &[u8], budget: RedisReadBudget) -> fluxdb_core::Result<RedisValue> {
        redis_read_value_with_budget(&mut std::io::Cursor::new(payload), budget)
    }

    #[test]
    fn oversized_bulk_and_array_are_rejected_from_headers() {
        for payload in [b"$1048576\r\n".as_slice(), b"*1000000\r\n".as_slice()] {
            let error = read(payload, RedisReadBudget::default()).unwrap_err();
            assert!(error.message.contains("VALUE_TOO_LARGE"));
        }
    }

    #[test]
    fn total_bytes_elements_and_depth_are_cumulative() {
        for (payload, budget) in [
            (b"*2\r\n$4\r\nabcd\r\n$4\r\nefgh\r\n".as_slice(), RedisReadBudget { max_total_bytes: 20, ..Default::default() }),
            (b"*2\r\n*2\r\n:1\r\n:2\r\n*2\r\n:3\r\n:4\r\n".as_slice(), RedisReadBudget { max_elements: 6, ..Default::default() }),
            (b"*1\r\n*1\r\n:1\r\n".as_slice(), RedisReadBudget { max_depth: 2, ..Default::default() }),
        ] {
            assert!(read(payload, budget).unwrap_err().message.contains("VALUE_TOO_LARGE"));
        }
    }

    #[test]
    fn binary_and_nested_values_remain_exact() {
        let value = read(b"*2\r\n$2\r\n\x00\xff\r\n:5\r\n", RedisReadBudget::default()).unwrap();
        assert_eq!(value, RedisValue::Array(vec![RedisValue::Bulk(Some(vec![0, 255])), RedisValue::Int(5)]));
    }
}
