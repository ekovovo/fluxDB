//! 与 Python 参考实现（dmpy）随机向量的交叉验证。
//! 向量由 `scripts/gen_vectors.py` 生成（随机种子 42），验证：
//! DATE/TIME/DATETIME 位压缩编解码互逆、DECIMAL 私有大数解码。

mod vectors;

use dm_pure::result::{
    decode_date, decode_datetime, decode_decimal, decode_time, encode_date, encode_datetime,
    encode_time, DmDate, DmDateTime, DmTime,
};

fn hex2b(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn date_vectors_roundtrip() {
    for (hex, y, m, d) in vectors::DATE_VECTORS {
        let raw = hex2b(hex);
        let dt = decode_date(&raw);
        assert_eq!(dt, DmDate::new(*y, *m, *d), "decode {hex}");
        // Rust 编码应与 Python 完全一致
        let enc = encode_date(&dt);
        assert_eq!(enc.to_vec(), raw, "encode {hex}");
    }
}

#[test]
fn time_vectors_roundtrip() {
    for (hex, h, m, s, us) in vectors::TIME_VECTORS {
        let raw = hex2b(hex);
        let t = decode_time(&raw);
        assert_eq!(t, DmTime::new(*h, *m, *s, *us), "decode {hex}");
        let enc = encode_time(&t);
        assert_eq!(enc.to_vec(), raw, "encode {hex}");
    }
}

#[test]
fn datetime_vectors_roundtrip() {
    for (hex, y, m, d, h, mi, s, us) in vectors::DATETIME_VECTORS {
        let raw = hex2b(hex);
        let dt = decode_datetime(&raw);
        assert_eq!(
            dt,
            DmDateTime::new(DmDate::new(*y, *m, *d), DmTime::new(*h, *mi, *s, *us)),
            "decode {hex}"
        );
        let enc = encode_datetime(&dt);
        assert_eq!(enc, raw, "encode {hex}");
    }
}

#[test]
fn decimal_vectors_match_python() {
    for (hex, expected) in vectors::DECIMAL_VECTORS {
        let raw = hex2b(hex);
        let got = decode_decimal(&raw);
        assert_eq!(&got, expected, "decode {hex}");
    }
}
