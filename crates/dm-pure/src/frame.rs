//! 达梦 DM8 协议帧封装（64 字节定长头 + body）。
//!
//! 帧格式（对照抓包 + Go 驱动源码验证）：
//!
//! ```text
//! 偏移  类型   字段
//! 0     int32  语句ID (stmt_id)
//! 4     int16  命令号 (cmd)
//! 6     int32  body 长度
//! 10    int32  返回码
//! 19    byte   XOR 校验 (偏移 0..18 逐字节异或)
//! 20..63       参数区（消息参数写这里，最多 44 字节）
//! 64+          body
//! ```

use std::io::{Read, Write};
use std::net::TcpStream;

use crate::{DmError, Result};

pub const FRAME_HEADER_LEN: usize = 64;
pub const CMD_HANDSHAKE: i16 = 200;
pub const CMD_LOGIN: i16 = 1;
pub const CMD_SESSION_QUERY: i16 = 3;
pub const CMD_EXECUTE: i16 = 5;
pub const CMD_EXEC_COMPLETE: i16 = 4;
pub const CMD_FETCH: i16 = 44;
pub const CMD_BIND: i16 = 13;
pub const MAX_MSG_LEN: usize = 0x2000_0000;

/// 构造 64 字节帧头。`opts` 为参数区内容（最多 44 字节），其余补零。
pub fn make_header(stmt_id: i32, cmd: i16, body_len: u32, opts: &[u8]) -> [u8; 64] {
    debug_assert!(opts.len() <= 44, "参数区最大 44 字节");
    let mut hdr = [0u8; 64];
    hdr[0..4].copy_from_slice(&(stmt_id as u32).to_le_bytes());
    hdr[4..6].copy_from_slice(&(cmd as u16).to_le_bytes());
    hdr[6..10].copy_from_slice(&body_len.to_le_bytes());
    if !opts.is_empty() {
        let n = opts.len().min(44);
        hdr[20..20 + n].copy_from_slice(&opts[..n]);
    }
    // XOR 校验：偏移 0..18 逐字节异或
    let mut xor = 0u8;
    for &b in &hdr[..19] {
        xor ^= b;
    }
    hdr[19] = xor;
    hdr
}

/// 组装完整帧：`opts`=参数区(20-63)，`body`=帧数据区(64+)。
pub fn encode_frame(stmt_id: i32, cmd: i16, opts: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + body.len());
    out.extend_from_slice(&make_header(stmt_id, cmd, body.len() as u32, opts));
    out.extend_from_slice(body);
    out
}

/// 一帧解析结果。
#[derive(Debug, Clone)]
pub struct Frame {
    pub stmt_id: i32,
    pub cmd: i16,
    pub length: u32,
    pub retcode: i32,
    /// 参数区 20-63
    pub opts: Vec<u8>,
    /// 数据区 64+
    pub body: Vec<u8>,
}

/// 从 socket 流中读取完整帧（自带缓冲，处理粘包/半包）。
pub struct FrameReader {
    stream: TcpStream,
    buf: Vec<u8>,
    recv_size: usize,
}

impl FrameReader {
    pub fn new(stream: TcpStream) -> Self {
        FrameReader { stream, buf: Vec::with_capacity(65536), recv_size: 65536 }
    }

    /// 确保缓冲至少有 `n` 字节；不足则从 socket 读取。返回是否足够。
    fn fill(&mut self, n: usize) -> std::io::Result<bool> {
        while self.buf.len() < n {
            let mut chunk = vec![0u8; self.recv_size];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(k) => self.buf.extend_from_slice(&chunk[..k]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// 读取一帧。连接关闭抛 `DmError::conn`。
    pub fn read_frame(&mut self) -> Result<Frame> {
        if !self.fill(FRAME_HEADER_LEN)? {
            return Err(DmError::conn("连接关闭（读取帧头）"));
        }
        let stmt_id = i32::from_le_bytes(self.buf[0..4].try_into().unwrap());
        let cmd = i16::from_le_bytes(self.buf[4..6].try_into().unwrap());
        let length = u32::from_le_bytes(self.buf[6..10].try_into().unwrap());
        let retcode = i32::from_le_bytes(self.buf[10..14].try_into().unwrap());
        let mut xor = 0u8;
        for &b in &self.buf[..19] {
            xor ^= b;
        }
        if xor != self.buf[19] {
            return Err(DmError::protocol(format!("XOR 校验失败 cmd={cmd}")));
        }
        if length as usize > MAX_MSG_LEN {
            return Err(DmError::protocol(format!("帧过长: {length}")));
        }
        let total = FRAME_HEADER_LEN + length as usize;
        if !self.fill(total)? {
            return Err(DmError::conn("连接关闭（读取body）"));
        }
        let opts = self.buf[20..64].to_vec();
        let body = self.buf[64..total].to_vec();
        self.buf.drain(..total);
        Ok(Frame { stmt_id, cmd, length, retcode, opts, body })
    }

    /// 发送一帧（一次 write，配合 TCP_NODELAY 保证独立 TCP 段）。
    pub fn send_frame(&mut self, stmt_id: i32, cmd: i16, opts: &[u8], body: &[u8]) -> Result<()> {
        let frame = encode_frame(stmt_id, cmd, opts, body);
        self.stream.write_all(&frame)?;
        self.stream.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_and_xor() {
        let hdr = make_header(3, CMD_EXECUTE, 1024, &[0x01, 0x00]);
        // stmt_id=3, cmd=5, len=1024
        assert_eq!(&hdr[0..4], &3u32.to_le_bytes());
        assert_eq!(&hdr[4..6], &5u16.to_le_bytes());
        assert_eq!(&hdr[6..10], &1024u32.to_le_bytes());
        // opts 写入参数区
        assert_eq!(hdr[20], 0x01);
        assert_eq!(hdr[21], 0x00);
        // XOR 校验
        let mut xor = 0u8;
        for &b in &hdr[..19] {
            xor ^= b;
        }
        assert_eq!(hdr[19], xor);
    }

    #[test]
    fn encode_roundtrip() {
        let f = encode_frame(1, CMD_LOGIN, &[1, 2, 3], b"hello");
        assert_eq!(f.len(), 64 + 5);
        assert_eq!(i32::from_le_bytes(f[0..4].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(f[4..6].try_into().unwrap()), CMD_LOGIN);
        assert_eq!(u32::from_le_bytes(f[6..10].try_into().unwrap()), 5);
        assert_eq!(&f[64..], b"hello");
    }
}
