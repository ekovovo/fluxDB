//! 达梦批量参数化 INSERT 回归：确认帧随行数增长时仍被识别为 DML，而不是结果集。

use dm_pure::{ClientConfig, DmClient, Param};

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
#[ignore = "需要 FLUXDB_DM_SMOKE"]
fn dm_batch_insert_reports_affected_rows() {
    let mut client = client();
    let _ = client.execute("DROP TABLE SYSDBA.DM_BATCH_PROBE", &[]);
    client
        .execute(
            "CREATE TABLE SYSDBA.DM_BATCH_PROBE(A INT, B VARCHAR(40), C INT, D VARCHAR(40), E INT, F VARCHAR(40), G INT, H VARCHAR(40), I INT, J VARCHAR(40))",
            &[],
        )
        .expect("create");

    for rows in [1usize, 2, 3, 4, 5, 8, 13, 50] {
        let mut params = Vec::new();
        let mut placeholders = Vec::new();
        for row in 0..rows {
            let mut one = Vec::new();
            for col in 0..10 {
                if col % 2 == 0 {
                    one.push("?".to_string());
                    params.push(Param::Int((row * 10 + col) as i64));
                } else {
                    one.push("?".to_string());
                    params.push(Param::Str(format!("v{row}_{col}")));
                }
            }
            placeholders.push(format!("({})", one.join(", ")));
        }
        let sql = format!(
            "INSERT INTO SYSDBA.DM_BATCH_PROBE VALUES {}",
            placeholders.join(", ")
        );
        // 4 行这一档曾因确认帧超过 30 字节被误判为结果集而报错。
        let result = client.execute(&sql, &params);
        assert!(
            result.is_ok(),
            "rows={rows} params={} failed: {:?}",
            params.len(),
            result.err().map(|error| error.msg)
        );
    }
    let _ = client.execute("DROP TABLE SYSDBA.DM_BATCH_PROBE", &[]);
    client.close();
}
