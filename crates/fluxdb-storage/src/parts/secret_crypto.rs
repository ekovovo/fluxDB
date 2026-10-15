//! 共享数据密钥加载：连接凭据与模型 API Key 共用同一把 AES-256-GCM 数据密钥
//! （设计文档 §33）。
//!
//! 规则：
//! - 读取 `config.toml` 的 `secret_key`；缺失或没有该键 → 用内置 `DEFAULT_KEY`
//!   （只避免 SQLite 明文，不能抵御同时拿到程序与库的攻击者）；
//! - 本期不做密钥轮换，也不改写已有连接凭据。

use std::fs;
use std::path::Path;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::KeyInit;

use crate::credential::CredentialError;

/// 共享数据密钥（连接 + 模型共用），存在 `config.toml` 的该键下。
pub const CONFIG_KEY: &str = "secret_key";
/// 内置密钥，仅避免 SQLite 明文存储。
pub const DEFAULT_KEY: [u8; 32] = *b"fluxdb-default-aes256-key-v1-001";

/// 密钥来源，用于日志提示（不含密钥内容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `secret_key`
    Config,
    /// 内置默认密钥
    Builtin,
}

/// 读取共享数据密钥并构造 AES-GCM 实例。
pub fn load_cipher(root: &Path) -> Result<(Aes256Gcm, KeySource), CredentialError> {
    let (key, source) = load_key(root)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| invalid_key())?;
    Ok((cipher, source))
}

/// 只取原始 32 字节密钥（测试与密钥初始化流程使用）。
pub(crate) fn load_key(root: &Path) -> Result<([u8; 32], KeySource), CredentialError> {
    let text = match fs::read_to_string(root.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((DEFAULT_KEY, KeySource::Builtin));
        }
        Err(e) => return Err(CredentialError::Failure(format!("读取密钥配置失败: {e}"))),
    };
    let config: toml::Value = toml::from_str(&text)
        .map_err(|_| CredentialError::Failure("config.toml 格式无效".into()))?;

    let Some(primary) = config.get(CONFIG_KEY).and_then(toml::Value::as_str) else {
        return Ok((DEFAULT_KEY, KeySource::Builtin));
    };
    Ok((decode_key(primary)?, KeySource::Config))
}

fn decode_key(hex: &str) -> Result<[u8; 32], CredentialError> {
    let bytes = hex.as_bytes();
    if bytes.is_empty() || bytes.len() % 2 != 0 {
        return Err(invalid_key());
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[i * 2..i * 2 + 2]).map_err(|_| invalid_key())?;
        *slot = u8::from_str_radix(pair, 16).map_err(|_| invalid_key())?;
    }
    Ok(out)
}

fn invalid_key() -> CredentialError {
    CredentialError::Failure(format!(
        "{CONFIG_KEY} 必须是 64 位十六进制字符串（32 字节）"
    ))
}

/// 连接凭据的历史 AAD（保持不变，密文按此认证）。
pub(crate) fn connection_aad(connection_id: u64, secret_kind: &str) -> Vec<u8> {
    format!("fluxdb-connection-secret\0v1\0{connection_id}\0{secret_kind}").into_bytes()
}

/// 模型 API Key 的 AAD：绑定域、模型、密钥代次，避免密文跨行搬运复用。
///
/// 编码使用长度前缀，避免分隔符歧义（旧连接 AAD 是历史格式，不改）。
pub(crate) fn model_aad(profile_id: &str, key_id: &str, secret_revision: i64) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(b"fluxdb-ai-model-key\0v1\0");
    for part in [profile_id, key_id] {
        aad.extend_from_slice(part.len().to_string().as_bytes());
        aad.push(b':');
        aad.extend_from_slice(part.as_bytes());
        aad.push(0);
    }
    aad.extend_from_slice(secret_revision.to_string().as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str) {
        fs::create_dir_all(dir).expect("创建目录");
        fs::write(dir.join("config.toml"), body).expect("写配置");
    }

    /// 生成 32 字节（全为 byte）的十六进制串。
    fn hex_of(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    #[test]
    fn missing_config_uses_builtin_key() {
        let dir = std::env::temp_dir().join(format!("fluxdb-key-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (key, source) = load_key(&dir).expect("无配置应回退内置");
        assert_eq!(key, DEFAULT_KEY);
        assert_eq!(source, KeySource::Builtin);
    }

    #[test]
    fn primary_key_is_preferred() {
        let dir = std::env::temp_dir().join(format!("fluxdb-key-a-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_config(&dir, &format!("{CONFIG_KEY} = \"{}\"\n", hex_of(1)));
        let (key, source) = load_key(&dir).expect("读取主字段");
        assert_eq!(key, [1u8; 32]);
        assert_eq!(source, KeySource::Config);
    }

    #[test]
    fn invalid_hex_is_rejected() {
        let dir = std::env::temp_dir().join(format!("fluxdb-key-e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_config(&dir, &format!("{CONFIG_KEY} = \"zz\"\n"));
        assert!(load_key(&dir).is_err());
    }

    #[test]
    fn model_aad_is_unambiguous() {
        let a = model_aad("ab", "c", 1);
        let b = model_aad("a", "bc", 1);
        assert_ne!(a, b, "不同切分不能产生相同 AAD");
        assert_ne!(model_aad("m", "k", 1), model_aad("m", "k", 2));
    }
}
