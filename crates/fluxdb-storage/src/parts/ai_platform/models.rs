//! 模型档案的读取与保存（设计文档 §17/§34、T009/T010）。
//!
//! 保存走对象级 CAS：`UPDATE ... WHERE id=? AND revision=?`，命中 0 行即冲突；
//! API Key 只有显式 Set/Clear 才动密文列，Keep 完全不触碰密文列。

use rusqlite::{Connection, OptionalExtension, params};

use super::model_secret::{self, CredentialStatus, EncryptedSecret, SecretInput};

use crate::credential::CredentialError;
use crate::sqlite::sqlite_error;
use aes_gcm::Aes256Gcm;
use fluxdb_core::{Error, ErrorKind, Result};

/// 模型用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Generation,
    Decision,
}

impl ModelKind {
    fn as_str(self) -> &'static str {
        match self {
            ModelKind::Generation => "generation",
            ModelKind::Decision => "decision",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "generation" => Ok(ModelKind::Generation),
            "decision" => Ok(ModelKind::Decision),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知模型用途: {other}"),
            )),
        }
    }
}

/// 认证方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    None,
    ApiKey,
}

impl AuthMode {
    fn as_str(self) -> &'static str {
        match self {
            AuthMode::None => "none",
            AuthMode::ApiKey => "api_key",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(AuthMode::None),
            "api_key" => Ok(AuthMode::ApiKey),
            other => Err(Error::new(
                ErrorKind::Internal,
                format!("未知认证方式: {other}"),
            )),
        }
    }
    fn requires_key(self) -> bool {
        matches!(self, AuthMode::ApiKey)
    }
}

/// 模型编辑输入（不含任何凭据字段）。
#[derive(Debug, Clone)]
pub struct ModelEdit {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub api_protocol: String,
    pub kind: ModelKind,
    pub model: String,
    pub endpoint: Option<String>,
    pub endpoint_is_full_url: bool,
    pub enabled: bool,
    pub auth_mode: AuthMode,
    pub context_window_tokens: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub input_price_micros_per_million: Option<i64>,
    pub output_price_micros_per_million: Option<i64>,
    pub cached_input_price_micros_per_million: Option<i64>,
}

/// 公开视图：不含密文列，也不含任何可还原的凭据材料。
#[derive(Debug, Clone)]
pub struct ModelProfileView {
    pub id: String,
    pub display_name: String,
    pub provider: String,
    pub api_protocol: String,
    pub kind: ModelKind,
    pub model: String,
    pub endpoint: Option<String>,
    pub endpoint_is_full_url: bool,
    pub enabled: bool,
    pub auth_mode: AuthMode,
    pub context_window_tokens: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub input_price_micros_per_million: Option<i64>,
    pub output_price_micros_per_million: Option<i64>,
    pub cached_input_price_micros_per_million: Option<i64>,
    pub credential_status: CredentialStatus,
    pub secret_revision: i64,
    /// 最近一次连接探测结果：`unknown` / `ok` / `failed`（运行时元数据）。
    pub connection_status: String,
    /// 最近一次连接探测时间戳（毫秒）；从未探测为 `None`。
    pub connection_checked_at_ms: Option<i64>,
    pub revision: i64,
}

/// API Key 的显式变更意图。
pub enum ApiKeyChange {
    /// 不动密文列（改名称/endpoint 等不影响已存凭据）。
    Keep,
    /// 用新明文替换并递增 secret_revision。
    Set(SecretInput),
    /// 清空密文并递增 secret_revision。
    Clear,
}

/// 存储层内部错误 → fluxdb 统一错误。
fn map_cred(err: CredentialError) -> Error {
    crate::credential::to_storage_error(&err)
}

/// 校验编辑内容（不触库）。
fn validate(edit: &ModelEdit) -> Result<()> {
    if edit.id.trim().is_empty() || edit.display_name.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "模型 ID 与名称不能为空"));
    }
    if edit.model.trim().is_empty() {
        return Err(Error::new(ErrorKind::Internal, "模型标识不能为空"));
    }
    if let Some(window) = edit.context_window_tokens
        && window < 1024
    {
        return Err(Error::new(ErrorKind::Internal, "上下文窗口至少 1024 Token"));
    }
    if let Some(out) = edit.max_output_tokens
        && out < 256
    {
        return Err(Error::new(ErrorKind::Internal, "最大输出至少 256 Token"));
    }
    for price in [
        edit.input_price_micros_per_million,
        edit.output_price_micros_per_million,
        edit.cached_input_price_micros_per_million,
    ] {
        if let Some(value) = price
            && value < 0
        {
            return Err(Error::new(ErrorKind::Internal, "计价不能为负"));
        }
    }
    if let Some(endpoint) = &edit.endpoint {
        // Endpoint 不得内嵌凭据或控制字符。
        if endpoint.contains('@')
            || endpoint.contains('?')
            || endpoint.chars().any(char::is_control)
        {
            return Err(Error::new(
                ErrorKind::Internal,
                "Endpoint 不能包含凭据、查询串或控制字符",
            ));
        }
    }
    Ok(())
}

/// 创建或更新模型；`expected_revision` 为 `None` 表示新建。
pub fn save_model(
    conn: &Connection,
    cipher: &Aes256Gcm,
    key_id: &str,
    now_ms: i64,
    edit: &ModelEdit,
    expected_revision: Option<i64>,
    change: ApiKeyChange,
) -> Result<ModelProfileView> {
    validate(edit)?;
    match expected_revision {
        None => insert_model(conn, cipher, key_id, now_ms, edit, change),
        Some(rev) => update_model(conn, cipher, key_id, now_ms, edit, rev, change),
    }
}

/// 删除模型及同一行的加密凭据；引用检查和 CAS 在同一个写事务内完成。
pub fn delete_model(conn: &Connection, id: &str, expected_revision: i64) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let row = load_model_row(&tx, id)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "模型不存在，请刷新列表"))?;
    if row.revision != expected_revision {
        return Err(Error::new(
            ErrorKind::Internal,
            "配置已被其他窗口修改，请重新加载",
        ));
    }
    // 默认主模型目前无外键，必须显式保护；其余引用仍保留数据库 RESTRICT 防线。
    let referenced: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_runtime_settings WHERE default_model_profile_id=?1 OR default_decision_model_profile_id=?1)
             OR EXISTS(SELECT 1 FROM ai_agents WHERE default_model_profile_id=?1)
             OR EXISTS(SELECT 1 FROM ai_agent_model_bindings WHERE model_profile_id=?1)
             OR EXISTS(SELECT 1 FROM ai_context_checkpoints WHERE model_profile_id=?1)
             OR EXISTS(SELECT 1 FROM ai_usage_ledger WHERE model_profile_id=?1)",
        [id], |row| row.get(0)).map_err(sqlite_error)?;
    if referenced {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型仍被默认设置、Agent 或历史会话引用，请先解除引用；历史会话引用的模型可改为停用",
        ));
    }
    let changed = tx
        .execute(
            "DELETE FROM ai_model_profiles WHERE id=?1 AND revision=?2",
            params![id, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(Error::new(ErrorKind::Internal, "模型已变化，请刷新后重试"));
    }
    tx.commit().map_err(sqlite_error)
}

fn insert_model(
    conn: &Connection,
    cipher: &Aes256Gcm,
    key_id: &str,
    now_ms: i64,
    edit: &ModelEdit,
    change: ApiKeyChange,
) -> Result<ModelProfileView> {
    if load_model_row(conn, &edit.id)?.is_some() {
        return Err(Error::new(ErrorKind::Internal, "模型已存在，请使用编辑"));
    }
    let (secret_revision, sealed) =
        apply_secret(cipher, &edit.id, key_id, 0, edit.auth_mode, change)?;
    conn.execute(
        "INSERT INTO ai_model_profiles (
            id, display_name, provider, api_protocol, model_kind, model, endpoint, enabled,
            auth_mode, context_window_tokens, max_output_tokens,
            input_price_micros_per_million, output_price_micros_per_million,
            cached_input_price_micros_per_million, revision, created_at_ms, updated_at_ms,
            secret_revision, credential_ciphertext, credential_nonce, cipher_version, key_id, provider_options_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1,?15,?15,?16,?17,?18,?19,?20,?21)",
        params![
            edit.id,
            edit.display_name,
            edit.provider,
            edit.api_protocol,
            edit.kind.as_str(),
            edit.model,
            edit.endpoint,
            edit.enabled as i64,
            edit.auth_mode.as_str(),
            edit.context_window_tokens,
            edit.max_output_tokens,
            edit.input_price_micros_per_million,
            edit.output_price_micros_per_million,
            edit.cached_input_price_micros_per_million,
            now_ms,
            secret_revision,
            sealed.as_ref().map(|s| s.ciphertext.clone()),
            sealed.as_ref().map(|s| s.nonce.clone()),
            sealed.as_ref().map(|s| s.cipher_version),
            sealed.as_ref().map(|s| s.key_id.clone()),
            serde_json::json!({"endpoint_is_full_url": edit.endpoint_is_full_url}).to_string(),
        ],
    )
    .map_err(sqlite_error)?;
    load_model(conn, &edit.id)
}

fn update_model(
    conn: &Connection,
    cipher: &Aes256Gcm,
    key_id: &str,
    now_ms: i64,
    edit: &ModelEdit,
    expected: i64,
    change: ApiKeyChange,
) -> Result<ModelProfileView> {
    let row = load_model_row(conn, &edit.id)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "模型不存在"))?;
    if row.revision != expected {
        return Err(Error::new(
            ErrorKind::Internal,
            "配置已被其他窗口修改，请重新加载",
        ));
    }
    let (secret_revision, sealed) = apply_secret(
        cipher,
        &edit.id,
        key_id,
        row.secret_revision,
        edit.auth_mode,
        change,
    )?;

    // Keep 路径完全不出现秘密列，确保改名不会重加密。
    let changed = match &sealed {
        Some(secret) => conn.execute(
            "UPDATE ai_model_profiles SET
                display_name=?2, provider=?3, api_protocol=?4, model_kind=?5, model=?6, endpoint=?7,
                enabled=?8, auth_mode=?9, context_window_tokens=?10, max_output_tokens=?11,
                input_price_micros_per_million=?12, output_price_micros_per_million=?13,
                cached_input_price_micros_per_million=?14, secret_revision=?15,
                credential_ciphertext=?16, credential_nonce=?17, cipher_version=?18, key_id=?19,
                revision=revision+1, updated_at_ms=?20,
                provider_options_json=json_set(provider_options_json, '$.endpoint_is_full_url', json(?22))
              WHERE id=?1 AND revision=?21",
            params![
                edit.id,
                edit.display_name,
                edit.provider,
                edit.api_protocol,
                edit.kind.as_str(),
                edit.model,
                edit.endpoint,
                edit.enabled as i64,
                edit.auth_mode.as_str(),
                edit.context_window_tokens,
                edit.max_output_tokens,
                edit.input_price_micros_per_million,
                edit.output_price_micros_per_million,
                edit.cached_input_price_micros_per_million,
                secret_revision,
                secret.ciphertext,
                secret.nonce,
                secret.cipher_version,
                secret.key_id,
                now_ms,
                expected,
                if edit.endpoint_is_full_url { "true" } else { "false" },
            ],
        ),
        None if row.secret_revision != secret_revision => conn.execute(
            "UPDATE ai_model_profiles SET
                display_name=?2, provider=?3, api_protocol=?4, model_kind=?5, model=?6, endpoint=?7,
                enabled=?8, auth_mode=?9, context_window_tokens=?10, max_output_tokens=?11,
                input_price_micros_per_million=?12, output_price_micros_per_million=?13,
                cached_input_price_micros_per_million=?14, secret_revision=?15,
                credential_ciphertext=NULL, credential_nonce=NULL, cipher_version=NULL, key_id=NULL,
                revision=revision+1, updated_at_ms=?16,
                provider_options_json=json_set(provider_options_json, '$.endpoint_is_full_url', json(?18))
              WHERE id=?1 AND revision=?17",
            params![
                edit.id,
                edit.display_name,
                edit.provider,
                edit.api_protocol,
                edit.kind.as_str(),
                edit.model,
                edit.endpoint,
                edit.enabled as i64,
                edit.auth_mode.as_str(),
                edit.context_window_tokens,
                edit.max_output_tokens,
                edit.input_price_micros_per_million,
                edit.output_price_micros_per_million,
                edit.cached_input_price_micros_per_million,
                secret_revision,
                now_ms,
                expected,
                if edit.endpoint_is_full_url { "true" } else { "false" },
            ],
        ),
        None => conn.execute(
            "UPDATE ai_model_profiles SET
                display_name=?2, provider=?3, api_protocol=?4, model_kind=?5, model=?6, endpoint=?7,
                enabled=?8, auth_mode=?9, context_window_tokens=?10, max_output_tokens=?11,
                input_price_micros_per_million=?12, output_price_micros_per_million=?13,
                cached_input_price_micros_per_million=?14,
                revision=revision+1, updated_at_ms=?15,
                provider_options_json=json_set(provider_options_json, '$.endpoint_is_full_url', json(?17))
              WHERE id=?1 AND revision=?16",
            params![
                edit.id,
                edit.display_name,
                edit.provider,
                edit.api_protocol,
                edit.kind.as_str(),
                edit.model,
                edit.endpoint,
                edit.enabled as i64,
                edit.auth_mode.as_str(),
                edit.context_window_tokens,
                edit.max_output_tokens,
                edit.input_price_micros_per_million,
                edit.output_price_micros_per_million,
                edit.cached_input_price_micros_per_million,
                now_ms,
                expected,
                if edit.endpoint_is_full_url { "true" } else { "false" },
            ],
        ),
    }
    .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "配置已被其他窗口修改，请重新加载",
        ));
    }
    load_model(conn, &edit.id)
}

/// 按变更意图计算新的 secret_revision 与密文（None 表示不写密文）。
fn apply_secret(
    cipher: &Aes256Gcm,
    profile_id: &str,
    key_id: &str,
    current_revision: i64,
    auth_mode: AuthMode,
    change: ApiKeyChange,
) -> Result<(i64, Option<EncryptedSecret>)> {
    match change {
        ApiKeyChange::Keep => Ok((current_revision, None)),
        ApiKeyChange::Clear => {
            if current_revision == 0 {
                Ok((current_revision, None))
            } else {
                Ok((current_revision + 1, None))
            }
        }
        ApiKeyChange::Set(input) => {
            if !auth_mode.requires_key() {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "当前认证方式不需要 API Key",
                ));
            }
            let next = current_revision + 1;
            let sealed =
                model_secret::seal(cipher, profile_id, key_id, next, &input).map_err(map_cred)?;
            Ok((next, Some(sealed)))
        }
    }
}

struct ModelRow {
    revision: i64,
    secret_revision: i64,
}

fn load_model_row(conn: &Connection, id: &str) -> Result<Option<ModelRow>> {
    conn.query_row(
        "SELECT revision, secret_revision FROM ai_model_profiles WHERE id=?1",
        [id],
        |row| {
            Ok(ModelRow {
                revision: row.get(0)?,
                secret_revision: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// 读取公开视图（不含密文列）。凭据状态只由“是否需要凭据 + 是否有密文”推导。
pub fn load_model(conn: &Connection, id: &str) -> Result<ModelProfileView> {
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        i64,
        String,
        Option<i64>,
        bool,
    );
    let row = conn
        .query_row(
            "SELECT id, display_name, provider, api_protocol, model_kind, model, endpoint, enabled,
                    auth_mode, context_window_tokens, max_output_tokens,
                    input_price_micros_per_million, output_price_micros_per_million,
                    cached_input_price_micros_per_million, secret_revision, revision,
                    credential_ciphertext IS NOT NULL, connection_status, connection_checked_at_ms,
                    COALESCE(json_extract(provider_options_json, '$.endpoint_is_full_url'), 0)
               FROM ai_model_profiles WHERE id=?1",
            [id],
            |row| -> rusqlite::Result<Row> {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                    row.get(19)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| Error::new(ErrorKind::Internal, "模型不存在"))?;
    let (
        id,
        display_name,
        provider,
        api_protocol,
        kind,
        model,
        endpoint,
        enabled,
        auth_mode,
        context_window_tokens,
        max_output_tokens,
        input_price,
        output_price,
        cached_price,
        secret_revision,
        revision,
        has_secret,
        connection_status,
        connection_checked_at_ms,
        endpoint_is_full_url,
    ) = row;
    let auth = AuthMode::parse(&auth_mode)?;
    Ok(ModelProfileView {
        id,
        display_name,
        provider,
        api_protocol,
        kind: ModelKind::parse(&kind)?,
        model,
        endpoint,
        endpoint_is_full_url,
        enabled: enabled != 0,
        auth_mode: auth,
        context_window_tokens,
        max_output_tokens,
        input_price_micros_per_million: input_price,
        output_price_micros_per_million: output_price,
        cached_input_price_micros_per_million: cached_price,
        credential_status: CredentialStatus::from_stored(auth.requires_key(), has_secret != 0),
        secret_revision,
        connection_status,
        connection_checked_at_ms,
        revision,
    })
}

/// 读取模型行的密文材料（运行时解密调用用；不进入任何公开视图）。
///
/// 调用方负责尽快使用并在用完后丢弃返回值，不得写入日志/事件/快照。
pub fn load_encrypted_secret(conn: &Connection, id: &str) -> Result<Option<EncryptedSecret>> {
    let row = conn
        .query_row(
            "SELECT credential_ciphertext, credential_nonce, cipher_version, key_id, secret_revision
               FROM ai_model_profiles WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((ciphertext, nonce, cipher_version, key_id, secret_revision)) = row else {
        return Err(Error::new(ErrorKind::Internal, "模型不存在"));
    };
    Ok(match (ciphertext, nonce, cipher_version, key_id) {
        (Some(ciphertext), Some(nonce), Some(cipher_version), Some(key_id)) => {
            Some(EncryptedSecret {
                ciphertext,
                nonce,
                cipher_version,
                key_id,
                secret_revision,
            })
        }
        _ => None,
    })
}

/// 分页列出模型（按显示名排序；只返回公开视图）。
pub fn list_models_page(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> Result<Vec<ModelProfileView>> {
    let mut stmt = conn
        .prepare("SELECT id FROM ai_model_profiles ORDER BY display_name, id LIMIT ?1 OFFSET ?2")
        .map_err(sqlite_error)?;
    let ids = stmt
        .query_map(params![limit, offset], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    ids.into_iter().map(|id| load_model(conn, &id)).collect()
}

/// 统计模型总数（分页用）。
pub fn count_models(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM ai_model_profiles", [], |row| {
        row.get(0)
    })
    .map_err(sqlite_error)
}

/// 切换模型启用状态（对象级 CAS）。
pub fn set_model_enabled(
    conn: &Connection,
    now_ms: i64,
    id: &str,
    enabled: bool,
    expected_revision: i64,
) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE ai_model_profiles SET enabled=?2, revision=revision+1, updated_at_ms=?3
              WHERE id=?1 AND revision=?4",
            params![id, enabled as i64, now_ms, expected_revision],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(
            ErrorKind::Internal,
            "模型配置已被其他窗口修改，请重新加载",
        ));
    }
    Ok(())
}

/// 写入最近一次连接探测结果（运行时元数据）。
///
/// 该更新**不 bump `revision`**：它不改变用户配置，不应让打开中的编辑弹框失效。
/// `status` 只接受 `unknown`/`ok`/`failed`。
pub fn set_connection_status(conn: &Connection, now_ms: i64, id: &str, status: &str) -> Result<()> {
    if !matches!(status, "unknown" | "ok" | "failed") {
        return Err(Error::new(
            ErrorKind::Internal,
            format!("非法连接状态: {status}"),
        ));
    }
    let changed = conn
        .execute(
            "UPDATE ai_model_profiles
                SET connection_status=?2, connection_checked_at_ms=?3
              WHERE id=?1",
            params![id, status, now_ms],
        )
        .map_err(sqlite_error)?;
    if changed == 0 {
        return Err(Error::new(ErrorKind::Internal, "模型不存在"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_platform::ensure_schema;
    use crate::secret_crypto::load_cipher;

    fn setup() -> (Connection, Aes256Gcm) {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("外键");
        ensure_schema(&conn).expect("建表");
        let dir = std::env::temp_dir().join(format!("fluxdb-models-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (cipher, _) = load_cipher(&dir).expect("密钥");
        (conn, cipher)
    }

    fn edit() -> ModelEdit {
        ModelEdit {
            id: "m1".into(),
            display_name: "主模型".into(),
            provider: "兼容 API".into(),
            api_protocol: "openai-compatible".into(),
            kind: ModelKind::Generation,
            model: "main-model".into(),
            endpoint: Some("https://model.example.invalid/v1".into()),
            endpoint_is_full_url: false,
            enabled: true,
            auth_mode: AuthMode::ApiKey,
            context_window_tokens: Some(400_000),
            max_output_tokens: Some(32_000),
            input_price_micros_per_million: Some(2_500),
            output_price_micros_per_million: Some(10_000),
            cached_input_price_micros_per_million: Some(250),
        }
    }

    fn created() -> (Connection, Aes256Gcm, ModelProfileView) {
        let (conn, cipher) = setup();
        let view = save_model(
            &conn,
            &cipher,
            "k1",
            1_000,
            &edit(),
            None,
            ApiKeyChange::Set(SecretInput::new("sk-original").unwrap()),
        )
        .expect("新建模型");
        (conn, cipher, view)
    }

    #[test]
    fn create_then_view_hides_cipher() {
        let (_conn, _cipher, view) = created();
        assert_eq!(view.revision, 1);
        assert_eq!(
            view.credential_status,
            CredentialStatus::Configured,
            "视图只依据密文是否存在推导状态，不含密文本身"
        );
        assert_eq!(view.secret_revision, 1);
        assert_eq!(view.context_window_tokens, Some(400_000));
    }

    #[test]
    fn keep_does_not_touch_secret_columns() {
        let (conn, cipher, view) = created();
        let mut renamed = edit();
        renamed.display_name = "改名".into();
        let saved = save_model(
            &conn,
            &cipher,
            "k1",
            2_000,
            &renamed,
            Some(view.revision),
            ApiKeyChange::Keep,
        )
        .expect("改名");
        assert_eq!(saved.display_name, "改名");
        assert_eq!(saved.secret_revision, 1, "Keep 不递增密钥代次");
        assert_eq!(saved.revision, 2);
    }

    #[test]
    fn stale_revision_is_rejected() {
        let (conn, cipher, _) = created();
        let mut e = edit();
        e.display_name = "并发写入".into();
        let err = save_model(
            &conn,
            &cipher,
            "k1",
            3_000,
            &e,
            Some(99),
            ApiKeyChange::Keep,
        )
        .expect_err("过期 revision 必须拒绝");
        assert!(err.message.contains("重新加载"));
    }

    #[test]
    fn clear_removes_cipher_and_bumps_revision() {
        let (conn, cipher, view) = created();
        let saved = save_model(
            &conn,
            &cipher,
            "k1",
            4_000,
            &edit(),
            Some(view.revision),
            ApiKeyChange::Clear,
        )
        .expect("清除");
        assert_eq!(saved.secret_revision, 2);
        let has: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ai_model_profiles WHERE id='m1' AND credential_ciphertext IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has, 1, "清除后密文列必须为空");
    }

    #[test]
    fn set_replaces_cipher_and_bumps_revision() {
        let (conn, cipher, view) = created();
        let saved = save_model(
            &conn,
            &cipher,
            "k1",
            5_000,
            &edit(),
            Some(view.revision),
            ApiKeyChange::Set(SecretInput::new("sk-new").unwrap()),
        )
        .expect("替换");
        assert_eq!(saved.secret_revision, 2);
    }

    #[test]
    fn set_on_auth_none_is_rejected() {
        let (conn, cipher, _) = created();
        let mut e = edit();
        e.id = "m-no-auth".into();
        e.auth_mode = AuthMode::None;
        let err = save_model(
            &conn,
            &cipher,
            "k1",
            6_000,
            &e,
            None,
            ApiKeyChange::Set(SecretInput::new("sk").unwrap()),
        )
        .expect_err("无需凭据的模型不能设置 Key");
        assert!(err.message.contains("不需要"), "实际错误: {}", err.message);
    }

    #[test]
    fn endpoint_with_credentials_is_rejected() {
        let (conn, cipher, _) = created();
        let mut e = edit();
        e.endpoint = Some("https://user:pass@host/v1".into());
        assert!(save_model(&conn, &cipher, "k1", 7_000, &e, None, ApiKeyChange::Keep).is_err());
    }

    #[test]
    fn small_context_window_is_rejected() {
        let (conn, cipher, _) = created();
        let mut e = edit();
        e.context_window_tokens = Some(10);
        assert!(save_model(&conn, &cipher, "k1", 8_000, &e, None, ApiKeyChange::Keep).is_err());
    } // 分页读取
    #[test]
    fn page_and_count_cover_all_models() {
        let (conn, _) = setup();
        for i in 0..3 {
            let mut e = edit();
            e.id = format!("m{i}");
            save_model(
                &conn,
                &crate::secret_crypto::load_cipher(&std::env::temp_dir())
                    .unwrap()
                    .0,
                "k1",
                1_000,
                &e,
                None,
                ApiKeyChange::Keep,
            )
            .expect("新建");
        }
        assert_eq!(count_models(&conn).unwrap(), 3);
        let page = list_models_page(&conn, 2, 0).expect("第一页");
        assert_eq!(page.len(), 2);
        let page2 = list_models_page(&conn, 2, 2).expect("第二页");
        assert_eq!(page2.len(), 1);
    }
}
