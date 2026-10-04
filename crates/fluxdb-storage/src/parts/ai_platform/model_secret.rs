//! 模型 API Key 的加密行存取（设计文档 §33–§34、T008）。
//!
//! 明文 Key 只经 [`SecretInput`] 进入本模块，落库一律为 AES-256-GCM 密文；
//! AAD 绑定模型行与密钥代次，密文不能跨模型搬运复用。

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

use crate::credential::CredentialError;

/// 密文版本，与 `ai_model_profiles.cipher_version` 对应。
pub const CIPHER_VERSION: i64 = 1;
const NONCE_LEN: usize = 12;

/// 明文 API Key 的内存载体：不实现 Serialize，Debug 永远脱敏。
#[derive(Clone)]
pub struct SecretInput(String);

impl SecretInput {
    /// 包装明文；空值视为无效（由调用方给出可见错误）。
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CredentialError::Failure("API Key 不能为空".into()));
        }
        Ok(Self(value))
    }

    /// 仅在加解密路径内短暂借用明文。
    pub(crate) fn expose(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// 落库前的加密结果（对应模型行的加密列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedSecret {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub cipher_version: i64,
    pub key_id: String,
    pub secret_revision: i64,
}

/// 用共享数据密钥加密模型 Key；每次生成新的 12 字节随机 nonce。
pub fn seal(
    cipher: &Aes256Gcm,
    profile_id: &str,
    key_id: &str,
    secret_revision: i64,
    input: &SecretInput,
) -> Result<EncryptedSecret, CredentialError> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    let aad = crate::secret_crypto::model_aad(profile_id, key_id, secret_revision);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: input.expose(),
                aad: &aad,
            },
        )
        .map_err(|_| CredentialError::Failure("模型 API Key 加密失败".into()))?;
    Ok(EncryptedSecret {
        ciphertext,
        nonce: nonce.to_vec(),
        cipher_version: CIPHER_VERSION,
        key_id: key_id.to_string(),
        secret_revision,
    })
}

/// 解密模型行中的 Key。AAD 与行不匹配（换模型/换代次）一律失败。
pub fn open(
    cipher: &Aes256Gcm,
    profile_id: &str,
    secret: &EncryptedSecret,
) -> Result<String, CredentialError> {
    if secret.cipher_version != CIPHER_VERSION {
        return Err(CredentialError::Failure(format!(
            "模型凭据密文版本 {} 不受支持",
            secret.cipher_version
        )));
    }
    if secret.nonce.len() != NONCE_LEN {
        return Err(CredentialError::Failure("模型凭据 nonce 长度异常".into()));
    }
    let aad = crate::secret_crypto::model_aad(profile_id, &secret.key_id, secret.secret_revision);
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&secret.nonce),
            Payload {
                msg: &secret.ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| {
            CredentialError::Failure("模型凭据认证失败（密钥错误、密文损坏或行不匹配）".into())
        })?;
    String::from_utf8(plaintext)
        .map_err(|_| CredentialError::Failure("模型凭据解密结果不是 UTF-8".into()))
}

/// 凭据状态：UI/Runtime 只能看到这个，不接触密文或明文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    NotRequired,
    Missing,
    Configured,
    Unavailable,
}

impl CredentialStatus {
    /// 由“是否需要凭据 + 是否存在密文”推导，不做解密。
    pub fn from_stored(auth_requires_key: bool, has_ciphertext: bool) -> Self {
        match (auth_requires_key, has_ciphertext) {
            (false, _) => Self::NotRequired,
            (true, true) => Self::Configured,
            (true, false) => Self::Missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_crypto::{KeySource, load_cipher};

    fn cipher() -> Aes256Gcm {
        let dir = std::env::temp_dir().join(format!("fluxdb-model-secret-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (cipher, source) = load_cipher(&dir).expect("加载密钥");
        assert!(matches!(source, KeySource::Builtin | KeySource::Config));
        cipher
    }

    #[test]
    fn roundtrip_recovers_plaintext() {
        let c = cipher();
        let input = SecretInput::new("sk-demo-1234567890").unwrap();
        let sealed = seal(&c, "model-1", "k1", 1, &input).expect("加密");
        assert_ne!(sealed.ciphertext, b"sk-demo-1234567890");
        let opened = open(&c, "model-1", &sealed).expect("解密");
        assert_eq!(opened, "sk-demo-1234567890");
    }

    #[test]
    fn nonce_is_fresh_each_seal() {
        let c = cipher();
        let input = SecretInput::new("same-key").unwrap();
        let a = seal(&c, "m", "k", 1, &input).unwrap();
        let b = seal(&c, "m", "k", 1, &input).unwrap();
        assert_ne!(a.nonce, b.nonce, "相同明文也不得复用 nonce");
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn ciphertext_cannot_move_to_another_profile() {
        let c = cipher();
        let input = SecretInput::new("sk-x").unwrap();
        let sealed = seal(&c, "model-a", "k1", 1, &input).unwrap();
        assert!(
            open(&c, "model-b", &sealed).is_err(),
            "换模型行必须解密失败"
        );
    }

    #[test]
    fn revision_change_invalidates_ciphertext() {
        let c = cipher();
        let input = SecretInput::new("sk-y").unwrap();
        let sealed = seal(&c, "model-a", "k1", 1, &input).unwrap();
        let bumped = EncryptedSecret {
            secret_revision: 2,
            ..sealed.clone()
        };
        assert!(open(&c, "model-a", &bumped).is_err(), "换代次必须解密失败");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let c = cipher();
        let input = SecretInput::new("sk-z").unwrap();
        let mut sealed = seal(&c, "model-a", "k1", 1, &input).unwrap();
        sealed.ciphertext[0] ^= 0xff;
        assert!(open(&c, "model-a", &sealed).is_err());
    }

    #[test]
    fn empty_secret_rejected() {
        assert!(SecretInput::new("").is_err());
    }

    #[test]
    fn secret_input_debug_is_redacted() {
        let input = SecretInput::new("sk-secret-value").unwrap();
        assert_eq!(format!("{input:?}"), "[REDACTED]");
        assert!(!format!("{input:?}").contains("sk-secret"));
    }

    #[test]
    fn credential_status_derivation() {
        assert_eq!(
            CredentialStatus::from_stored(false, false),
            CredentialStatus::NotRequired
        );
        assert_eq!(
            CredentialStatus::from_stored(true, false),
            CredentialStatus::Missing
        );
        assert_eq!(
            CredentialStatus::from_stored(true, true),
            CredentialStatus::Configured
        );
    }
}
