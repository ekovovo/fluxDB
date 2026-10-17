// 连接凭据后端：生产写入仅使用 SQLite AES-256-GCM 密文表。
// 系统凭据库不再被访问；旧密码需由用户重输一次。
use std::fmt::Debug;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CredentialError {
    NotFound,
    Locked,
    Unavailable,
    Denied,
    Failure(String),
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "凭据条目不存在"),
            Self::Locked => write!(f, "凭据库已锁定"),
            Self::Unavailable => write!(f, "凭据存储不可用"),
            Self::Denied => write!(f, "凭据访问被拒绝"),
            Self::Failure(msg) => write!(f, "凭据操作失败: {msg}"),
        }
    }
}
impl std::error::Error for CredentialError {}

pub trait CredentialBackend: Debug + Send + Sync {
    fn read(&self, connection_id: u64, kind: &str) -> Result<Option<String>, CredentialError>;
    fn write(&self, connection_id: u64, kind: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, connection_id: u64, kind: &str) -> Result<(), CredentialError>;
}

/// 连接加密凭据后端。
pub fn backend(root: &std::path::Path) -> std::sync::Arc<dyn CredentialBackend> {
    std::sync::Arc::new(crate::connection_secrets::SqliteEncryptedBackend::new(
        root.to_path_buf(),
    ))
}

/// 将凭据错误映射为 storage 统一 Error（供上层传播给 UI/日志）：
/// Locked/Denied -> Permission；NotFound / Unavailable / Failure -> Internal。
pub fn to_storage_error(err: &CredentialError) -> crate::Error {
    let kind = match err {
        CredentialError::Locked | CredentialError::Denied => crate::ErrorKind::Permission,
        CredentialError::NotFound | CredentialError::Unavailable | CredentialError::Failure(_) => {
            crate::ErrorKind::Internal
        }
    };
    crate::Error::new(kind, err.to_string())
}
