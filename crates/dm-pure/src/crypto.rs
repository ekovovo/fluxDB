//! 达梦登录加密（移植自 dm-pure Python 驱动 / dm-go-driver security 包）。
//!
//! - DH 密钥交换（固定参数 P/G）
//! - 会话密钥 = serverPub ^ clientPriv mod P
//! - 对称加密：DES/AES/3DES + ECB/CBC/CFB/OFB + PKCS5 + 固定 IV + MD5 摘要
//! - 客户端登录实际使用 `DES_CFB`（算法 ID=132）

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes192, Aes256};
use des::{Des, TdesEde3};
use num_bigint::BigUint;
use rand::RngCore;

use crate::{DmError, Result};

/// DH 固定参数 P（Go 源码 zzd.go）
pub const DH_P_HEX: &str = "C009D877BAF5FAF416B7F778E6115DCB90D65217DCC2F08A9DFCB5A192C593EB\
                           AB02929266B8DBFC2021039FDBD4B7FDE2B996E00008F57AE6EFB4ED3F17B6D3";
pub const DH_G: u32 = 5;
pub const DH_KEY_LENGTH: usize = 64;

// 算法 ID（Go 源码 zzd.go）
pub const WORK_MODE_MASK: u16 = 0x007F;
pub const ECB_MODE: u16 = 0x1;
pub const CBC_MODE: u16 = 0x2;
pub const CFB_MODE: u16 = 0x4;
pub const OFB_MODE: u16 = 0x8;
pub const ALGO_MASK: u16 = 0xFF80;
pub const DES_ID: u16 = 0x0080;
pub const DES3_ID: u16 = 0x0100;
pub const AES128: u16 = 0x0200;
pub const AES192: u16 = 0x0400;
pub const AES256: u16 = 0x0800;
pub const RC4: u16 = 0x1000;

/// 用户名密码加密：DES + CFB
pub const DES_CFB: u16 = 132;
pub const MD5_DIGEST_SIZE: usize = 16;

/// 固定 IV（Go 源码 zzd.go defaultIV：跳过 0x22，末尾补 0x20）
pub const DEFAULT_IV: [u8; 32] = [
    0x20, 0x21, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e,
    0x3f, 0x20,
];

pub fn dh_p() -> BigUint {
    BigUint::parse_bytes(DH_P_HEX.as_bytes(), 16).expect("DH_P 解析失败")
}

/// 生成客户端 DH 密钥对，返回 (私钥 x, 公钥 G^x mod P 的 64 字节大端编码)。
pub fn new_client_keypair() -> (BigUint, Vec<u8>) {
    let p = dh_p();
    let mut buf = [0u8; DH_KEY_LENGTH];
    rand::thread_rng().fill_bytes(&mut buf);
    let x = BigUint::from_bytes_be(&buf) % (&p - 1u32) + 1u32;
    let pub_bn = BigUint::from(DH_G).modpow(&x, &p);
    (x, bn_to_64_bytes(&pub_bn))
}

/// 大整数转固定 64 字节大端（Go Bn2Bytes）
pub fn bn_to_64_bytes(bn: &BigUint) -> Vec<u8> {
    let raw = bn.to_bytes_be();
    let mut out = vec![0u8; DH_KEY_LENGTH];
    let n = raw.len().min(DH_KEY_LENGTH);
    out[DH_KEY_LENGTH - n..].copy_from_slice(&raw[raw.len() - n..]);
    out
}

/// 会话密钥 = serverPub^x mod P（Go Bn2Bytes(bn, 0)：不补零，取有效字节）。
pub fn compute_session_key(client_priv_x: &BigUint, server_pub: &[u8]) -> Vec<u8> {
    let p = dh_p();
    let srv = BigUint::from_bytes_be(server_pub);
    let key = srv.modpow(client_priv_x, &p);
    key.to_bytes_be()
}

/// PKCS5 padding（块大小：DES=8 / AES=16）
pub fn pkcs5_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad_len = block_size - (data.len() % block_size);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    out
}

/// PKCS5 unpad
pub fn pkcs5_unpad(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Err(DmError::protocol("pkcs5_unpad: 空数据"));
    }
    let pad_len = data[data.len() - 1] as usize;
    if pad_len == 0 || pad_len > data.len() {
        return Err(DmError::protocol("pkcs5_unpad: 非法填充"));
    }
    Ok(data[..data.len() - pad_len].to_vec())
}

/// 对称加密通道（对应 Go security.SymmCipher）
pub struct SymmCipher {
    algo: u16,
    mode: u16,
    key: Vec<u8>,
}

impl SymmCipher {
    pub fn new(algorithm_id: u16, key: Vec<u8>) -> Self {
        SymmCipher {
            algo: algorithm_id & ALGO_MASK,
            mode: algorithm_id & WORK_MODE_MASK,
            key,
        }
    }

    pub fn block_size(&self) -> usize {
        match self.algo {
            DES_ID | DES3_ID => 8,
            AES128 | AES192 | AES256 => 16,
            _ => 8,
        }
    }

    fn need_padding(&self) -> bool {
        self.mode == ECB_MODE || self.mode == CBC_MODE
    }

    /// 加密。`gen_digest=true` 时在尾部附加明文 MD5 摘要（Go 行为）。
    pub fn encrypt(&self, plaintext: &[u8], gen_digest: bool) -> Result<Vec<u8>> {
        let data = if self.need_padding() {
            pkcs5_pad(plaintext, self.block_size())
        } else {
            plaintext.to_vec()
        };
        let mut ret = self.crypt(&data, true)?;
        if gen_digest {
            let digest = md5::compute(plaintext);
            ret.extend_from_slice(&digest.0);
        }
        Ok(ret)
    }

    /// 解密。`check_digest=true` 时校验尾部 16 字节 MD5 摘要。
    pub fn decrypt(&self, ciphertext: &[u8], check_digest: bool) -> Result<Vec<u8>> {
        let (data, digest) = if check_digest {
            if ciphertext.len() < MD5_DIGEST_SIZE {
                return Err(DmError::protocol("密文过短，无摘要"));
            }
            ciphertext.split_at(ciphertext.len() - MD5_DIGEST_SIZE)
        } else {
            (ciphertext, &[][..])
        };
        let mut ret = self.crypt(data, false)?;
        if self.need_padding() {
            ret = pkcs5_unpad(&ret)?;
        }
        if check_digest {
            let got = md5::compute(&ret);
            if got.0.as_slice() != digest {
                return Err(DmError::protocol("MD5 摘要校验失败"));
            }
        }
        Ok(ret)
    }

    // ---------- 底层块/流加密 ----------

    fn block_encrypt(&self, block: &[u8]) -> Result<Vec<u8>> {
        match self.algo {
            DES_ID => Ok(des_crypt(Des::new(GenericArray::from_slice(&self.key[..8])), block, true)),
            DES3_ID => Ok(des_crypt(TdesEde3::new(GenericArray::from_slice(&self.key[..24])), block, true)),
            AES128 => Ok(aes_crypt(Aes128::new(GenericArray::from_slice(&self.key[..16])), block, true)),
            AES192 => Ok(aes_crypt(Aes192::new(GenericArray::from_slice(&self.key[..24])), block, true)),
            AES256 => Ok(aes_crypt(Aes256::new(GenericArray::from_slice(&self.key[..32])), block, true)),
            RC4 => Err(DmError::protocol("RC4 未在 Rust 版实现（登录仅用 DES_CFB）")),
            _ => Err(DmError::protocol(format!("不支持的算法ID: {}", self.algo))),
        }
    }

    fn block_decrypt(&self, block: &[u8]) -> Result<Vec<u8>> {
        match self.algo {
            DES_ID => Ok(des_crypt(Des::new(GenericArray::from_slice(&self.key[..8])), block, false)),
            DES3_ID => Ok(des_crypt(TdesEde3::new(GenericArray::from_slice(&self.key[..24])), block, false)),
            AES128 => Ok(aes_crypt(Aes128::new(GenericArray::from_slice(&self.key[..16])), block, false)),
            AES192 => Ok(aes_crypt(Aes192::new(GenericArray::from_slice(&self.key[..24])), block, false)),
            AES256 => Ok(aes_crypt(Aes256::new(GenericArray::from_slice(&self.key[..32])), block, false)),
            RC4 => Err(DmError::protocol("RC4 未在 Rust 版实现（登录仅用 DES_CFB）")),
            _ => Err(DmError::protocol(format!("不支持的算法ID: {}", self.algo))),
        }
    }

    fn crypt(&self, data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
        let bs = self.block_size();
        let iv = DEFAULT_IV[..bs].to_vec();
        match self.mode {
            ECB_MODE => {
                let mut out = Vec::with_capacity(data.len());
                for chunk in data.chunks(bs) {
                    let b = if encrypt { self.block_encrypt(chunk)? } else { self.block_decrypt(chunk)? };
                    out.extend_from_slice(&b);
                }
                Ok(out)
            }
            CBC_MODE => {
                let mut out = Vec::with_capacity(data.len());
                let mut prev = iv;
                for chunk in data.chunks(bs) {
                    if encrypt {
                        let mut block = chunk.to_vec();
                        for i in 0..bs {
                            block[i] ^= prev[i];
                        }
                        let enc = self.block_encrypt(&block)?;
                        prev = enc.clone();
                        out.extend_from_slice(&enc);
                    } else {
                        let dec = self.block_decrypt(chunk)?;
                        let mut d = dec.clone();
                        for i in 0..bs {
                            d[i] ^= prev[i];
                        }
                        prev = chunk.to_vec();
                        out.extend_from_slice(&d);
                    }
                }
                Ok(out)
            }
            CFB_MODE => {
                // 标准 CFB（segment=块大小），与 Go cipher.NewCFBEncrypter/Decrypter 一致。
                // 寄存器始终用密文更新；加解密仅 XOR 方向不同。
                let mut reg = iv;
                let mut out = Vec::with_capacity(data.len());
                for chunk in data.chunks(bs) {
                    let ks = self.block_encrypt(&reg)?;
                    let n = chunk.len();
                    let mut ct = vec![0u8; n];
                    for i in 0..n {
                        ct[i] = chunk[i] ^ ks[i];
                    }
                    out.extend_from_slice(&ct);
                    if encrypt {
                        reg[..n].copy_from_slice(&ct);
                    } else {
                        reg[..n].copy_from_slice(chunk);
                    }
                    if n < bs {
                        reg[n..].copy_from_slice(&ks[n..]);
                    }
                }
                Ok(out)
            }
            OFB_MODE => {
                // Output Feedback：keystream 独立于明文，加解密相同
                let mut ks_reg = iv;
                let mut out = Vec::with_capacity(data.len());
                for chunk in data.chunks(bs) {
                    let next = self.block_encrypt(&ks_reg)?;
                    for (i, &b) in chunk.iter().enumerate() {
                        out.push(b ^ next[i]);
                    }
                    ks_reg = next[..bs].to_vec();
                }
                Ok(out)
            }
            _ => Err(DmError::protocol(format!("未实现的模式: {}", self.mode))),
        }
    }
}

fn des_crypt<C: BlockEncrypt + BlockDecrypt>(cipher: C, block: &[u8], encrypt: bool) -> Vec<u8> {
    let mut b = GenericArray::clone_from_slice(block);
    if encrypt {
        cipher.encrypt_block(&mut b);
    } else {
        cipher.decrypt_block(&mut b);
    }
    b.to_vec()
}

fn aes_crypt<C: BlockEncrypt + BlockDecrypt>(cipher: C, block: &[u8], encrypt: bool) -> Vec<u8> {
    let mut b = GenericArray::clone_from_slice(block);
    if encrypt {
        cipher.encrypt_block(&mut b);
    } else {
        cipher.decrypt_block(&mut b);
    }
    b.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_traits::One;

    #[test]
    fn des_cfb_matches_python_reference() {
        // 与 Python dmpy.crypto 参考实现交叉验证（key=0x00..0x3f, IV=DEFAULT_IV[:8]）
        let key: Vec<u8> = (0u8..64).collect();
        let c = SymmCipher::new(DES_CFB, key);
        assert_eq!(c.encrypt(b"SYSDBA", false).unwrap().hex(), "c67b0c7a4a04");
        assert_eq!(c.encrypt(b"password", false).unwrap().hex(), "e5432c4d7f2a6eb6");
        assert_eq!(c.encrypt(b"12345678", false).unwrap().hex(), "a4106c0a3d732bea");
        assert_eq!(c.encrypt(b"123456789", false).unwrap().hex(), "a4106c0a3d732bea3c");
    }

    #[test]
    fn des_cfb_digest_roundtrip() {
        let key: Vec<u8> = (0u8..64).collect();
        let c = SymmCipher::new(DES_CFB, key);
        let ct = c.encrypt(b"hello world", true).unwrap();
        assert_eq!(
            ct.hex(),
            "fd47335267656bbd6bbf765eb63bbbe01eeed093cb22bb8f5acdc3"
        );
        assert_eq!(c.decrypt(&ct, true).unwrap(), b"hello world");
    }

    #[test]
    fn dh_keypair_size() {
        let (_x, pub_bn) = new_client_keypair();
        assert_eq!(pub_bn.len(), 64);
        // 公钥值域应在 (1, P)
        let p = dh_p();
        let val = BigUint::from_bytes_be(&pub_bn);
        assert!(val > BigUint::one() && val < p);
    }

    #[test]
    fn dh_session_key_symmetry() {
        // 双方各自计算出的会话密钥应一致
        let (xa, pub_a) = new_client_keypair();
        let (xb, pub_b) = new_client_keypair();
        let k1 = compute_session_key(&xa, &pub_b);
        let k2 = compute_session_key(&xb, &pub_a);
        assert_eq!(k1, k2);
    }

    #[test]
    fn aes_modes_roundtrip() {
        let key = vec![0x42u8; 32];
        for (algo, mode) in [(AES128, CBC_MODE), (AES256, ECB_MODE), (AES128, CFB_MODE), (AES128, OFB_MODE)] {
            let c = SymmCipher::new(algo | mode, key.clone());
            let ct = c.encrypt(b"aes mode roundtrip data!", true).unwrap();
            let pt = c.decrypt(&ct, true).unwrap();
            assert_eq!(pt, b"aes mode roundtrip data!", "algo={algo} mode={mode}");
        }
    }

    #[test]
    fn des_modes_roundtrip() {
        let key = vec![0x11u8; 64];
        for mode in [ECB_MODE, CBC_MODE, CFB_MODE, OFB_MODE] {
            let c = SymmCipher::new(DES_ID | mode, key.clone());
            let ct = c.encrypt(b"des mode roundtrip!", true).unwrap();
            let pt = c.decrypt(&ct, true).unwrap();
            assert_eq!(pt, b"des mode roundtrip!", "mode={mode}");
        }
    }

    trait Hex {
        fn hex(&self) -> String;
    }
    impl Hex for [u8] {
        fn hex(&self) -> String {
            self.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
