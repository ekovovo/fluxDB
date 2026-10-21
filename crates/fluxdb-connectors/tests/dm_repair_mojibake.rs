//! 一次性修复 FLUXSALES demo 数据中“UTF-8 字节被按 GB18030 解码后再以 UTF-8 存储”的乱码。
//!
//! 只在严格可逆时修改，并在提交前逐行复核。需 `FLUXDB_DM_SMOKE=host:port:user:password`。

use dm_pure::{ClientConfig, DmClient, Param, Value};

const TARGETS: [(&str, &str, &str); 4] = [
    ("SALES_ORDER", "ORDER_ID", "CUSTOMER_NAME"),
    ("SALES_ORDER", "ORDER_ID", "REMARK"),
    ("SALES_ORDER_ITEM", "ITEM_ID", "PRODUCT_NAME"),
    ("SALES_REGION", "REGION_ID", "REGION_NAME"),
];

fn recover(text: &str) -> Option<String> {
    if text.is_ascii() {
        return None;
    }
    let (bytes, _, had_errors) = encoding_rs::GB18030.encode(text);
    if had_errors {
        return None;
    }
    let recovered = std::str::from_utf8(&bytes).ok()?;
    if recovered == text || recovered.is_ascii() {
        return None;
    }
    // 还原结果必须仍是合法中文文本：不能变成 ASCII，也不能含替换字符。
    if recovered.is_ascii() || recovered.contains('\u{FFFD}') {
        return None;
    }
    Some(recovered.to_string())
}

fn client() -> DmClient {
    let raw = std::env::var("FLUXDB_DM_SMOKE").expect("FLUXDB_DM_SMOKE");
    let parts = raw.splitn(4, ':').collect::<Vec<_>>();
    let mut client = DmClient::with_config(ClientConfig {
        host: parts[0].into(),
        port: parts[1].parse().expect("port"),
        user: parts[2].into(),
        password: parts[3].into(),
        ..ClientConfig::default()
    });
    client.connect().expect("connect");
    client
}

#[test]
#[ignore = "会修改 FLUXSALES demo 数据，需 FLUXDB_DM_SMOKE"]
fn dm_repair_mojibake_rows() {
    let mut client = client();
    client.execute("BEGIN", &[]).ok();
    let mut repaired = 0usize;
    for (table, key, column) in TARGETS {
        let sql = format!("SELECT {key}, {column} FROM FLUXSALES.{table} ORDER BY {key}");
        let result = client.execute(&sql, &[]).expect("select rows");
        let rows = result.rows().expect("result rows");
        for row in rows {
            let Some(Value::Str(current)) = row.get(1) else {
                continue;
            };
            let Some(fixed) = recover(current) else {
                continue;
            };
            let id = match row.first().cloned().expect("key column") {
                Value::TinyInt(value) => Param::Int(i64::from(value)),
                Value::SmallInt(value) => Param::Int(i64::from(value)),
                Value::Int(value) => Param::Int(i64::from(value)),
                Value::BigInt(value) => Param::Int(value),
                other => panic!("unexpected key type: {other:?}"),
            };
            let update = format!("UPDATE FLUXSALES.{table} SET {column} = ? WHERE {key} = ?");
            client
                .execute(&update, &[Param::Str(fixed.clone()), id])
                .expect("update row");
            repaired += 1;
            eprintln!("{table}.{column}: {current} -> {fixed}");
        }
    }
    client.execute("COMMIT", &[]).expect("commit");

    // 提交后复核：不应再存在可还原的行。
    for (table, key, column) in TARGETS {
        let sql = format!("SELECT {key}, {column} FROM FLUXSALES.{table}");
        let result = client.execute(&sql, &[]).expect("verify select");
        let rows = result.rows().expect("rows");
        let remaining = rows
            .iter()
            .filter(|row| matches!(row.get(1), Some(Value::Str(text)) if recover(text).is_some()))
            .count();
        assert_eq!(remaining, 0, "{table}.{column} 仍有乱码");
    }
    eprintln!("repaired {repaired} values");
    client.close();
}
