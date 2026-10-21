//! 端到端协议验证：用真实抓包（my_lob.pcap）的服务器响应回放，
//! 验证客户端请求帧与抓包逐字节一致（DH 公钥/加密字段除外），
//! 并验证 `select 1` 结果解析正确。无需真实服务器。

mod mock_responses;

use std::io::Write;
use std::net::TcpListener;
use std::thread;

use dm_pure::frame::{encode_frame, Frame, FrameReader};
use dm_pure::{DmClient, ExecuteResult, Param, Value};

const HANDSHAKE_OPTS: [u8; 17] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x08,
    0x00,
];
const LOGIN_OPTS: [u8; 23] = [
    0x05, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
];

fn assert_frame(f: &Frame, expect_cmd: i16, expect_opts: &[u8], expect_body: &[u8]) {
    assert_eq!(f.cmd, expect_cmd, "cmd 不匹配");
    assert_eq!(
        &f.opts[..expect_opts.len()],
        expect_opts,
        "opts 前 {} 字节不匹配",
        expect_opts.len()
    );
    assert_eq!(
        &f.body[..expect_body.len()],
        expect_body,
        "body 前 {} 字节不匹配",
        expect_body.len()
    );
}

fn mock_server(listener: TcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = FrameReader::new(stream.try_clone().unwrap());

        // 1. 握手 cmd=200 → 回放 cmd=228
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 200, &HANDSHAKE_OPTS, &[]);
        // body: [i32 8]["8.1.5.60"\0][i32 64][64B DH pub]
        assert_eq!(&f.body[0..4], &8i32.to_le_bytes());
        assert_eq!(&f.body[4..13], b"8.1.5.60\x00");
        assert_eq!(&f.body[13..17], &64i32.to_le_bytes());
        assert_eq!(f.body.len(), 81);
        stream.write_all(&mock_responses::RESP0).unwrap();

        // 2. 登录 cmd=1 → 回放 cmd=163
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 1, &LOGIN_OPTS, &[]);
        // body: [i32 6][6B 密文][i32 8][8B 密文][i32 0][i32 5]"linux"[i32 0][0x00][u16 0]
        assert_eq!(&f.body[0..4], &6i32.to_le_bytes());
        assert_eq!(&f.body[10..14], &8i32.to_le_bytes());
        assert_eq!(&f.body[22..26], &0i32.to_le_bytes());
        assert_eq!(&f.body[26..30], &5i32.to_le_bytes());
        assert_eq!(&f.body[30..35], b"linux");
        assert_eq!(&f.body[35..39], &0i32.to_le_bytes());
        assert_eq!(f.body[39], 0x00);
        assert_eq!(&f.body[40..42], &0u16.to_le_bytes());
        assert_eq!(f.body.len(), 42);
        stream.write_all(&mock_responses::RESP1).unwrap();

        // 3. 会话查询 cmd=3 → 回放 cmd=187(24)
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 3, &[1, 0, 0, 0], &[]);
        stream.write_all(&mock_responses::RESP2).unwrap();

        // 4. 执行 "select 1" cmd=5 → 回放 cmd=187(60)
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 5, &[0x01, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00], &[]);
        assert_eq!(&f.body, b"select 1\x00");
        stream.write_all(&mock_responses::RESP3).unwrap();

        // 5. 释放 cmd=4 → 回放 cmd=0(24)
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 4, &[0u8; 44], &[]);
        stream.write_all(&mock_responses::RESP4).unwrap();

        // 若客户端继续发 cmd=3（下一次 execute 前），进入参数化查询场景
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 3, &[1, 0, 0, 0], &[]);
        stream.write_all(&mock_responses::RESP2).unwrap();

        // 6. 参数化 SELECT：cmd=5（SQL 带 ?）→ 回放 cmd=0(106) 列描述符
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 5, &[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00], &[]);
        assert_eq!(&f.body, b"SELECT content FROM lob_test3 WHERE id = ?\x00");
        stream.write_all(&mock_responses::PREP_PARAM).unwrap();

        // 7. cmd=13 参数绑定 → 回放 cmd=187(97) 行（CLOB 行内值）
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 13, &[0x01, 0x01, 0x00, 0x01, 0x01], &[]);
        // 描述区：ioType=0, colType=8(BIGINT), prec=8, scale=0；数据 [u16 8][i64 1]
        assert_eq!(
            &f.body[..13],
            &[0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            &f.body[13..],
            &[0x08, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
        stream.write_all(&mock_responses::ROWS_CLOB).unwrap();

        // 8. cmd=44（取结果完成）→ 回放 cmd=0(34, ret=111)
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 44, &[0u8; 44], &[]);
        stream.write_all(&mock_responses::CMD44_RESP).unwrap();

        // 9. cmd=4（释放）→ 回放 cmd=0(24)
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 4, &[0u8; 44], &[]);
        stream.write_all(&mock_responses::CMD4_RESP2).unwrap();
    })
}

#[test]
fn e2e_mock_server_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = mock_server(listener);

    let mut c = DmClient::new(&addr.ip().to_string(), addr.port(), "SYSDBA", "password");
    c.connect().expect("连接失败");
    assert!(c.session_key().is_some());

    let r = c.execute("select 1", &[]).unwrap();
    match r {
        ExecuteResult::Query { rows, .. } => {
            assert_eq!(rows, vec![vec![Value::Int(1)]]);
        }
        other => panic!("期望 Query，得到 {other:?}"),
    }

    // 参数化查询 + CLOB 解码
    let r = c
        .execute(
            "SELECT content FROM lob_test3 WHERE id = ?",
            &[Param::Int(1)],
        )
        .unwrap();
    match r {
        ExecuteResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].col_type, dm_pure::result::DM_CLOB);
            assert_eq!(
                rows,
                vec![vec![Value::Str(
                    "这是LOB测试内容，包含中文和English混合。".into()
                )]]
            );
        }
        other => panic!("期望 Query，得到 {other:?}"),
    }
    c.close();
    handle.join().unwrap();
}

// ---------- 超大结果集 cmd=7 循环（非参数化大查询） ----------

/// 构建 INT 列描述符（parse_columns 格式）：col_type=7, prec=4, scale=0, nullable=0
fn build_col_desc_int() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&7i32.to_le_bytes()); // col_type INT
    b.extend_from_slice(&4i32.to_le_bytes()); // prec
    b.extend_from_slice(&0i32.to_le_bytes()); // scale
    b.extend_from_slice(&0i32.to_le_bytes()); // nullable
    b.extend_from_slice(&[0u8; 8]); // 保留
    b.extend_from_slice(&2i16.to_le_bytes()); // name_len
    b.extend_from_slice(&0i16.to_le_bytes()); // type_len
    b.extend_from_slice(&0i16.to_le_bytes()); // table_len
    b.extend_from_slice(&0i16.to_le_bytes()); // schema_len
    b.extend_from_slice(b"ID");
    b.extend_from_slice(&0i16.to_le_bytes()); // baseName 长度 0
    b
}

/// 普通查询行（parse_rows 格式）：[int32 行长][6B][偏移表 2×ncols][int16 长度 + 数据]
fn build_rows_nonparam(rows: &[i32]) -> Vec<u8> {
    let mut b = Vec::new();
    for &v in rows {
        let data = v.to_le_bytes();
        let rlen = 4 + 6 + 2 + 2 + data.len();
        b.extend_from_slice(&(rlen as i32).to_le_bytes());
        b.extend_from_slice(&[0u8; 6]);
        b.extend_from_slice(&12i16.to_le_bytes()); // 偏移表（解析时跳过，任意值）
        b.extend_from_slice(&(data.len() as i16).to_le_bytes());
        b.extend_from_slice(&data);
    }
    b
}

/// 参数化/分批 fetch 行（parse_rows_param 格式）：[int16 行长][int16 行号][6B][偏移表][int16 长度 + 数据]
fn build_rows_param(rows: &[i32]) -> Vec<u8> {
    let mut b = Vec::new();
    for &v in rows {
        let data = v.to_le_bytes();
        let rlen = 2 + 2 + 6 + 2 + 2 + data.len();
        b.extend_from_slice(&(rlen as i16).to_le_bytes());
        b.extend_from_slice(&1i16.to_le_bytes()); // 行号
        b.extend_from_slice(&[0u8; 6]);
        b.extend_from_slice(&12i16.to_le_bytes()); // 偏移表
        b.extend_from_slice(&(data.len() as i16).to_le_bytes());
        b.extend_from_slice(&data);
    }
    b
}

fn mock_server_fetch_loop(listener: TcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = FrameReader::new(stream.try_clone().unwrap());

        // 1-3. 握手/登录/会话查询（回放抓包）
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 200, &HANDSHAKE_OPTS, &[]);
        stream.write_all(&mock_responses::RESP0).unwrap();
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 1, &LOGIN_OPTS, &[]);
        stream.write_all(&mock_responses::RESP1).unwrap();
        let f = reader.read_frame().unwrap();
        assert_frame(&f, 3, &[1, 0, 0, 0], &[]);
        stream.write_all(&mock_responses::RESP2).unwrap();

        // 4. 非参数化大查询 cmd=5 → 首批 2 行 + nrows=0xFFFF（还有更多）
        let f = reader.read_frame().unwrap();
        assert_eq!(f.cmd, 5);
        assert_eq!(&f.body, b"SELECT id FROM big_table\x00");
        let mut opts = [0u8; 44];
        opts[2..4].copy_from_slice(&1u16.to_le_bytes()); // ncols
        opts[4..6].copy_from_slice(&0xFFFFu16.to_le_bytes()); // nrows=65535 → 还有更多
        let mut body = build_col_desc_int();
        body.extend_from_slice(&build_rows_nonparam(&[1, 2]));
        stream
            .write_all(&encode_frame(0, 187, &opts, &body))
            .unwrap();

        // 5. cmd=7 第一次拉取：startPos=2 → 返回行 3,4（总行数 5）
        let f = reader.read_frame().unwrap();
        assert_eq!(f.cmd, 7);
        assert_eq!(&f.opts[..8], &2i64.to_le_bytes(), "startPos 应为 2");
        assert_eq!(&f.opts[8..16], &i64::MAX.to_le_bytes());
        let mut opts = [0u8; 44];
        opts[..8].copy_from_slice(&5i64.to_le_bytes()); // 总行数
        stream
            .write_all(&encode_frame(0, 187, &opts, &build_rows_param(&[3, 4])))
            .unwrap();

        // 6. cmd=7 第二次拉取：startPos=4 → 返回行 5（start=5 >= total=5 → 结束）
        let f = reader.read_frame().unwrap();
        assert_eq!(f.cmd, 7);
        assert_eq!(&f.opts[..8], &4i64.to_le_bytes(), "startPos 应为 4");
        let mut opts = [0u8; 44];
        opts[..8].copy_from_slice(&5i64.to_le_bytes());
        stream
            .write_all(&encode_frame(0, 187, &opts, &build_rows_param(&[5])))
            .unwrap();

        // 7. cmd=44（取结果完成）→ cmd=0(34)
        let f = reader.read_frame().unwrap();
        assert_eq!(f.cmd, 44);
        stream.write_all(&mock_responses::CMD44_RESP).unwrap();

        // 8. cmd=4（释放）→ cmd=0(24)
        let f = reader.read_frame().unwrap();
        assert_eq!(f.cmd, 4);
        stream.write_all(&mock_responses::CMD4_RESP2).unwrap();
    })
}

#[test]
fn e2e_fetch_loop_large_nonparam() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = mock_server_fetch_loop(listener);

    let mut c = DmClient::new(&addr.ip().to_string(), addr.port(), "SYSDBA", "password");
    c.connect().expect("连接失败");
    let r = c.execute("SELECT id FROM big_table", &[]).unwrap();
    match r {
        ExecuteResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 5, "应取回全部 5 行，实际 {}", rows.len());
            let got: Vec<i64> = rows.iter().map(|r| r[0].as_i64().unwrap()).collect();
            assert_eq!(got, vec![1, 2, 3, 4, 5]);
        }
        other => panic!("期望 Query，得到 {other:?}"),
    }
    c.close();
    handle.join().unwrap();
}
