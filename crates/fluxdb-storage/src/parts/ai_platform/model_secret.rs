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
