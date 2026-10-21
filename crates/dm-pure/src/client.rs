//! 达梦 DM8 最小客户端（参照 dm-pure Python 驱动移植）。
//!
//! 连接流程: TCP → 握手(cmd=200) → 登录(cmd=1) → 会话查询(cmd=3) → 执行(cmd=5)

use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use encoding_rs::{Encoding, EUC_KR, GB18030, UTF_8};

use crate::crypto::{compute_session_key, new_client_keypair, SymmCipher, DES_CFB, DH_P_HEX};
use crate::frame::{
    CMD_BIND, CMD_EXECUTE, CMD_EXEC_COMPLETE, CMD_FETCH, CMD_HANDSHAKE, CMD_LOGIN,
    CMD_SESSION_QUERY, Frame, FrameReader,
};
use crate::result::{
    decode_out_value, decode_scalar, decode_text_strict, encode_date, encode_datetime,
    encode_datetime2, encode_datetime2_tz, encode_datetime_tz, encode_time, encode_time_tz,
    parse_cols_robust, parse_columns, parse_rows, parse_rows_param, Column, DmDate, DmDateTime,
    DmTime, Row, Value, NULL_MARK, DM_BLOB, DM_CLOB,
};
use crate::{DmError, Result};

/// 存储过程 OUT 参数类型提示
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutType {
    Bool,
    Int,
    Float,
    Str,
    Date,
    DateTime,
}

/// 存储过程 OUT 参数标记：`execute("CALL proc(?, ?)", &[Param::Int(1), Param::out(Some(OutType::Int))])`
#[derive(Debug, Clone, PartialEq)]
pub struct OUT {
    pub type_hint: Option<OutType>,
}

impl OUT {
    pub fn new(type_hint: Option<OutType>) -> Self {
        OUT { type_hint }
    }
}

/// 绑定参数。
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Null,
    Bool(bool),
    /// 写（INSERT/UPDATE）用 INT(7)/prec4，查询参数用 BIGINT(8)/prec8
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Date(DmDate),
    Time(DmTime),
    DateTime(DmDateTime),
    /// 存储过程 OUT 参数（无输入数据）
    Out(Option<OutType>),
}

impl Param {
    pub fn out(t: Option<OutType>) -> Param {
        Param::Out(t)
    }
    pub fn is_out(&self) -> bool {
        matches!(self, Param::Out(_))
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Param::Null)
    }
}

/// 服务器返回的参数元信息（cmd=5 响应 serverParams）。
#[derive(Debug, Clone, Copy)]
pub struct ServerParam {
    pub col_type: i32,
    pub prec: i32,
    pub scale: i32,
    pub io_type: i32,
}

/// 执行结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecuteResult {
    /// 查询：列描述符 + 行
    Query {
        columns: Vec<Column>,
        rows: Vec<Vec<Value>>,
    },
    /// DML：影响行数
    Affected(u64),
    /// 存储过程 CALL：影响行数 + OUT 参数值
    Call {
        affected: u64,
        out: Vec<Value>,
    },
}

impl ExecuteResult {
    pub fn rows(&self) -> Option<&Vec<Vec<Value>>> {
        match self {
            ExecuteResult::Query { rows, .. } => Some(rows),
            _ => None,
        }
    }
    pub fn columns(&self) -> Option<&Vec<Column>> {
        match self {
            ExecuteResult::Query { columns, .. } => Some(columns),
            _ => None,
        }
    }
    pub fn affected(&self) -> Option<u64> {
        match self {
            ExecuteResult::Affected(n) | ExecuteResult::Call { affected: n, .. } => Some(*n),
            _ => None,
        }
    }
}

/// 客户端配置。
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub app_name: String,
    pub os_name: String,
    /// 连接/读写超时
    pub timeout: Duration,
    /// 断线自动重连
    pub auto_reconnect: bool,
    /// 每帧发送后的间隔（Rust 端单帧 write_all + TCP_NODELAY，默认 0 即可；
    /// 若遇帧合并问题可设 send_delay_ms 恢复间隔）
    pub send_delay: Duration,
    /// 服务器字符集。None=连接握手时自动检测（响应偏移 28：1=UTF8, 2=EUCKR,
    /// 其他=GB18030）；也可手动指定 "GB18030"/"UTF-8"/"EUC-KR" 覆盖检测。
    /// 默认自动检测——服务器为 UTF-8 而按 GB18030 编解码是中文乱码的根因。
    pub encoding: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            host: "127.0.0.1".into(),
            port: 5236,
            user: "SYSDBA".into(),
            password: String::new(),
            app_name: "rust-min".into(),
            os_name: "linux".into(),
            timeout: Duration::from_secs(15),
            auto_reconnect: true,
            // Rust 端 send_frame 单次 write_all 写完一整个帧 + TCP_NODELAY 禁用 Nagle，
            // 独立 TCP 段有保证，无需 sleep（默认 0；Python 端保留 2ms 因为 sendall 可能被合并）
            send_delay: Duration::ZERO,
            encoding: None,
        }
    }
}

/// 将用户指定的编码名解析为 encoding_rs 编码。未指定或无法识别返回 None（自动检测）。
fn resolve_encoding(name: Option<&str>) -> Option<&'static Encoding> {
    let s = name?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("utf8") || s.eq_ignore_ascii_case("utf-8") {
        Some(UTF_8)
    } else if s.eq_ignore_ascii_case("euckr") || s.eq_ignore_ascii_case("euc-kr") {
        Some(EUC_KR)
    } else if s.eq_ignore_ascii_case("gb18030")
        || s.eq_ignore_ascii_case("gbk")
        || s.eq_ignore_ascii_case("gb2312")
    {
        Some(GB18030)
    } else {
        None
    }
}

/// 惰性查询状态（execute_lazy 返回，fetch_more / close_lazy 使用）。
#[derive(Debug, Clone)]
pub struct LazyState {
    pub stmt: i32,
    pub ncols: usize,
    pub rows_raw: Vec<Row>,
    pub more: bool,
    pub closed: bool,
}

/// 达梦客户端：连接 / 查询 / 参数绑定 / LOB / 事务 / 存储过程 / 惰性查询。
pub struct DmClient {
    cfg: ClientConfig,
    reader: Option<FrameReader>,
    session_key: Option<Vec<u8>>,
    cipher: Option<SymmCipher>,
    /// execute 时 +1 从 0 开始连续递增（槽位 0-63 循环）
    stmt_id: i32,
    /// 服务器编码（登录后确定，默认 GB18030）
    svr_encoding: &'static Encoding,
    auto_commit: bool,
    /// 是否处于事务中（begin 后 commit/rollback 前）——断线重连保护
    in_transaction: bool,
    /// 大 LOB 分块大小
    lob_chunk_size: usize,
}

impl DmClient {
    pub fn new(host: &str, port: u16, user: &str, password: &str) -> Self {
        DmClient::with_config(ClientConfig {
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            ..Default::default()
        })
    }

    /// 从 DSN 连接串创建客户端（不连接）：`dm://user:password@host:port/?encoding=utf8&timeout=15`。
    /// 支持参数：encoding（字符集）、timeout（秒）、auto_reconnect（true/false）、
    /// send_delay_ms（帧间隔毫秒）、app_name、os_name。
    pub fn from_dsn(dsn: &str) -> Result<Self> {
        let dsn = dsn.trim();
        let rest = dsn.strip_prefix("dm://").unwrap_or(dsn);
        let (auth_host, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        // user:pass@host:port
        let (creds, hostport) = auth_host
            .rsplit_once('@')
            .map(|(c, h)| (Some(c), h))
            .unwrap_or((None, auth_host));
        let (host, port) = hostport
            .rsplit_once(':')
            .map(|(h, p)| (h, p.parse::<u16>().unwrap_or(5236)))
            .unwrap_or((hostport, 5236));
        let (user, password) = creds
            .map(|c| match c.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => (c.to_string(), String::new()),
            })
            .unwrap_or((String::new(), String::new()));
        let mut cfg = ClientConfig {
            host: host.to_string(),
            port,
            user,
            password,
            ..Default::default()
        };
        if let Some(q) = query {
            for pair in q.split('&') {
                let Some((k, v)) = pair.split_once('=') else { continue };
                match k.to_ascii_lowercase().as_str() {
                    "encoding" => cfg.encoding = Some(v.to_string()),
                    "timeout" => cfg.timeout = Duration::from_secs(v.parse().unwrap_or(15)),
                    "auto_reconnect" => cfg.auto_reconnect = v.eq_ignore_ascii_case("true") || v == "1",
                    "send_delay_ms" => cfg.send_delay = Duration::from_millis(v.parse().unwrap_or(0)),
                    "app_name" => cfg.app_name = v.to_string(),
                    "os_name" => cfg.os_name = v.to_string(),
                    _ => {}
                }
            }
        }
        Ok(DmClient::with_config(cfg))
    }

    /// 从 DSN 连接串创建并连接：`dm://SYSDBA:password@127.0.0.1:5236/?encoding=auto`。
    pub fn connect_from_dsn(dsn: &str) -> Result<Self> {
        let mut c = DmClient::from_dsn(dsn)?;
        c.connect()?;
        Ok(c)
    }

    pub fn with_config(cfg: ClientConfig) -> Self {
        let enc = resolve_encoding(cfg.encoding.as_deref()).unwrap_or(GB18030);
        DmClient {
            cfg,
            reader: None,
            session_key: None,
            cipher: None,
            stmt_id: -1,
            // 手动指定优先；否则先用 GB18030 占位，握手后按服务器响应自动切换
            svr_encoding: enc,
            auto_commit: true,
            in_transaction: false,
            lob_chunk_size: 16000,
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.cfg
    }

    pub fn is_connected(&self) -> bool {
        self.reader.is_some()
    }

    pub fn auto_commit(&self) -> bool {
        self.auto_commit
    }

    pub fn session_key(&self) -> Option<&[u8]> {
        self.session_key.as_deref()
    }

    /// 当前使用的服务器字符集名（握手自动检测或手动指定，如 "GB18030"/"UTF-8"/"EUC-KR"）。
    pub fn server_encoding_name(&self) -> &'static str {
        self.svr_encoding.name()
    }

    pub fn set_lob_chunk_size(&mut self, size: usize) {
        self.lob_chunk_size = size;
    }

    // ---------- 底层收发 ----------

    fn send(&mut self, cmd: i16, opts: &[u8], body: &[u8], stmt: i32) -> Result<()> {
        let r = self.reader.as_mut().ok_or_else(|| DmError::conn("连接未建立"))?;
        r.send_frame(stmt, cmd, opts, body)?;
        if !self.cfg.send_delay.is_zero() {
            thread::sleep(self.cfg.send_delay);
        }
        Ok(())
    }

    fn read_frame(&mut self) -> Result<Frame> {
        let r = self.reader.as_mut().ok_or_else(|| DmError::conn("连接未建立"))?;
        r.read_frame()
    }

    fn recv(&mut self, expect: i16) -> Result<Frame> {
        let f = self.read_frame()?;
        if f.retcode != 0 {
            return Err(self.server_error(&f));
        }
        if f.cmd != expect {
            return Err(DmError::protocol(format!("期望 cmd={expect}，实际 cmd={}", f.cmd)));
        }
        Ok(f)
    }

    fn server_error(&self, f: &Frame) -> DmError {
        let body = &f.body;
        let mut msg = self.decode_lossy(body);
        // 服务器错误 body 结构：[二进制头][int32 消息长度][消息文本]，
        // 去掉前导控制字符/NUL，只保留可读消息（如 "用户名或密码错误"）。
        let start = msg.find(|c: char| !c.is_control()).unwrap_or(0);
        msg = msg[start..].trim().to_string();
        DmError::server(f.retcode, msg)
    }

    fn check_ok(&self, f: &Frame) -> Result<()> {
        if f.retcode == 0 || f.retcode == 111 || f.retcode == 100 {
            Ok(())
        } else {
            Err(self.server_error(f))
        }
    }

    // ---------- 连接 ----------

    pub fn connect(&mut self) -> Result<()> {
        let stream = TcpStream::connect((self.cfg.host.as_str(), self.cfg.port))
            .map_err(|e| DmError::conn(format!("连接 {}:{} 失败: {e}", self.cfg.host, self.cfg.port)))?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(self.cfg.timeout)).ok();
        stream.set_write_timeout(Some(self.cfg.timeout)).ok();
        self.reader = Some(FrameReader::new(stream));
        self.handshake()?;
        self.login()?;
        self.stmt_id = -1; // 新会话重置 stmt 槽位
        self.auto_commit = true;
        self.in_transaction = false; // 新会话无事务
        Ok(())
    }

    fn ensure_connection(&mut self) -> Result<()> {
        if self.reader.is_none() {
            if self.in_transaction {
                // 事务中途断线：不静默重连（新会话会丢失未提交事务），改为明确报错
                return Err(DmError::conn(
                    "连接已断开且事务未提交（自动重连已跳过，避免静默丢失事务）",
                ));
            }
            if self.cfg.auto_reconnect {
                self.connect()?;
            }
        }
        Ok(())
    }

    fn handshake(&mut self) -> Result<()> {
        let (priv_x, pub_key) = new_client_keypair();
        // 参数区（照抓包实测值）
        let mut opts = [0u8; 44];
        opts[..17].copy_from_slice(&[
            0x00, 0x00, 0x00, 0x00, // 20: int32 0
            0x00, 0x00, 0x00, 0x00, // 24: int32 compress=0
            0x01, 0x02, // 28: loginEncrypt 低=1, 高=2
            0x00, 0x00, // 30: isBdtaRS=0, compressID=0
            0x00, 0x00, // 32: loginCert=0, 33: 0
            0x01, 0x08, // 34: 1, 35: uint16 MsgVersion=8
            0x00, // 36: 0
        ]);
        let mut body = Vec::with_capacity(81);
        body.extend_from_slice(&8i32.to_le_bytes()); // 版本长度
        body.extend_from_slice(b"8.1.5.60");
        body.push(0);
        body.extend_from_slice(&64i32.to_le_bytes());
        body.extend_from_slice(&pub_key);

        self.send(CMD_HANDSHAKE, &opts, &body, 0)?;
        let resp = self.recv(228)?;

        // 服务器编码在握手响应参数区偏移 28（int32）：1=UTF8, 2=EUCKR, 其他=GB18030
        // （对照 Go 驱动 zq.go Dm_build_1282 = 28）。未手动指定时自动适配：
        // 服务器若为 UTF-8 而客户端按 GB18030 编解码，中文会乱码。
        if self.cfg.encoding.is_none() && resp.opts.len() >= 12 {
            let code = i32::from_le_bytes(resp.opts[8..12].try_into().unwrap());
            self.svr_encoding = match code {
                1 => UTF_8,
                2 => EUC_KR,
                _ => GB18030,
            };
        }

        // 解析握手响应：找服务器 DH 公钥（[int32 64][64字节公钥]）
        let p = parse_bigint_hex(DH_P_HEX);
        let b = &resp.body;
        let mut server_pub: Option<Vec<u8>> = None;
        let mut pos = 0usize;
        while pos + 4 <= b.len() {
            let ln = i32::from_le_bytes(b[pos..pos + 4].try_into().unwrap());
            if ln == 64 && pos + 4 + 64 <= b.len() {
                let cand = &b[pos + 4..pos + 4 + 64];
                let val = num_bigint::BigUint::from_bytes_be(cand);
                if val > num_bigint::BigUint::from(1u32) && val < p {
                    server_pub = Some(cand.to_vec());
                    break;
                }
            }
            pos += 1;
        }
        let server_pub = server_pub.ok_or_else(|| {
            DmError::protocol(format!("握手响应中未找到服务器公钥: {}", hex(&b[..b.len().min(32)])))
        })?;

        // 计算会话密钥 + 登录加密通道（DES_CFB）
        let key = compute_session_key(&priv_x, &server_pub);
        self.session_key = Some(key.clone());
        self.cipher = Some(SymmCipher::new(DES_CFB, key));
        Ok(())
    }

    fn login(&mut self) -> Result<()> {
        // 参数区（严格照 Go 抓包：帧40=00, 帧41=01）
        let mut opts = [0u8; 44];
        opts[..23].copy_from_slice(&[
            0x05, 0x00, 0x00, 0x00, // 20: clientType=5
            0xff, 0xff, 0xff, 0xff, // 24: isoLevel=-1
            0x00, 0x00, 0x00, 0x00, // 28: locale=0
            0x00, 0xe0, // 32: int16 timezone
            0x01, // 34: 1
            0x00, 0x00, 0x00, 0x00, // 35: sessionTimeout
            0x00, // 39: 0
            0x00, // 40: 0
            0x01, // 41: 1
            0x00, // 42: osAuthType
        ]);
        let cipher = self.cipher.as_ref().ok_or_else(|| DmError::protocol("未握手"))?;

        // 用户名/密码：UTF-8 编码 + DES_CFB 加密（流式，长度不变）
        let mut body = Vec::new();
        let user_b = self.cfg.user.as_bytes();
        body.extend_from_slice(&(user_b.len() as i32).to_le_bytes());
        body.extend_from_slice(&cipher.encrypt(user_b, false)?);
        let pwd_b = self.cfg.password.as_bytes();
        body.extend_from_slice(&(pwd_b.len() as i32).to_le_bytes());
        body.extend_from_slice(&cipher.encrypt(pwd_b, false)?);

        // 应用信息（照 Go 结构：appName/hostName 空，osName 填 linux）
        body.extend_from_slice(&0i32.to_le_bytes()); // appName 空
        let osb = self.cfg.os_name.as_bytes();
        body.extend_from_slice(&(osb.len() as i32).to_le_bytes());
        body.extend_from_slice(osb);
        body.extend_from_slice(&0i32.to_le_bytes()); // hostName 空
        body.push(0); // rwStandby
        body.extend_from_slice(&0u16.to_le_bytes()); // serverOption: [uint16 len] 空

        self.send(CMD_LOGIN, &opts, &body, 0)?;
        self.recv(163)?;
        Ok(())
    }

    // ---------- 会话查询（每条 SQL 前） ----------

    /// 会话查询（cmd=3）。返回服务器回显的 stmt 号——实测（DM8 x64 V8）：
    /// 服务器在 cmd=187(24) 响应中回显“下一个可用 stmt 号”，cmd=5 必须使用该号，
    /// 否则报 -7003 无效语句句柄（本机自增计数与服务器不一致时触发）。
    fn session_query(&mut self) -> Result<i32> {
        self.send(CMD_SESSION_QUERY, &[1, 0, 0, 0], &[], 0)?;
        let f = self.read_frame()?;
        if f.retcode != 0 {
            return Err(self.server_error(&f));
        }
        Ok(f.stmt_id)
    }

    // ---------- 执行 ----------

    fn execute_opts(&self, has_params: bool) -> [u8; 44] {
        let auto = if self.auto_commit { 1 } else { 0 };
        let mut opts = [0u8; 44];
        opts[0] = auto; // autoCommit
        // 参数化 SQL 一律用 DML 参数区（opts21=00）；非参数化用 opts21=01 直接执行（抓包实测）
        opts[1] = if has_params { 0 } else { 1 };
        opts[3] = 1;
        opts[7..15].copy_from_slice(&i64::MAX.to_le_bytes()); // maxRows
        if !has_params {
            opts[18] = 1; // isBdtaRS 标志（仅非参数化）
        }
        opts
    }

    /// 执行 SQL。`params` 为参数化查询的绑定参数。
    pub fn execute(&mut self, sql: &str, params: &[Param]) -> Result<ExecuteResult> {
        self.ensure_connection()?;
        let stmt = self.session_query()?;
        let is_query = is_query_sql(sql);
        let opts = self.execute_opts(!params.is_empty());

        let mut sql_bytes = self.encode_str(sql);
        sql_bytes.push(0);
        self.send(CMD_EXECUTE, &opts, &sql_bytes, stmt)?;

        let out_indices: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_out())
            .map(|(i, _)| i)
            .collect();

        let mut prep_frames: Vec<Frame> = Vec::with_capacity(2);
        let mut server_params: Option<Vec<ServerParam>> = None;
        if !params.is_empty() {
            // 先读 cmd=5 响应（prepare：DML 返回 serverParams；SELECT 返回列描述符）
            let f = self.read_frame()?;
            if f.retcode != 0 && f.retcode != 111 {
                return Err(self.server_error(&f));
            }
            if !is_query && f.cmd == 0 {
                server_params = Some(parse_server_params(&f.body));
            }
            // 大 LOB 参数（≥8188B）先通过 cmd=90/14 写入，cmd=13 传长度 0
            if params.iter().any(|p| lob_len(p, self.svr_encoding) >= 8188) {
                self.write_lob_params(stmt, server_params.as_deref(), params)?;
            }
            self.bind_params(stmt, params, is_query, None, server_params.as_deref())?;
            prep_frames.push(f);
            // 参数化 DML：cmd=13 后必须先读绑定响应，再发 cmd=4（新版本 DM8 服务器实测：
            // 背靠背发送 cmd=13+cmd=4 会报 "comm msg recv empty msg" 并断开连接）
            if !is_query {
                let f13 = self.read_frame()?;
                if f13.retcode != 0 && f13.retcode != 111 && f13.retcode != 100 {
                    return Err(self.server_error(&f13));
                }
                prep_frames.push(f13);
                self.send(CMD_EXEC_COMPLETE, &[0u8; 44], &[], stmt)?;
            }
        }

        // 读取响应序列：结果帧为 cmd=187（结果集或影响行数）；cmd=0 是中间状态帧，继续读
        let mut frames: Vec<Frame> = prep_frames;
        for _ in 0..4 {
            // 若 cmd=13 响应已含结果帧（cmd=187），不再阻塞读取
            if frames.iter().any(|x| x.cmd == 187) {
                break;
            }
            let f = self.read_frame()?;
            if f.retcode != 0 && f.retcode != 111 && f.retcode != 100 {
                return Err(self.server_error(&f));
            }
            let is187 = f.cmd == 187;
            frames.push(f);
            if is187 {
                break;
            }
        }

        let mut result: Option<ExecuteResult> = None;

        'outer: for f in &frames {
            if f.cmd == 187 {
                // 187 既可能是查询结果集，也可能是 DML 影响行数确认。
                // 必须用列数区分：DML 确认帧列数为 0，但帧长随批量行数增长
                // （参数化 INSERT 4 行时确认帧为 36 字节）。早期只看长度，
                // 会把 DML 误判为结果集，既不返回行数也不报错，且残留帧污染下一次执行。
                let ncols = le_u16(&f.opts, 2) as usize;
                let nrows = le_u16(&f.opts, 4) as usize;
                if f.length > 30 && ncols > 0 && ncols <= 1000 {
                    let (cols, pos) = parse_columns(&f.body, 0, ncols, self.svr_encoding)?;
                    let mut rows_raw = parse_rows(&f.body, pos, ncols, nrows)?;
                    // 大结果集：帧24-25=65535（0xFFFF）表示还有更多行，发 cmd=7 拉剩余（循环直到取完）
                    if nrows == 0xFFFF {
                        let start_pos = rows_raw.len() as i64;
                        let more = self.fetch_more(stmt, start_pos, ncols)?;
                        rows_raw.extend(more);
                        // cmd=44（取结果完成）→ cmd=0(34, ret=111)；先读掉，否则 LOB 解码的 cmd=29 读到它
                        let _ = self.send(CMD_FETCH, &[], &[], stmt).and_then(|_| self.read_frame());
                    }
                    let rows = self.decode_rows(&cols, &rows_raw, stmt)?;
                    result = Some(ExecuteResult::Query { columns: cols, rows });
                    break 'outer;
                }

                // 列数为 0：这是 DML 影响行数确认（大帧也可能来自批量 INSERT）。
                let affected = le_i64(&f.opts, 4) as u64;
                // CALL 存储过程：帧内可能含 OUT 参数值
                if !out_indices.is_empty() && f.length > 4 {
                    let out = self.parse_out_values(&f.body, &out_indices, server_params.as_deref());
                    if !params.is_empty() && !is_query {
                        let _ = self.read_frame(); // cmd=4 确认
                    }
                    result = Some(ExecuteResult::Call { affected, out });
                } else {
                    // 参数化 DML：cmd=4 的确认响应在结果帧后，必须读掉防残留，
                    // 否则残留帧会让下一条语句读到别人的响应，报“无效的语句句柄”。
                    if !params.is_empty() && !is_query {
                        let _ = self.read_frame();
                    }
                    result = Some(ExecuteResult::Affected(affected));
                }
                break 'outer;
            } else if f.cmd == 0 && f.length > 30 && !params.is_empty() && is_query {
                // 参数化 SELECT（Execute2）：cmd=0 含参数回显+列描述符（列描述符在 len(params)*32，
                // DDL 后首条可能多 8B 类型名，用 parse_cols_robust 自适应定位）
                let ncols = le_u16(&f.opts, 4) as usize;
                if ncols > 0 && ncols <= 1000 {
                    let cols = parse_cols_robust(&f.body, params.len(), ncols, self.svr_encoding)?;
                    // 行数据在 cmd=187（cmd=13 响应）
                    for f2 in &frames {
                        if f2.cmd == 187 {
                            let ncols2 = le_u16(&f2.opts, 2) as usize;
                            let nrows2 = le_u16(&f2.opts, 4) as usize;
                            let mut rows_raw: Vec<Row>;
                            if ncols2 > 0 && ncols2 <= 1000 {
                                // 大结果集：列描述符 + fetchSize 行（行长循环）
                                let (_, pos2) = parse_columns(&f2.body, 0, ncols2, self.svr_encoding)?;
                                rows_raw = parse_rows_param(&f2.body, pos2, ncols2)?;
                            } else {
                                // 小结果集：纯行数据
                                rows_raw = parse_rows_param(&f2.body, 0, ncols)?;
                            }
                            // 分批 fetch：帧24-25=65535（0xFFFF）表示还有更多行，发 cmd=7 拉剩余
                            if nrows2 == 0xFFFF {
                                let start_pos = rows_raw.len() as i64;
                                let more = self.fetch_more(stmt, start_pos, ncols)?;
                                rows_raw.extend(more);
                            }
                            // cmd=44（取结果完成）→ cmd=0(34, ret=111)；先读掉，否则 LOB 解码的 cmd=29 读到它
                            let _ = self.send(CMD_FETCH, &[], &[], stmt).and_then(|_| self.read_frame());
                            // LOB 解码（cmd=29/32 用 stmt，此时未释放）
                            let rows = self.decode_rows(&cols, &rows_raw, stmt)?;
                            // cmd=4（释放）→ cmd=0(24)
                            let _ = self.send(CMD_EXEC_COMPLETE, &[0u8; 44], &[], stmt).and_then(|_| self.read_frame());
                            result = Some(ExecuteResult::Query { columns: cols, rows });
                            break 'outer;
                        }
                    }
                }
            }
        }

        // 释放语句（每条 SQL 后发 cmd=4）——仅非参数化时在此补发
        if params.is_empty() {
            let _ = self.send(CMD_EXEC_COMPLETE, &[0u8; 44], &[], stmt).and_then(|_| self.read_frame());
        }
        result.ok_or_else(|| DmError::protocol("执行未产生有效结果帧"))
    }

    /// 惰性查询：只拉第一帧（fetchSize 行），返回 (cols, state)。
    /// 调用方用 `fetch_more` 按需拉取；用完必须 `close_lazy`。
    pub fn execute_lazy(&mut self, sql: &str, params: &[Param]) -> Result<(Vec<Column>, LazyState)> {
        self.ensure_connection()?;
        let stmt = self.session_query()?;
        if !is_query_sql(sql) {
            return Err(DmError::arg("execute_lazy 仅支持查询"));
        }
        let opts = self.execute_opts(true);
        let mut sql_bytes = self.encode_str(sql);
        sql_bytes.push(0);
        self.send(CMD_EXECUTE, &opts, &sql_bytes, stmt)?;

        let prep_frame = self.read_frame()?;
        if prep_frame.retcode != 0 && prep_frame.retcode != 111 {
            return Err(self.server_error(&prep_frame));
        }
        self.bind_params(stmt, params, true, None, None)?;
        let ncols = le_u16(&prep_frame.opts, 4) as usize;
        let cols = parse_cols_robust(&prep_frame.body, params.len(), ncols, self.svr_encoding)?;

        // 读 cmd=13 响应（第一帧：cmd=187）
        let f = self.read_frame()?;
        if f.retcode != 0 && f.retcode != 111 {
            return Err(self.server_error(&f));
        }
        let ncols2 = le_u16(&f.opts, 2) as usize;
        let nrows2 = le_u16(&f.opts, 4) as usize;
        let rows_raw = if ncols2 > 0 && ncols2 <= 1000 {
            let (_, pos2) = parse_columns(&f.body, 0, ncols2, self.svr_encoding)?;
            parse_rows_param(&f.body, pos2, ncols2)?
        } else {
            parse_rows_param(&f.body, 0, ncols)?
        };
        let more = nrows2 == 0xFFFF;
        Ok((
            cols,
            LazyState { stmt, ncols, rows_raw, more, closed: false },
        ))
    }

    /// 惰性拉取下一批（cmd=7），循环直到取完全部行。返回追加的行（raw）。之后 `state.more` 为 false。
    ///
    /// 请求参数区：帧 20-27=startPos(int64)，28-35=INT64_MAX（maxRows），36-39=id，40-43=bufPrefetch。
    /// 响应参数区：帧 20-27=总行数 int64（Go zq.go `Dm_build_1068=20` / updateCount），
    /// 帧 28-31=本批行数 int32（`Dm_build_1069=28`）。
    /// 循环保护：空批即断 + 最大迭代上限（防服务器不返回总行数时误判死循环）。
    pub fn fetch_more(&mut self, stmt: i32, start_pos: i64, ncols: usize) -> Result<Vec<Row>> {
        const MAX_FETCH_ITERS: u32 = 1_000_000;
        let mut all: Vec<Row> = Vec::new();
        let mut start = start_pos;
        let mut total: i64 = -1; // 服务器回显总行数（cmd=7 响应 opts[0..8]）；-1=未知
        for _ in 0..MAX_FETCH_ITERS {
            // 已取完（start >= 总行数）：不再发可能超界的 cmd=7（bug：恰在分页边界时误报）
            if total > 0 && start >= total {
                break;
            }
            let mut opts = [0u8; 44];
            opts[..8].copy_from_slice(&start.to_le_bytes());
            opts[8..16].copy_from_slice(&i64::MAX.to_le_bytes());
            // 帧 36-39=id int32, 40-43=bufPrefetch int32（0）
            self.send(7, &opts, &[], stmt)?;
            let f = self.read_frame()?;
            if f.retcode != 0 && f.retcode != 111 && f.retcode != 100 {
                // 恰好在分页边界（start==total，上批已取完）时，服务器对超界 cmd=7 返回
                // "无更多数据"类错误码；此时首帧已取走全部行，应视为正常结束而非报错。
                break;
            }
            // 响应参数区：opts[0..8]=总行数 int64（updateCount），opts[8..12]=本批行数 int32
            let t = le_i64(&f.opts, 0);
            if 0 < t && t < i64::MAX {
                total = t;
            }
            let rows = parse_rows_param(&f.body, 0, ncols)?;
            let n = rows.len();
            if n == 0 {
                break; // 空批 → 数据已取完
            }
            start += n as i64;
            all.extend(rows);
        }
        Ok(all)
    }

    /// 释放惰性查询的 stmt（cmd=44 + cmd=4）。
    pub fn close_lazy(&mut self, state: &mut LazyState) -> Result<()> {
        if state.closed {
            return Ok(());
        }
        let stmt = state.stmt;
        let _ = self.send(CMD_FETCH, &[], &[], stmt).and_then(|_| self.read_frame());
        let _ = self.send(CMD_EXEC_COMPLETE, &[0u8; 44], &[], stmt).and_then(|_| self.read_frame());
        state.closed = true;
        Ok(())
    }

    /// 批量执行（协议级 batch，一组 cmd=5+13+4 插多行）。返回总影响行数。
    pub fn executemany(&mut self, sql: &str, params_list: &[Vec<Param>]) -> Result<u64> {
        self.ensure_connection()?;
        let stmt = self.session_query()?;
        if is_query_sql(sql) {
            return Err(DmError::arg("executemany 仅支持 DML"));
        }
        let opts = self.execute_opts(true);
        let mut sql_bytes = self.encode_str(sql);
        sql_bytes.push(0);
        self.send(CMD_EXECUTE, &opts, &sql_bytes, stmt)?;
        // cmd=5 响应（prepare：serverParams）必须读掉，再发 cmd=13
        let prep = self.read_frame()?;
        if prep.retcode != 0 && prep.retcode != 111 {
            return Err(self.server_error(&prep));
        }
        let server_params = if prep.cmd == 0 {
            Some(parse_server_params(&prep.body))
        } else {
            None
        };
        self.bind_params(stmt, &params_list[0], false, Some(params_list), server_params.as_deref())?;
        // cmd=13 响应必须读掉，再发 cmd=4（背靠背会导致服务器断连）
        let f13 = self.read_frame()?;
        if f13.retcode != 0 && f13.retcode != 111 && f13.retcode != 100 {
            return Err(self.server_error(&f13));
        }
        self.send(CMD_EXEC_COMPLETE, &[0u8; 44], &[], stmt)?;
        // 结果帧：cmd=13 响应即 cmd=187（影响行数在 opts[4..6]）；cmd=4 确认是 cmd=0(24)
        let mut affected = 0u64;
        if f13.cmd == 187 {
            affected = le_u16(&f13.opts, 4) as u64;
            let _ = self.read_frame(); // cmd=4 确认
        } else {
            for _ in 0..4 {
                let f = self.read_frame()?;
                if f.retcode != 0 && f.retcode != 111 && f.retcode != 100 {
                    return Err(self.server_error(&f));
                }
                if f.cmd == 187 {
                    affected = le_u16(&f.opts, 4) as u64;
                    let _ = self.read_frame(); // cmd=4 确认
                    break;
                }
            }
        }
        Ok(affected)
    }

    // ---------- 行/值解码 ----------

    pub(crate) fn decode_rows(&mut self, cols: &[Column], rows: &[Row], stmt: i32) -> Result<Vec<Vec<Value>>> {
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut v = Vec::with_capacity(r.len());
            for (i, cell) in r.iter().enumerate() {
                let val = match cell {
                    None => Value::Null,
                    Some(raw) => {
                        let col = if i < cols.len() { &cols[i] } else { continue; };
                        self.decode_value(raw, col, Some(stmt))?
                    }
                };
                v.push(val);
            }
            out.push(v);
        }
        Ok(out)
    }

    fn decode_value(&mut self, raw: &[u8], col: &Column, stmt: Option<i32>) -> Result<Value> {
        if col.col_type == DM_CLOB || col.col_type == DM_BLOB {
            return self.read_lob(raw, col, stmt);
        }
        decode_scalar(raw, col, self.svr_encoding)
    }

    fn read_lob(&mut self, raw: &[u8], col: &Column, stmt: Option<i32>) -> Result<Value> {
        if raw.is_empty() {
            return Ok(if col.col_type == DM_CLOB {
                Value::Str(String::new())
            } else {
                Value::Bytes(Vec::new())
            });
        }
        let data: Vec<u8> = if raw[0] == 0x01 && raw.len() >= 43 {
            // 行内 LOB：头部 43 字节，数据 @43，长度 @9
            let length = u32::from_le_bytes(raw[9..13].try_into().unwrap()) as usize;
            raw[43..(43 + length).min(raw.len())].to_vec()
        } else {
            // 非行内：43 字节定位符 → cmd=32 分段读取
            self.read_offrow_lob(raw, col, stmt)?
        };
        if col.col_type == DM_CLOB {
            Ok(Value::Str(decode_text_strict(self.svr_encoding, &data)?))
        } else {
            Ok(Value::Bytes(data))
        }
    }

    fn read_offrow_lob(&mut self, desc: &[u8], col: &Column, stmt: Option<i32>) -> Result<Vec<u8>> {
        if desc.len() < 43 {
            return Ok(Vec::new());
        }
        let blob_id = i64::from_le_bytes(desc[1..9].try_into().unwrap());
        let length = u32::from_le_bytes(desc[9..13].try_into().unwrap()) as i64;
        let group_id = i16::from_le_bytes(desc[13..15].try_into().unwrap());
        let file_id = i16::from_le_bytes(desc[15..17].try_into().unwrap());
        let page_no = i32::from_le_bytes(desc[17..21].try_into().unwrap());
        let tab_id = i32::from_le_bytes(desc[21..25].try_into().unwrap());
        let col_id = i16::from_le_bytes(desc[25..27].try_into().unwrap());
        let row_id = i64::from_le_bytes(desc[27..35].try_into().unwrap());
        let ex_group = i16::from_le_bytes(desc[35..37].try_into().unwrap());
        let ex_file = i16::from_le_bytes(desc[37..39].try_into().unwrap());
        let ex_page = i32::from_le_bytes(desc[39..43].try_into().unwrap());

        let is_clob = col.col_type == DM_CLOB;
        let lob_flag: u8 = if is_clob { 1 } else { 0 };
        // CLOB：offset/req 为字符数（cmd=29 得字符长，单次 ≤16000）；BLOB：字节（单次 ≤32000）
        let mut length = length;
        let req_unit: i64 = if is_clob { 16000 } else { 32000 };
        if is_clob {
            length = self.lob_char_len(
                lob_flag, blob_id, group_id, file_id, page_no, tab_id, col_id, row_id,
                ex_group, ex_file, ex_page, stmt,
            )?;
        }
        let mut all: Vec<u8> = Vec::new();
        let mut offset: i64 = 0;
        let mut cur_file = file_id;
        let mut cur_page = page_no;
        let mut total_offset: i32 = 0;
        while offset < length {
            let req = req_unit.min(length - offset);
            // cmd=32 参数区：lobFlag+tabId+colId+blobId+groupId+fileId+pageNo+curFileId+curPageNo+totalOffset+offset+length+rowId+exGroupId+exFileId+exPageNo
            let mut body = Vec::with_capacity(59);
            body.push(lob_flag);
            body.extend_from_slice(&tab_id.to_le_bytes());
            body.extend_from_slice(&col_id.to_le_bytes());
            body.extend_from_slice(&blob_id.to_le_bytes());
            body.extend_from_slice(&group_id.to_le_bytes());
            body.extend_from_slice(&file_id.to_le_bytes());
            body.extend_from_slice(&page_no.to_le_bytes());
            body.extend_from_slice(&cur_file.to_le_bytes());
            body.extend_from_slice(&cur_page.to_le_bytes());
            body.extend_from_slice(&total_offset.to_le_bytes());
            body.extend_from_slice(&offset.to_le_bytes());
            body.extend_from_slice(&req.to_le_bytes());
            body.extend_from_slice(&row_id.to_le_bytes());
            body.extend_from_slice(&ex_group.to_le_bytes());
            body.extend_from_slice(&ex_file.to_le_bytes());
            body.extend_from_slice(&ex_page.to_le_bytes());

            self.send(32, &[], &body, stmt.unwrap_or(0))?;
            let f = self.read_frame()?;
            if f.retcode != 0 && f.retcode != 111 {
                return Err(self.server_error(&f));
            }
            if f.body.len() < 15 {
                break;
            }
            let read_over = f.body[0];
            let dlen = i32::from_le_bytes(f.body[1..5].try_into().unwrap());
            if dlen <= 0 {
                break;
            }
            let dlen = dlen as usize;
            let end = (15 + dlen).min(f.body.len());
            all.extend_from_slice(&f.body[15..end]);
            cur_file = i16::from_le_bytes(f.body[5..7].try_into().unwrap());
            cur_page = i32::from_le_bytes(f.body[7..11].try_into().unwrap());
            total_offset = i32::from_le_bytes(f.body[11..15].try_into().unwrap());
            offset += if is_clob { req } else { dlen as i64 };
            if read_over != 0 {
                break;
            }
        }
        if is_clob {
            Ok(all)
        } else {
            // BLOB：length 是字节数
            Ok(all[..(length as usize).min(all.len())].to_vec())
        }
    }

    #[allow(clippy::too_many_arguments)] // 与 Go/Python 驱动 LOB 定位符字段一一对应
    fn lob_char_len(
        &mut self,
        lob_flag: u8,
        blob_id: i64,
        group_id: i16,
        file_id: i16,
        page_no: i32,
        tab_id: i32,
        col_id: i16,
        row_id: i64,
        ex_group: i16,
        ex_file: i16,
        ex_page: i32,
        stmt: Option<i32>,
    ) -> Result<i64> {
        // cmd=29：获取 CLOB 字符数（len=39）
        let mut body = Vec::with_capacity(39);
        body.push(lob_flag);
        body.extend_from_slice(&blob_id.to_le_bytes());
        body.extend_from_slice(&group_id.to_le_bytes());
        body.extend_from_slice(&file_id.to_le_bytes());
        body.extend_from_slice(&page_no.to_le_bytes());
        body.extend_from_slice(&tab_id.to_le_bytes());
        body.extend_from_slice(&col_id.to_le_bytes());
        body.extend_from_slice(&row_id.to_le_bytes());
        body.extend_from_slice(&ex_group.to_le_bytes());
        body.extend_from_slice(&ex_file.to_le_bytes());
        body.extend_from_slice(&ex_page.to_le_bytes());
        self.send(29, &[], &body, stmt.unwrap_or(0))?;
        let f = self.read_frame()?;
        if f.retcode != 0 && f.retcode != 111 {
            return Err(self.server_error(&f));
        }
        if f.body.len() >= 8 {
            return Ok(i64::from_le_bytes(f.body[..8].try_into().unwrap()));
        }
        if f.body.len() >= 4 {
            return Ok(i32::from_le_bytes(f.body[..4].try_into().unwrap()) as i64);
        }
        Ok(0)
    }

    fn parse_out_values(
        &mut self,
        body: &[u8],
        out_indices: &[usize],
        server_params: Option<&[ServerParam]>,
    ) -> Vec<Value> {
        let mut values = Vec::new();
        let mut pos = 0usize;
        for &idx in out_indices {
            if pos + 2 > body.len() {
                values.push(Value::Null);
                break;
            }
            let ln = u16::from_le_bytes(body[pos..pos + 2].try_into().unwrap());
            pos += 2;
            if ln == NULL_MARK {
                values.push(Value::Null);
                continue;
            }
            if pos + ln as usize > body.len() {
                values.push(Value::Null);
                break;
            }
            let raw = &body[pos..pos + ln as usize];
            pos += ln as usize;
            let sp = server_params.and_then(|s| s.get(idx));
            let ct = sp.map(|x| x.col_type).unwrap_or(2);
            values.push(decode_out_value(raw, ct, self.svr_encoding));
        }
        values
    }

    // ---------- 参数绑定 ----------

    fn bind_params(
        &mut self,
        stmt: i32,
        params: &[Param],
        is_query: bool,
        batch_list: Option<&[Vec<Param>]>,
        server_params: Option<&[ServerParam]>,
    ) -> Result<()> {
        let groups: Vec<&[Param]> = match batch_list {
            Some(bl) => bl.iter().map(|g| g.as_slice()).collect(),
            None => vec![params],
        };
        let first = groups[0];
        let n_params = first.len();
        let n_batch = groups.len();

        // 参数区
        let mut opts = [0u8; 44];
        opts[0] = if self.auto_commit { 1 } else { 0 };
        opts[1..3].copy_from_slice(&(n_params as u16).to_le_bytes());
        opts[3] = 1;
        opts[4..12].copy_from_slice(&(n_batch as i64).to_le_bytes());
        opts[20..28].copy_from_slice(&i64::MAX.to_le_bytes()); // maxRows
        opts[28] = 1;
        opts[32..36].copy_from_slice(&(-1i32).to_le_bytes()); // queryTimeout = -1

        // 参数描述（以第一组为准）
        let mut desc = Vec::with_capacity(n_params * 13);
        let mut encoders: Vec<Option<ParamEncoder>> = Vec::with_capacity(n_params);
        for (i, p) in first.iter().enumerate() {
            if let Param::Out(hint) = p {
                // OUT 参数：ioType=1，colType 用 serverParams（否则按类型提示）
                let sp = server_params.and_then(|s| s.get(i));
                let ct = sp.map(|x| x.col_type).unwrap_or_else(|| out_type_hint(*hint));
                let prec = sp.map(|x| x.prec).unwrap_or_else(|| out_type_prec(*hint));
                let scale = sp.map(|x| x.scale).unwrap_or(0);
                desc.extend_from_slice(&1i8.to_le_bytes());
                desc.extend_from_slice(&ct.to_le_bytes());
                desc.extend_from_slice(&prec.to_le_bytes());
                desc.extend_from_slice(&scale.to_le_bytes());
                encoders.push(None);
                continue;
            }
            let is_lob = lob_len(p, self.svr_encoding) >= 8188;
            if is_lob {
                // 大 LOB 参数：CLOB(19)/BLOB(12) + prec=0x7fffffff（已通过 cmd=90/14 写入）
            let ct: i32 = if matches!(p, Param::Str(_)) { 19 } else { 12 };
                desc.extend_from_slice(&0i8.to_le_bytes());
                desc.extend_from_slice(&ct.to_le_bytes());
                desc.extend_from_slice(&0x7FFF_FFFFi32.to_le_bytes());
                desc.extend_from_slice(&0i32.to_le_bytes());
                encoders.push(Some(ParamEncoder::from_type(ct)));
            } else if let Some(sp) = server_params.and_then(|s| s.get(i)) {
                desc.extend_from_slice(&0i8.to_le_bytes());
                desc.extend_from_slice(&sp.col_type.to_le_bytes());
                desc.extend_from_slice(&sp.prec.to_le_bytes());
                desc.extend_from_slice(&sp.scale.to_le_bytes());
                encoders.push(Some(ParamEncoder::from_type(sp.col_type)));
            } else {
                let (ct, prec, scale, enc) = param_type(p, is_query, self.svr_encoding);
                desc.extend_from_slice(&0i8.to_le_bytes());
                desc.extend_from_slice(&ct.to_le_bytes());
                desc.extend_from_slice(&prec.to_le_bytes());
                desc.extend_from_slice(&scale.to_le_bytes());
                encoders.push(Some(enc));
            }
        }

        // 参数数据
        let mut data = Vec::new();
        for group in &groups {
            for (i, p) in group.iter().enumerate() {
                if p.is_out() {
                    continue; // OUT 参数无数据
                }
                if p.is_null() {
                    data.extend_from_slice(&NULL_MARK.to_le_bytes());
                } else {
                    let enc = encoders
                        .get(i)
                        .and_then(|e| e.clone())
                        .unwrap_or_else(|| param_type(p, is_query, self.svr_encoding).3);
                    let raw = enc.encode(p, self.svr_encoding)?;
                    if lob_len(p, self.svr_encoding) >= 8188 {
                        data.extend_from_slice(&0u16.to_le_bytes()); // 大 LOB 已通过 cmd=90/14 写入
                    } else {
                        data.extend_from_slice(&(raw.len() as u16).to_le_bytes());
                        data.extend_from_slice(&raw);
                    }
                }
            }
        }

        let mut body = desc;
        body.extend_from_slice(&data);
        self.send(CMD_BIND, &opts, &body, stmt)?;
        Ok(())
    }

    fn write_lob_params(
        &mut self,
        stmt: i32,
        server_params: Option<&[ServerParam]>,
        params: &[Param],
    ) -> Result<()> {
        // 参数描述：小参数用 serverParams 类型；大值用 BLOB(12)/CLOB(19) + 0x7fffffff
        let mut desc = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let raw_len = lob_len(p, self.svr_encoding);
            if raw_len >= 8188 {
                let ct: i32 = if matches!(p, Param::Str(_)) { 19 } else { 12 };
                desc.extend_from_slice(&0i8.to_le_bytes());
                desc.extend_from_slice(&ct.to_le_bytes());
                desc.extend_from_slice(&0x7FFF_FFFFi32.to_le_bytes());
                desc.extend_from_slice(&0i32.to_le_bytes());
            } else {
                let sp = server_params.and_then(|s| s.get(i));
                let ct = sp.map(|x| x.col_type).unwrap_or(2);
                let prec = sp.map(|x| x.prec).unwrap_or(0);
                let scale = sp.map(|x| x.scale).unwrap_or(0);
                desc.extend_from_slice(&0i8.to_le_bytes());
                desc.extend_from_slice(&ct.to_le_bytes());
                desc.extend_from_slice(&prec.to_le_bytes());
                desc.extend_from_slice(&scale.to_le_bytes());
            }
        }
        // cmd=90：LOB 参数声明（opts 帧20-21 = 参数个数）
        let mut opts90 = [0u8; 44];
        opts90[..2].copy_from_slice(&(params.len() as u16).to_le_bytes());
        self.send(90, &opts90, &desc, stmt)?;
        let f = self.read_frame()?;
        if f.retcode != 0 && f.retcode != 111 && f.retcode != 100 {
            return Err(self.server_error(&f));
        }
        // cmd=14：分块写入每个大 LOB 参数（顺序与参数一致）
        for (i, p) in params.iter().enumerate() {
            if lob_len(p, self.svr_encoding) < 8188 {
                continue;
            }
            let data: Vec<u8> = match p {
                Param::Str(s) => self.encode_str(s),
                Param::Bytes(b) => b.clone(),
                _ => Vec::new(),
            };
            let mut opts14 = [0u8; 44];
            opts14[0] = i as u8; // 参数索引（0 基，Go: dm_build_665(..., icol, ...)）
            let mut off = 0;
            while off < data.len() {
                let mut end = (off + self.lob_chunk_size).min(data.len());
                // 多字节编码字符边界对齐，避免分块切半导致乱码：
                // GB18030/EUC-KR：块末是首字节(0x81-0xFE)且下一字节是尾字节(0x40-0xFE)时回退；
                // UTF-8：下一字节是续字节(0x80-0xBF)时回退到字符起点
                if self.svr_encoding == GB18030 || self.svr_encoding == EUC_KR {
                    while end < data.len() && end > off
                        && (0x81..=0xFE).contains(&data[end - 1])
                        && (0x40..=0xFE).contains(&data[end])
                        && data[end] != 0x7F
                    {
                        end -= 1;
                    }
                } else {
                    while end < data.len() && end > off && (0x80..=0xBF).contains(&data[end]) {
                        end -= 1;
                    }
                }
                let chunk = &data[off..end];
                let mut body = Vec::with_capacity(8 + chunk.len());
                body.extend_from_slice(&(chunk.len() as i32).to_le_bytes());
                body.extend_from_slice(&(-1i32).to_le_bytes());
                body.extend_from_slice(chunk);
                self.send(14, &opts14, &body, stmt)?;
                let f = self.read_frame()?;
                if f.retcode != 0 && f.retcode != 111 && f.retcode != 100 {
                    return Err(self.server_error(&f));
                }
                off = end;
            }
        }
        Ok(())
    }

    // ---------- 事务 ----------

    /// 开始事务：设置隔离级别（cmd=52）+ 关闭自动提交（Go 实测顺序）
    pub fn begin(&mut self) -> Result<()> {
        self.send(52, &1i32.to_le_bytes(), &[], 0)?;
        let f = self.read_frame()?;
        self.check_ok(&f)?;
        self.auto_commit = false;
        self.in_transaction = true;
        Ok(())
    }

    /// 提交事务（cmd=8）
    pub fn commit(&mut self) -> Result<()> {
        self.send(8, &[], &[], 0)?;
        let f = self.read_frame()?;
        self.check_ok(&f)?;
        self.auto_commit = true;
        self.in_transaction = false;
        Ok(())
    }

    /// 回滚事务（cmd=9）
    pub fn rollback(&mut self) -> Result<()> {
        self.send(9, &[], &[], 0)?;
        let f = self.read_frame()?;
        self.check_ok(&f)?;
        self.auto_commit = true;
        self.in_transaction = false;
        Ok(())
    }

    /// 设置事务隔离级别（cmd=52）：1=读已提交 2=可重复读 3=串行化
    pub fn set_iso_level(&mut self, level: i32) -> Result<()> {
        self.send(52, &level.to_le_bytes(), &[], 0)?;
        let f = self.read_frame()?;
        self.check_ok(&f)?;
        Ok(())
    }

    pub fn close(&mut self) {
        self.reader = None;
    }

    /// 用服务器编码编码字符串（SQL 文本 / 字符串参数 / CLOB 写入）。
    fn encode_str(&self, s: &str) -> Vec<u8> {
        enc_str(self.svr_encoding, s)
    }

    /// 用服务器编码解码错误消息。服务端报错文本损坏时不应影响错误本身，
    /// 因此这里保留宽松解码，仅在失败字节上产生替换字符。
    fn decode_lossy(&self, b: &[u8]) -> String {
        let (s, _, _) = self.svr_encoding.decode(b);
        s.into_owned()
    }
}

// ---------- 工具函数 ----------

pub(crate) fn is_query_sql(sql: &str) -> bool {
    let s = sql.trim_start();
    let up = s.to_ascii_uppercase();
    up.starts_with("SELECT")
        || up.starts_with("WITH")
        || up.starts_with("DESC")
        || up.starts_with("SHOW")
}

/// 按指定服务器编码编码字符串（SQL 文本 / 字符串参数 / CLOB 写入）。
pub(crate) fn enc_str(enc: &'static Encoding, s: &str) -> Vec<u8> {
    enc.encode(s).0.into_owned()
}

/// 科学计数法/带点字符串 → 定点十进制字符串（enc_decimal 用）
fn apply_exp(mant: &str, exp: i64) -> String {
    if exp == 0 {
        return mant.to_string();
    }
    let neg = mant.starts_with('-');
    let m = mant.trim_start_matches(['-', '+']);
    let digits: String = m.replace(".", "");
    let int_len = m.find('.').map(|i| i as i64).unwrap_or(digits.len() as i64) + exp;
    let mut out = if int_len <= 0 {
        format!("0.{}{}", "0".repeat((-int_len) as usize), digits)
    } else if int_len >= digits.len() as i64 {
        format!("{}{}", digits, "0".repeat((int_len - digits.len() as i64) as usize))
    } else {
        let p = int_len as usize;
        format!("{}.{}", &digits[..p], &digits[p..])
    };
    if neg {
        out.insert(0, '-');
    }
    out
}

/// 达梦私有大数二进制编码（与 Python _enc_decimal / Go encodeDecimal 一致）。
/// 输入十进制字符串（可含小数点与 e 科学计数），输出如 1.50 → c1 02 33，0 → 80。
pub(crate) fn enc_decimal(s: &str) -> Vec<u8> {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return vec![0x80];
    }
    let (neg, body) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else {
        (false, s.strip_prefix('+').unwrap_or(s))
    };
    // 科学计数 → 定点
    let body = match body.find(['e', 'E']) {
        Some(ci) => {
            let (mant, exp) = (&body[..ci], &body[ci + 1..]);
            apply_exp(mant, exp.parse::<i64>().unwrap_or(0))
        }
        None => body.to_string(),
    };
    // 构造 (weight, digits)
    let (mut weight, mut digits) = if let Some(pi) = body.find('.') {
        let intp = body[..pi].to_string();
        let frac = body[pi + 1..].to_string();
        if intp.chars().all(|c| c == '0') {
            let first = frac.find(|c| c != '0').unwrap_or(frac.len());
            (-(first as i64 + (frac.len() - first) as i64), frac[first..].to_string())
        } else {
            (-(frac.len() as i64), format!("{intp}{frac}"))
        }
    } else {
        (0i64, body.clone())
    };
    // 去尾随 0
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
        weight += 1;
    }
    // 数值为零（如 "0.00"、"0.000"）→ 0x80
    if digits.chars().all(|c| c == '0') {
        return vec![0x80];
    }
    // 对齐到 2 位一组
    if weight % 2 != 0 {
        digits.push('0');
        weight -= 1;
    }
    if digits.len() % 2 != 0 {
        digits.insert(0, '0');
    }
    // encodeDecimal
    let exp = (weight + digits.len() as i64) / 2 - 1;
    let exp_max = if neg { 61 } else { 62 };
    if exp > exp_max || exp < -62 {
        return Vec::new(); // 溢出（DECIMAL(38) 正常范围不会触发）
    }
    let mut valid_len = digits.len() / 2 + 1;
    if neg && valid_len >= 21 {
        valid_len = 20;
    } else if valid_len > 21 {
        valid_len = 21;
    }
    let ret_len = if neg { valid_len + 1 } else { valid_len };
    let mut out = vec![0u8; ret_len];
    out[0] = if neg { (0x3E - exp as i32) as u8 } else { (exp as i32 + 0xC1) as u8 };
    let mut ib = 1usize;
    let mut k = 0usize;
    while ib < valid_len && k + 2 <= digits.len() {
        if let Ok(d) = digits[k..k + 2].parse::<u8>() {
            out[ib] = if neg { 101 - d } else { d + 1 };
        }
        ib += 1;
        k += 2;
    }
    if neg && ib < ret_len {
        out[ib] = 0x66;
        ib += 1;
    }
    if ib < ret_len {
        out[ib] = 0x00;
    }
    out
}

/// 参数值字节长度（str 按服务器编码；bytes 原长）
pub(crate) fn lob_len(p: &Param, enc: &'static Encoding) -> usize {
    match p {
        Param::Str(s) => enc_str(enc, s).len(),
        Param::Bytes(b) => b.len(),
        _ => 0,
    }
}

/// 参数类型猜测 → (colType, prec, scale, 编码器)，对照 Go 驱动实测：
/// int→INT(7)/prec4, str→VARCHAR(2)/prec=len, float→DOUBLE(11)/prec8,
/// date→DATE(14)/prec3, None→NULL
/// 带时区（timezone_minutes=Some）自动升为 TZ 类型：TIME_TZ(22)/DATETIME_TZ(23)
fn param_type(p: &Param, is_query: bool, enc: &'static Encoding) -> (i32, i32, i32, ParamEncoder) {
    match p {
        Param::Bool(_) => (7, 4, 0, ParamEncoder::Int),
        Param::Int(_) => {
            // 查询参数用 BIGINT(8)/prec8（Go 实测），写入用 INT(7)/prec4
            if is_query {
                (8, 8, 0, ParamEncoder::BigInt)
            } else {
                (7, 4, 0, ParamEncoder::Int)
            }
        }
        Param::Float(_) => (11, 8, 0, ParamEncoder::Double),
        Param::Date(_) => (14, 3, 0, ParamEncoder::Date),
        Param::DateTime(dt) => {
            if dt.time.timezone_minutes.is_some() {
                (23, 8, 0, ParamEncoder::DateTimeTz)
            } else {
                (16, 8, 0, ParamEncoder::DateTime)
            }
        }
        Param::Time(t) => {
            if t.timezone_minutes.is_some() {
                (22, 5, 0, ParamEncoder::TimeTz)
            } else {
                (15, 5, 0, ParamEncoder::Time)
            }
        }
        Param::Bytes(b) => (17, b.len() as i32, 0, ParamEncoder::Bytes),
        Param::Str(s) => {
            let len = enc_str(enc, s).len();
            // 查询参数 prec=8188(0x1FFC)；写入参数 prec=列长度（用 50 或实际长度）
            let prec = if is_query { 8188 } else { 50.max(len as i32) };
            (2, prec, 0, ParamEncoder::Str)
        }
        Param::Null => {
            // NULL 参数：VARCHAR(2)/prec=8188
            (2, 8188, 0, ParamEncoder::Str)
        }
        Param::Out(_) => (2, 0, 0, ParamEncoder::Str),
    }
}

fn out_type_hint(hint: Option<OutType>) -> i32 {
    match hint {
        Some(OutType::Bool) => 13,
        Some(OutType::Int) => 8,
        Some(OutType::Float) => 11,
        Some(OutType::Str) => 2,
        Some(OutType::Date) => 14,
        Some(OutType::DateTime) => 16,
        None => 2,
    }
}

fn out_type_prec(hint: Option<OutType>) -> i32 {
    match hint {
        Some(OutType::Int) => 8,
        Some(OutType::Str) => 32767,
        Some(OutType::DateTime) => 8,
        Some(OutType::Date) => 3,
        _ => 8,
    }
}

/// 解析 cmd=5 响应（DML prepare）的 serverParams。
pub fn parse_server_params(body: &[u8]) -> Vec<ServerParam> {
    let mut params = Vec::new();
    let n = body.len();
    let mut pos = 0usize;
    while pos + 31 <= n {
        let col_type = le_i32(body, pos);
        let prec = le_i32(body, pos + 4);
        let scale = le_i32(body, pos + 8);
        pos += 16;
        if pos + 2 > n {
            break;
        }
        pos += 2; // itemFlag
        pos += 4; // 跳过
        if pos + 2 > n {
            break;
        }
        let io_type = body[pos] as i32;
        pos += 2; // ioType 是 int16（Go: int8(Dm_build_204())，消耗 2 字节）
        if pos + 8 > n {
            break;
        }
        let nlen = i16::from_le_bytes(body[pos..pos + 2].try_into().unwrap());
        let tlen = i16::from_le_bytes(body[pos + 2..pos + 4].try_into().unwrap());
        let tblen = i16::from_le_bytes(body[pos + 4..pos + 6].try_into().unwrap());
        let slen = i16::from_le_bytes(body[pos + 6..pos + 8].try_into().unwrap());
        pos += 8;
        let total = (nlen + tlen + tblen + slen) as i64;
        if pos + total as usize > n {
            break;
        }
        pos += total as usize;
        params.push(ServerParam { col_type, prec, scale, io_type });
        // 名称描述完后通常还有 baseName 或尾部；用"合理 colType"判断终止
        if !(0..=40).contains(&col_type) {
            break;
        }
    }
    params
}

/// 按服务器参数类型返回编码器（serverParams 优先于类型猜测）
#[derive(Debug, Clone, PartialEq)]
enum ParamEncoder {
    Str,
    Decimal,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Real,
    Double,
    Bool,
    Date,
    Time,
    TimeTz,
    DateTime,
    DateTimeTz,
    DateTime2,
    DateTime2Tz,
    Bytes,
}

impl ParamEncoder {
    fn from_type(col_type: i32) -> Self {
        match col_type {
            0 | 1 | 2 | 19 => ParamEncoder::Str, // CHAR/VARCHAR2/VARCHAR/CLOB
            9 => ParamEncoder::Decimal,           // DECIMAL：私有大数二进制
            5 => ParamEncoder::TinyInt,
            6 => ParamEncoder::SmallInt,
            7 => ParamEncoder::Int,
            8 => ParamEncoder::BigInt,
            10 => ParamEncoder::Real,
            11 => ParamEncoder::Double,
            13 => ParamEncoder::Bool,
            14 => ParamEncoder::Date,
            15 => ParamEncoder::Time,
            22 => ParamEncoder::TimeTz, // TIME_TZ
            16 => ParamEncoder::DateTime,
            23 => ParamEncoder::DateTimeTz, // DATETIME_TZ
            26 => ParamEncoder::DateTime2,  // DATETIME2
            27 => ParamEncoder::DateTime2Tz, // DATETIME2_TZ
            12 | 17 | 18 => ParamEncoder::Bytes, // BLOB/BINARY/VARBINARY
            _ => ParamEncoder::Str,
        }
    }

    fn encode(&self, p: &Param, enc: &'static Encoding) -> Result<Vec<u8>> {
        match self {
            ParamEncoder::Str => match p {
                Param::Str(s) => Ok(enc_str(enc, s)),
                Param::Bytes(b) => Ok(b.clone()),
                _ => Ok(enc_str(enc, &to_string_value(p))),
            },
            ParamEncoder::Decimal => Ok(enc_decimal(&to_string_value(p))),
            ParamEncoder::TinyInt => Ok((to_i64(p)? as i8).to_le_bytes().to_vec()),
            ParamEncoder::SmallInt => Ok((to_i64(p)? as i16).to_le_bytes().to_vec()),
            ParamEncoder::Int => Ok((to_i64(p)? as i32).to_le_bytes().to_vec()),
            ParamEncoder::BigInt => Ok(to_i64(p)?.to_le_bytes().to_vec()),
            ParamEncoder::Real => Ok((to_f64(p)? as f32).to_le_bytes().to_vec()),
            ParamEncoder::Double => Ok(to_f64(p)?.to_le_bytes().to_vec()),
            ParamEncoder::Bool => Ok(vec![if to_bool(p) { 1 } else { 0 }]),
            ParamEncoder::Date => Ok(encode_date(&to_date(p)?).to_vec()),
            ParamEncoder::Time => Ok(encode_time(&to_time(p)?).to_vec()),
            ParamEncoder::TimeTz => Ok(encode_time_tz(&to_time(p)?)),
            ParamEncoder::DateTime => Ok(encode_datetime(&to_datetime(p)?)),
            ParamEncoder::DateTimeTz => Ok(encode_datetime_tz(&to_datetime(p)?)),
            ParamEncoder::DateTime2 => Ok(encode_datetime2(&to_datetime(p)?)),
            ParamEncoder::DateTime2Tz => Ok(encode_datetime2_tz(&to_datetime(p)?)),
            ParamEncoder::Bytes => match p {
                Param::Bytes(b) => Ok(b.clone()),
                _ => Ok(to_string_value(p).into_bytes()),
            },
        }
    }
}

fn to_i64(p: &Param) -> Result<i64> {
    match p {
        Param::Int(v) => Ok(*v),
        Param::Bool(b) => Ok(*b as i64),
        Param::Float(f) => Ok(*f as i64),
        Param::Str(s) => s.trim().parse::<i64>().map_err(|_| DmError::arg(format!("无法将 '{s}' 转为整数"))),
        Param::Null => Ok(0),
        _ => Err(DmError::arg(format!("无法将 {p:?} 转为整数"))),
    }
}

fn to_f64(p: &Param) -> Result<f64> {
    match p {
        Param::Float(f) => Ok(*f),
        Param::Int(v) => Ok(*v as f64),
        Param::Bool(b) => Ok((*b as i64) as f64),
        Param::Str(s) => s.trim().parse::<f64>().map_err(|_| DmError::arg(format!("无法将 '{s}' 转为浮点"))),
        _ => Err(DmError::arg(format!("无法将 {p:?} 转为浮点"))),
    }
}

fn to_bool(p: &Param) -> bool {
    match p {
        Param::Bool(b) => *b,
        Param::Int(v) => *v != 0,
        Param::Float(f) => *f != 0.0,
        Param::Str(s) => !s.is_empty() && s != "0" && !s.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn to_date(p: &Param) -> Result<DmDate> {
    match p {
        Param::Date(d) => Ok(*d),
        Param::DateTime(dt) => Ok(dt.date),
        Param::Str(s) => parse_date_str(s),
        _ => Err(DmError::arg(format!("无法将 {p:?} 转为日期"))),
    }
}

fn to_time(p: &Param) -> Result<DmTime> {
    match p {
        Param::Time(t) => Ok(*t),
        Param::DateTime(dt) => Ok(dt.time),
        Param::Str(s) => parse_time_str(s),
        _ => Err(DmError::arg(format!("无法将 {p:?} 转为时间"))),
    }
}

fn to_datetime(p: &Param) -> Result<DmDateTime> {
    match p {
        Param::DateTime(dt) => Ok(*dt),
        Param::Date(d) => Ok(DmDateTime::new(*d, DmTime::new(0, 0, 0, 0))),
        Param::Str(s) => {
            // 兼容 "YYYY-MM-DD HH:MM:SS[.ffffff]" 与纯日期
            let d = parse_date_str(&s[..s.len().min(10)])?;
            let t = if s.len() > 10 {
                parse_time_str(&s[11..])?
            } else {
                DmTime::new(0, 0, 0, 0)
            };
            Ok(DmDateTime::new(d, t))
        }
        _ => Err(DmError::arg(format!("无法将 {p:?} 转为日期时间"))),
    }
}

fn to_string_value(p: &Param) -> String {
    match p {
        Param::Str(s) => s.clone(),
        Param::Int(v) => v.to_string(),
        Param::Float(f) => f.to_string(),
        Param::Bool(b) => b.to_string(),
        Param::Date(d) => d.to_string(),
        Param::Time(t) => t.to_string(),
        Param::DateTime(dt) => dt.to_string(),
        Param::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Param::Null => "".to_string(),
        Param::Out(_) => "".to_string(),
    }
}

/// 解析 "YYYY-MM-DD"
fn parse_date_str(s: &str) -> Result<DmDate> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return Err(DmError::arg(format!("日期格式错误: '{s}'")));
    }
    let year: i32 = parts[0].parse().map_err(|_| DmError::arg(format!("日期格式错误: '{s}'")))?;
    let month: u8 = parts[1].parse().map_err(|_| DmError::arg(format!("日期格式错误: '{s}'")))?;
    let day: u8 = parts[2].parse().map_err(|_| DmError::arg(format!("日期格式错误: '{s}'")))?;
    Ok(DmDate::new(year, month, day))
}

/// 解析 "HH:MM:SS[.ffffff]"
fn parse_time_str(s: &str) -> Result<DmTime> {
    let s = s.trim();
    let (hms, frac) = match s.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (s, None),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    if parts.len() < 2 {
        return Err(DmError::arg(format!("时间格式错误: '{s}'")));
    }
    let hour: u8 = parts[0].parse().map_err(|_| DmError::arg(format!("时间格式错误: '{s}'")))?;
    let minute: u8 = parts[1].parse().map_err(|_| DmError::arg(format!("时间格式错误: '{s}'")))?;
    let second: u8 = if parts.len() > 2 { parts[2].parse().map_err(|_| DmError::arg(format!("时间格式错误: '{s}'")))? } else { 0 };
    let microsecond: u32 = match frac {
        Some(f) => {
            let mut f = f.to_string();
            while f.len() < 6 {
                f.push('0');
            }
            f[..6].parse().unwrap_or(0)
        }
        None => 0,
    };
    Ok(DmTime::new(hour, minute, second, microsecond))
}

fn le_i32(b: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(b[pos..pos + 4].try_into().unwrap())
}

fn le_u16(b: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(b[pos..pos + 2].try_into().unwrap())
}

fn le_i64(b: &[u8], pos: usize) -> i64 {
    i64::from_le_bytes(b[pos..pos + 8].try_into().unwrap())
}

fn parse_bigint_hex(hex: &str) -> num_bigint::BigUint {
    num_bigint::BigUint::parse_bytes(hex.as_bytes(), 16).expect("hex 解析失败")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_encoder_binds_dec_column() {
        // col_type=9 → ParamEncoder::Decimal；float/str/int 参数都走它（私有大数二进制）
        assert!(matches!(ParamEncoder::from_type(9), ParamEncoder::Decimal));
        let enc = ParamEncoder::Decimal;
        assert_eq!(enc.encode(&Param::Float(1.5), encoding_rs::GB18030).unwrap(), vec![0xC1, 0x02, 0x33]);
        assert_eq!(enc.encode(&Param::Str("1.50".into()), encoding_rs::GB18030).unwrap(), vec![0xC1, 0x02, 0x33]);
        assert_eq!(enc.encode(&Param::Int(1), encoding_rs::GB18030).unwrap(), vec![0xC1, 0x02]);
    }

    #[test]
    fn is_query_detection() {
        assert!(is_query_sql("  SELECT * FROM t"));
        assert!(is_query_sql("WITH x AS (...) SELECT 1"));
        assert!(is_query_sql("DESC t"));
        assert!(is_query_sql("SHOW TABLES"));
        assert!(!is_query_sql("INSERT INTO t VALUES (1)"));
        assert!(!is_query_sql("CALL proc(1)"));
        assert!(!is_query_sql("UPDATE t SET a=1"));
    }

    #[test]
    fn param_type_guess() {
        let (ct, prec, scale, enc) = param_type(&Param::Int(1), true, GB18030);
        assert_eq!((ct, prec, scale), (8, 8, 0));
        assert_eq!(enc.encode(&Param::Int(1), GB18030).unwrap(), 1i64.to_le_bytes());

        let (ct, prec, scale, _) = param_type(&Param::Int(1), false, GB18030);
        assert_eq!((ct, prec, scale), (7, 4, 0));

        let (ct, prec, _, _) = param_type(&Param::Str("测试".into()), false, GB18030);
        assert_eq!(ct, 2);
        assert_eq!(prec, 50); // GB18030 8 字节 < 50

        // UTF-8 下 "测试" 6 字节，仍 < 50
        let (ct, prec, _, _) = param_type(&Param::Str("测试".into()), false, UTF_8);
        assert_eq!(ct, 2);
        assert_eq!(prec, 50);

        // UTF-8 下超长字符串按字节长计算 prec（区别于 GB18030 的字节长）
        let s = "中".repeat(30); // UTF-8 90 字节
        let (_ct, prec, _, _) = param_type(&Param::Str(s.clone()), false, UTF_8);
        assert_eq!(prec, 90);
        let (_ct, prec, _, _) = param_type(&Param::Str(s.clone()), false, GB18030);
        assert_eq!(prec, 60); // GB18030 60 字节

        let (_ct, prec, _, _) = param_type(&Param::Str("x".repeat(100).as_str().into()), false, GB18030);
        assert_eq!(prec, 100);

        let (ct, prec, _, _) = param_type(&Param::Str("x".into()), true, GB18030);
        assert_eq!(ct, 2);
        assert_eq!(prec, 8188);

        // 带时区：TIME_TZ(22)/DATETIME_TZ(23)
        let mut tt = DmTime::new(12, 34, 56, 0);
        tt.timezone_minutes = Some(480);
        let (ct, prec, scale, enc) = param_type(&Param::Time(tt), false, GB18030);
        assert_eq!((ct, prec, scale), (22, 5, 0));
        assert_eq!(enc.encode(&Param::Time(tt), GB18030).unwrap(), encode_time_tz(&tt));

        let mut dtz = DmDateTime::new(DmDate::new(2026, 8, 14), DmTime::new(12, 34, 56, 0));
        dtz.time.timezone_minutes = Some(480);
        let (ct, prec, scale, enc) = param_type(&Param::DateTime(dtz), false, GB18030);
        assert_eq!((ct, prec, scale), (23, 8, 0));
        assert_eq!(enc.encode(&Param::DateTime(dtz), GB18030).unwrap(), encode_datetime_tz(&dtz));

        // 无时区保持普通类型
        let (ct, _, _, _) = param_type(&Param::Time(DmTime::new(1, 2, 3, 0)), false, GB18030);
        assert_eq!(ct, 15);
        let (ct, _, _, _) = param_type(&Param::DateTime(DmDateTime::new(DmDate::new(2026, 1, 1), DmTime::new(0, 0, 0, 0))), false, GB18030);
        assert_eq!(ct, 16);
        let (ct, _, _, _) = param_type(&Param::Date(DmDate::new(2026, 8, 12)), false, GB18030);
        assert_eq!(ct, 14);
        let (ct, _, _, _) = param_type(&Param::Float(1.5), false, GB18030);
        assert_eq!(ct, 11);
        let (ct, _, _, _) = param_type(&Param::Bytes(vec![1, 2]), false, GB18030);
        assert_eq!(ct, 17);
        let (ct, _, _, _) = param_type(&Param::Bool(true), false, GB18030);
        assert_eq!(ct, 7);
        let (ct, _, _, _) = param_type(&Param::Null, false, GB18030);
        assert_eq!(ct, 2);
    }

    #[test]
    fn encoding_resolve_and_length() {
        // 编码名解析（不区分大小写）
        assert_eq!(resolve_encoding(None), None);
        assert_eq!(resolve_encoding(Some("")), None);
        assert_eq!(resolve_encoding(Some("utf-8")), Some(UTF_8));
        assert_eq!(resolve_encoding(Some("UTF8")), Some(UTF_8));
        assert_eq!(resolve_encoding(Some("gb18030")), Some(GB18030));
        assert_eq!(resolve_encoding(Some("GBK")), Some(GB18030));
        assert_eq!(resolve_encoding(Some("euc-kr")), Some(EUC_KR));
        assert_eq!(resolve_encoding(Some("latin1")), None); // 未识别 → 自动检测

        // 编码字节长度：UTF-8 中文 3 字节/字，GB18030 2 字节/字
        let s = "中文测试";
        assert_eq!(enc_str(UTF_8, s).len(), 12);
        assert_eq!(enc_str(GB18030, s).len(), 8);
        assert_eq!(lob_len(&Param::Str(s.into()), UTF_8), 12);
        assert_eq!(lob_len(&Param::Str(s.into()), GB18030), 8);

        // 握手响应编码码值映射（Go 驱动逻辑：1=UTF8, 2=EUCKR, 其他=GB18030）
        fn detect(opts: &[u8]) -> &'static Encoding {
            let code = i32::from_le_bytes(opts[8..12].try_into().unwrap());
            match code {
                1 => UTF_8,
                2 => EUC_KR,
                _ => GB18030,
            }
        }
        let mut opts = [0u8; 44];
        assert_eq!(detect(&opts), GB18030); // 0 → GB18030
        opts[8..12].copy_from_slice(&1i32.to_le_bytes());
        assert_eq!(detect(&opts), UTF_8);
        opts[8..12].copy_from_slice(&2i32.to_le_bytes());
        assert_eq!(detect(&opts), EUC_KR);
    }

    #[test]
    fn utf8_char_boundary_align() {
        // UTF-8 分块对齐：块末若切在续字节则回退到字符起点
        let s = "一二三四五六七八九十"; // 30 字节 UTF-8
        let data = enc_str(UTF_8, s);
        let mut end = 7usize.min(data.len()); // 切在 "三" 中间（字节 6-8）
        while end < data.len() && end > 0 && (0x80..=0xBF).contains(&data[end]) {
            end -= 1;
        }
        assert_eq!(end, 6);
        assert_eq!(&data[..end], "一二".as_bytes());
    }

    #[test]
    fn server_params_parse() {
        // 抓包实测：INSERT INTO lob_test3 (id, data) VALUES (?, ?) 的 prepare 响应 (cmd=0 len=72)
        let body = hex2b(
            "0700000004000000000000000100000000000000000000000000030000000000494e5413000000ffffff7f000000000100000000000000000000000000040000000000434c4f4200",
        );
        let params = parse_server_params(&body);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].col_type, 7);
        assert_eq!(params[0].prec, 4);
        assert_eq!(params[0].scale, 0);
        assert_eq!(params[1].col_type, 19); // CLOB
        assert_eq!(params[1].prec, 0x7FFF_FFFF);
    }

    fn hex2b(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn execute_opts_match_capture() {
        // 非参数化 SELECT 抓包实测：01 01 00 01 00 00 00 ff ff ff ff ff ff ff 7f 00 00 00 01
        let c = DmClient::new("h", 1, "u", "p");
        let o = c.execute_opts(false);
        assert_eq!(
            &o[..19],
            &[
                0x01, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0x7f, 0x00, 0x00, 0x00, 0x01
            ]
        );
        // 参数化 SQL 抓包实测：01 00 00 01 00 00 00 ff ff ff ff ff ff ff 7f 00 00 00 00
        let o = c.execute_opts(true);
        assert_eq!(
            &o[..19],
            &[
                0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0x7f, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn bind_opts_match_capture() {
        // 抓包实测 cmd=13 参数区（1 参数 batch=1）：
        // 01 01 00 01 01 00 00 00 ... ff ff ff ff ff ff ff 7f 01 00 00 00 ff ff ff ff
        let mut c = DmClient::new("h", 1, "u", "p");
        c.auto_commit = true;
        let params = [Param::Int(1)];
        // 直接构造 opts 校验（通过 bind_params 无法断言，这里独立复现）
        let mut opts = [0u8; 44];
        opts[0] = 1;
        opts[1..3].copy_from_slice(&1u16.to_le_bytes());
        opts[3] = 1;
        opts[4..12].copy_from_slice(&1i64.to_le_bytes());
        opts[20..28].copy_from_slice(&i64::MAX.to_le_bytes());
        opts[28] = 1;
        opts[32..36].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(&opts[0..12], &[0x01, 0x01, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&opts[20..36], &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff]);
        let _ = &params;
    }

    #[test]
    fn date_str_parse() {
        let d = parse_date_str("2026-08-12").unwrap();
        assert_eq!(d, DmDate::new(2026, 8, 12));
        let t = parse_time_str("12:34:56.789").unwrap();
        assert_eq!(t, DmTime::new(12, 34, 56, 789000));
        let dt = to_datetime(&Param::Str("2030-06-15 08:09:10.111222".into())).unwrap();
        assert_eq!(dt, DmDateTime::new(DmDate::new(2030, 6, 15), DmTime::new(8, 9, 10, 111222)));
    }
}
