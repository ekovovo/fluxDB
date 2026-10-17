//! 执行策略复用运行设置的 revision/CAS；不把模型凭据或业务数据放进配置。
use fluxdb_core::{Error, ErrorKind, ExecutionPolicySettings, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::sqlite::sqlite_error;

fn parse_policy(json: &str) -> Result<ExecutionPolicySettings> {
    serde_json::from_str(json)
        .map_err(|_| Error::new(ErrorKind::Internal, "执行策略配置无效，拒绝执行"))
}

pub fn load(conn: &Connection) -> Result<(ExecutionPolicySettings, i64)> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT execution_policy_json, revision FROM ai_runtime_settings WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match row {
        None => Ok((ExecutionPolicySettings::default(), 0)),
        Some((json, revision)) => {
            let policy = parse_policy(&json)?;
            Ok((policy, revision))
        }
    }
}

pub fn save(
    conn: &Connection,
    now: i64,
    policy: &ExecutionPolicySettings,
    expected_revision: i64,
) -> Result<i64> {
    // 阈值与权重必须先通过区间校验：越界配置会让自动放行范围失真。
    policy.validate()?;
    let json = serde_json::to_string(policy)
        .map_err(|_| Error::new(ErrorKind::Internal, "执行策略序列化失败"))?;
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let count = if expected_revision == 0 {
        tx.execute(
            "INSERT INTO ai_runtime_settings(id,max_active_turns,execution_policy_json,revision,updated_at_ms)
             VALUES(1,2,?1,1,?2) ON CONFLICT(id) DO NOTHING",
            params![json, now],
        ).map_err(sqlite_error)?
    } else {
        tx.execute(
            "UPDATE ai_runtime_settings SET execution_policy_json=?1,revision=revision+1,
             updated_at_ms=?2 WHERE id=1 AND revision=?3",
            params![json, now, expected_revision],
        )
        .map_err(sqlite_error)?
    };
    if count != 1 {
        return Err(Error::new(
            ErrorKind::Internal,
            "执行策略已变化，请刷新后重试",
        ));
    }
    // 策略变化使等待中的批准失效；已发出的操作无法撤回，不进行自动重放。
    tx.execute(
        "UPDATE ai_approvals SET status='invalidated',reason_code='execution_policy_changed',
         decided_at_ms=?1,revision=revision+1 WHERE status IN ('pending','approved')",
        [now],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(expected_revision + 1)
}
