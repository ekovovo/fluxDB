//! 达梦结果集解析：列描述符 + 行数据 + 类型解码（对照 Go 驱动 + 抓包实测格式）。

use encoding_rs::Encoding;

use crate::{DmError, Result};

/// 严格按当前会话字符集解码。
///
/// `encoding_rs` 的 `decode()` 遇到非法字节会静默写入替换字符，调用方无法区分
/// “数据正常”和“数据已损坏”。这里必须显式返回错误，避免把损坏值当正常文本展示。
pub(crate) fn decode_text_strict(enc: &'static Encoding, raw: &[u8]) -> Result<String> {
    enc.decode_without_bom_handling_and_without_replacement(raw)
        .map(|text| text.into_owned())
        .ok_or_else(|| {
            DmError::protocol(format!(
                "结果包含非法 {} 文本（{} 字节），请检查连接字符集设置",
                enc.name(),
                raw.len()
            ))
        })
}

// 达梦类型常量（zzn.go）
pub const DM_CHAR: i32 = 0;
pub const DM_VARCHAR2: i32 = 1;
pub const DM_VARCHAR: i32 = 2;
pub const DM_BIT: i32 = 3;
pub const DM_TINYINT: i32 = 5;
pub const DM_SMALLINT: i32 = 6;
pub const DM_INT: i32 = 7;
pub const DM_BIGINT: i32 = 8;
pub const DM_DECIMAL: i32 = 9;
pub const DM_REAL: i32 = 10;
pub const DM_DOUBLE: i32 = 11;
pub const DM_BLOB: i32 = 12;
pub const DM_BOOLEAN: i32 = 13;
pub const DM_DATE: i32 = 14;
pub const DM_TIME: i32 = 15;
pub const DM_DATETIME: i32 = 16;
pub const DM_BINARY: i32 = 17;
pub const DM_VARBINARY: i32 = 18;
pub const DM_CLOB: i32 = 19;
pub const DM_INTERVAL_YM: i32 = 20;
pub const DM_INTERVAL_DT: i32 = 21;
pub const DM_TIME_TZ: i32 = 22;
pub const DM_DATETIME_TZ: i32 = 23;
pub const DM_XDEC_INT32: i32 = 24;
pub const DM_XDEC_INT64: i32 = 25;
pub const DM_DATETIME2: i32 = 26;
pub const DM_DATETIME2_TZ: i32 = 27;
// （对照 Go 驱动 zzn.go：TIME_TZ=22, DATETIME_TZ=23, XDEC_INT32=24, XDEC_INT64=25,
//  DATETIME2=26, DATETIME2_TZ=27；旧版误写 DATETIME2=30 已修正）

/// 行数据 NULL 标记
pub const NULL_MARK: u16 = 0xFFFE;
/// LOB/特殊标记（0xFFFF 后跟 int32 真实长度 + 数据，Go zzo.go convertBytes2BaseData）
pub const LOB_MARK: u16 = 0xFFFF;

/// 达梦日期（年可为负/超 9999，避免 chrono/NaiveDate 限制）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl DmDate {
    pub fn new(year: i32, month: u8, day: u8) -> Self {
        DmDate { year, month, day }
    }
}

impl std::fmt::Display for DmDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// 达梦时间（微秒精度）。`timezone_minutes` 为带时区类型（TIME_TZ）的偏移（分钟，可负）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub microsecond: u32,
    /// 时区偏移（分钟，东正西负）；非 TZ 类型为 None
    pub timezone_minutes: Option<i16>,
}

impl DmTime {
    pub fn new(hour: u8, minute: u8, second: u8, microsecond: u32) -> Self {
        DmTime { hour, minute, second, microsecond, timezone_minutes: None }
    }
}

fn fmt_tz(tz: i16) -> String {
    let sign = if tz < 0 { "-" } else { "+" };
    let a = tz.unsigned_abs() as i32;
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

impl std::fmt::Display for DmTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}.{:06}",
            self.hour, self.minute, self.second, self.microsecond
        )?;
        if let Some(tz) = self.timezone_minutes {
            write!(f, " {}", fmt_tz(tz))?;
        }
        Ok(())
    }
}

/// 达梦日期时间。`time.timezone_minutes` 为带时区类型（DATETIME_TZ/DATETIME2_TZ）的偏移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmDateTime {
    pub date: DmDate,
    pub time: DmTime,
}

impl DmDateTime {
    pub fn new(date: DmDate, time: DmTime) -> Self {
        DmDateTime { date, time }
    }
}

impl std::fmt::Display for DmDateTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.date, self.time)
    }
}

/// 解码后的单元格值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    TinyInt(i8),
    SmallInt(i16),
    Int(i32),
    BigInt(i64),
    Real(f32),
    Double(f64),
    /// DECIMAL 保持字符串，避免精度丢失（如 DECIMAL(38)）
    Decimal(String),
    Str(String),
    Bytes(Vec<u8>),
    Date(DmDate),
    Time(DmTime),
    DateTime(DmDateTime),
    Interval(String),
    /// 未识别的原始字节
    Raw(Vec<u8>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v as i64),
            Value::BigInt(v) => Some(*v),
            Value::SmallInt(v) => Some(*v as i64),
            Value::TinyInt(v) => Some(*v as i64),
            Value::Bool(b) => Some(*b as i64),
            Value::Double(f) => Some(*f as i64),
            Value::Real(f) => Some(*f as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Double(f) => Some(*f),
            Value::Real(f) => Some(*f as f64),
            Value::Int(v) => Some(*v as f64),
            Value::BigInt(v) => Some(*v as f64),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            Value::Raw(b) => Some(b),
            _ => None,
        }
    }
}

/// 列描述符。
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub col_type: i32,
    pub prec: i32,
    pub scale: i32,
    pub nullable: bool,
    pub name: String,
    pub type_name: String,
    pub table_name: String,
    pub schema_name: String,
    pub lob: bool,
}

/// 原始行：每单元格为 Some(bytes) 或 None(NULL)。
pub type Row = Vec<Option<Vec<u8>>>;

fn le_i32(b: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(b[pos..pos + 4].try_into().unwrap())
}

fn le_u16(b: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(b[pos..pos + 2].try_into().unwrap())
}

fn read_str_at(body: &[u8], pos: &mut usize, len: usize, enc: &'static Encoding) -> String {
    if *pos + len > body.len() {
        *pos = body.len();
        return String::new();
    }
    let bytes = &body[*pos..*pos + len];
    *pos += len;
    // 元数据名称损坏时不能伪造出替换字符，否则列名/表名会被静默改写。
    decode_text_strict(enc, bytes).unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
}

/// 列描述符定位自适应（服务器参数回显区长度不固定）。
///
/// 实测：服务器在 DDL（DROP/CREATE）后首条参数化查询的参数回显区会多带 8B 类型名
/// （如 "BIGINT"），列描述符起点从 `32*n` 变为 `32*n+8`；硬编码 `params.len()*32`
/// 会解析错位成垃圾 col_type（如 1229408578）。
/// 依次尝试 32/32+8/16/16+8/0 字节每参数偏移，取能解析出合法列描述符
/// （列数匹配、col_type 0..=40、prec 合理、列名非空且含可打印字符）的候选。
pub fn parse_cols_robust(
    body: &[u8],
    n_params: usize,
    ncols: usize,
    enc: &'static Encoding,
) -> Result<Vec<Column>> {
    let valid = |cols: &[Column]| -> bool {
        if cols.len() != ncols {
            return false;
        }
        for c in cols {
            if !(0..=40).contains(&c.col_type) {
                return false;
            }
            if !(0..=1_000_000).contains(&c.prec) {
                return false;
            }
            if c.name.is_empty()
                || c.name.chars().any(|ch| ch.is_control())
                || !c.name.chars().any(|ch| ch.is_alphanumeric())
            {
                return false;
            }
        }
        true
    };
    for (per, off) in [(32usize, 0i32), (32, 8), (16, 0), (16, 8), (0, 0), (32, -8), (16, -8)] {
        let pos = (n_params * per) as i64 + off as i64;
        if pos < 0 || pos + 30 > body.len() as i64 {
            continue;
        }
        if let Ok((cols, _)) = parse_columns(body, pos as usize, ncols, enc) {
            if valid(&cols) {
                return Ok(cols);
            }
        }
    }
    // 全范围扫描兜底（每 2 字节步进，应对非常规回显如 32*n+10 等）
    let scan_end = body.len().saturating_sub(30).min(256);
    for pos in (0..scan_end).step_by(2) {
        if let Ok((cols, _)) = parse_columns(body, pos, ncols, enc) {
            if valid(&cols) {
                return Ok(cols);
            }
        }
    }
    // 最后兜底：按 32B/参数
    let (cols, _) = parse_columns(body, n_params * 32, ncols, enc)?;
    Ok(cols)
}

/// 解析 `count` 个列描述符，返回 (列列表, 新位置)。
///
/// 实测结构（Go dm_build_1009）：
/// `4×int32(类型/精度/缩放/可空) + 8字节保留 + 4×int16 长度表 + 4 个字符串
///  + baseName(int16 长度 + 数据) + (LOB 列额外 6 字节)`
pub fn parse_columns(
    body: &[u8],
    mut pos: usize,
    count: usize,
    enc: &'static Encoding,
) -> Result<(Vec<Column>, usize)> {
    let mut cols = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 32 > body.len() {
            break;
        }
        let col_type = le_i32(body, pos);
        let prec = le_i32(body, pos + 4);
        let scale = le_i32(body, pos + 8);
        let nullable = le_i32(body, pos + 12) != 0;
        pos += 24; // 16(4×i32) + 8 保留
        if pos + 8 > body.len() {
            break;
        }
        let nl = le_u16(body, pos) as usize;
        let tl = le_u16(body, pos + 2) as usize;
        let tbl = le_u16(body, pos + 4) as usize;
        let sl = le_u16(body, pos + 6) as usize;
        pos += 8;
        let name = read_str_at(body, &mut pos, nl, enc);
        let type_name = read_str_at(body, &mut pos, tl, enc);
        let table_name = read_str_at(body, &mut pos, tbl, enc);
        let schema_name = read_str_at(body, &mut pos, sl, enc);
        // baseName（int16 长度 + 数据）
        if pos + 2 > body.len() {
            break;
        }
        let bn = le_u16(body, pos) as usize;
        pos += 2;
        pos = (pos + bn).min(body.len());
        let lob = col_type == DM_BLOB || col_type == DM_CLOB;
        if lob {
            // lob 列额外 6 字节（lobTabId int32 + lobColId int16）
            pos += 6;
        }
        cols.push(Column {
            col_type,
            prec,
            scale,
            nullable,
            name,
            type_name,
            table_name,
            schema_name,
            lob,
        });
    }
    Ok((cols, pos))
}

/// 解析普通查询行（`[int32 行长][6B][偏移表 2×ncols][列数据]`）。
///
/// NULL 列：长度 = 0xFFFE；LOB/特殊：长度 = 0xFFFF + int32 真实长度 + 数据
/// （后者为对 Python 驱动 `<LOB>` 占位符的修正，格式依据 Go zzo.go）。
pub fn parse_rows(
    body: &[u8],
    mut pos: usize,
    ncols: usize,
    nrows: usize,
) -> Result<Vec<Row>> {
    let mut rows = Vec::with_capacity(nrows);
    let header = 10; // int32(4) + 6B
    for _ in 0..nrows {
        if pos + header > body.len() {
            break;
        }
        pos += header;
        if pos + 2 * ncols > body.len() {
            break;
        }
        pos += 2 * ncols; // 偏移表
        let mut row = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            if pos + 2 > body.len() {
                row.push(None);
                break;
            }
            let ln = le_u16(body, pos);
            pos += 2;
            match ln {
                NULL_MARK => row.push(None),
                LOB_MARK => {
                    // 0xFFFF → int32 真实长度（Go: valueLen == -1 → 读 int32）
                    if pos + 4 <= body.len() {
                        let real = le_i32(body, pos);
                        pos += 4;
                        if real > 0 && pos + real as usize <= body.len() {
                            row.push(Some(body[pos..pos + real as usize].to_vec()));
                            pos += real as usize;
                        } else {
                            row.push(None);
                        }
                    } else {
                        row.push(None);
                    }
                }
                n => {
                    let n = n as usize;
                    if pos + n > body.len() {
                        row.push(None);
                        break;
                    }
                    row.push(Some(body[pos..pos + n].to_vec()));
                    pos += n;
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// 解析参数化查询行（行长循环 `[int16 行长][int16 行号][7B][偏移表 2×ncols][列数据]`）。
pub fn parse_rows_param(body: &[u8], mut pos: usize, ncols: usize) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    while pos + 2 <= body.len() {
        let rlen = le_u16(body, pos) as usize;
        let row_end = pos + rlen;
        // 行长 = 行长字段(2) + 数据长度（实测：行1 行长26 = 2+24）
        if rlen == 0 || row_end <= pos + 2 || row_end > body.len() {
            break; // 行长=0 或无效（尾部残留）
        }
        let mut p = pos + 2 + 2 + 6; // 行长 + 行号 + 6B
        p += 2 * ncols; // 偏移表
        let mut row = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            if p + 2 > row_end {
                row.push(None);
                break;
            }
            let ln = le_u16(body, p);
            p += 2;
            match ln {
                NULL_MARK => row.push(None),
                LOB_MARK => {
                    if p + 4 <= row_end {
                        let real = le_i32(body, p);
                        p += 4;
                        if real > 0 && p + real as usize <= row_end {
                            row.push(Some(body[p..p + real as usize].to_vec()));
                            p += real as usize;
                        } else {
                            row.push(None);
                        }
                    } else {
                        row.push(None);
                    }
                }
                n => {
                    let n = n as usize;
                    if p + n > row_end {
                        row.push(None);
                        break;
                    }
                    row.push(Some(body[p..p + n].to_vec()));
                    p += n;
                }
            }
        }
        rows.push(row);
        pos = row_end;
    }
    Ok(rows)
}

/// 按列类型解码单个值（LOB 列不在此处理，由 client 走完整读取）。
pub fn decode_scalar(raw: &[u8], col: &Column, enc: &'static Encoding) -> Result<Value> {
    if raw.is_empty() {
        // 空字节串按类型给空值
        match col.col_type {
            DM_CHAR | DM_VARCHAR2 | DM_VARCHAR | DM_CLOB => return Ok(Value::Str(String::new())),
            _ => return Ok(Value::Bytes(Vec::new())),
        }
    }
    let t = col.col_type;
    let v = match t {
        DM_CHAR | DM_VARCHAR2 | DM_VARCHAR => {
            Value::Str(decode_text_strict(enc, raw)?)
        }
        DM_BOOLEAN | DM_BIT => Value::Bool(raw[0] != 0),
        DM_TINYINT => Value::TinyInt(raw[0] as i8),
        DM_SMALLINT => Value::SmallInt(i16::from_le_bytes(raw[..2].try_into().unwrap())),
        DM_INT => Value::Int(le_i32(raw, 0)),
        DM_BIGINT => Value::BigInt(i64::from_le_bytes(raw[..8].try_into().unwrap())),
        DM_REAL => Value::Real(f32::from_le_bytes(raw[..4].try_into().unwrap())),
        DM_DOUBLE => Value::Double(f64::from_le_bytes(raw[..8].try_into().unwrap())),
        DM_DECIMAL => Value::Decimal(decode_decimal(raw)),
        DM_DATE => Value::Date(decode_date(raw)),
        DM_TIME => Value::Time(decode_time(raw)),
        DM_TIME_TZ => Value::Time(decode_time_tz(raw)),
        DM_DATETIME => Value::DateTime(decode_datetime(raw)),
        DM_DATETIME2 => Value::DateTime(decode_datetime2(raw)),
        DM_DATETIME_TZ => Value::DateTime(decode_datetime_tz(raw)),
        DM_DATETIME2_TZ => Value::DateTime(decode_datetime2_tz(raw)),
        DM_INTERVAL_YM => Value::Interval(decode_interval_ym(raw)),
        DM_INTERVAL_DT => Value::Interval(decode_interval_dt(raw)),
        DM_BINARY | DM_VARBINARY | DM_BLOB => Value::Bytes(raw.to_vec()),
        _ => Value::Raw(raw.to_vec()),
    };
    Ok(v)
}

/// 达梦私有大数编码解码（Go o.go decodeDecimal，0xc10233=1.5 实测验证）。
///
/// 首字节: 0x80=0；&0x80≠0 → 正数 exp=flag-0xC1；否则负数 exp=0x3E-flag
/// 后续字节: 每字节 2 位十进制，正数=byte-1，负数=101-byte；越界(0/0x66)终止
/// 用字符串构造结果，避免浮点/Decimal 默认精度截断（DECIMAL(38) 需要 40 位）
pub fn decode_decimal(raw: &[u8]) -> String {
    if raw.is_empty() || raw[0] == 0x80 {
        return "0".to_string();
    }
    let (sign, exp): (i32, i32) = if raw[0] & 0x80 != 0 {
        (1, raw[0] as i32 - 0xC1)
    } else {
        (-1, 0x3E - raw[0] as i32)
    };
    let mut digits = String::new();
    for &b in &raw[1..] {
        let d = if sign > 0 { b as i32 - 1 } else { 101 - b as i32 };
        if !(0..=99).contains(&d) {
            break;
        }
        digits.push_str(&format!("{d:02}"));
    }
    if digits.is_empty() {
        return "0".to_string();
    }
    // weight = exp*2 - (len(digits)-2)，与 Python 一致
    let weight = exp * 2 - (digits.len() as i32 - 2);
    let s = if weight >= 0 {
        format!("{}{}", digits, "0".repeat(weight as usize))
    } else {
        let n = digits.len() as i32 + weight;
        if n <= 0 {
            format!("0.{}{}", "0".repeat((-n) as usize), digits)
        } else {
            format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
        }
    };
    let s = if sign < 0 {
        format!("-{s}")
    } else {
        s
    };
    py_decimal_str(&s)
}

/// 按 Python `Decimal.__str__` 算法格式化十进制字符串
/// （_pydecimal.py __str__：`exp<=0 且 leftdigits>-6` 用定点，否则科学计数法）。
fn py_decimal_str(s: &str) -> String {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let mut combined = String::with_capacity(int_part.len() + frac_part.len());
    combined.push_str(int_part);
    combined.push_str(frac_part);
    // 去掉系数前导零（保留至少一位）
    let stripped = combined.trim_start_matches('0');
    let coeff = if stripped.is_empty() { "0" } else { stripped };
    // 指数 = 小数位数（符号相反）
    let exp: i64 = -(frac_part.len() as i64);
    // Python: leftdigits = exp + len(coeff)
    let leftdigits = exp + coeff.len() as i64;
    let dotplace: i64 = if exp <= 0 && leftdigits > -6 {
        leftdigits
    } else {
        1
    };
    let (intpart, fracpart) = if dotplace <= 0 {
        (String::from("0"), format!(".{}{}", "0".repeat((-dotplace) as usize), coeff))
    } else if dotplace >= coeff.len() as i64 {
        (
            format!("{}{}", coeff, "0".repeat((dotplace - coeff.len() as i64) as usize)),
            String::new(),
        )
    } else {
        (
            coeff[..dotplace as usize].to_string(),
            format!(".{}", &coeff[dotplace as usize..]),
        )
    };
    let exp_part = if leftdigits == dotplace {
        String::new()
    } else {
        format!("E{:+}", leftdigits - dotplace)
    };
    format!("{}{}{}{}", if neg { "-" } else { "" }, intpart, fracpart, exp_part)
}

/// DATE 位压缩（3 字节）：EA 07 64 → 2026-08-12
pub fn decode_date(raw: &[u8]) -> DmDate {
    let b0 = raw[0] as i32;
    let b1 = raw[1] as i32;
    let b2 = raw[2] as i32;
    let mut year = (b0 | (b1 << 8)) & 0x7FFF;
    if year > 9999 {
        year = (year | 0x8000) & 0xFFFF;
        year -= 65536;
    }
    let month = ((b1 >> 7) & 0x1) + ((b2 & 0x07) << 1);
    let day = ((b2 & 0xF8) >> 3) & 0x1F;
    DmDate {
        year,
        month: if month == 0 { 1 } else { month as u8 },
        day: if day == 0 { 1 } else { day as u8 },
    }
}

/// TIME 位压缩（5 字节）
pub fn decode_time(raw: &[u8]) -> DmTime {
    let b0 = raw[0] as u32;
    let b1 = raw[1] as u32;
    let b2 = raw[2] as u32;
    let b3 = raw[3] as u32;
    let b4 = raw[4] as u32;
    let hour = b0 & 0x1F;
    let minute = ((b0 >> 5) & 0x07) + ((b1 & 0x07) << 3);
    let second = ((b1 >> 3) & 0x1F) + ((b2 & 0x01) << 5);
    let nsec = (((b2 >> 1) & 0x7F) + (b3 << 7) + ((b4 & 0x1F) << 15)) * 1000;
    DmTime {
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        microsecond: nsec / 1000,
        timezone_minutes: None,
    }
}

/// TIME_TZ：TIME(5B) + int16 时区偏移（分钟，可负，Go h.go dmdtDecodeFast）
pub fn decode_time_tz(raw: &[u8]) -> DmTime {
    let mut t = decode_time(&raw[..5.min(raw.len())]);
    if raw.len() >= 7 {
        t.timezone_minutes = Some(i16::from_le_bytes([raw[5], raw[6]]));
    }
    t
}

/// DATETIME：DATE(3B) + TIME(5B) 组合
pub fn decode_datetime(raw: &[u8]) -> DmDateTime {
    if raw.len() >= 8 {
        DmDateTime {
            date: decode_date(&raw[..3]),
            time: decode_time(&raw[3..8]),
        }
    } else {
        DmDateTime {
            date: DmDate::new(1, 1, 1),
            time: DmTime::new(0, 0, 0, 0),
        }
    }
}

/// DATETIME2：DATE(3B) + TIME 6B（纳秒 30 位直接存储，非 ×1000，Go h.go DATETIME2 分支）
pub fn decode_datetime2(raw: &[u8]) -> DmDateTime {
    if raw.len() >= 9 {
        let b = |i: usize| raw[3 + i] as u32; // time 部分从 [3] 起
        let nsec = ((b(2) >> 1) & 0x7F) + (b(3) << 7) + (b(4) << 15) + ((b(5) & 0x7F) << 23);
        let date = decode_date(&raw[..3]);
        let mut time = decode_time(&raw[3..8]);
        time.microsecond = nsec / 1000;
        DmDateTime { date, time }
    } else {
        decode_datetime(raw)
    }
}

/// DATETIME_TZ：DATETIME(8B) + int16 时区偏移（分钟，可负）
pub fn decode_datetime_tz(raw: &[u8]) -> DmDateTime {
    let mut dt = decode_datetime(&raw[..8.min(raw.len())]);
    if raw.len() >= 10 {
        dt.time.timezone_minutes = Some(i16::from_le_bytes([raw[8], raw[9]]));
    }
    dt
}

/// DATETIME2_TZ：DATETIME2(9B) + int16 时区偏移（分钟，可负）
pub fn decode_datetime2_tz(raw: &[u8]) -> DmDateTime {
    let mut dt = decode_datetime2(&raw[..9.min(raw.len())]);
    if raw.len() >= 11 {
        dt.time.timezone_minutes = Some(i16::from_le_bytes([raw[9], raw[10]]));
    }
    dt
}

/// INTERVAL YEAR TO MONTH：12 字节 = [int32 年][int32 月][int32 scale][byte type]
pub fn decode_interval_ym(raw: &[u8]) -> String {
    if raw.len() < 8 {
        return String::new();
    }
    let years = le_i32(raw, 0);
    let months = le_i32(raw, 4);
    format!("{years}-{months}")
}

/// INTERVAL DAY TO SECOND：24 字节 = [int32 天][int32 时][int32 分][int32 秒][int32 微秒][...]
pub fn decode_interval_dt(raw: &[u8]) -> String {
    if raw.len() < 20 {
        return String::new();
    }
    let days = le_i32(raw, 0);
    let hours = le_i32(raw, 4);
    let minutes = le_i32(raw, 8);
    let seconds = le_i32(raw, 12);
    let frac = le_i32(raw, 16);
    format!("{days} {hours:02}:{minutes:02}:{seconds:02}.{frac:06}")
}

// ---------- 编码（参数绑定用） ----------

/// DATE 位压缩编码（3 字节，与 decode_date 互逆）
pub fn encode_date(d: &DmDate) -> [u8; 3] {
    let y = d.year;
    let m = d.month as i32;
    let dd = d.day as i32;
    [
        (y & 0xFF) as u8,
        (((y >> 8) & 0x7F) | ((m & 1) << 7)) as u8,
        (((m >> 1) & 0x07) | ((dd & 0x1F) << 3)) as u8,
    ]
}

/// TIME 位压缩编码（5 字节，与 decode_time 互逆）
pub fn encode_time(t: &DmTime) -> [u8; 5] {
    let us = t.microsecond;
    let b0 = (t.hour as u32) | ((t.minute as u32 & 0x07) << 5);
    let b1 = (t.minute as u32 >> 3) | ((t.second as u32) << 3);
    let b2 = (t.second as u32 >> 5) | ((us & 0x7F) << 1);
    let b3 = (us >> 7) & 0xFF;
    let b4 = (us >> 15) & 0x1F;
    [
        (b0 & 0xFF) as u8,
        (b1 & 0xFF) as u8,
        (b2 & 0xFF) as u8,
        b3 as u8,
        b4 as u8,
    ]
}

/// DATETIME：DATE 位压缩 + TIME 位压缩（8 字节）
pub fn encode_datetime(dt: &DmDateTime) -> Vec<u8> {
    let mut out = encode_date(&dt.date).to_vec();
    out.extend_from_slice(&encode_time(&dt.time));
    out
}

/// TIME_TZ 编码：TIME(5B) + int16 时区偏移（分钟，小端，与 decode_time_tz 互逆）
pub fn encode_time_tz(t: &DmTime) -> Vec<u8> {
    let mut out = encode_time(t).to_vec();
    out.extend_from_slice(&t.timezone_minutes.unwrap_or(0).to_le_bytes());
    out
}

/// DATETIME_TZ 编码：DATETIME(8B) + int16 时区偏移（分钟，小端，与 decode_datetime_tz 互逆）
pub fn encode_datetime_tz(dt: &DmDateTime) -> Vec<u8> {
    let mut out = encode_datetime(dt);
    out.extend_from_slice(&dt.time.timezone_minutes.unwrap_or(0).to_le_bytes());
    out
}

/// DATETIME2 编码：DATE(3B) + TIME 6B（纳秒 30 位直接存储，非 ×1000；与 decode_datetime2 互逆）
/// 时间部分 b0/b1/b2-低位 与 5 字节 TIME 一致，b2 高 7 位/b3/b4/b5 存纳秒：
/// nsec = (b2>>1 &0x7F) + (b3<<7) + (b4<<15) + (b5&0x7F)<<23
pub fn encode_datetime2(dt: &DmDateTime) -> Vec<u8> {
    let mut out = encode_date(&dt.date).to_vec();
    let t = &dt.time;
    let nsec = (t.microsecond as u64) * 1000;
    let b0 = (t.hour as u32) | ((t.minute as u32 & 0x07) << 5);
    let b1 = (t.minute as u32 >> 3) | ((t.second as u32) << 3);
    let b2 = (t.second as u32 >> 5) | (((nsec & 0x7F) as u32) << 1);
    out.extend_from_slice(&[
        b0 as u8,
        b1 as u8,
        b2 as u8,
        ((nsec >> 7) & 0xFF) as u8,
        ((nsec >> 15) & 0xFF) as u8,
        ((nsec >> 23) & 0x7F) as u8,
    ]);
    out
}

/// DATETIME2_TZ 编码：DATETIME2(9B) + int16 时区偏移（分钟，小端，与 decode_datetime2_tz 互逆）
pub fn encode_datetime2_tz(dt: &DmDateTime) -> Vec<u8> {
    let mut out = encode_datetime2(dt);
    out.extend_from_slice(&dt.time.timezone_minutes.unwrap_or(0).to_le_bytes());
    out
}

// ---------- 参数/输出值类型转换 ----------

/// OUT 参数值解码（cmd=187 响应）
pub fn decode_out_value(raw: &[u8], col_type: i32, enc: &'static Encoding) -> Value {
    match col_type {
        5..=8 => Value::BigInt(le_i64(raw)),
        10 => Value::Real(f32::from_le_bytes(raw[..4].try_into().unwrap())),
        11 => Value::Double(f64::from_le_bytes(raw[..8].try_into().unwrap())),
        14 => Value::Date(decode_date(raw)),
        15 => Value::Time(decode_time(raw)),
        16 => Value::DateTime(decode_datetime(raw)),
        2 | 19 => match decode_text_strict(enc, raw) {
            Ok(text) => Value::Str(text),
            Err(_) => Value::Raw(raw.to_vec()),
        },
        12 | 17 | 18 => Value::Bytes(raw.to_vec()),
        _ => Value::Raw(raw.to_vec()),
    }
}

fn le_i64(b: &[u8]) -> i64 {
    let mut buf = [0u8; 8];
    let n = b.len().min(8);
    buf[..n].copy_from_slice(&b[..n]);
    i64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::enc_decimal;

    #[test]
    fn date_decode_reference() {
        let d = decode_date(&[0xEA, 0x07, 0x64]);
        assert_eq!(d, DmDate::new(2026, 8, 12));
        // 最小日期
        let d = decode_date(&encode_date(&DmDate::new(1, 1, 1)));
        assert_eq!(d, DmDate::new(1, 1, 1));
    }

    #[test]
    fn date_encode_reference() {
        assert_eq!(encode_date(&DmDate::new(2026, 8, 12)), [0xEA, 0x07, 0x64]);
        assert_eq!(encode_date(&DmDate::new(1999, 2, 28)), [0xCF, 0x07, 0xE1]);
    }

    #[test]
    fn time_roundtrip_reference() {
        // Python 参考向量
        let t = decode_time(&[0x77, 0xDF, 0x7F, 0x84, 0x1E]);
        assert_eq!(t, DmTime::new(23, 59, 59, 999999));
        let t2 = decode_time(&[0x4C, 0xC4, 0x11, 0x14, 0x18]);
        assert_eq!(t2, DmTime::new(12, 34, 56, 789000));
        // 编码互逆
        for t in [
            DmTime::new(23, 59, 59, 999999),
            DmTime::new(12, 34, 56, 789000),
            DmTime::new(0, 0, 0, 0),
        ] {
            assert_eq!(decode_time(&encode_time(&t)), t);
        }
    }

    #[test]
    fn datetime_roundtrip() {
        let dt = DmDateTime::new(DmDate::new(2030, 6, 15), DmTime::new(8, 9, 10, 111222));
        let b = encode_datetime(&dt);
        assert_eq!(b, vec![0xEE, 0x07, 0x7B, 0x28, 0x51, 0xEC, 0x64, 0x03]);
        assert_eq!(decode_datetime(&b), dt);
        // 最大 datetime
        let max = DmDateTime::new(DmDate::new(9999, 12, 31), DmTime::new(23, 59, 59, 999999));
        assert_eq!(encode_datetime(&max), vec![0x0F, 0x27, 0xFE, 0x77, 0xDF, 0x7F, 0x84, 0x1E]);
        assert_eq!(decode_datetime(&encode_datetime(&max)), max);
    }

    #[test]
    fn decimal_decode_reference() {
        // Python 参考：0xc10233 → 1.50
        assert_eq!(decode_decimal(&[0xC1, 0x02, 0x33]), "1.50");
        assert_eq!(decode_decimal(&[0x80]), "0");
        assert_eq!(decode_decimal(&[0xC1, 0x00]), "0");
        assert_eq!(decode_decimal(&[0xC0, 0x01]), "0.00");
        // 负数
        assert_eq!(decode_decimal(&[0x3D, 0x02, 0x33]), "-9950");
        assert_eq!(decode_decimal(&[0x3E, 0x63, 0x66]), "-2");
    }

    #[test]
    fn interval_decode() {
        let mut b = Vec::new();
        for v in [1i32, 2, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0);
        assert_eq!(decode_interval_ym(&b), "1-2");

        let mut b = Vec::new();
        for v in [1i32, 2, 3, 4, 567890, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0);
        assert_eq!(decode_interval_dt(&b), "1 02:03:04.567890");
    }

    #[test]
    fn parse_columns_real_capture() {
        // 抓包实测（select 1 响应 cmd=187）：INT 列（列名为字面量 "1"）
        let body = [
            0x07, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x07, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x31, 0x49, 0x4e, 0x54, 0x45, 0x47, 0x45, 0x52, 0x00, 0x00,
            0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x04, 0x00,
            0x01, 0x00, 0x00, 0x00,
        ];
        let enc: &'static Encoding = encoding_rs::GB18030;
        let (cols, pos) = parse_columns(&body, 0, 1, enc).unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].col_type, DM_INT);
        assert_eq!(cols[0].prec, 4);
        assert_eq!(cols[0].name, "1");
        assert_eq!(cols[0].type_name, "INTEGER");
        assert_eq!(cols[0].table_name, "");
        assert_eq!(cols[0].schema_name, "");
        // 行数据
        let rows = parse_rows(&body, pos, 1, 1).unwrap();
        assert_eq!(rows, vec![vec![Some(vec![1, 0, 0, 0])]]);
    }

    #[test]
    fn parse_rows_param_real_capture() {
        // 抓包实测（my_lob SELECT content，1 列 CLOB 行内值 83 字节）
        let body = hex2b(
            "610002000000000000000c00530001020403000000000028000000000000000000000002040000010002000000000000000000000000000000d5e2cac74c4f42b2e2cad4c4dac8dda3acb0fcbaacd6d0cec4bacd456e676c697368bbecbacfa1a3",
        );
        let rows = parse_rows_param(&body, 0, 1).unwrap();
        assert_eq!(rows.len(), 1);
        let cell = rows[0][0].as_ref().unwrap();
        assert_eq!(cell.len(), 83);
        assert_eq!(cell[0], 0x01); // inRow 标志
        // 长度 @9 = 40
        assert_eq!(u32::from_le_bytes(cell[9..13].try_into().unwrap()), 40);
        // 数据 @43："这是LOB测试内容，包含中文和English混合。"
        let (s, _, _) = encoding_rs::GB18030.decode(&cell[43..]);
        assert_eq!(s, "这是LOB测试内容，包含中文和English混合。");
    }

    fn hex2b(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn gb18030_scalar_decode() {
        let enc: &'static Encoding = encoding_rs::GB18030;
        let (s, _, _) = enc.decode(&[0xb2, 0xe2, 0xca, 0xd4]);
        assert_eq!(s, "测试");
        // GB18030 4 字节字符 U+20000
        let (s, _, _) = enc.decode(&[0x95, 0x32, 0x82, 0x36]);
        assert_eq!(s, "\u{20000}");
    }

    #[test]
    fn tz_and_datetime2_decode() {
        // 向量由 Python 参考实现生成（dmpy._enc_time/_enc_datetime/_enc_date）
        // TIME_TZ：TIME(5B) + int16 时区（分钟，东八区 +480 → e0 01）
        let t = decode_time_tz(&hex2b("4cc4111418e001"));
        assert_eq!((t.hour, t.minute, t.second, t.microsecond), (12, 34, 56, 789000));
        assert_eq!(t.timezone_minutes, Some(480));
        assert!(t.to_string().ends_with("+08:00"), "{}", t);
        let t2 = decode_time_tz(&hex2b("4cc4111418d4fe"));
        assert_eq!(t2.timezone_minutes, Some(-300));
        assert!(t2.to_string().ends_with("-05:00"), "{}", t2);

        // DATETIME_TZ：DATETIME(8B) + int16 时区
        let dt = decode_datetime_tz(&hex2b("ea07744cc4111418e001"));
        assert_eq!((dt.date.year, dt.date.month, dt.date.day), (2026, 8, 14));
        assert_eq!(dt.time.hour, 12);
        assert_eq!(dt.time.timezone_minutes, Some(480));
        assert!(dt.to_string().ends_with("+08:00"), "{}", dt);

        // DATETIME2：9B，纳秒 30 位直接存储（789000us = 789_000_000ns）
        let dt2 = decode_datetime2(&hex2b("ea07744cc4815e0e5e"));
        assert_eq!(dt2.date.year, 2026);
        assert_eq!(dt2.time.microsecond, 789000);
        assert_eq!(dt2.time.timezone_minutes, None);

        // DATETIME2_TZ：11B = DATETIME2(9B) + tz
        let dtz = decode_datetime2_tz(&hex2b("ea07744cc4815e0e5ee001"));
        assert_eq!(dtz.time.microsecond, 789000);
        assert_eq!(dtz.time.timezone_minutes, Some(480));
        assert!(dtz.to_string().ends_with("+08:00"));
    }

    #[test]
    fn tz_and_datetime2_encode_roundtrip() {
        // TIME_TZ：TIME(5B) + int16 时区（东八区 +480 → e0 01）
        let mut t = DmTime::new(12, 34, 56, 789000);
        t.timezone_minutes = Some(480);
        assert_eq!(encode_time_tz(&t), hex2b("4cc4111418e001"));
        assert_eq!(decode_time_tz(&encode_time_tz(&t)), t);

        // DATETIME_TZ：DATETIME(8B) + int16 时区
        let mut dt = DmDateTime::new(DmDate::new(2026, 8, 14), DmTime::new(12, 34, 56, 789000));
        dt.time.timezone_minutes = Some(-300);
        assert_eq!(encode_datetime_tz(&dt), hex2b("ea07744cc4111418d4fe"));
        assert_eq!(decode_datetime_tz(&encode_datetime_tz(&dt)), dt);

        // DATETIME2：9B 纳秒 30 位直接存储（789000us = 789_000_000ns）
        let dt2 = DmDateTime::new(DmDate::new(2026, 8, 14), DmTime::new(12, 34, 56, 789000));
        assert_eq!(encode_datetime2(&dt2), hex2b("ea07744cc4815e0e5e"));
        assert_eq!(decode_datetime2(&encode_datetime2(&dt2)), dt2);

        // DATETIME2_TZ：11B = DATETIME2(9B) + tz
        let mut dtz = dt2;
        dtz.time.timezone_minutes = Some(480);
        assert_eq!(encode_datetime2_tz(&dtz), hex2b("ea07744cc4815e0e5ee001"));
        assert_eq!(decode_datetime2_tz(&encode_datetime2_tz(&dtz)), dtz);

        // 无时区：TZ 编码退化为偏移 0
        let plain = DmTime::new(1, 2, 3, 0);
        assert_eq!(encode_time_tz(&plain).len(), 7);
        assert_eq!(&encode_time_tz(&plain)[5..], &[0u8, 0]);
    }

    #[test]
    fn decimal_encode_vectors() {
        // 对照 Python/Go：1.50 → c1 02 33，0 → 80，-2 → 3e 63 66
        assert_eq!(enc_decimal("1.5"), vec![0xC1, 0x02, 0x33]);
        assert_eq!(enc_decimal("1.50"), vec![0xC1, 0x02, 0x33]);
        assert_eq!(enc_decimal("1"), vec![0xC1, 0x02]);
        assert_eq!(enc_decimal("0"), vec![0x80]);
        assert_eq!(enc_decimal("0.00"), vec![0x80]);
        assert_eq!(enc_decimal("-2"), vec![0x3E, 0x63, 0x66]);
        assert_eq!(enc_decimal("9950"), vec![0xC2, 0x64, 0x33]);
        assert_eq!(enc_decimal("0.0015"), vec![0xBF, 0x10]);
        assert_eq!(enc_decimal("100.5"), vec![0xC2, 0x02, 0x01, 0x33]);
        assert_eq!(enc_decimal("-100.5"), vec![0x3D, 0x64, 0x65, 0x33, 0x66]);
        // 科学计数
        assert_eq!(enc_decimal("1e-2"), enc_decimal("0.01"));
        assert_eq!(enc_decimal("1.5E1"), enc_decimal("15"));
        // 解码往返（用 result::decode_decimal）
        for v in ["1.5", "9950", "-2", "0.0015", "12345678901234567890.1234567890", "-100.5"] {
            let enc = enc_decimal(v);
            assert_eq!(crate::result::decode_decimal(&enc), crate::result::decode_decimal(&enc));
        }
        assert_eq!(crate::result::decode_decimal(&enc_decimal("1.5")), "1.50");
        assert_eq!(crate::result::decode_decimal(&enc_decimal("-2")), "-2");
        assert_eq!(crate::result::decode_decimal(&enc_decimal("9950")), "9950");
    }

    #[test]
    fn strict_text_decode_rejects_invalid_bytes() {
        // 合法 UTF-8 必须原样通过，不能被替换或改写。
        assert_eq!(
            decode_text_strict(encoding_rs::UTF_8, "北京星辰".as_bytes()).expect("valid utf8"),
            "北京星辰"
        );
        // UTF-8 会话下遇到 GBK 字节时必须报错，而不是静默返回替换字符。
        let error = decode_text_strict(encoding_rs::UTF_8, &[0xb1, 0xb1, 0xbe, 0xa9])
            .expect_err("gbk bytes are invalid utf8");
        assert!(error.msg.contains("UTF-8"), "{}", error.msg);
        // 合法 GB18030 字节在 GB18030 会话下应正常解码。
        assert_eq!(
            decode_text_strict(encoding_rs::GB18030, &[0xb1, 0xb1, 0xbe, 0xa9])
                .expect("valid gb18030"),
            "北京"
        );
    }
}
